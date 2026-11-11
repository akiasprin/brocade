// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import (
	"encoding/binary"
	"errors"
	"net"
	"os"
	"path/filepath"
	"testing"

	"github.com/cilium/ebpf"
)

func TestLANBPFInterruptedCleanupKeepsAnOwnedTombstone(t *testing.T) {
	if os.Getenv("CUBE_RUN_BPF_TESTS") != "1" {
		t.Skip("requires explicit isolated privileged BPF acceptance")
	}
	spec, err := loadLan()
	if err != nil {
		t.Fatal(err)
	}
	for _, m := range spec.Maps {
		m.Pinning = ebpf.PinNone
	}
	obj, err := ebpf.NewCollection(spec)
	if err != nil {
		t.Fatal(err)
	}
	defer obj.Close()
	e := lanExample()
	meta, prefixes, err := lanMetadata(e)
	if err != nil {
		t.Fatal(err)
	}
	if err := obj.Maps[lanEndpointMap].Update(e.Ifindex, meta, ebpf.UpdateAny); err != nil {
		t.Fatal(err)
	}
	if err := obj.Maps[lanStatsMap].Update(e.Ifindex, lanLanCounter{Token: meta.Token}, ebpf.UpdateAny); err != nil {
		t.Fatal(err)
	}
	for _, prefix := range prefixes {
		if err := obj.Maps[lanPrefixMap].Update(prefix, uint8(1), ebpf.UpdateAny); err != nil {
			t.Fatal(err)
		}
	}
	key := lanLanFlow{Token: meta.Token, Family: 6}
	if err := obj.Maps[lanFlowMap].Update(key, uint64(1), ebpf.UpdateAny); err != nil {
		t.Fatal(err)
	}
	original := openLANMap
	defer func() { openLANMap = original }()
	fail := true
	openLANMap = func(path string, _ *ebpf.LoadPinOptions) (*ebpf.Map, error) {
		name := filepath.Base(path)
		if name == lanPrefixMap && fail {
			fail = false
			return nil, errors.New("interrupted cleanup")
		}
		return obj.Maps[name].Clone()
	}
	if err := RemoveLANEndpoint(e.SandboxID, e.InterfaceID, e.Ifindex); err == nil {
		t.Fatal("incomplete cleanup was acknowledged")
	}
	var retained lanLanEndpoint
	if err := obj.Maps[lanEndpointMap].Lookup(e.Ifindex, &retained); err != nil || retained.Active != 0 || retained.Token != meta.Token {
		t.Fatal("cleanup lost its disabled owner tombstone")
	}
	if err := RemoveLANEndpoint(e.SandboxID, e.InterfaceID, e.Ifindex); err != nil {
		t.Fatal(err)
	}
	if err := obj.Maps[lanEndpointMap].Lookup(e.Ifindex, &retained); !errors.Is(err, ebpf.ErrKeyNotExist) {
		t.Fatal("metadata survived successful cleanup")
	}
	var seen uint64
	if err := obj.Maps[lanFlowMap].Lookup(key, &seen); !errors.Is(err, ebpf.ErrKeyNotExist) {
		t.Fatal("reply permission survived cleanup")
	}
	var prefix lanLanPrefix
	var allowed uint8
	if obj.Maps[lanPrefixMap].Iterate().Next(&prefix, &allowed) {
		t.Fatal("address prefix survived cleanup")
	}
}

func lanExample() LANEndpoint {
	return LANEndpoint{SandboxID: "vm-example", InterfaceID: "nic-private", Ifindex: 1, Generation: 7, MAC: "02:01:02:03:04:05", IPv4: "198.19.1.2/24", IPv6: "fd91::2/64", Connected: []string{"198.19.1.0/24", "fd91::/64"}}
}

func TestLANIdentityAndAddressFamiliesAreIndependent(t *testing.T) {
	e := lanExample()
	m, prefixes, err := lanMetadata(e)
	if err != nil || len(prefixes) != 2 || m.Ipv4 != [4]uint8{198, 19, 1, 2} || m.Ipv6[0] != 0xfd || m.Linklocal[8] != 0 || m.Active != 1 {
		t.Fatalf("invalid dual-stack endpoint: %+v %v", m, err)
	}
	e.Generation++
	next, _, err := lanMetadata(e)
	if err != nil || next.Token != m.Token || next.Generation == m.Generation {
		t.Fatal("policy generation changed endpoint ownership")
	}
	e.InterfaceID = "another"
	next, _, _ = lanMetadata(e)
	if next.Token == m.Token {
		t.Fatal("two attachments share a reply identity")
	}
	for _, invalid := range []LANEndpoint{{}, {SandboxID: "vm", InterfaceID: "nic", Ifindex: 1, Generation: 1, MAC: "ff:ff:ff:ff:ff:ff", IPv4: "198.19.1.2/24"}} {
		if _, _, err := lanMetadata(invalid); err == nil {
			t.Fatal("invalid LAN binding accepted")
		}
	}
}

