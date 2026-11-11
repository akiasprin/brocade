// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
	"net"
	"net/netip"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

func allocatedNIC(nic object) (*wire.AllocatedNIC, error) {
	raw, err := json.Marshal(nic["allocated"])
	if err != nil {
		return nil, err
	}
	var assigned *wire.AllocatedNIC
	if json.Unmarshal(raw, &assigned) != nil || assigned == nil {
		return nil, errors.New("business NIC requires a Store allocation")
	}
	if !safeID.MatchString(assigned.ResourceDomain) || !safeID.MatchString(assigned.AllocationID) || !safeID.MatchString(assigned.PublicationID) || assigned.PublicationID != str(nic, "source_id") || assigned.MTU < 1280 || assigned.MTU > 1500 {
		return nil, errors.New("invalid Store NIC assignment")
	}
	mac, err := net.ParseMAC(assigned.MAC)
	if err != nil || len(mac) != 6 || mac[0]&3 != 2 {
		return nil, errors.New("invalid assigned NIC MAC")
	}
	if assigned.IPv4 == "" && assigned.IPv6 == "" {
		return nil, errors.New("assigned NIC requires an address")
	}
	for _, family := range []struct {
		raw string
		v4  bool
	}{{assigned.IPv4, true}, {assigned.IPv6, false}} {
		if family.raw == "" {
			continue
		}
		ip, err := netip.ParseAddr(family.raw)
		if err != nil || ip.Is4() != family.v4 || ip.Is4In6() || ip.IsUnspecified() || ip.IsMulticast() || ip.IsLoopback() {
			return nil, errors.New("invalid assigned NIC address")
		}
	}
	return assigned, nil
}
func assignedPrefix(address string) string {
	if address == "" {
		return ""
	}
	ip, err := netip.ParseAddr(address)
	if err != nil {
		return ""
	}
	return netip.PrefixFrom(ip, ip.BitLen()).String()
}
func (s *server) sourceFor(a *attachment) bridgeSource {
	if a.Allocated == nil {
		return bridgeSource{}
	}
	source := bridgeSource{Bridge: "bn" + hash([]byte(a.ID))[:12]}
	if a.IPv4 != "" {
		source.IPv4Pool = a.IPv4
		source.IPv4Gateway = "169.254.128.1"
	}
	if a.IPv6 != "" {
		source.IPv6Pool = a.IPv6
		source.IPv6Gateway = "fe80::1"
	}
	return source
}
func (s *server) applyAssigned(r *record, nic object, a *attachment) error {
	assigned, err := allocatedNIC(nic)
	if err != nil {
		return err
	}
	for owner, other := range s.records {
		if other.Deleted {
			continue
		}
		for id, device := range other.NICs {
			if device.Removed || (owner == r.Instance && id == a.ID) {
				continue
			}
			if id == a.ID {
				return errors.New("Store NIC identity already belongs to another container")
			}
			if owner != r.Instance && (device.Allocated == nil || device.Allocated.ResourceDomain != assigned.ResourceDomain) {
				continue
			}
			for _, address := range []string{assignedPrefix(assigned.IPv4), assignedPrefix(assigned.IPv6)} {
				if address != "" && (address == device.IPv4 || address == device.IPv6) {
					return errors.New("assigned address still belongs to another NIC")
				}
			}
		}
	}
	if a.Allocated != nil && !strings.EqualFold(a.MAC, assigned.MAC) {
		return errors.New("Store changed a stable NIC MAC")
	}
	ipv4, ipv6 := assignedPrefix(assigned.IPv4), assignedPrefix(assigned.IPv6)
	if a.IPv4 != "" && a.IPv4 != ipv4 {
		a.PreviousIPv4 = a.IPv4
	}
	if a.IPv6 != "" && a.IPv6 != ipv6 {
		a.PreviousIPv6 = a.IPv6
	}
	a.Allocated = assigned
	a.MAC = strings.ToLower(assigned.MAC)
	a.IPv4 = ipv4
	a.IPv6 = ipv6
	// Transport revisions do not change the device/address identity.
	raw, _ := json.Marshal([]string{assigned.AllocationID, a.MAC, a.IPv4, a.IPv6})
	a.SourceSHA256 = hash(raw)
	return nil
}
func assignedGuardName(a *attachment) string { return "bcg" + hash([]byte(a.ID))[:12] }
func nftOwned(ctx context.Context, a *attachment) (bool, error) {
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	output, err := exec.CommandContext(bounded, "nft", "-j", "list", "table", "bridge", assignedGuardName(a)).Output()
	if err != nil {
		// Listing the table namespace distinguishes absence from an unavailable nft executable.
		output, err = exec.CommandContext(bounded, "nft", "-j", "list", "tables").Output()
		if err != nil {
			return false, errors.New("cannot inspect assigned NIC guard")
		}
		var value struct {
			NFTables []struct{ Table struct{ Family, Name string } } `json:"nftables"`
		}
		if json.Unmarshal(output, &value) != nil {
			return false, errors.New("invalid nftables inventory")
		}
		for _, entry := range value.NFTables {
			if entry.Table.Family == "bridge" && entry.Table.Name == assignedGuardName(a) {
				return false, errors.New("assigned NIC guard could not be inspected")
			}
		}
		return false, nil
	}
	var value struct {
		NFTables []struct{ Table struct{ Comment string } } `json:"nftables"`
	}
	if json.Unmarshal(output, &value) != nil {
		return false, errors.New("invalid assigned NIC guard inventory")
	}
	for _, entry := range value.NFTables {
		if entry.Table.Comment == "brocade-nic:"+a.ID {
			return true, nil
		}
	}
	return false, errors.New("assigned NIC guard belongs to another owner")
}
func nftApply(ctx context.Context, script string) error {
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	command := exec.CommandContext(bounded, "nft", "-f", "-")
	command.Stdin = strings.NewReader(script)
	if command.Run() != nil {
		return errors.New("assigned NIC guard operation failed")
	}
	return nil
}
func (s *server) ensureAssignedGuard(a *attachment) error {
	existing, err := nftOwned(s.ctx, a)
	if err != nil {
		return err
	}
	table := assignedGuardName(a)
	var script strings.Builder
	if existing {
		fmt.Fprintf(&script, "delete table bridge %s\n", table)
	}
	fmt.Fprintf(&script, "add table bridge %s { comment \"brocade-nic:%s\"; }\nadd chain bridge %s forward { type filter hook forward priority -200; policy accept; }\n", table, a.ID, table)
	rule := func(text string) { fmt.Fprintf(&script, "add rule bridge %s forward %s\n", table, text) }
	input := fmt.Sprintf("iifname \"%s\"", a.Tap)
	output := fmt.Sprintf("oifname \"%s\"", a.Tap)
	rule(input + " ether saddr != " + a.MAC + " drop")
	rule(input + " ether type != { ip, ip6, arp } drop")
	if a.Allocated.Blocked || a.Allocated.QuotaExceeded {
		rule(input + " ether type { ip, ip6 } drop")
		rule(output + " ether type { ip, ip6 } drop")
	}
	if a.Allocated.IPv4 != "" {
		rule(input + " ether type ip ip saddr != " + a.Allocated.IPv4 + " drop")
		rule(output + " ether type ip ip daddr != " + a.Allocated.IPv4 + " drop")
		rule(input + " ether type arp arp saddr ip != { 0.0.0.0, " + a.Allocated.IPv4 + " } drop")
		rule(input + " ether type arp arp saddr ether != " + a.MAC + " drop")
	} else {
		rule(input + " ether type { ip, arp } drop")
		rule(output + " ether type ip drop")
	}
	// Local neighbor discovery is required by the Ethernet-facing Guest device;
	// it does not extend a remote broadcast domain through the IP tunnel.
	rule(input + " ether type ip6 ip6 saddr { ::, fe80::/10 } icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert } accept")
	rule(output + " ether type ip6 icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert } accept")
	if a.Allocated.IPv6 != "" {
		rule(input + " ether type ip6 ip6 saddr != " + a.Allocated.IPv6 + " drop")
		rule(output + " ether type ip6 ip6 daddr != " + a.Allocated.IPv6 + " drop")
	} else {
		rule(input + " ether type ip6 drop")
		rule(output + " ether type ip6 drop")
	}
	return nftApply(s.ctx, script.String())
}
func (s *server) removeAssignedGuard(a *attachment) error {
	exists, err := nftOwned(s.ctx, a)
	if err != nil {
		return err
	}
	if !exists {
		return nil
	}
	return nftApply(s.ctx, "delete table bridge "+assignedGuardName(a)+"\n")
}
func checkAssignedBridge(source bridgeSource, a *attachment) error {
	data, err := os.ReadFile(filepath.Join("/sys/class/net", source.Bridge, "ifalias"))
	if err != nil || strings.TrimSpace(string(data)) != "brocade-nic:"+a.ID {
		return errors.New("assigned NIC bridge is not prepared by its network executor")
	}
	return nil
}
