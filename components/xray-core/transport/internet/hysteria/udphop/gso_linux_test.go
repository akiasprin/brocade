//go:build linux

package udphop

import (
	"bytes"
	"encoding/binary"
	"errors"
	"net"
	"testing"
	"time"
	"unsafe"

	"github.com/xtls/xray-core/transport/internet/finalmask"
	"github.com/xtls/xray-core/transport/internet/finalmask/salamander"
	"golang.org/x/sys/unix"
)

func TestUDPHopAndSalamanderPreserveKernelGSO(t *testing.T) {
	serverRaw, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer serverRaw.Close()
	serverAddr := serverRaw.LocalAddr().(*net.UDPAddr)

	clientRaw, err := net.DialUDP("udp4", nil, serverAddr)
	if err != nil {
		t.Fatal(err)
	}
	hopConn, err := NewUDPHopPacketConn(
		&UDPHopAddr{IP: serverAddr.IP, Ports: []uint32{uint32(serverAddr.Port)}},
		0,
		3600,
		3600,
		func(*net.UDPAddr) (net.PacketConn, error) {
			return nil, errors.New("unexpected hop")
		},
		clientRaw,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer hopConn.Close()

	clientManager := finalmask.NewUdpmaskManager([]finalmask.Udpmask{
		&salamander.Config{Password: "udp-hop-gso-test"},
	})
	clientConn, err := clientManager.WrapPacketConnClient(hopConn)
	if err != nil {
		t.Fatal(err)
	}
	client, ok := clientConn.(oobPacketConn)
	if !ok {
		t.Fatal("Salamander over UDP hop did not preserve OOB capability")
	}

	serverManager := finalmask.NewUdpmaskManager([]finalmask.Udpmask{
		&salamander.Config{Password: "udp-hop-gso-test"},
	})
	serverConn, err := serverManager.WrapPacketConnServer(serverRaw)
	if err != nil {
		t.Fatal(err)
	}
	server, ok := serverConn.(oobPacketConn)
	if !ok {
		t.Fatal("Salamander server did not preserve OOB capability")
	}
	if err := server.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatal(err)
	}

	const segmentSize = 1200
	first := bytes.Repeat([]byte{0x11}, segmentSize)
	second := bytes.Repeat([]byte{0x22}, 800)
	payload := append(append([]byte(nil), first...), second...)
	n, _, err := client.WriteMsgUDP(payload, udpSegmentOOB(segmentSize), serverAddr)
	if err != nil {
		if errors.Is(err, unix.EINVAL) || errors.Is(err, unix.EIO) || errors.Is(err, unix.ENOPROTOOPT) || errors.Is(err, unix.EPERM) {
			t.Skipf("UDP GSO is unavailable on this kernel: %v", err)
		}
		t.Fatal(err)
	}
	if n != len(payload) {
		t.Fatalf("WriteMsgUDP returned %d bytes, want %d", n, len(payload))
	}

	for i, want := range [][]byte{first, second} {
		buf := make([]byte, segmentSize)
		n, _, _, _, err := server.ReadMsgUDP(buf, nil)
		if err != nil {
			t.Fatalf("read GSO segment %d: %v", i, err)
		}
		if !bytes.Equal(buf[:n], want) {
			t.Fatalf("GSO segment %d did not survive UDP hop and Salamander", i)
		}
	}
}

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
