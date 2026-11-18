// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import (
	"encoding/binary"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"os"
	"strings"

	"github.com/cilium/ebpf"
)

const primaryChainMap = "primary_chain_routes"
const PrimaryChainIPv6Prefix = "fd68:bcad:0:1::/96"

func removePrimaryRoutes(index, version uint32) error {
	m, err := loadPinnedMap(primaryChainMap)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer m.Close()
	var value mvmtapChainRoutes
	if err = m.Lookup(index, &value); errors.Is(err, ebpf.ErrKeyNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if value.Version != version {
		return errors.New("primary selector belongs to another TAP incarnation")
	}
	return m.Delete(index)
}

type PrimaryRoute struct {
	Destination        string
	Protocol           string
	PortStart, PortEnd uint16
	Mark               uint32
}

// PrimaryChainReady checks the loaded map/program relationship. A new userspace
// binary beside an old BPF generation must never advertise this capability.
func PrimaryChainReady() bool {
	m, err := loadPinnedMap(primaryChainMap)
	if err != nil {
		return false
	}
	defer m.Close()
	id, ok := func() (ebpf.MapID, bool) {
		i, e := m.Info()
		if e != nil {
			return 0, false
		}
		return i.ID()
	}()
	if !ok {
		return false
	}
	for _, name := range []string{programNameFromCube, programNameFromEnvoy} {
		p, e := ebpf.LoadPinnedProgram(pinPath(name), nil)
		if e != nil {
			return false
		}
		info, e := p.Info()
		p.Close()
		if e != nil {
			return false
		}
		ids, ok := info.MapIDs()
		if !ok {
			return false
		}
		found := false
		for _, used := range ids {
			found = found || used == id
		}
		if !found {
			return false
		}
	}
	return true
}

func PrimaryChainIPv6(address net.IP) (netip.Addr, error) {
	ip := address.To4()
	if ip == nil {
		return netip.Addr{}, errors.New("primary chain requires a Cube delivery IPv4")
	}
	bytes := netip.MustParsePrefix(PrimaryChainIPv6Prefix).Addr().As16()
	copy(bytes[12:], ip)
	return netip.AddrFrom16(bytes), nil
}

// SetPrimaryRoutes atomically installs the complete sorted classifier. The TAP
// incarnation and sandbox owner are checked against the native CubeVS ledger.
func SetPrimaryRoutes(index uint32, owner string, generation uint64, mac string, routes []PrimaryRoute) (netip.Addr, error) {
	if index == 0 || generation == 0 || len(routes) > 256 {
		return netip.Addr{}, errors.New("invalid primary route assignment")
	}
	m, e := loadPinnedMap(MapNameIfindexToMVMMetadata)
	if e != nil {
		return netip.Addr{}, e
	}
	defer m.Close()
	var meta mvmMetadata
	if e = m.Lookup(index, &meta); e != nil {
		return netip.Addr{}, e
	}
	if strings.TrimRight(string(meta.UUID[:]), "\x00") != owner {
		return netip.Addr{}, errors.New("primary route TAP has another owner")
	}
	ip := make(net.IP, 4)
	binary.NativeEndian.PutUint32(ip, meta.IP)
	ip6, e := PrimaryChainIPv6(ip)
	if e != nil {
		return ip6, e
	}
	hardware, e := net.ParseMAC(mac)
	if e != nil || len(hardware) != 6 {
		return ip6, errors.New("invalid primary route MAC")
	}
	value := mvmtapChainRoutes{Version: meta.Version, Generation: generation, Count: uint32(len(routes))}
	copy(value.Mac[:], hardware)
	for i, r := range routes {
		prefix, e := netip.ParsePrefix(r.Destination)
		if e != nil || prefix != prefix.Masked() {
			return ip6, errors.New("invalid primary exit prefix")
		}
		row := &value.Routes[i]
		row.Bits = uint8(prefix.Bits())
		row.Family = 6
		row.Mark = r.Mark
		if prefix.Addr().Is4() {
			row.Family = 4
			ip := prefix.Addr().As4()
			copy(row.Address[:], ip[:])
		} else {
			row.Address = prefix.Addr().As16()
		}
		switch r.Protocol {
		case "":
		case "tcp":
			row.Protocol = 6
		case "udp":
			row.Protocol = 17
		default:
			return ip6, errors.New("invalid primary exit protocol")
		}
		if r.Protocol != "" && (r.PortStart == 0 || r.PortEnd < r.PortStart) {
			return ip6, errors.New("invalid primary exit ports")
		}
		row.PortStart, row.PortEnd = r.PortStart, r.PortEnd
	}
	target, e := loadPinnedMap(primaryChainMap)
	if e != nil {
		return ip6, e
	}
	defer target.Close()
	if e = target.Update(index, value, ebpf.UpdateAny); e != nil {
		return ip6, fmt.Errorf("install primary exit classifier: %w", e)
	}
	return ip6, nil
}
