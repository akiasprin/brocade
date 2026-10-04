package udphop

import (
	"errors"
	"math/rand"
	"net"
	"sync"
	"syscall"
	"time"

	"github.com/xtls/xray-core/common/crypto"
	"github.com/xtls/xray-core/transport/internet/finalmask"
	"golang.org/x/net/ipv4"
)

const (
	packetQueueSize      = 1024
	udpBatchSize         = 8
	packetBatchQueueSize = packetQueueSize / udpBatchSize
	udpBufferSize        = finalmask.UDPSize
	udpOOBBufferSize     = 128

	defaultHopInterval = 30 * time.Second
)

type UdpHopPacketConn struct {
	Addr           net.Addr
	Addrs          []net.Addr
	HopIntervalMin int64
	HopIntervalMax int64
	ListenUDPFunc  ListenUDPFunc

	connMutex   sync.RWMutex
	prevConn    net.PacketConn
	currentConn net.PacketConn
	addrIndex   int

	readBufferSize  int
	writeBufferSize int

	recvQueue chan *udpPacketBatch
	closeChan chan struct{}
	closed    bool
	oob       bool

	readMutex        sync.Mutex
	pendingReadBatch *udpPacketBatch
	pendingReadIndex int
	remoteAddr       *net.UDPAddr

	bufPool   sync.Pool
	oobPool   sync.Pool
	batchPool sync.Pool
}

type udpPacket struct {
	Buf   []byte
	N     int
	OOB   []byte
	OOBN  int
	Flags int
}

type udpPacketBatch struct {
	Packets [udpBatchSize]udpPacket
	N       int
	Err     error
}

type ListenUDPFunc = func(*net.UDPAddr) (net.PacketConn, error)

type oobPacketConn interface {
	net.PacketConn
	SyscallConn() (syscall.RawConn, error)
	SetReadBuffer(int) error
	ReadMsgUDP(b, oob []byte) (n, oobn, flags int, addr *net.UDPAddr, err error)
	WriteMsgUDP(b, oob []byte, addr *net.UDPAddr) (n, oobn int, err error)
}

type batchPacketConn interface {
	ReadBatch([]ipv4.Message, int) (int, error)
}

// udpHopOOBConn only exists when all sockets used by the port hopper can preserve UDP OOB data.
// Keeping these methods off UdpHopPacketConn prevents quic-go from enabling GSO for a custom
// PacketConn that can't actually forward UDP_SEGMENT control messages.
type udpHopOOBConn struct {
	*UdpHopPacketConn
}

var (
	_ oobPacketConn   = (*udpHopOOBConn)(nil)
	_ batchPacketConn = (*udpHopOOBConn)(nil)
)

func NewUDPHopPacketConn(addr *UDPHopAddr, index int, intervalMin int64, intervalMax int64, listenUDPFunc ListenUDPFunc, pktConn net.PacketConn) (net.PacketConn, error) {
	if intervalMin == 0 || intervalMax == 0 {
		intervalMin = int64(defaultHopInterval / time.Second)
		intervalMax = int64(defaultHopInterval / time.Second)
	}
	if intervalMin < 5 || intervalMax < 5 {
		return nil, errors.New("hop interval must be at least 5 seconds")
	}
	// if listenUDPFunc == nil {
	// 	listenUDPFunc = func() (net.PacketConn, error) {
	// 		return net.ListenUDP("udp", nil)
	// 	}
	// }
	if listenUDPFunc == nil {
		return nil, errors.New("nil listenUDPFunc")
	}
	addrs, err := addr.addrs()
	if err != nil {
		return nil, err
	}
	if len(addrs) == 0 {
		return nil, errors.New("empty UDP hop port list")
	}
	if index < 0 || index >= len(addrs) {
		return nil, errors.New("UDP hop address index out of range")
	}
	// curConn, err := listenUDPFunc()
	// if err != nil {
	// 	return nil, err
	// }
	remoteAddr := cloneUDPAddr(addrs[index].(*net.UDPAddr))
	_, oob := pktConn.(oobPacketConn)
	queueSize := packetQueueSize
	if oob {
		queueSize = packetBatchQueueSize
	}
	hConn := &UdpHopPacketConn{
		Addr:           addr,
		Addrs:          addrs,
		HopIntervalMin: intervalMin,
		HopIntervalMax: intervalMax,
		ListenUDPFunc:  listenUDPFunc,
		prevConn:       nil,
		currentConn:    pktConn,
		addrIndex:      index,
		recvQueue:      make(chan *udpPacketBatch, queueSize),
		closeChan:      make(chan struct{}),
		oob:            oob,
		remoteAddr:     remoteAddr,
		bufPool: sync.Pool{
			New: func() interface{} {
				return make([]byte, udpBufferSize)
			},
		},
		oobPool: sync.Pool{
			New: func() interface{} {
				return make([]byte, udpOOBBufferSize)
			},
		},
		batchPool: sync.Pool{
			New: func() interface{} {
				return new(udpPacketBatch)
			},
		},
	}
	go hConn.recvLoop(pktConn)
	go hConn.hopLoop()
	if oob {
		return &udpHopOOBConn{UdpHopPacketConn: hConn}, nil
	}
	return hConn, nil
}

