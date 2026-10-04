package finalmask

import (
	"context"
	"io"
	"net"
	"sync"
	"syscall"

	"github.com/xtls/xray-core/common/bytespool"
	"github.com/xtls/xray-core/common/errors"
	"golang.org/x/net/ipv4"
)

type Udpmask interface {
	UDP()

	WrapPacketConnClient(raw net.PacketConn, level int, levelCount int) (net.PacketConn, error)
	WrapPacketConnServer(raw net.PacketConn, level int, levelCount int) (net.PacketConn, error)
}

type UdpmaskManager struct {
	udpmasks []Udpmask
}

func NewUdpmaskManager(udpmasks []Udpmask) *UdpmaskManager {
	return &UdpmaskManager{
		udpmasks: udpmasks,
	}
}

func (m *UdpmaskManager) WrapPacketConnClient(raw net.PacketConn) (net.PacketConn, error) {
	var sizes []int
	var conns []net.PacketConn
	for i, mask := range m.udpmasks {
		if _, ok := mask.(headerConn); ok {
			if mode, ok := mask.(headerConnMode); ok && !mode.UseHeaderConn() {
				if len(conns) > 0 {
					raw = newHeaderManagerConn(sizes, conns, raw)
					sizes = nil
					conns = nil
				}
				var err error
				raw, err = mask.WrapPacketConnClient(raw, i, len(m.udpmasks)-1)
				if err != nil {
					return nil, err
				}
				continue
			}
			conn, err := mask.WrapPacketConnClient(nil, i, len(m.udpmasks)-1)
			if err != nil {
				return nil, err
			}
			sizes = append(sizes, conn.(headerSize).Size())
			conns = append(conns, conn)
		} else {
			if len(conns) > 0 {
				raw = newHeaderManagerConn(sizes, conns, raw)
				sizes = nil
				conns = nil
			}
			var err error
			raw, err = mask.WrapPacketConnClient(raw, i, len(m.udpmasks)-1)
			if err != nil {
				return nil, err
			}
		}
	}

	if len(conns) > 0 {
		raw = newHeaderManagerConn(sizes, conns, raw)
		sizes = nil
		conns = nil
	}
	return raw, nil
}

func (m *UdpmaskManager) WrapPacketConnServer(raw net.PacketConn) (net.PacketConn, error) {
	var sizes []int
	var conns []net.PacketConn
	for i, mask := range m.udpmasks {
		if _, ok := mask.(headerConn); ok {
			if mode, ok := mask.(headerConnMode); ok && !mode.UseHeaderConn() {
				if len(conns) > 0 {
					raw = newHeaderManagerConn(sizes, conns, raw)
					sizes = nil
					conns = nil
				}
				var err error
				raw, err = mask.WrapPacketConnServer(raw, i, len(m.udpmasks)-1)
				if err != nil {
					return nil, err
				}
				continue
			}
			conn, err := mask.WrapPacketConnServer(nil, i, len(m.udpmasks)-1)
			if err != nil {
				return nil, err
			}
			sizes = append(sizes, conn.(headerSize).Size())
			conns = append(conns, conn)
		} else {
			if len(conns) > 0 {
				raw = newHeaderManagerConn(sizes, conns, raw)
				sizes = nil
				conns = nil
			}
			var err error
			raw, err = mask.WrapPacketConnServer(raw, i, len(m.udpmasks)-1)
			if err != nil {
				return nil, err
			}
		}
	}

	if len(conns) > 0 {
		raw = newHeaderManagerConn(sizes, conns, raw)
		sizes = nil
		conns = nil
	}
	return raw, nil
}

const (
	UDPSize = 4096
)

type headerConn interface {
	HeaderConn()
}

type headerConnMode interface {
	UseHeaderConn() bool
}

type headerSize interface {
	Size() int
}

type headerManagerConn struct {
	sync.Mutex
	net.PacketConn

	sizes    []int
	conns    []net.PacketConn
	writeBuf [UDPSize]byte
}

type oobPacketConn interface {
	net.PacketConn
	SyscallConn() (syscall.RawConn, error)
	SetReadBuffer(int) error
	ReadMsgUDP(b, oob []byte) (n, oobn, flags int, addr *net.UDPAddr, err error)
	WriteMsgUDP(b, oob []byte, addr *net.UDPAddr) (n, oobn int, err error)
}

type packetBufferConn interface {
	SetReadBuffer(int) error
	SetWriteBuffer(int) error
}