func lanUDP(v6, incoming bool, sourcePort, destPort uint16) []byte {
	size := 42
	if v6 {
		size = 62
	}
	p := make([]byte, size)
	mac, _ := net.ParseMAC("02:01:02:03:04:05")
	copy(p[6:12], mac)
	copy(p[:6], mac)
	start := 34
	if v6 {
		binary.BigEndian.PutUint16(p[12:14], 0x86dd)
		p[14] = 0x60
		p[20] = 17
		p[21] = 64
		binary.BigEndian.PutUint16(p[18:20], 8)
		local, remote := net.ParseIP("fd91::2").To16(), net.ParseIP("fd77::1").To16()
		if incoming {
			copy(p[22:38], remote)
			copy(p[38:54], local)
		} else {
			copy(p[22:38], local)
			copy(p[38:54], remote)
		}
		start = 54
	} else {
		binary.BigEndian.PutUint16(p[12:14], 0x0800)
		p[14] = 0x45
		p[22] = 64
		p[23] = 17
		binary.BigEndian.PutUint16(p[16:18], 28)
		local, remote := net.ParseIP("198.19.1.2").To4(), net.ParseIP("198.18.1.1").To4()
		if incoming {
			copy(p[26:30], remote)
			copy(p[30:34], local)
		} else {
			copy(p[26:30], local)
			copy(p[30:34], remote)
		}
	}
	binary.BigEndian.PutUint16(p[start:start+2], sourcePort)
	binary.BigEndian.PutUint16(p[start+2:start+4], destPort)
	binary.BigEndian.PutUint16(p[start+4:start+6], 8)
	return p
}

// Explicit opt-in runs actual verifier and program execution. This test must
// fail, rather than skip, when a requested privileged acceptance cannot run.
func TestLANBPFIdentityNoDefaultAndReplyBoundary(t *testing.T) {
	if os.Getenv("CUBE_RUN_BPF_TESTS") != "1" {
		t.Skip("requires explicit isolated privileged BPF acceptance")
	}
	spec, err := loadLan()
	if err != nil {
		t.Fatal(err)
	}
	for _, m := range spec.Maps {
		m.Pinning = ebpf.PinNone
	}
	obj, err := ebpf.NewCollection(spec)
	if err != nil {
		t.Fatalf("load real LAN programs: %+v", err)
	}
	defer obj.Close()
	e := lanExample()
	metadata, _, err := lanMetadata(e)
	if err != nil {
		t.Fatal(err)
	}
	// Test-run defaults use loopback or zero depending on the kernel. Both
	// indices get the same private test endpoint; no real TC hook is installed.
	for _, index := range []uint32{0, 1} {
		if err := obj.Maps[lanEndpointMap].Update(index, metadata, ebpf.UpdateAny); err != nil {
			t.Fatal(err)
		}
	}
	run := func(incoming bool, p []byte, want uint32) {
		t.Helper()
		name := "cubevs_lan_in"
		if incoming {
			name = "cubevs_lan_out"
		}
		code, _, err := obj.Programs[name].Test(p)
		if err != nil {
			t.Fatal(err)
		}
		if code != want {
			t.Fatalf("%s returned %d; want %d", name, code, want)
		}
	}
	for _, v6 := range []bool{false, true} {
		out := lanUDP(v6, false, 40000, 23001)
		run(false, out, 2)
		run(true, lanUDP(v6, true, 23001, 40000), 0)
		run(false, out, 0)
		forged := append([]byte(nil), out...)
		forged[6] ^= 2
		run(false, forged, 2)
		fresh := lanUDP(v6, false, 40001, 23001)
		run(false, fresh, 2)
	}
	metadata.Flags = 3
	for _, index := range []uint32{0, 1} {
		if err := obj.Maps[lanEndpointMap].Update(index, metadata, ebpf.UpdateAny); err != nil {
			t.Fatal(err)
		}
	}
	run(false, lanUDP(true, false, 40002, 23001), 0)
	run(false, lanUDP(false, false, 40002, 23001), 0)
	options := lanUDP(false, false, 40003, 23001)
	options = append(append(append([]byte{}, options[:34]...), []byte{131, 7, 4, 198, 19, 1, 1, 0}...), options[34:]...)
	options[14] = 0x47
	binary.BigEndian.PutUint16(options[16:18], 36)
	run(false, options, 2)
	routed := lanUDP(true, false, 40004, 23001)
	routed = append(append(append([]byte{}, routed[:54]...), []byte{17, 0, 4, 1, 0, 0, 0, 0}...), routed[54:]...)
	routed[20] = 43
	binary.BigEndian.PutUint16(routed[18:20], 16)
	run(false, routed, 2)
	routed[57] = 0
	run(false, routed, 0)
}