func (u *UdpHopPacketConn) recvLoop(conn net.PacketConn) {
	if u.oob {
		u.recvBatchLoop(conn.(oobPacketConn))
		return
	}
	u.recvSingleLoop(conn)
}

func (u *UdpHopPacketConn) recvSingleLoop(conn net.PacketConn) {
	for {
		buf := u.bufPool.Get().([]byte)
		n, _, err := conn.ReadFrom(buf)
		if err != nil {
			u.bufPool.Put(buf)
			u.enqueueReadError(err)
			return
		}

		batch := u.getPacketBatch()
		batch.N = 1
		batch.Packets[0] = udpPacket{Buf: buf, N: n}
		if !u.enqueueBatch(batch) {
			u.releasePacketBatch(batch)
		}
	}
}

func (u *UdpHopPacketConn) recvBatchLoop(conn oobPacketConn) {
	batchConn, ok := any(conn).(batchPacketConn)
	if !ok {
		// This is safe here because the port hopper owns the underlying socket. In contrast,
		// quic-go must use udpHopOOBConn.ReadBatch so it doesn't unwrap just the current fd.
		batchConn = ipv4.NewPacketConn(conn)
	}

	messages := make([]ipv4.Message, udpBatchSize)
	bufferVectors := make([][]byte, udpBatchSize)
	for {
		for i := range messages {
			buf := u.bufPool.Get().([]byte)
			oob := u.oobPool.Get().([]byte)
			bufferVectors[i] = buf
			messages[i] = ipv4.Message{
				Buffers: bufferVectors[i : i+1],
				OOB:     oob,
			}
		}

		n, err := batchConn.ReadBatch(messages, 0)
		if n > 0 {
			batch := u.getPacketBatch()
			batch.N = n
			for i := 0; i < n; i++ {
				batch.Packets[i] = udpPacket{
					Buf:   bufferVectors[i],
					N:     messages[i].N,
					OOB:   messages[i].OOB,
					OOBN:  messages[i].NN,
					Flags: messages[i].Flags,
				}
			}
			for i := n; i < len(messages); i++ {
				u.bufPool.Put(bufferVectors[i])
				u.oobPool.Put(messages[i].OOB)
			}
			if !u.enqueueBatch(batch) {
				u.releasePacketBatch(batch)
			}
		} else {
			for i := range messages {
				u.bufPool.Put(bufferVectors[i])
				u.oobPool.Put(messages[i].OOB)
			}
		}
		if err != nil {
			u.enqueueReadError(err)
			return
		}
	}
}

func (u *UdpHopPacketConn) getPacketBatch() *udpPacketBatch {
	batch := u.batchPool.Get().(*udpPacketBatch)
	*batch = udpPacketBatch{}
	return batch
}

func (u *UdpHopPacketConn) enqueueBatch(batch *udpPacketBatch) bool {
	select {
	case u.recvQueue <- batch:
		return true
	case <-u.closeChan:
		return false
	default:
		return false
	}
}

func (u *UdpHopPacketConn) enqueueReadError(err error) {
	var netErr net.Error
	if !errors.As(err, &netErr) || !netErr.Timeout() {
		// Closing the previous socket is part of every hop, so permanent read errors from it
		// must not tear down the active QUIC connection.
		return
	}
	batch := u.getPacketBatch()
	batch.Err = netErr
	if !u.enqueueBatch(batch) {
		u.releasePacketBatch(batch)
	}
}

func (u *UdpHopPacketConn) releasePacket(packet *udpPacket) {
	if packet.Buf != nil {
		u.bufPool.Put(packet.Buf)
	}
	if packet.OOB != nil {
		u.oobPool.Put(packet.OOB)
	}
	*packet = udpPacket{}
}

func (u *UdpHopPacketConn) releasePacketBatch(batch *udpPacketBatch) {
	for i := 0; i < batch.N; i++ {
		u.releasePacket(&batch.Packets[i])
	}
	*batch = udpPacketBatch{}
	u.batchPool.Put(batch)
}

func (u *UdpHopPacketConn) hopLoop() {
	ticker := time.NewTicker(time.Duration(crypto.RandBetween(u.HopIntervalMin, u.HopIntervalMax)) * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ticker.C:
			u.hop()
			ticker.Reset(time.Duration(crypto.RandBetween(u.HopIntervalMin, u.HopIntervalMax)) * time.Second)
		case <-u.closeChan:
			return
		}
	}
}

