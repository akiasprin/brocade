package freedom

import (
	"bytes"
	stdnet "net"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet"
)

func TestPacketReaderPreservesLargeUDPPackets(t *testing.T) {
	receiver, err := stdnet.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer receiver.Close()

	sender, err := stdnet.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer sender.Close()

	payload := bytes.Repeat([]byte{0xa5}, 32768)
	if _, err := sender.WriteTo(payload, receiver.LocalAddr()); err != nil {
		t.Fatal(err)
	}

	conn := &internet.PacketConnWrapper{
		PacketConn: receiver,
		Dest:       sender.LocalAddr(),
	}
	reader := NewPacketReader(
		conn,
		xnet.Destination{},
		xnet.TCPDestination(xnet.IPAddress([]byte{127, 0, 0, 1}), 0),
		nil,
	)
	mb, err := reader.ReadMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(mb)

	if got := mb.Len(); got != int32(len(payload)) {
		t.Fatalf("packet length = %d, want %d", got, len(payload))
	}
	got := make([]byte, len(payload))
	if n := mb.Copy(got); n != len(payload) || !bytes.Equal(got, payload) {
		t.Fatalf("packet payload mismatch: copied %d bytes", n)
	}
}