type batchPacketConn interface {
	ReadBatch([]ipv4.Message, int) (int, error)
}

type syscallPacketConn interface {
	SyscallConn() (syscall.RawConn, error)
}

type headerManagerBufferedConn struct {
	*headerManagerConn
	raw packetBufferConn
}

type headerManagerSyscallConn struct {
	*headerManagerBufferedConn
	raw syscallPacketConn
}

// headerManagerOOBConn preserves the UDP capabilities quic-go probes for. ReadBatch must stay on
// this wrapper: letting ipv4.PacketConn unwrap the file descriptor would bypass the masks entirely.
type headerManagerOOBConn struct {
	*headerManagerConn
	raw   oobPacketConn
	batch batchPacketConn
}

func newHeaderManagerConn(sizes []int, conns []net.PacketConn, raw net.PacketConn) net.PacketConn {
	managed := &headerManagerConn{sizes: sizes, conns: conns, PacketConn: raw}
	oob, ok := raw.(oobPacketConn)
	if ok {
		batch, batchOK := raw.(batchPacketConn)
		if !batchOK {
			batch = ipv4.NewPacketConn(raw)
		}
		return &headerManagerOOBConn{headerManagerConn: managed, raw: oob, batch: batch}
	}
	buffered, bufferOK := raw.(packetBufferConn)
	if bufferOK {
		wrapped := &headerManagerBufferedConn{headerManagerConn: managed, raw: buffered}
		if syscallConn, syscallOK := raw.(syscallPacketConn); syscallOK {
			return &headerManagerSyscallConn{headerManagerBufferedConn: wrapped, raw: syscallConn}
		}
		return wrapped
	}
	return managed
}

type headerReadAddrAware interface {
	SetReadAddr(net.Addr)
}

func (c *headerManagerConn) headerSize() int {
	size := 0
	for _, n := range c.sizes {
		size += n
	}
	return size
}

func (c *headerManagerConn) decodePacket(buf []byte, addr net.Addr) ([]byte, error) {
	headerSize := c.headerSize()
	if len(buf) < headerSize {
		return nil, io.ErrUnexpectedEOF
	}

	packet := buf
	decodedSize := len(buf)
	for i := range c.conns {
		if aware, ok := c.conns[i].(headerReadAddrAware); ok {
			aware.SetReadAddr(addr)
		}
		n, _, err := c.conns[i].ReadFrom(packet)
		if err != nil {
			return nil, err
		}
		if n < 0 || c.sizes[i]+n > len(packet) {
			return nil, io.ErrUnexpectedEOF
		}
		decodedSize = n
		packet = packet[c.sizes[i] : c.sizes[i]+n]
	}
	return buf[headerSize : headerSize+decodedSize], nil
}

func (c *headerManagerConn) encodePacket(dst, payload []byte, addr net.Addr) ([]byte, error) {
	headerSize := c.headerSize()
	if len(dst) < headerSize+len(payload) {
		return nil, io.ErrShortBuffer
	}

	copy(dst[headerSize:], payload)
	encodedSize := len(payload)
	prefix := headerSize
	for i := len(c.conns) - 1; i >= 0; i-- {
		prefix -= c.sizes[i]
		packet := dst[prefix : headerSize+encodedSize]
		n, err := c.conns[i].WriteTo(packet, addr)
		if err != nil {
			return nil, err
		}
		// Some masks append a trailer as well as their declared prefix. They return the expanded
		// length and use the spare capacity behind packet, as the legacy WriteTo path allows.
		if n < len(packet) || n > cap(packet) {
			return nil, io.ErrShortWrite
		}
		encodedSize = n
		headerSize = prefix
	}
	return dst[:encodedSize], nil
}

func (c *headerManagerConn) ReadFrom(p []byte) (n int, addr net.Addr, err error) {
	bufferSize := max(UDPSize, len(p)+c.headerSize())
	buf := bytespool.Alloc(int32(bufferSize))
	buf = buf[:bufferSize]
	defer bytespool.Free(buf)

	n, addr, err = c.PacketConn.ReadFrom(buf)
	if n == 0 || err != nil {
		return n, addr, err
	}

	payload, err := c.decodePacket(buf[:n], addr)
	if err != nil {
		errors.LogDebug(context.Background(), addr, " mask read err ", err)
		return 0, addr, nil
	}
	if len(p) < len(payload) {
		errors.LogDebug(context.Background(), addr, " mask read err short buffer")
		return 0, addr, nil
	}
	return copy(p, payload), addr, nil
}