func (u *UdpHopPacketConn) hop() {
	u.connMutex.Lock()
	defer u.connMutex.Unlock()
	if u.closed {
		return
	}
	nextAddrIndex := rand.Intn(len(u.Addrs))
	newConn, err := u.ListenUDPFunc(u.Addrs[nextAddrIndex].(*net.UDPAddr))
	if err != nil {
		// Could be temporary, just skip this hop
		return
	}
	if u.oob {
		if _, ok := newConn.(oobPacketConn); !ok {
			// Don't silently lose GSO / ECN after quic-go has cached those capabilities from
			// the initial socket. Keep using the current socket and retry at the next hop.
			_ = newConn.Close()
			return
		}
	}
	// We need to keep receiving packets from the previous connection,
	// because otherwise there will be packet loss due to the time gap
	// between we hop to a new port and the server acknowledges this change.
	// So we do the following:
	// Close prevConn,
	// move currentConn to prevConn,
	// set newConn as currentConn,
	// start recvLoop on newConn.
	if u.prevConn != nil {
		_ = u.prevConn.Close() // recvLoop for this conn will exit
	}
	u.prevConn = u.currentConn
	u.currentConn = newConn
	u.addrIndex = nextAddrIndex
	// Set buffer sizes if previously set
	if u.readBufferSize > 0 {
		_ = trySetReadBuffer(u.currentConn, u.readBufferSize)
	}
	if u.writeBufferSize > 0 {
		_ = trySetWriteBuffer(u.currentConn, u.writeBufferSize)
	}
	go u.recvLoop(newConn)
}

func (u *UdpHopPacketConn) ReadFrom(b []byte) (n int, addr net.Addr, err error) {
	u.readMutex.Lock()
	defer u.readMutex.Unlock()

	packet, err := u.readPacket()
	if err != nil {
		return 0, nil, err
	}
	n = copy(b, packet.Buf[:packet.N])
	u.releasePacket(&packet)
	// Keep the virtual address stable across hops, as the original single-packet path did.
	return n, u.Addr, nil
}

func (u *UdpHopPacketConn) WriteTo(b []byte, addr net.Addr) (n int, err error) {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	if u.closed {
		return 0, net.ErrClosed
	}
	// Skip the check for now, always write to the server,
	// for the same reason as in ReadFrom.
	return u.currentConn.WriteTo(b, u.Addrs[u.addrIndex])
}

func (u *UdpHopPacketConn) readPacket() (udpPacket, error) {
	batch, err := u.waitForReadBatch()
	if err != nil {
		return udpPacket{}, err
	}
	packet := batch.Packets[u.pendingReadIndex]
	batch.Packets[u.pendingReadIndex] = udpPacket{}
	u.pendingReadIndex++
	if u.pendingReadIndex == batch.N {
		u.finishPendingReadBatch()
	}
	return packet, nil
}

func (u *UdpHopPacketConn) waitForReadBatch() (*udpPacketBatch, error) {
	if u.pendingReadBatch != nil {
		return u.pendingReadBatch, nil
	}
	select {
	case batch := <-u.recvQueue:
		if batch.Err != nil {
			err := batch.Err
			u.releasePacketBatch(batch)
			return nil, err
		}
		u.pendingReadBatch = batch
		u.pendingReadIndex = 0
		return batch, nil
	case <-u.closeChan:
		return nil, net.ErrClosed
	}
}

func (u *UdpHopPacketConn) finishPendingReadBatch() {
	batch := u.pendingReadBatch
	u.pendingReadBatch = nil
	u.pendingReadIndex = 0
	*batch = udpPacketBatch{}
	u.batchPool.Put(batch)
}

func (c *udpHopOOBConn) ReadMsgUDP(b, oob []byte) (n, oobn, flags int, addr *net.UDPAddr, err error) {
	c.readMutex.Lock()
	defer c.readMutex.Unlock()

	packet, err := c.readPacket()
	if err != nil {
		return 0, 0, 0, nil, err
	}
	n = copy(b, packet.Buf[:packet.N])
	oobn = copy(oob, packet.OOB[:packet.OOBN])
	flags = packet.Flags
	c.releasePacket(&packet)
	return n, oobn, flags, c.remoteAddr, nil
}

func (c *udpHopOOBConn) WriteMsgUDP(b, oob []byte, _ *net.UDPAddr) (n, oobn int, err error) {
	c.connMutex.RLock()
	defer c.connMutex.RUnlock()
	if c.closed {
		return 0, 0, net.ErrClosed
	}
	conn := c.currentConn.(oobPacketConn)
	destination := c.Addrs[c.addrIndex].(*net.UDPAddr)
	if remoteConn, ok := c.currentConn.(interface{ RemoteAddr() net.Addr }); ok && remoteConn.RemoteAddr() != nil {
		// net.UDPConn requires a nil destination when it is connected.
		destination = nil
	}
	return conn.WriteMsgUDP(b, oob, destination)
}

