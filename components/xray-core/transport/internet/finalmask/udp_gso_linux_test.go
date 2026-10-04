//go:build linux

package finalmask_test

import (
	"bytes"
	"encoding/binary"
	"net"
	"testing"
	"unsafe"

	"golang.org/x/sys/unix"
)

func udpSegmentOOB(size uint16) []byte {
	oob := make([]byte, unix.CmsgSpace(2))
	// SAFETY: oob is sized and aligned for one Cmsghdr plus its two-byte payload.
	header := (*unix.Cmsghdr)(unsafe.Pointer(&oob[0]))
	header.Level = unix.IPPROTO_UDP
	header.Type = unix.UDP_SEGMENT
	header.SetLen(unix.CmsgLen(2))
	binary.NativeEndian.PutUint16(oob[unix.CmsgSpace(0):], size)
	return oob
}

func parseUDPSegmentSize(t *testing.T, oob []byte) uint16 {
	t.Helper()
	header, body, _, err := unix.ParseOneSocketControlMessage(oob)
	if err != nil {
		t.Fatal(err)
	}
	if header.Level != unix.IPPROTO_UDP || header.Type != unix.UDP_SEGMENT || len(body) < 2 {
		t.Fatalf("unexpected UDP_SEGMENT control message: header=%+v body=%x", header, body)
	}
	return binary.NativeEndian.Uint16(body)
}

func TestSalamanderQUICWriteMsgUDPPreservesGSO(t *testing.T) {
	clientRaw, serverRaw, client, server := wrapSalamanderOOBPair(t)
	const segmentSize = 1200
	first := bytes.Repeat([]byte{0x11}, segmentSize)
	second := bytes.Repeat([]byte{0x22}, 800)
	payload := append(append([]byte(nil), first...), second...)

	n, _, err := client.WriteMsgUDP(payload, udpSegmentOOB(segmentSize), serverRaw.LocalAddr().(*net.UDPAddr))
	if err != nil {
		t.Fatal(err)
	}
	if n != len(payload) {
		t.Fatalf("unexpected payload write size: got=%d want=%d", n, len(payload))
	}
	wire := <-clientRaw.writes
	wireSegmentSize := int(parseUDPSegmentSize(t, wire.oob))
	if wireSegmentSize != segmentSize+8 {
		t.Fatalf("masked GSO segment size was not adjusted: got=%d want=%d", wireSegmentSize, segmentSize+8)
	}
	if len(wire.payload) != len(payload)+16 {
		t.Fatalf("unexpected masked GSO payload size: got=%d want=%d", len(wire.payload), len(payload)+16)
	}

	wireSegments := [][]byte{wire.payload[:wireSegmentSize], wire.payload[wireSegmentSize:]}
	want := [][]byte{first, second}
	for i := range wireSegments {
		serverRaw.reads <- recordedOOBPacket{
			payload: wireSegments[i],
			addr:    clientRaw.LocalAddr().(*net.UDPAddr),
		}
		buf := make([]byte, segmentSize)
		got, _, _, _, readErr := server.ReadMsgUDP(buf, nil)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if !bytes.Equal(buf[:got], want[i]) {
			t.Fatalf("GSO segment %d did not round trip", i)
		}
	}
}
