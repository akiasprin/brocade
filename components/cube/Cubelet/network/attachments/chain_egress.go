// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"strings"

	"github.com/tencentcloud/CubeSandbox/CubeNet/cubevs"
	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
	"github.com/vishvananda/netlink"
	"golang.org/x/sys/unix"
)

func validateChainBindings(req wire.Request, routing assignedRouting) error {
	if len(req.ChainEgress) > 256 {
		return errors.New("too many chain exit bindings")
	}
	bindings := map[string]bool{}
	marks := map[uint32]bool{}
	for _, b := range req.ChainEgress {
		if !safeID.MatchString(b.ChainID) || bindings[b.ChainID] || marks[b.Mark] || b.Mark&0xf0000000 != 0xb0000000 || !interfaceName(b.Device) || !strings.HasPrefix(b.Device, "bcg") {
			return errors.New("invalid or duplicate chain exit binding")
		}
		bindings[b.ChainID] = true
		marks[b.Mark] = true
	}
	referenced := map[string]bool{}
	for _, r := range routing.Routes {
		if r.ChainID != "" {
			if r.InterfaceID != "" || !bindings[r.ChainID] {
				return errors.New("chain route has no frozen binding")
			}
			referenced[r.ChainID] = true
		}
	}
	for chain := range bindings {
		if !referenced[chain] {
			return errors.New("unreferenced chain exit binding")
		}
	}
	return nil
}

// GuardPrimaryRoutes installs the terminal classifier before the primary NAT policy
// can be relaxed. It changes no Guest device or route and requires the real runtime owner.
func (m *Manager) GuardPrimaryRoutes(ctx context.Context, req wire.Request) (wire.Result, error) {
	result := wire.Result{Contract: wire.Contract}
	d, _, _, err := m.desired(req)
	if err != nil {
		return result, err
	}
	if !m.operation.TryLock() {
		return result, errors.New("primary route reconciliation is busy")
	}
	defer m.operation.Unlock()
	s := m.server
	s.ctx = ctx
	r := s.records[req.InstanceID]
	if r == nil || r.Runtime != req.RuntimeID || r.Deleted || req.Generation < r.Generation {
		return result, errors.New("primary route runtime assignment is not current")
	}
	info, err := s.vmInfo(r)
	if err != nil {
		return result, err
	}
	copy := *r
	copy.ChainEgress = req.ChainEgress
	if err = s.ensurePrimaryRoutes(&copy, d, info); err != nil {
		return result, err
	}
	r.PrimaryIPv6 = copy.PrimaryIPv6
	r.PrimaryGatewayMAC = copy.PrimaryGatewayMAC
	return result, s.persist(r)
}

func ensurePrimaryIPv6Route() error {
	link, e := netlink.LinkByName("cube-dev")
	if e != nil {
		return e
	}
	if link.Type() != "dummy" {
		return errors.New("primary delivery device is not Cube's dummy gateway")
	}
	_, prefix, e := net.ParseCIDR(cubevs.PrimaryChainIPv6Prefix)
	if e != nil {
		return e
	}
	routes, e := netlink.RouteListFiltered(unix.AF_INET6, &netlink.Route{Dst: prefix, Table: 254}, netlink.RT_FILTER_DST|netlink.RT_FILTER_TABLE)
	if e != nil {
		return e
	}
	for _, r := range routes {
		if r.LinkIndex != link.Attrs().Index || r.Protocol != 186 {
			return errors.New("primary chain IPv6 prefix is already owned by another route")
		}
	}
	return netlink.RouteReplace(&netlink.Route{Dst: prefix, LinkIndex: link.Attrs().Index, Table: 254, Protocol: 186, Scope: netlink.SCOPE_LINK})
}

func (s *server) ensurePrimaryRoutes(r *record, d object, info vmmInfo) error {
	if len(r.ChainEgress) == 0 && r.PrimaryIPv6 == "" {
		return nil
	}
	if !cubevs.PrimaryChainReady() {
		return errors.New("loaded CubeVS has no verified chain egress classifier")
	}
	var routing assignedRouting
	raw, e := json.Marshal(obj(obj(d, "spec"), "routing"))
	if e != nil {
		return e
	}
	if e = json.Unmarshal(raw, &routing); e != nil {
		return e
	}
	marks := map[string]uint32{}
	for _, b := range r.ChainEgress {
		marks[b.ChainID] = b.Mark
	}
	routes := []cubevs.PrimaryRoute{}
	for _, v4 := range []bool{true, false} {
		for _, item := range orderedAssignedRoutes(routing, v4) {
			row := cubevs.PrimaryRoute{Destination: item.prefix.String()}
			if item.route.ChainID != "" {
				row.Mark = marks[item.route.ChainID]
				if row.Mark == 0 {
					return errors.New("primary chain selector has no assigned mark")
				}
			}
			if service := item.route.Service; service != nil {
				row.Protocol, row.PortStart, row.PortEnd = service.Protocol, service.PortStart, service.PortEnd
			}
			routes = append(routes, row)
		}
	}
	for _, nic := range info.Config.Net {
		if strings.HasPrefix(nic.ID, "cvm-") {
			continue
		}
		if !interfaceName(nic.Tap) {
			return errors.New("invalid primary TAP name")
		}
		link, e := net.InterfaceByName(nic.Tap)
		if e != nil {
			return e
		}
		ip6, e := cubevs.SetPrimaryRoutes(uint32(link.Index), r.Runtime, number(d, "generation"), nic.MAC, routes)
		if e != nil {
			return e
		}
		if e = ensurePrimaryIPv6Route(); e != nil {
			return e
		}
		r.PrimaryIPv6 = ip6.String()
		gateway, e := net.InterfaceByName("cube-dev")
		if e != nil {
			return e
		}
		r.PrimaryGatewayMAC = gateway.HardwareAddr.String()
		return nil
	}
	return errors.New("original primary TAP is not observed")
}