func (c *headerManagerConn) WriteTo(p []byte, addr net.Addr) (n int, err error) {
	c.Lock()
	defer c.Unlock()

	if c.headerSize()+len(p) > len(c.writeBuf) {
		errors.LogDebug(context.Background(), addr, " mask write err short write")
		return 0, nil
	}
	wire, err := c.encodePacket(c.writeBuf[:], p, addr)
	if err != nil {
		errors.LogDebug(context.Background(), addr, " mask write err ", err)
		return 0, nil
	}
	n, err = c.PacketConn.WriteTo(wire, addr)
	if err != nil {
		return n, err
	}
	if n != len(wire) {
		return 0, io.ErrShortWrite
	}
	return len(p), nil
}

func (c *headerManagerBufferedConn) SetReadBuffer(bytes int) error {
	return c.raw.SetReadBuffer(bytes)
}

func (c *headerManagerBufferedConn) SetWriteBuffer(bytes int) error {
	return c.raw.SetWriteBuffer(bytes)
}

func (c *headerManagerSyscallConn) SyscallConn() (syscall.RawConn, error) {
	return c.raw.SyscallConn()
}

func (c *headerManagerOOBConn) SyscallConn() (syscall.RawConn, error) {
	return c.raw.SyscallConn()
}

func (c *headerManagerOOBConn) SetReadBuffer(bytes int) error {
	return c.raw.SetReadBuffer(bytes)
}

func (c *headerManagerOOBConn) SetWriteBuffer(bytes int) error {
	if conn, ok := c.raw.(packetBufferConn); ok {
		return conn.SetWriteBuffer(bytes)
	}
	return nil
}

func (c *headerManagerOOBConn) ReadMsgUDP(b, oob []byte) (n, oobn, flags int, addr *net.UDPAddr, err error) {
	bufferSize := max(UDPSize, len(b)+c.headerSize())
	buf := bytespool.Alloc(int32(bufferSize))
	buf = buf[:bufferSize]
	defer bytespool.Free(buf)

	n, oobn, flags, addr, err = c.raw.ReadMsgUDP(buf, oob)
	if n == 0 || err != nil {
		return n, oobn, flags, addr, err
	}
	payload, decodeErr := c.decodePacket(buf[:n], addr)
	if decodeErr != nil {
		errors.LogDebug(context.Background(), addr, " mask read msg err ", decodeErr)
		return 0, oobn, flags, addr, nil
	}
	if len(b) < len(payload) {
		return 0, oobn, flags, addr, io.ErrShortBuffer
	}
	return copy(b, payload), oobn, flags, addr, nil
}

func (c *headerManagerOOBConn) WriteMsgUDP(b, oob []byte, addr *net.UDPAddr) (n, oobn int, err error) {
	c.Lock()
	defer c.Unlock()

	gsoSize, err := udpSegmentSize(oob)
	if err != nil {
		return 0, 0, err
	}
	segments := 1
	if gsoSize > 0 && len(b) > gsoSize {
		segments = (len(b) + gsoSize - 1) / gsoSize
	}
	// UDPSize is the established maximum encoded size for one finalmask datagram. Reserving it
	// per segment also leaves room for masks such as AES-GCM that append a trailer not included in
	// headerSize.
	bufferSize := len(b) + segments*UDPSize
	buf := bytespool.Alloc(int32(bufferSize))
	buf = buf[:bufferSize]
	defer bytespool.Free(buf)

	wireSize := 0
	wireOverhead := -1
	for offset := 0; offset < len(b) || offset == 0; {
		end := len(b)
		if gsoSize > 0 && end-offset > gsoSize {
			end = offset + gsoSize
		}
		wire, encodeErr := c.encodePacket(buf[wireSize:], b[offset:end], addr)
		if encodeErr != nil {
			return 0, 0, encodeErr
		}
		segmentOverhead := len(wire) - (end - offset)
		if wireOverhead < 0 {
			wireOverhead = segmentOverhead
		} else if segmentOverhead != wireOverhead {
			return 0, 0, errors.New("mask produces variable GSO segment overhead")
		}
		wireSize += len(wire)
		offset = end
		if len(b) == 0 {
			break
		}
	}
	adjustedOOB, err := adjustUDPSegmentSize(oob, wireOverhead)
	if err != nil {
		return 0, 0, err
	}

	n, oobn, err = c.raw.WriteMsgUDP(buf[:wireSize], adjustedOOB, addr)
	if err != nil {
		return n, oobn, err
	}
	if n != wireSize {
		return 0, oobn, io.ErrShortWrite
	}
	return len(b), oobn, nil
}

