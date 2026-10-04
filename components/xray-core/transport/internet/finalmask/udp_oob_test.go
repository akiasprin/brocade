package finalmask_test

import (
	"bytes"
	"io"
	"net"
	"sync/atomic"
	"syscall"
	"testing"

	"github.com/xtls/xray-core/transport/internet/finalmask"
	"github.com/xtls/xray-core/transport/internet/finalmask/salamander"
	"golang.org/x/net/ipv4"
)

type recordedOOBPacket struct {
	payload []byte
	oob     []byte
	flags   int
	addr    *net.UDPAddr
}

type scriptedOOBPacketConn struct {
	*net.UDPConn
	writes          chan recordedOOBPacket
	reads           chan recordedOOBPacket
	readBufferSize  atomic.Int64
	writeBufferSize atomic.Int64
	batchReads      atomic.Int64
}

func newScriptedOOBPacketConn(t *testing.T) *scriptedOOBPacketConn {
	t.Helper()
	raw, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.ParseIP("127.0.0.1")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = raw.Close() })
	return &scriptedOOBPacketConn{
		UDPConn: raw,
		writes:  make(chan recordedOOBPacket, 8),
		reads:   make(chan recordedOOBPacket, 8),
	}
}

func (c *scriptedOOBPacketConn) SetReadBuffer(bytes int) error {
	c.readBufferSize.Store(int64(bytes))
	return nil
}

func (c *scriptedOOBPacketConn) SetWriteBuffer(bytes int) error {
	c.writeBufferSize.Store(int64(bytes))
	return nil
}

func (c *scriptedOOBPacketConn) ReadMsgUDP(b, oob []byte) (n, oobn, flags int, addr *net.UDPAddr, err error) {
	packet, ok := <-c.reads
	if !ok {
		return 0, 0, 0, nil, io.EOF
	}
	n = copy(b, packet.payload)
	oobn = copy(oob, packet.oob)
	return n, oobn, packet.flags, packet.addr, nil
}

func (c *scriptedOOBPacketConn) WriteMsgUDP(b, oob []byte, addr *net.UDPAddr) (n, oobn int, err error) {
	c.writes <- recordedOOBPacket{
		payload: append([]byte(nil), b...),
		oob:     append([]byte(nil), oob...),
		addr:    addr,
	}
	return len(b), len(oob), nil
}

func (c *scriptedOOBPacketConn) ReadBatch(messages []ipv4.Message, _ int) (int, error) {
	c.batchReads.Add(1)
	read := 0
	for i := range messages {
		var packet recordedOOBPacket
		if i == 0 {
			var ok bool
			packet, ok = <-c.reads
			if !ok {
				return 0, io.EOF
			}
		} else {
			select {
			case packet = <-c.reads:
			default:
				return read, nil
			}
		}
		messages[i].N = copy(messages[i].Buffers[0], packet.payload)
		messages[i].NN = copy(messages[i].OOB, packet.oob)
		messages[i].Flags = packet.flags
		messages[i].Addr = packet.addr
		read++
	}
	return read, nil
}

type quicOOBPacketConn interface {
	net.PacketConn
	SyscallConn() (syscall.RawConn, error)
	SetReadBuffer(int) error
	SetWriteBuffer(int) error
	ReadMsgUDP([]byte, []byte) (int, int, int, *net.UDPAddr, error)
	WriteMsgUDP([]byte, []byte, *net.UDPAddr) (int, int, error)
	ReadBatch([]ipv4.Message, int) (int, error)
}

func wrapSalamanderOOBPair(t *testing.T) (
	*scriptedOOBPacketConn,
	*scriptedOOBPacketConn,
	quicOOBPacketConn,
	quicOOBPacketConn,
) {
	t.Helper()
	clientRaw := newScriptedOOBPacketConn(t)
	serverRaw := newScriptedOOBPacketConn(t)
	manager := finalmask.NewUdpmaskManager([]finalmask.Udpmask{
		&salamander.Config{Password: "oob-test-password"},
	})
	clientPacketConn, err := manager.WrapPacketConnClient(clientRaw)
	if err != nil {
		t.Fatal(err)
	}
	serverPacketConn, err := manager.WrapPacketConnServer(serverRaw)
	if err != nil {
		t.Fatal(err)
	}
	client, ok := clientPacketConn.(quicOOBPacketConn)
	if !ok {
		t.Fatalf("client finalmask dropped QUIC UDP capabilities: %T", clientPacketConn)
	}
	server, ok := serverPacketConn.(quicOOBPacketConn)
	if !ok {
		t.Fatalf("server finalmask dropped QUIC UDP capabilities: %T", serverPacketConn)
	}
	return clientRaw, serverRaw, client, server
}

