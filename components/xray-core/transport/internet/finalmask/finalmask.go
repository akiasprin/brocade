package finalmask

import (
	"context"
	"fmt"
	"github.com/xtls/xray-core/common/bytespool"
	"golang.org/x/net/ipv4"
	"io"
	"slices"
	"sync"
	"syscall"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
)

type Dialer struct {
	DialTCP func(net.Destination) (net.Conn, error)
	DialUDP func(net.Destination) (net.Conn, error)
}

type ListenConfig struct {
	Listen       func(net.Addr) (net.Listener, error)
	ListenPacket func(net.Addr) (net.PacketConn, error)
}

type TCPMask interface {
	WrapConnClient(net.Conn, *net.Destination, *Dialer) (net.Conn, error)
	WrapConnServer(net.Conn) (net.Conn, error)
	// Listen(net.Listener) (net.Listener, error)
}

type UDPMask interface {
	WrapPacketConnClient(net.PacketConn, *net.Destination, *Dialer) (net.PacketConn, error)
	WrapPacketConnServer(net.PacketConn, net.Addr, *ListenConfig) (net.PacketConn, error)
}

type FinalMask struct {
	tcpMasks     []TCPMask
	udpMasks     []UDPMask
	dialTCP      func(context.Context, net.Destination) (net.Conn, error)
	listen       func(context.Context, net.Addr) (net.Listener, error)
	dialUDP      func(context.Context, net.Destination) (net.PacketConn, net.Addr, error)
	listenPacket func(context.Context, net.Addr) (net.PacketConn, error)
}

func NewFinalMask(tcpMasks []TCPMask, udpMasks []UDPMask, dialTCP func(context.Context, net.Destination) (net.Conn, error), listen func(context.Context, net.Addr) (net.Listener, error), dialUDP func(context.Context, net.Destination) (net.PacketConn, net.Addr, error), listenPacket func(context.Context, net.Addr) (net.PacketConn, error)) *FinalMask {
	slices.Reverse(tcpMasks)
	slices.Reverse(udpMasks)
	return &FinalMask{
		tcpMasks:     tcpMasks,
		udpMasks:     udpMasks,
		dialTCP:      dialTCP,
		dialUDP:      dialUDP,
		listen:       listen,
		listenPacket: listenPacket,
	}
}

// WithPacketListener returns a copy whose raw packet binds use listenPacket. It lets
// listener lifecycle controllers register the socket before masks wrap it, without
// changing the shared stream configuration or the mask order.
func (fm *FinalMask) WithPacketListener(listenPacket func(context.Context, net.Addr) (net.PacketConn, error)) *FinalMask {
	config := *fm
	config.listenPacket = listenPacket
	return &config
}

func (fm *FinalMask) DialTCP(ctx context.Context, dest net.Destination) (net.Conn, error) {
	if len(fm.tcpMasks) == 0 {
		return fm.dialTCP(ctx, dest)
	}
	for i := range fm.tcpMasks {
		if i > 0 {
			if _, ok := fm.tcpMasks[i].(interface{ HandleDial() }); ok {
				return nil, fmt.Errorf("incorrect index: %d %T", i, fm.tcpMasks[i])
			}
		}
	}
	var conn net.Conn
	var err error
	if _, ok := fm.tcpMasks[0].(interface{ HandleDial() }); !ok {
		conn, err = fm.dialTCP(ctx, dest)
		if err != nil {
			return nil, err
		}
	}
	dialer := &Dialer{
		DialTCP: func(dest net.Destination) (net.Conn, error) {
			return fm.dialTCP(ctx, dest)
		},
		DialUDP: func(dest net.Destination) (net.Conn, error) {
			conn, addr, err := fm.dialUDP(ctx, dest)
			if err != nil {
				return nil, err
			}
			return &net.PacketConnWrapper{PacketConn: conn, Dest: addr}, err
		},
	}
	for i := range fm.tcpMasks {
		var newConn net.Conn
		newConn, err = fm.tcpMasks[i].WrapConnClient(conn, &dest, dialer)
		if err != nil {
			common.CloseIfExists(conn)
			return nil, err
		}
		conn = newConn
	}
	return conn, nil
}

func (fm *FinalMask) Listen(ctx context.Context, addr net.Addr) (net.Listener, error) {
	if len(fm.tcpMasks) == 0 {
		return fm.listen(ctx, addr)
	}
	off := 0
	listener, err := fm.listen(ctx, addr)
	if err != nil {
		return nil, err
	}
	for i := range fm.tcpMasks {
		if _, ok := fm.tcpMasks[i].(interface {
			Listen(net.Listener) (net.Listener, error)
		}); ok {
			if i-off == 0 {
				l, err := fm.tcpMasks[i].(interface {
					Listen(net.Listener) (net.Listener, error)
				}).Listen(listener)
				if err != nil {
					listener.Close()
					return nil, err
				}
				listener = l
			} else {
				l, err := fm.tcpMasks[i].(interface {
					Listen(net.Listener) (net.Listener, error)
				}).Listen(&TCPListener{Listener: listener, tcpMasks: fm.tcpMasks[off:i]})
				if err != nil {
					listener.Close()
					return nil, err
				}
				listener = l
			}
			off = i + 1
		}
	}
	if off < len(fm.tcpMasks) {
		return &TCPListener{Listener: listener, tcpMasks: fm.tcpMasks[off:]}, nil
	}
	return listener, nil
}

