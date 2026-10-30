package udphop

import (
	"bytes"
	"errors"
	"net"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/net/ipv4"
)

type scriptedPacket struct {
	payload []byte
	oob     []byte
	flags   int
	addr    *net.UDPAddr
}

type scriptedBatch struct {
	packets []scriptedPacket
	err     error
}

type recordedWrite struct {
	payload []byte
	oob     []byte
	addr    *net.UDPAddr
}

type scriptedOOBConn struct {
	*net.UDPConn

	reads      chan scriptedBatch
	writes     chan recordedWrite
	done       chan struct{}
	closeOnce  sync.Once
	batchReads atomic.Int32
}

func newScriptedOOBConn(t *testing.T) *scriptedOOBConn {
	t.Helper()
	raw, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	return &scriptedOOBConn{
		UDPConn: raw,
		reads:   make(chan scriptedBatch, 1),
		writes:  make(chan recordedWrite, 1),
		done:    make(chan struct{}),
	}
}

func (c *scriptedOOBConn) ReadBatch(messages []ipv4.Message, _ int) (int, error) {
	select {
	case batch := <-c.reads:
		c.batchReads.Add(1)
		if len(batch.packets) > len(messages) {
			return 0, errors.New("scripted batch is too large")
		}
		for i, packet := range batch.packets {
			messages[i].N = copyToBuffers(messages[i].Buffers, packet.payload)
			messages[i].NN = copy(messages[i].OOB, packet.oob)
			messages[i].Flags = packet.flags
			messages[i].Addr = packet.addr
		}
		return len(batch.packets), batch.err
	case <-c.done:
		return 0, net.ErrClosed
	}
}

func (c *scriptedOOBConn) WriteMsgUDP(payload, oob []byte, addr *net.UDPAddr) (int, int, error) {
	write := recordedWrite{
		payload: append([]byte(nil), payload...),
		oob:     append([]byte(nil), oob...),
		addr:    cloneUDPAddr(addr),
	}
	select {
	case c.writes <- write:
		return len(payload), len(oob), nil
	case <-c.done:
		return 0, 0, net.ErrClosed
	}
}

func (c *scriptedOOBConn) Close() error {
	var err error
	c.closeOnce.Do(func() {
		close(c.done)
		err = c.UDPConn.Close()
	})
	return err
}

type basicPacketConn struct {
	done      chan struct{}
	closeOnce sync.Once
	closed    atomic.Bool
}

func newBasicPacketConn() *basicPacketConn {
	return &basicPacketConn{done: make(chan struct{})}
}

func (c *basicPacketConn) ReadFrom([]byte) (int, net.Addr, error) {
	<-c.done
	return 0, nil, net.ErrClosed
}

func (c *basicPacketConn) WriteTo(payload []byte, _ net.Addr) (int, error) {
	return len(payload), nil
}

func (c *basicPacketConn) Close() error {
	c.closeOnce.Do(func() {
		c.closed.Store(true)
		close(c.done)
	})
	return nil
}

func (c *basicPacketConn) LocalAddr() net.Addr              { return &net.UDPAddr{} }
func (c *basicPacketConn) SetDeadline(time.Time) error      { return nil }
func (c *basicPacketConn) SetReadDeadline(time.Time) error  { return nil }
func (c *basicPacketConn) SetWriteDeadline(time.Time) error { return nil }