func (c *headerManagerOOBConn) ReadBatch(messages []ipv4.Message, flags int) (int, error) {
	if len(messages) == 0 {
		return 0, nil
	}

	for {
		temporary := make([]ipv4.Message, len(messages))
		buffers := make([][]byte, len(messages))
		bufferVectors := make([][]byte, len(messages))
		for i := range messages {
			bufferSize := max(UDPSize, messageBufferSize(messages[i].Buffers)+c.headerSize())
			buffers[i] = bytespool.Alloc(int32(bufferSize))
			buffers[i] = buffers[i][:bufferSize]
			bufferVectors[i] = buffers[i]
			temporary[i].Buffers = bufferVectors[i : i+1]
			temporary[i].OOB = messages[i].OOB
		}

		n, readErr := c.batch.ReadBatch(temporary, flags)
		valid := 0
		for i := 0; i < n; i++ {
			payload, decodeErr := c.decodePacket(buffers[i][:temporary[i].N], temporary[i].Addr)
			if decodeErr != nil {
				errors.LogDebug(context.Background(), temporary[i].Addr, " mask batch read err ", decodeErr)
				continue
			}
			if len(payload) > messageBufferSize(messages[valid].Buffers) {
				for _, buffer := range buffers {
					bytespool.Free(buffer)
				}
				return valid, io.ErrShortBuffer
			}
			messages[valid].N = copyMessageBuffers(messages[valid].Buffers, payload)
			messages[valid].NN = copy(messages[valid].OOB, temporary[i].OOB[:temporary[i].NN])
			messages[valid].Flags = temporary[i].Flags
			messages[valid].Addr = temporary[i].Addr
			valid++
		}
		for _, buffer := range buffers {
			bytespool.Free(buffer)
		}

		if valid > 0 || readErr != nil {
			return valid, readErr
		}
	}
}

func messageBufferSize(buffers [][]byte) int {
	size := 0
	for _, buffer := range buffers {
		size += len(buffer)
	}
	return size
}

func copyMessageBuffers(buffers [][]byte, payload []byte) int {
	written := 0
	for _, buffer := range buffers {
		written += copy(buffer, payload[written:])
		if written == len(payload) {
			break
		}
	}
	return written
}

type Tcpmask interface {
	TCP()

	WrapConnClient(net.Conn) (net.Conn, error)
	WrapConnServer(net.Conn) (net.Conn, error)
}

type TcpmaskManager struct {
	tcpmasks []Tcpmask
}

func NewTcpmaskManager(tcpmasks []Tcpmask) *TcpmaskManager {
	return &TcpmaskManager{
		tcpmasks: tcpmasks,
	}
}

func (m *TcpmaskManager) WrapConnClient(raw net.Conn) (net.Conn, error) {
	var err error
	for _, mask := range m.tcpmasks {
		raw, err = mask.WrapConnClient(raw)
		if err != nil {
			return nil, err
		}
	}
	return raw, nil
}

func (m *TcpmaskManager) WrapConnServer(raw net.Conn) (net.Conn, error) {
	var err error
	for _, mask := range m.tcpmasks {
		raw, err = mask.WrapConnServer(raw)
		if err != nil {
			return nil, err
		}
	}
	return raw, nil
}

func (m *TcpmaskManager) WrapListener(l net.Listener) (net.Listener, error) {
	return NewTcpListener(m, l)
}

type tcpListener struct {
	m *TcpmaskManager
	net.Listener
}

func NewTcpListener(m *TcpmaskManager, l net.Listener) (net.Listener, error) {
	return &tcpListener{
		m:        m,
		Listener: l,
	}, nil
}

func (l *tcpListener) Accept() (net.Conn, error) {
	conn, err := l.Listener.Accept()
	if err != nil {
		return conn, err
	}

	newConn, err := l.m.WrapConnServer(conn)
	if err != nil {
		errors.LogDebugInner(context.Background(), err, "mask err")
		_ = conn.Close()
		return nil, err
	}

	return newConn, nil
}

type TcpMaskConn interface {
	TcpMaskConn()
	RawConn() net.Conn
	Splice() bool
}

func UnwrapTcpMask(conn net.Conn) net.Conn {
	for {
		if v, ok := conn.(TcpMaskConn); ok {
			if !v.Splice() {
				return conn
			}
			conn = v.RawConn()
		} else {
			return conn
		}
	}
}