func TestSalamanderPreservesQUICUDPCapabilities(t *testing.T) {
	clientRaw, _, client, _ := wrapSalamanderOOBPair(t)

	if _, err := client.SyscallConn(); err != nil {
		t.Fatal(err)
	}
	if err := client.SetReadBuffer(8 * 1024 * 1024); err != nil {
		t.Fatal(err)
	}
	if err := client.SetWriteBuffer(8 * 1024 * 1024); err != nil {
		t.Fatal(err)
	}
	if got := clientRaw.readBufferSize.Load(); got != 8*1024*1024 {
		t.Fatalf("read buffer setting was not forwarded: %d", got)
	}
	if got := clientRaw.writeBufferSize.Load(); got != 8*1024*1024 {
		t.Fatalf("write buffer setting was not forwarded: %d", got)
	}
}

func TestSalamanderQUICReadWriteMsgUDP(t *testing.T) {
	clientRaw, serverRaw, client, server := wrapSalamanderOOBPair(t)
	payload := []byte("salamander-oob-payload")

	n, oobn, err := client.WriteMsgUDP(payload, nil, serverRaw.LocalAddr().(*net.UDPAddr))
	if err != nil {
		t.Fatal(err)
	}
	if n != len(payload) || oobn != 0 {
		t.Fatalf("unexpected write result: n=%d oobn=%d", n, oobn)
	}
	wire := <-clientRaw.writes
	if bytes.Equal(wire.payload, payload) {
		t.Fatal("Salamander payload was written without masking")
	}
	if len(wire.payload) != len(payload)+8 {
		t.Fatalf("unexpected Salamander wire size: got=%d want=%d", len(wire.payload), len(payload)+8)
	}

	readOOB := []byte{0x01, 0x02, 0x03, 0x04}
	serverRaw.reads <- recordedOOBPacket{
		payload: wire.payload,
		oob:     readOOB,
		flags:   7,
		addr:    clientRaw.LocalAddr().(*net.UDPAddr),
	}
	buf := make([]byte, 128)
	oob := make([]byte, 16)
	n, oobn, flags, addr, err := server.ReadMsgUDP(buf, oob)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(buf[:n], payload) {
		t.Fatalf("unexpected decoded payload: %q", buf[:n])
	}
	if !bytes.Equal(oob[:oobn], readOOB) || flags != 7 {
		t.Fatalf("OOB metadata changed: oob=%x flags=%d", oob[:oobn], flags)
	}
	if addr.String() != clientRaw.LocalAddr().String() {
		t.Fatalf("source address changed: got=%s want=%s", addr, clientRaw.LocalAddr())
	}
}

func TestSalamanderQUICReadBatchDecodesEveryDatagram(t *testing.T) {
	clientRaw, serverRaw, client, server := wrapSalamanderOOBPair(t)
	payloads := [][]byte{[]byte("first-batched-packet"), []byte("second-batched-packet")}
	for i, payload := range payloads {
		if _, _, err := client.WriteMsgUDP(payload, nil, serverRaw.LocalAddr().(*net.UDPAddr)); err != nil {
			t.Fatal(err)
		}
		wire := <-clientRaw.writes
		serverRaw.reads <- recordedOOBPacket{
			payload: wire.payload,
			oob:     []byte{byte(i + 1)},
			flags:   i + 10,
			addr:    clientRaw.LocalAddr().(*net.UDPAddr),
		}
	}

	messages := make([]ipv4.Message, len(payloads))
	for i := range messages {
		messages[i].Buffers = [][]byte{make([]byte, 128)}
		messages[i].OOB = make([]byte, 16)
	}
	n, err := server.ReadBatch(messages, 0)
	if err != nil {
		t.Fatal(err)
	}
	if n != len(payloads) {
		t.Fatalf("unexpected batch size: got=%d want=%d", n, len(payloads))
	}
	if serverRaw.batchReads.Load() != 1 {
		t.Fatalf("batch receive was not preserved: calls=%d", serverRaw.batchReads.Load())
	}
	for i := range payloads {
		if !bytes.Equal(messages[i].Buffers[0][:messages[i].N], payloads[i]) {
			t.Fatalf("packet %d mismatch: %q", i, messages[i].Buffers[0][:messages[i].N])
		}
		if messages[i].NN != 1 || messages[i].OOB[0] != byte(i+1) || messages[i].Flags != i+10 {
			t.Fatalf("packet %d metadata changed: NN=%d OOB=%x flags=%d", i, messages[i].NN, messages[i].OOB[:messages[i].NN], messages[i].Flags)
		}
	}
}
