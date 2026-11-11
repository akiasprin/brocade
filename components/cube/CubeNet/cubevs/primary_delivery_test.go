// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

//go:generate go run github.com/cilium/ebpf/cmd/bpf2go -target $GOARCH primarydelivery ../src/primary_delivery_test.bpf.c -- -I../vmlinux/$GOARCH

import (
	"encoding/binary"
	"github.com/cilium/ebpf"
	"net"
	"testing"
)

func deliveryChecksum(data []byte) uint16 {
	var sum uint32
	for len(data) >= 2 {
		sum += uint32(binary.BigEndian.Uint16(data))
		data = data[2:]
	}
	if len(data) > 0 {
		sum += uint32(data[0]) << 8
	}
	for sum>>16 != 0 {
		sum = (sum & 65535) + (sum >> 16)
	}
	return ^uint16(sum)
}
func deliveryPacket(source, destination string, protocol byte, payload []byte, fragment uint16) []byte {
	packet := make([]byte, 14+20+len(payload))
	packet[12] = 8
	ip := packet[14:]
	ip[0] = 0x45
	binary.BigEndian.PutUint16(ip[2:4], uint16(len(ip)))
	binary.BigEndian.PutUint16(ip[6:8], fragment)
	ip[8] = 64
	ip[9] = protocol
	copy(ip[12:16], net.ParseIP(source).To4())
	copy(ip[16:20], net.ParseIP(destination).To4())
	binary.BigEndian.PutUint16(ip[10:12], deliveryChecksum(ip[:20]))
	copy(ip[20:], payload)
	return packet
}
func deliveryUDP(source, destination string, from, to uint16) []byte {
	udp := make([]byte, 12)
	binary.BigEndian.PutUint16(udp[:2], from)
	binary.BigEndian.PutUint16(udp[2:4], to)
	binary.BigEndian.PutUint16(udp[4:6], uint16(len(udp)))
	copy(udp[8:], []byte("test"))
	pseudo := append(append([]byte{}, net.ParseIP(source).To4()...), net.ParseIP(destination).To4()...)
	pseudo = append(pseudo, 0, 17, 0, byte(len(udp)))
	pseudo = append(pseudo, udp...)
	binary.BigEndian.PutUint16(udp[6:8], deliveryChecksum(pseudo))
	return udp
}
func primaryCollection(t *testing.T) *ebpf.Collection {
	t.Helper()
	spec, err := loadPrimarydelivery()
	if err != nil {
		t.Fatal(err)
	}
	for name, m := range spec.Maps {
		if name != "primary_delivery" && name != "ifindex_to_mvmmeta" && name != ".rodata" {
			delete(spec.Maps, name)
		} else {
			m.Pinning = ebpf.PinNone
		}
	}
	coll, err := ebpf.NewCollection(spec)
	if err != nil {
		if bpfTestUnavailable(err) {
			t.Skipf("privileged BPF required: %v", err)
		}
		t.Fatalf("load primary delivery: %+v", err)
	}
	t.Cleanup(coll.Close)
	meta := mvmMetadata{Version: 42, IP: binary.NativeEndian.Uint32(net.ParseIP("203.0.113.10").To4())}
	if err = coll.Maps["ifindex_to_mvmmeta"].Update(uint32(1), meta, ebpf.UpdateAny); err != nil {
		t.Fatal(err)
	}
	return coll
}
func runPrimary(t *testing.T, c *ebpf.Collection, name string, packet []byte, want uint32) []byte {
	t.Helper()
	ret, out, err := c.Programs[name].Test(packet)
	if err != nil {
		t.Fatal(err)
	}
	if ret != want {
		t.Fatalf("%s verdict %d, want %d", name, ret, want)
	}
	return out
}
func TestPrimaryDeliveryReplyScopeAndGeneration(t *testing.T) {
	c := primaryCollection(t)
	incoming := deliveryPacket("203.0.113.1", "203.0.113.10", 17, deliveryUDP("203.0.113.1", "203.0.113.10", 40000, 9999), 0)
	reply := deliveryPacket("169.254.68.6", "169.254.68.5", 17, deliveryUDP("169.254.68.6", "169.254.68.5", 9999, 40000), 0)
	runPrimary(t, c, "test_primary_reply", reply, 2)
	runPrimary(t, c, "test_primary_record", incoming, 0)
	runPrimary(t, c, "test_primary_reply", reply, 0)
	wrong := deliveryPacket("169.254.68.6", "169.254.68.5", 17, deliveryUDP("169.254.68.6", "169.254.68.5", 9998, 40000), 0)
	runPrimary(t, c, "test_primary_reply", wrong, 2)
	fragmented := deliveryPacket("169.254.68.6", "169.254.68.5", 17, []byte("noninitial-fragment"), 8)
	runPrimary(t, c, "test_primary_reply", fragmented, 0)
	meta := mvmMetadata{Version: 43, IP: binary.NativeEndian.Uint32(net.ParseIP("203.0.113.10").To4())}
	if err := c.Maps["ifindex_to_mvmmeta"].Update(uint32(1), meta, ebpf.UpdateAny); err != nil {
		t.Fatal(err)
	}
	runPrimary(t, c, "test_primary_reply", reply, 2)
}
func TestPrimaryDeliveryRawProtocolAndICMPError(t *testing.T) {
	c := primaryCollection(t)
	runPrimary(t, c, "test_primary_record", deliveryPacket("203.0.113.1", "203.0.113.10", 253, []byte("raw"), 0), 0)
	runPrimary(t, c, "test_primary_reply", deliveryPacket("169.254.68.6", "169.254.68.5", 253, []byte("reply"), 0), 0)
	runPrimary(t, c, "test_primary_reply", deliveryPacket("169.254.68.6", "169.254.68.5", 254, []byte("unsolicited"), 0), 2)
	incoming := deliveryPacket("203.0.113.1", "203.0.113.10", 17, deliveryUDP("203.0.113.1", "203.0.113.10", 40000, 9999), 0)
	runPrimary(t, c, "test_primary_record", incoming, 0)
	quote := deliveryPacket("169.254.68.5", "169.254.68.6", 17, deliveryUDP("169.254.68.5", "169.254.68.6", 40000, 9999), 0)[14:]
	icmp := append([]byte{3, 3, 0, 0, 0, 0, 0, 0}, quote...)
	binary.BigEndian.PutUint16(icmp[2:4], deliveryChecksum(icmp))
	reply := deliveryPacket("169.254.68.6", "169.254.68.5", 1, icmp, 0)
	out := runPrimary(t, c, "test_primary_reply", reply, 0)
	translated := out[42:]
	if !net.IP(translated[12:16]).Equal(net.ParseIP("203.0.113.1")) || !net.IP(translated[16:20]).Equal(net.ParseIP("203.0.113.10")) {
		t.Fatalf("ICMP quote addresses were not restored: %v", translated)
	}
	if deliveryChecksum(out[34:]) != 0 || deliveryChecksum(translated[:20]) != 0 {
		t.Fatal("ICMP or quoted IPv4 checksum invalid")
	}
	pseudo := append(append([]byte{}, translated[12:20]...), 0, 17, 0, byte(len(translated)-20))
	pseudo = append(pseudo, translated[20:]...)
	if deliveryChecksum(pseudo) != 0 {
		t.Fatal("quoted UDP pseudo-header checksum invalid")
	}
}

func TestPrimaryDeliveryProductionProgramsLoad(t *testing.T) {
	for name, loader := range map[string]func() (*ebpf.CollectionSpec, error){"localgw": loadLocalgw, "mvmtap": loadMvmtap} {
		t.Run(name, func(t *testing.T) {
			spec, err := loader()
			if err != nil {
				t.Fatal(err)
			}
			for _, m := range spec.Maps {
				m.Pinning = ebpf.PinNone
			}
			if err = populateDNSTailCalls(spec); err != nil {
				t.Fatal(err)
			}
			collection, err := ebpf.NewCollection(spec)
			if err != nil {
				if bpfTestUnavailable(err) {
					t.Skipf("privileged BPF required: %v", err)
				}
				t.Fatalf("production %s verifier: %+v", name, err)
			}
			collection.Close()
		})
	}
}
