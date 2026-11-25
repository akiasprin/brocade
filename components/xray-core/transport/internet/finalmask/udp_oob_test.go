package finalmask_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"sync/atomic"
	"syscall"
	"testing"

	xnet "github.com/xtls/xray-core/common/net"
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
	manager := finalmask.NewFinalMask(nil, []finalmask.UDPMask{&salamander.Config{Password: "oob-test-password"}}, nil, nil,
		func(context.Context, xnet.Destination) (net.PacketConn, net.Addr, error) {
			return clientRaw, serverRaw.LocalAddr(), nil
		},
		func(context.Context, net.Addr) (net.PacketConn, error) { return serverRaw, nil })
	wrapped, err := manager.DialUDP(context.Background(), xnet.DestinationFromAddr(serverRaw.LocalAddr()))
	if err != nil {
		t.Fatal(err)
	}
	clientPacketConn := wrapped.(*xnet.PacketConnWrapper).PacketConn
	serverPacketConn, err := manager.ListenPacket(context.Background(), serverRaw.LocalAddr())
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

func TestSalamanderQUICReadMsgUDPDropsOversizedDatagram(t *testing.T) {
	clientRaw, serverRaw, client, server := wrapSalamanderOOBPair(t)
	for i, payload := range [][]byte{bytes.Repeat([]byte{0x5a}, 2000), []byte("next-valid-packet")} {
		if _, _, err := client.WriteMsgUDP(payload, nil, serverRaw.LocalAddr().(*net.UDPAddr)); err != nil {
			t.Fatal(err)
		}
		wire := <-clientRaw.writes
		serverRaw.reads <- recordedOOBPacket{payload: wire.payload, oob: []byte{byte(i)}, addr: clientRaw.LocalAddr().(*net.UDPAddr)}
	}
	buffer, oob := make([]byte, 1452), make([]byte, 16)
	n, nn, _, addr, err := server.ReadMsgUDP(buffer, oob)
	if err != nil || !bytes.Equal(buffer[:n], []byte("next-valid-packet")) {
		t.Fatalf("listener did not continue after oversized datagram: n=%d err=%v", n, err)
	}
	if nn != 1 || oob[0] != 1 || addr.String() != clientRaw.LocalAddr().String() {
		t.Fatal("next valid datagram metadata changed")
	}
}

func TestSalamanderQUICReadBatchDropsOversizedDatagrams(t *testing.T) {
	for _, test := range []struct {
		name      string
		oversized []bool
		batchSize int
	}{
		{"before-valid", []bool{true, false}, 2},
		{"after-valid", []bool{false, true}, 2},
		{"between-valid", []bool{false, true, false}, 3},
		{"whole-batch", []bool{true, true, false}, 2},
	} {
		t.Run(test.name, func(t *testing.T) {
			clientRaw, serverRaw, client, server := wrapSalamanderOOBPair(t)
			var expected [][]byte
			var expectedMetadata []byte
			for i, oversized := range test.oversized {
				payload := bytes.Repeat([]byte{byte(i + 1)}, 17)
				if oversized {
					payload = bytes.Repeat([]byte{0x5a}, 2000)
				} else {
					expected = append(expected, payload)
					expectedMetadata = append(expectedMetadata, byte(i+1))
				}
				if _, _, err := client.WriteMsgUDP(payload, nil, serverRaw.LocalAddr().(*net.UDPAddr)); err != nil {
					t.Fatal(err)
				}
				wire := <-clientRaw.writes
				serverRaw.reads <- recordedOOBPacket{payload: wire.payload, oob: []byte{byte(i + 1)}, flags: i + 1, addr: clientRaw.LocalAddr().(*net.UDPAddr)}
			}
			messages := make([]ipv4.Message, test.batchSize)
			for i := range messages {
				messages[i].Buffers = [][]byte{make([]byte, 8), make([]byte, 24)}
				messages[i].OOB = make([]byte, 16)
			}
			n, err := server.ReadBatch(messages, 0)
			if err != nil || n != len(expected) {
				t.Fatalf("valid datagrams must survive oversized packets in the batch: n=%d want=%d err=%v", n, len(expected), err)
			}
			for i := range expected {
				packet := append(append([]byte{}, messages[i].Buffers[0]...), messages[i].Buffers[1]...)
				if messages[i].N != len(expected[i]) || !bytes.Equal(packet[:messages[i].N], expected[i]) {
					t.Fatalf("valid packet %d changed", i)
				}
				if messages[i].NN != 1 || messages[i].OOB[0] != expectedMetadata[i] || messages[i].Flags != int(expectedMetadata[i]) || messages[i].Addr.String() != clientRaw.LocalAddr().String() {
					t.Fatalf("valid packet %d metadata changed after batch compaction", i)
				}
			}
			wantReads := int64(1)
			if test.name == "whole-batch" {
				wantReads = 2
			}
			if serverRaw.batchReads.Load() != wantReads {
				t.Fatalf("batch read count=%d want=%d", serverRaw.batchReads.Load(), wantReads)
			}
		})
	}
}

func TestSalamanderQUICReadsPreserveSocketErrors(t *testing.T) {
	for _, method := range []string{"ReadMsgUDP", "ReadBatch"} {
		for _, test := range []struct {
			name   string
			packet []byte
		}{
			{"empty", nil},
			{"after-malformed", []byte{1}},
			{"after-oversized", bytes.Repeat([]byte{0x5a}, 2000)},
		} {
			t.Run(method+"/"+test.name, func(t *testing.T) {
				_, serverRaw, _, server := wrapSalamanderOOBPair(t)
				if test.packet != nil {
					serverRaw.reads <- recordedOOBPacket{payload: test.packet, addr: serverRaw.LocalAddr().(*net.UDPAddr)}
				}
				close(serverRaw.reads)
				var err error
				if method == "ReadMsgUDP" {
					_, _, _, _, err = server.ReadMsgUDP(make([]byte, 1452), nil)
				} else {
					_, err = server.ReadBatch([]ipv4.Message{{Buffers: [][]byte{make([]byte, 1452)}}}, 0)
				}
				if !errors.Is(err, io.EOF) {
					t.Fatalf("%s lost the underlying error: %v", method, err)
				}
			})
		}
	}
}
