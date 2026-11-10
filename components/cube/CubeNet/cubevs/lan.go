// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import (
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"sync"
	"syscall"

	"github.com/cilium/ebpf"
	"github.com/florianl/go-tc"
	"golang.org/x/sys/unix"
)

const lanEndpointMap = "cube_lan_ep_v1"
const lanPrefixMap = "cube_lan_pfx_v1"
const lanFlowMap = "cube_lan_ct_v1"
const lanStatsMap = "cube_lan_stat_v1"

// LANEndpoint is a native CubeVS attachment. Its token binds the stable
// sandbox/interface identity; generation commits a complete policy. The bridge
// and host FIB remain the LAN forwarding substrate, behind CubeVS on both TAP
// directions. This does not alter the established primary-NIC IPv4 NAT ABI.
type LANEndpoint struct {
	SandboxID   string
	InterfaceID string
	Ifindex     uint32
	Generation  uint64
	MAC         string
	IPv4        string
	IPv6        string
	AllowIPv4   bool
	AllowIPv6   bool
	Connected   []string
}

var lanLoadMutex sync.Mutex
var lanCollection *ebpf.Collection
var openLANMap = ebpf.LoadPinnedMap

func endpointToken(sandbox, id string) uint64 {
	h := sha256.Sum256([]byte(sandbox + ":" + id))
	return binary.LittleEndian.Uint64(h[:8])
}

func lanMetadata(e LANEndpoint) (lanLanEndpoint, []lanLanPrefix, error) {
	m := lanLanEndpoint{Token: endpointToken(e.SandboxID, e.InterfaceID), Generation: e.Generation, Active: 1}
	if e.SandboxID == "" || e.InterfaceID == "" || e.Ifindex == 0 || e.Generation == 0 || len(e.Connected) > 32 {
		return m, nil, errors.New("invalid CubeVS LAN attachment identity")
	}
	mac, err := net.ParseMAC(e.MAC)
	if err != nil || len(mac) != 6 || mac[0]&1 != 0 {
		return m, nil, errors.New("invalid CubeVS LAN MAC")
	}
	copy(m.Mac[:], mac)
	p, err := netip.ParsePrefix(e.IPv4)
	if err != nil || !p.Addr().Is4() {
		return m, nil, errors.New("invalid CubeVS LAN IPv4")
	}
	ip4 := p.Addr().As4()
	copy(m.Ipv4[:], ip4[:])
	m.Linklocal = [16]uint8{0xfe, 0x80, 0, 0, 0, 0, 0, 0, mac[0] ^ 2, mac[1], mac[2], 0xff, 0xfe, mac[3], mac[4], mac[5]}
	if e.IPv6 != "" {
		p, err = netip.ParsePrefix(e.IPv6)
		if err != nil || !p.Addr().Is6() || p.Addr().Is4In6() {
			return m, nil, errors.New("invalid CubeVS LAN IPv6")
		}
		m.Ipv6 = p.Addr().As16()
	}
	if e.AllowIPv4 {
		m.Flags |= 1
	}
	if e.AllowIPv6 {
		m.Flags |= 2
	}
	prefixes := make([]lanLanPrefix, 0, len(e.Connected))
	for _, raw := range e.Connected {
		p, err := netip.ParsePrefix(raw)
		if err != nil {
			return m, nil, errors.New("invalid CubeVS connected prefix")
		}
		p = p.Masked()
		key := lanLanPrefix{Ifindex: e.Ifindex, Generation: e.Generation, Family: 6, Prefixlen: uint32(128 + p.Bits())}
		if p.Addr().Is4() {
			key.Family = 4
			a := p.Addr().As4()
			copy(key.Address[:], a[:])
		} else {
			key.Address = p.Addr().As16()
		}
		prefixes = append(prefixes, key)
	}
	return m, prefixes, nil
}