// ReadBatch exposes batches already read from the current and previous hop sockets. It must not
// let ipv4.PacketConn unwrap SyscallConn, since that would read only the current socket and bypass
// the queue that bridges a port transition.
func (c *udpHopOOBConn) ReadBatch(messages []ipv4.Message, _ int) (int, error) {
	if len(messages) == 0 {
		return 0, nil
	}
	c.readMutex.Lock()
	defer c.readMutex.Unlock()

	batch, err := c.waitForReadBatch()
	if err != nil {
		return 0, err
	}
	n := min(len(messages), batch.N-c.pendingReadIndex)
	for i := 0; i < n; i++ {
		packet := &batch.Packets[c.pendingReadIndex]
		messages[i].N = copyToBuffers(messages[i].Buffers, packet.Buf[:packet.N])
		messages[i].NN = copy(messages[i].OOB, packet.OOB[:packet.OOBN])
		messages[i].Flags = packet.Flags
		messages[i].Addr = c.remoteAddr
		c.releasePacket(packet)
		c.pendingReadIndex++
	}
	if c.pendingReadIndex == batch.N {
		c.finishPendingReadBatch()
	}
	return n, nil
}

func copyToBuffers(buffers [][]byte, payload []byte) int {
	copied := 0
	for _, buffer := range buffers {
		if len(payload) == 0 {
			break
		}
		n := copy(buffer, payload)
		copied += n
		payload = payload[n:]
	}
	return copied
}

func cloneUDPAddr(addr *net.UDPAddr) *net.UDPAddr {
	if addr == nil {
		return nil
	}
	clone := *addr
	clone.IP = append(net.IP(nil), addr.IP...)
	return &clone
}

func (u *UdpHopPacketConn) Close() error {
	u.connMutex.Lock()
	defer u.connMutex.Unlock()
	if u.closed {
		return nil
	}
	// Close prevConn and currentConn
	// Close closeChan to unblock ReadFrom & hopLoop
	// Set closed flag to true to prevent double close
	if u.prevConn != nil {
		_ = u.prevConn.Close()
	}
	err := u.currentConn.Close()
	close(u.closeChan)
	u.closed = true
	u.Addrs = nil // For GC
	return err
}

func (u *UdpHopPacketConn) LocalAddr() net.Addr {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	return u.currentConn.LocalAddr()
}

func (u *UdpHopPacketConn) SetDeadline(t time.Time) error {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	if u.prevConn != nil {
		_ = u.prevConn.SetDeadline(t)
	}
	return u.currentConn.SetDeadline(t)
}

func (u *UdpHopPacketConn) SetReadDeadline(t time.Time) error {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	if u.prevConn != nil {
		_ = u.prevConn.SetReadDeadline(t)
	}
	return u.currentConn.SetReadDeadline(t)
}

func (u *UdpHopPacketConn) SetWriteDeadline(t time.Time) error {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	if u.prevConn != nil {
		_ = u.prevConn.SetWriteDeadline(t)
	}
	return u.currentConn.SetWriteDeadline(t)
}

// UDP-specific methods below

func (u *UdpHopPacketConn) SetReadBuffer(bytes int) error {
	u.connMutex.Lock()
	defer u.connMutex.Unlock()
	u.readBufferSize = bytes
	if u.prevConn != nil {
		_ = trySetReadBuffer(u.prevConn, bytes)
	}
	return trySetReadBuffer(u.currentConn, bytes)
}

func (u *UdpHopPacketConn) SetWriteBuffer(bytes int) error {
	u.connMutex.Lock()
	defer u.connMutex.Unlock()
	u.writeBufferSize = bytes
	if u.prevConn != nil {
		_ = trySetWriteBuffer(u.prevConn, bytes)
	}
	return trySetWriteBuffer(u.currentConn, bytes)
}

func (u *UdpHopPacketConn) SyscallConn() (syscall.RawConn, error) {
	u.connMutex.RLock()
	defer u.connMutex.RUnlock()
	sc, ok := u.currentConn.(syscall.Conn)
	if !ok {
		return nil, errors.New("not supported")
	}
	return sc.SyscallConn()
}

func trySetReadBuffer(pc net.PacketConn, bytes int) error {
	sc, ok := pc.(interface {
		SetReadBuffer(bytes int) error
	})
	if ok {
		return sc.SetReadBuffer(bytes)
	}
	return nil
}

func trySetWriteBuffer(pc net.PacketConn, bytes int) error {
	sc, ok := pc.(interface {
		SetWriteBuffer(bytes int) error
	})
	if ok {
		return sc.SetWriteBuffer(bytes)
	}
	return nil
}