func TestUDPHopPreservesBatchAndOOBThroughHop(t *testing.T) {
	initial := newScriptedOOBConn(t)
	next := newScriptedOOBConn(t)
	addr := &UDPHopAddr{
		IP:    net.IPv4(192, 0, 2, 1),
		Ports: []uint32{20000, 20001},
	}
	var hopTarget *net.UDPAddr
	conn, err := NewUDPHopPacketConn(addr, 0, 3600, 3600, func(target *net.UDPAddr) (net.PacketConn, error) {
		hopTarget = cloneUDPAddr(target)
		return next, nil
	}, initial)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	oobConn, ok := conn.(oobPacketConn)
	if !ok {
		t.Fatal("UDP hop connection did not preserve OOB capability")
	}
	batchConn, ok := conn.(batchPacketConn)
	if !ok {
		t.Fatal("UDP hop connection did not expose batch reads")
	}

	initial.reads <- scriptedBatch{packets: []scriptedPacket{
		{payload: []byte("first"), oob: []byte{1, 2}, flags: 3, addr: &net.UDPAddr{IP: addr.IP, Port: 20000}},
		{payload: []byte("second"), oob: []byte{4, 5}, flags: 6, addr: &net.UDPAddr{IP: addr.IP, Port: 20001}},
	}}
	messages := newTestMessages(2)
	n, err := batchConn.ReadBatch(messages, 0)
	if err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("ReadBatch returned %d messages, want 2", n)
	}
	assertMessage(t, messages[0], "first", []byte{1, 2}, 3, 20000)
	assertMessage(t, messages[1], "second", []byte{4, 5}, 6, 20000)
	if initial.batchReads.Load() != 1 {
		t.Fatalf("underlying ReadBatch called %d times, want 1", initial.batchReads.Load())
	}

	hopConn := conn.(*udpHopOOBConn)
	hopConn.hop()
	if hopTarget == nil {
		t.Fatal("hop dialer was not called")
	}

	payload := []byte("gso payload")
	oob := []byte{9, 8, 7, 6}
	n, oobn, err := oobConn.WriteMsgUDP(payload, oob, &net.UDPAddr{IP: addr.IP, Port: 65535})
	if err != nil {
		t.Fatal(err)
	}
	if n != len(payload) || oobn != len(oob) {
		t.Fatalf("WriteMsgUDP returned (%d, %d), want (%d, %d)", n, oobn, len(payload), len(oob))
	}
	write := <-next.writes
	if !bytes.Equal(write.payload, payload) || !bytes.Equal(write.oob, oob) {
		t.Fatalf("hop write = (%q, %v), want (%q, %v)", write.payload, write.oob, payload, oob)
	}
	if !udpAddrsEqual(write.addr, hopTarget) {
		t.Fatalf("hop write address = %v, want %v", write.addr, hopTarget)
	}

	next.reads <- scriptedBatch{packets: []scriptedPacket{{
		payload: []byte("after hop"),
		oob:     []byte{10, 11},
		addr:    &net.UDPAddr{IP: addr.IP, Port: 20001},
	}}}
	messages = newTestMessages(1)
	n, err = batchConn.ReadBatch(messages, 0)
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("ReadBatch returned %d messages after hop, want 1", n)
	}
	// quic-go must see one stable peer address even while the actual destination port changes.
	assertMessage(t, messages[0], "after hop", []byte{10, 11}, 0, 20000)
}

func TestUDPHopDoesNotAdvertiseOOBForBasicConn(t *testing.T) {
	initial := newBasicPacketConn()
	conn, err := NewUDPHopPacketConn(
		&UDPHopAddr{IP: net.IPv4(192, 0, 2, 1), Ports: []uint32{20000}},
		0,
		0,
		0,
		func(*net.UDPAddr) (net.PacketConn, error) { return newBasicPacketConn(), nil },
		initial,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if _, ok := conn.(oobPacketConn); ok {
		t.Fatal("basic PacketConn was incorrectly advertised as OOB-capable")
	}
	hopConn := conn.(*UdpHopPacketConn)
	if hopConn.HopIntervalMin != 30 || hopConn.HopIntervalMax != 30 {
		t.Fatalf("default hop interval = (%d, %d), want (30, 30) seconds", hopConn.HopIntervalMin, hopConn.HopIntervalMax)
	}
}

func TestUDPHopRejectsCapabilityLoss(t *testing.T) {
	initial := newScriptedOOBConn(t)
	incompatible := newBasicPacketConn()
	conn, err := NewUDPHopPacketConn(
		&UDPHopAddr{IP: net.IPv4(192, 0, 2, 1), Ports: []uint32{20000}},
		0,
		3600,
		3600,
		func(*net.UDPAddr) (net.PacketConn, error) { return incompatible, nil },
		initial,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	hopConn := conn.(*udpHopOOBConn)
	hopConn.hop()
	if !incompatible.closed.Load() {
		t.Fatal("hop socket without OOB capability was not closed")
	}
	if hopConn.currentConn != initial {
		t.Fatal("hop replaced an OOB-capable socket with an incompatible socket")
	}
}

func newTestMessages(n int) []ipv4.Message {
	messages := make([]ipv4.Message, n)
	for i := range messages {
		messages[i].Buffers = [][]byte{make([]byte, 64)}
		messages[i].OOB = make([]byte, 32)
	}
	return messages
}

func assertMessage(t *testing.T, message ipv4.Message, payload string, oob []byte, flags, port int) {
	t.Helper()
	if got := string(message.Buffers[0][:message.N]); got != payload {
		t.Fatalf("payload = %q, want %q", got, payload)
	}
	if got := message.OOB[:message.NN]; !bytes.Equal(got, oob) {
		t.Fatalf("OOB = %v, want %v", got, oob)
	}
	if message.Flags != flags {
		t.Fatalf("flags = %d, want %d", message.Flags, flags)
	}
	udpAddr, ok := message.Addr.(*net.UDPAddr)
	if !ok || udpAddr.Port != port {
		t.Fatalf("address = %v, want UDP port %d", message.Addr, port)
	}
}

func udpAddrsEqual(a, b *net.UDPAddr) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return a.IP.Equal(b.IP) && a.Port == b.Port && a.Zone == b.Zone
}