func loadLAN() (*ebpf.Collection, error) {
	lanLoadMutex.Lock()
	defer lanLoadMutex.Unlock()
	if lanCollection != nil {
		return lanCollection, nil
	}
	spec, err := loadLan()
	if err != nil {
		return nil, err
	}
	if err := resizePinnedMaps(spec); err != nil {
		return nil, err
	}
	obj, err := ebpf.NewCollectionWithOptions(spec, ebpf.CollectionOptions{Maps: ebpf.MapOptions{PinPath: bpfFSPath}})
	if err != nil {
		return nil, fmt.Errorf("load CubeVS dual-stack LAN programs: %w", err)
	}
	lanCollection = obj
	return obj, nil
}

// LANReady verifies the actual BPF verifier/map ABI prerequisites. Failed
// readiness never removes an existing attachment or its cleanup state.
func LANReady() error { _, err := loadLAN(); return err }

type LANObservation struct {
	Ifindex    uint32 `json:"ifindex"`
	Generation uint64 `json:"generation"`
	Token      uint64 `json:"token"`
	Active     bool   `json:"active"`
	Drops      uint64 `json:"drops"`
}

// InspectLANEndpoint reads actual pinned kernel state for local diagnostics.
func InspectLANEndpoint(index uint32) (LANObservation, error) {
	result := LANObservation{Ifindex: index}
	m, err := openLANMap(pinPath(lanEndpointMap), nil)
	if err != nil {
		return result, err
	}
	defer m.Close()
	var endpoint lanLanEndpoint
	if err := m.Lookup(index, &endpoint); err != nil {
		return result, err
	}
	stats, err := openLANMap(pinPath(lanStatsMap), nil)
	if err != nil {
		return result, err
	}
	defer stats.Close()
	var counter lanLanCounter
	if err := stats.Lookup(index, &counter); err != nil {
		return result, err
	}
	if counter.Token != endpoint.Token {
		return result, errors.New("CubeVS LAN observation identity mismatch")
	}
	result.Generation = endpoint.Generation
	result.Token = endpoint.Token
	result.Active = endpoint.Active != 0
	result.Drops = counter.Drops
	return result, nil
}

func lanFilter(index uint32, direction TCDirection, p *ebpf.Program, name string) error {
	if err := createQdisc(index); err != nil {
		return err
	}
	conn, err := tc.Open(&tc.Config{})
	if err != nil {
		return err
	}
	defer conn.Close()
	parent := tcMakeHandle(tcHandleClsact, uint32(direction))
	filters, err := conn.Filter().Get(&tc.Msg{Family: unix.AF_UNSPEC, Ifindex: index, Parent: parent})
	if err != nil {
		return err
	}
	const handle = 0x4252
	const priority = 30
	for _, f := range filters {
		if f.Handle == handle && f.Info>>16 == priority {
			if f.Kind != "bpf" || f.BPF == nil || f.BPF.Name == nil || *f.BPF.Name != name {
				return errors.New("CubeVS LAN TC attachment conflicts with an existing filter")
			}
		}
	}
	fd, flags := uint32(p.FD()), uint32(tcFlagDirectAction)
	return conn.Filter().Replace(&tc.Object{Msg: tc.Msg{Family: unix.AF_UNSPEC, Ifindex: index, Handle: handle, Parent: parent, Info: tcMakeHandle(priority<<16, uint32(htons(syscall.ETH_P_ALL)))}, Attribute: tc.Attribute{Kind: "bpf", BPF: &tc.Bpf{FD: &fd, Name: &name, Flags: &flags}}})
}