func (fm *FinalMask) DialUDP(ctx context.Context, dest net.Destination) (net.Conn, error) {
	if len(fm.udpMasks) == 0 {
		conn, addr, err := fm.dialUDP(ctx, dest)
		if err != nil {
			return nil, err
		}
		return &net.PacketConnWrapper{PacketConn: conn, Dest: addr}, nil
	}
	for i := range fm.udpMasks {
		if i > 0 {
			if _, ok := fm.udpMasks[i].(interface{ HandleDial() }); ok {
				return nil, fmt.Errorf("incorrect index: %d %T", i, fm.udpMasks[i])
			}
		}
	}
	var conn net.PacketConn
	var addr net.Addr
	var err error
	if _, ok := fm.udpMasks[0].(interface{ HandleDial() }); !ok {
		conn, addr, err = fm.dialUDP(ctx, dest)
		if err != nil {
			return nil, err
		}
	}
	dialer := &Dialer{
		DialTCP: func(dest net.Destination) (net.Conn, error) {
			return fm.dialTCP(ctx, dest)
		},
		DialUDP: func(dest net.Destination) (net.Conn, error) {
			conn, addr, err := fm.dialUDP(ctx, dest)
			if err != nil {
				return nil, err
			}
			return &net.PacketConnWrapper{PacketConn: conn, Dest: addr}, err
		},
	}
	var sizes []int
	var conns []net.PacketConn
	for i := range fm.udpMasks {
		var newConn net.PacketConn
		if usesHeaderConn(fm.udpMasks[i]) {
			newConn, err = fm.udpMasks[i].WrapPacketConnClient(nil, nil, nil)
			if err != nil {
				_ = conn.Close()
				return nil, err
			}
			sizes = append(sizes, newConn.(interface{ Size() int }).Size())
			conns = append(conns, newConn)
		} else {
			if len(conns) > 0 {
				conn = newHeaderManagerConn(sizes, conns, conn)
				sizes = nil
				conns = nil
			}
			newConn, err = fm.udpMasks[i].WrapPacketConnClient(conn, &dest, dialer)
			if err == nil && addr == nil {
				if peer, ok := newConn.(interface{ RemoteAddr() net.Addr }); ok {
					addr = peer.RemoteAddr()
				}
			}
			if err != nil {
				common.CloseIfExists(conn)
				return nil, err
			}
			conn = newConn
		}
	}
	if len(conns) > 0 {
		conn = newHeaderManagerConn(sizes, conns, conn)
		sizes = nil
		conns = nil
	}
	if addr == nil {
		if peer, ok := conn.(interface{ RemoteAddr() net.Addr }); ok {
			addr = peer.RemoteAddr()
		}
		if addr == nil {
			addr = &net.UDPAddr{IP: []byte{0, 0, 0, 0}}
		}
	}
	return &net.PacketConnWrapper{PacketConn: conn, Dest: addr}, nil
}

func (fm *FinalMask) ListenPacket(ctx context.Context, addr net.Addr) (net.PacketConn, error) {
	if len(fm.udpMasks) == 0 {
		return fm.listenPacket(ctx, addr)
	}
	for i := range fm.udpMasks {
		if i > 0 {
			if _, ok := fm.udpMasks[i].(interface{ HandleListen() }); ok {
				return nil, fmt.Errorf("incorrect index: %d %T", i, fm.udpMasks[i])
			}
		}
	}
	var conn net.PacketConn
	var err error
	if _, ok := fm.udpMasks[0].(interface{ HandleListen() }); !ok {
		conn, err = fm.listenPacket(ctx, addr)
		if err != nil {
			return nil, err
		}
	}
	lc := &ListenConfig{
		Listen:       func(addr net.Addr) (net.Listener, error) { return fm.listen(ctx, addr) },
		ListenPacket: func(addr net.Addr) (net.PacketConn, error) { return fm.listenPacket(ctx, addr) },
	}
	var sizes []int
	var conns []net.PacketConn
	for i := range fm.udpMasks {
		var newConn net.PacketConn
		if usesHeaderConn(fm.udpMasks[i]) {
			newConn, err = fm.udpMasks[i].WrapPacketConnServer(nil, nil, nil)
			if err != nil {
				common.CloseIfExists(conn)
				return nil, err
			}
			sizes = append(sizes, newConn.(interface{ Size() int }).Size())
			conns = append(conns, newConn)
		} else {
			if len(conns) > 0 {
				conn = newHeaderManagerConn(sizes, conns, conn)
				sizes = nil
				conns = nil
			}
			newConn, err = fm.udpMasks[i].WrapPacketConnServer(conn, addr, lc)
			if err != nil {
				common.CloseIfExists(conn)
				return nil, err
			}
			conn = newConn
		}
	}
	if len(conns) > 0 {
		conn = newHeaderManagerConn(sizes, conns, conn)
		sizes = nil
		conns = nil
	}
	return conn, nil
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

type TCPListener struct {
	net.Listener
	tcpMasks []TCPMask
}

func (l *TCPListener) Accept() (net.Conn, error) {
	conn, err := l.Listener.Accept()
	if err != nil {
		return conn, err
	}

	for i := range l.tcpMasks {
		var newConn net.Conn
		newConn, err = l.tcpMasks[i].WrapConnServer(conn)
		if err != nil {
			_ = conn.Close()
			return nil, err
		}
		conn = newConn
	}
	return conn, nil
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

func usesHeaderConn(mask UDPMask) bool {
	if _, ok := mask.(headerConn); !ok {
		return false
	}
	if mode, ok := mask.(headerConnMode); ok {
		return mode.UseHeaderConn()
	}
	return true
}