// EnsureLANEndpoint prepares policy before atomically publishing its generation.
// Missing metadata defaults to DROP while hooks are being attached.
func EnsureLANEndpoint(e LANEndpoint) error {
	metadata, prefixes, err := lanMetadata(e)
	if err != nil {
		return err
	}
	obj, err := loadLAN()
	if err != nil {
		return err
	}
	var old lanLanEndpoint
	err = obj.Maps[lanEndpointMap].Lookup(e.Ifindex, &old)
	if err == nil && old.Token != metadata.Token {
		return errors.New("CubeVS LAN ifindex is owned by another attachment")
	}
	if err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
		return err
	}
	if err == nil && old.Generation > e.Generation {
		return errors.New("CubeVS LAN policy generation is stale")
	}
	if err := lanFilter(e.Ifindex, TCIngress, obj.Programs["cubevs_lan_in"], "cubevs_lan_in"); err != nil {
		return err
	}
	if err := lanFilter(e.Ifindex, TCEgress, obj.Programs["cubevs_lan_out"], "cubevs_lan_out"); err != nil {
		return err
	}
	for _, prefix := range prefixes {
		if err := obj.Maps[lanPrefixMap].Update(prefix, uint8(1), ebpf.UpdateAny); err != nil {
			return err
		}
	}
	var counter lanLanCounter
	if err := obj.Maps[lanStatsMap].Lookup(e.Ifindex, &counter); err != nil {
		if !errors.Is(err, ebpf.ErrKeyNotExist) {
			return err
		}
		if err := obj.Maps[lanStatsMap].Update(e.Ifindex, lanLanCounter{Token: metadata.Token}, ebpf.UpdateNoExist); err != nil {
			return err
		}
	} else if counter.Token != metadata.Token {
		return errors.New("CubeVS LAN statistics identity conflict")
	}
	if err := obj.Maps[lanEndpointMap].Update(e.Ifindex, metadata, ebpf.UpdateAny); err != nil {
		return err
	}
	return pruneLANPrefixes(obj.Maps[lanPrefixMap], e.Ifindex, e.Generation, false)
}

func pruneLANPrefixes(m *ebpf.Map, index uint32, generation uint64, all bool) error {
	var key lanLanPrefix
	var value uint8
	iter := m.Iterate()
	keys := []lanLanPrefix{}
	for iter.Next(&key, &value) {
		if key.Ifindex == index && (all || key.Generation != generation) {
			keys = append(keys, key)
		}
	}
	if err := iter.Err(); err != nil {
		return err
	}
	for _, key := range keys {
		if err := m.Delete(key); err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
			return err
		}
	}
	return nil
}

// RemoveLANEndpoint requires the stable owner even after the TAP disappears.
// It retires metadata before cleaning address policy and reply permissions.
func RemoveLANEndpoint(sandbox, id string, index uint32) error {
	if index == 0 {
		return nil
	}
	m, err := openLANMap(pinPath(lanEndpointMap), nil)
	if errors.Is(err, syscall.ENOENT) {
		return nil
	}
	if err != nil {
		return err
	}
	defer m.Close()
	var old lanLanEndpoint
	if err = m.Lookup(index, &old); errors.Is(err, ebpf.ErrKeyNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	token := endpointToken(sandbox, id)
	if old.Token != token {
		return errors.New("CubeVS LAN cleanup owner mismatch")
	}
	// Retain the disabled owner until all policy and reply state is gone, so a
	// crash midway through cleanup remains distinguishable from full absence.
	old.Active = 0
	if err = m.Update(index, old, ebpf.UpdateAny); err != nil {
		return err
	}
	prefixes, err := openLANMap(pinPath(lanPrefixMap), nil)
	if err != nil {
		return err
	}
	defer prefixes.Close()
	if err := pruneLANPrefixes(prefixes, index, 0, true); err != nil {
		return err
	}
	flows, err := openLANMap(pinPath(lanFlowMap), nil)
	if err != nil {
		return err
	}
	defer flows.Close()
	var key lanLanFlow
	var seen uint64
	iter := flows.Iterate()
	keys := []lanLanFlow{}
	for iter.Next(&key, &seen) {
		if key.Token == token {
			keys = append(keys, key)
		}
	}
	if err := iter.Err(); err != nil {
		return err
	}
	for _, key := range keys {
		if err := flows.Delete(key); err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
			return err
		}
	}
	stats, err := openLANMap(pinPath(lanStatsMap), nil)
	if err != nil {
		return err
	}
	defer stats.Close()
	err = stats.Delete(index)
	if err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
		return err
	}
	err = m.Delete(index)
	if errors.Is(err, ebpf.ErrKeyNotExist) {
		return nil
	}
	return err
}
