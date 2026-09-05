package anytls

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"strings"
	"sync"
	"sync/atomic"

	M "github.com/sagernet/sing/common/metadata"
	"github.com/sagernet/sing/common/uot"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/singbridge"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type session struct {
	isClient bool
	conn     stat.Connection
	br       *buf.BufferedReader
	bw       *buf.BufferedWriter
	fw       *frameWriter

	writeMu sync.Mutex
	openMu  sync.Mutex
	stateMu sync.RWMutex

	streamsMu sync.Mutex
	streams   map[uint32]*stream
	// lastPeerSID is the greatest stream ID accepted from client SYN frames.
	// The session reader is the only writer; streamsMu keeps tests and future
	// readers from observing it independently of the stream maps.
	lastPeerSID uint32
	// drainingStreams keeps FIN-closed streams reachable until their queued
	// inbound payloads have been delivered or the session is force-closed.
	drainingStreams map[uint32]*stream

	peerVersion byte
	closed      atomic.Bool
	seq         uint64

	server                 *Server
	dispatcher             routing.Dispatcher
	handshakeDone          bool
	serverSettingsReceived bool
	clientPaddingMD5       string

	client       *Client
	nextSID      atomic.Uint32
	pktCounter   atomic.Uint32
	settingsSent bool

	schemeMu      sync.RWMutex
	paddingScheme *paddingScheme

	activeStreams atomic.Int32
	idleSinceNano atomic.Int64
	inIdlePool    atomic.Bool
	dieHookMu     sync.Mutex
	dieHook       func()
	cancel        context.CancelFunc
}

func (s *session) peerVersionValue() byte {
	s.stateMu.RLock()
	defer s.stateMu.RUnlock()
	return s.peerVersion
}

func (s *session) setPeerVersion(version byte) {
	s.stateMu.Lock()
	s.peerVersion = version
	s.stateMu.Unlock()
}

func (s *session) handshakeDoneValue() bool {
	s.stateMu.RLock()
	defer s.stateMu.RUnlock()
	return s.handshakeDone
}

func (s *session) setHandshakeDone() {
	s.stateMu.Lock()
	s.handshakeDone = true
	s.stateMu.Unlock()
}

func (s *session) markServerSettingsReceived() bool {
	s.stateMu.Lock()
	defer s.stateMu.Unlock()
	if s.serverSettingsReceived {
		return false
	}
	s.serverSettingsReceived = true
	return true
}

func (s *session) setClientPaddingMD5(value string) {
	s.stateMu.Lock()
	s.clientPaddingMD5 = value
	s.stateMu.Unlock()
}

func (s *session) clientPaddingMD5Value() string {
	s.stateMu.RLock()
	defer s.stateMu.RUnlock()
	return s.clientPaddingMD5
}

func (s *session) setDieHook(hook func()) {
	if hook == nil {
		return
	}
	s.dieHookMu.Lock()
	if s.isClosed() {
		s.dieHookMu.Unlock()
		hook()
		return
	}
	s.dieHook = hook
	s.dieHookMu.Unlock()
}

func (s *session) handlePSH(ctx context.Context, st *stream, br *buf.BufferedReader, length int) error {
	if st == nil || st.link == nil {
		return errors.New("anytls: received PSH for unknown stream")
	}
	body, err := readMultiBufferExact(br, length)
	if err != nil {
		buf.ReleaseMulti(body)
		return err
	}

	s.startStreamDelivery(st)
	if err := st.enqueueDelivery(body); err != nil {
		if err == io.ErrClosedPipe {
			return nil
		}
		return err
	}
	return nil
}

func (s *session) startStreamDelivery(st *stream) {
	st.startDeliveryWorker(func(body buf.MultiBuffer) error {
		if st.isUDP {
			data := make([]byte, body.Len())
			body.Copy(data)
			buf.ReleaseMulti(body)
			return s.handleUDPData(st, data)
		}
		if st.link == nil || st.link.Writer == nil {
			buf.ReleaseMulti(body)
			return errors.New("anytls: stream writer is unavailable")
		}
		return st.link.Writer.WriteMultiBuffer(body)
	})
}

func (s *session) handleNewStream(ctx context.Context, st *stream, br *buf.BufferedReader, length int) error {
	body := make([]byte, length)
	if _, err := io.ReadFull(br, body); err != nil {
		return err
	}
	bodyReader := bytes.NewReader(body)
	addr, err := M.SocksaddrSerializer.ReadAddrPort(bodyReader)
	if err != nil {
		rejectErr := errors.New("anytls: invalid destination address in SYN").Base(err)
		errors.LogWarning(ctx, "anytls: invalid destination address, streamId=", st.sid, " err=", err)
		return s.rejectStream(st.sid, rejectErr)
	}
	dest := singbridge.ToDestination(addr, net.Network_TCP)
	if dest.Address == nil {
		rejectErr := errors.New("anytls: invalid destination address in SYN")
		return s.rejectStream(st.sid, rejectErr)
	}

	// Check for UDP-over-TCP v2 magic domain in a new stream request.
	if dest.Address.String() == "sp.v2.udp-over-tcp.arpa" && dest.Port == 0 {
		st.isUDP = true
		if err := s.sendFrame(newFrame(cmdSYNACK, st.sid)); err != nil {
			errors.LogWarning(ctx, "anytls: UDP SYNACK send error, streamId=", st.sid, " err=", err)
			return err
		}
		return nil
	}

	if s.isClosed() {
		return errors.New("anytls: session closed")
	}
	l, err := s.dispatcher.Dispatch(ctx, dest)
	if err != nil {
		errors.LogWarning(ctx, "anytls: new stream dispatcher error, streamId=", st.sid, " err=", err)
		if sendErr := s.sendFrame(&frame{cmd: cmdSYNACK, sid: st.sid, data: []byte(err.Error())}); sendErr != nil {
			s.finishStream(st.sid, sendErr)
			return sendErr
		}
		s.finishStream(st.sid, err)
		return nil
	}
	if !s.attachStreamLink(st, l) {
		closeTransportLink(l)
		return errors.New("anytls: session closed")
	}

	if err := s.sendFrame(newFrame(cmdSYNACK, st.sid)); err != nil {
		errors.LogWarning(ctx, "anytls: new stream SYNACK send error, streamId=", st.sid, " err=", err)
		return err
	}

	if bodyReader.Len() > 0 {
		initial, err := io.ReadAll(bodyReader)
		if err != nil {
			return err
		}
		s.startStreamDelivery(st)
		if err := st.enqueueDelivery(buf.MultiBuffer{buf.FromBytes(initial)}); err != nil {
			return err
		}
	}
	s.startStreamDelivery(st)
	go s.pumpDownlink(st.sid, l)
	return nil
}

// rejectStream reports a fully-read but unusable stream request without
// tearing down the authenticated session. Frame-level failures still return
// from readLoop because the peer may no longer be frame-synchronised.
func (s *session) rejectStream(sid uint32, err error) error {
	if err == nil {
		err = errors.New("anytls: stream rejected")
	}
	if sendErr := s.sendFrame(&frame{cmd: cmdSYNACK, sid: sid, data: []byte(err.Error())}); sendErr != nil {
		s.finishStream(sid, sendErr)
		return sendErr
	}
	s.finishStream(sid, err)
	return nil
}

func (s *session) handleFirstUDPFrame(ctx context.Context, st *stream, br *buf.BufferedReader, length int) error {
	if st.link == nil {
		body := make([]byte, length)
		if _, err := io.ReadFull(br, body); err != nil {
			return err
		}
		bodyReader := bytes.NewReader(body)
		request, err := uot.ReadRequest(bodyReader)
		if err != nil {
			errors.LogWarning(ctx, "anytls: UDP failed to parse request:", err)
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
			s.finishStream(st.sid, nil)
			return nil
		}
		requestDest := singbridge.ToDestination(request.Destination, net.Network_UDP)

		if s.isClosed() {
			return errors.New("anytls: session closed")
		}
		link, err := s.dispatcher.Dispatch(ctx, requestDest)
		if err != nil {
			errors.LogWarning(ctx, "anytls: UDP dispatcher error, streamId=", st.sid, " err=", err)
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
			s.finishStream(st.sid, nil)
			return nil
		}

		if !s.attachStreamLink(st, link) {
			closeTransportLink(link)
			return errors.New("anytls: session closed")
		}
		st.uotConnect = request.IsConnect
		st.udpTarget = &requestDest
		if bodyReader.Len() > 0 {
			initial, err := io.ReadAll(bodyReader)
			if err != nil {
				return err
			}
			s.startStreamDelivery(st)
			if err := st.enqueueDelivery(buf.MultiBuffer{buf.FromBytes(initial)}); err != nil {
				return err
			}
		}

		s.startStreamDelivery(st)
		go s.pumpDownlink(st.sid, link)
		return nil
	}

	return nil
}

func (s *session) handleUDPData(st *stream, data []byte) error {
	st.uotBuffer = append(st.uotBuffer, data...)
	for {
		if st.uotConnect {
			if len(st.uotBuffer) < 2 {
				return nil
			}
			length := int(binary.BigEndian.Uint16(st.uotBuffer[:2]))
			if len(st.uotBuffer) < 2+length {
				return nil
			}
			payload := bytes.Clone(st.uotBuffer[2 : 2+length])
			st.uotBuffer = st.uotBuffer[2+length:]
			packet := buf.FromBytes(payload)
			if st.udpTarget != nil {
				destination := *st.udpTarget
				packet.UDP = &destination
			}
			if err := st.link.Writer.WriteMultiBuffer(buf.MultiBuffer{packet}); err != nil {
				return err
			}
			continue
		}

		reader := bytes.NewReader(st.uotBuffer)
		destination, err := uot.AddrParser.ReadAddrPort(reader)
		if err != nil {
			cause := errors.Cause(err)
			if cause == io.EOF || cause == io.ErrUnexpectedEOF {
				return nil
			}
			return errors.New("anytls: invalid UoT destination").Base(err)
		}
		if reader.Len() < 2 {
			return nil
		}
		lengthBytes := make([]byte, 2)
		if _, err := io.ReadFull(reader, lengthBytes); err != nil {
			return nil
		}
		length := int(binary.BigEndian.Uint16(lengthBytes))
		if reader.Len() < length {
			return nil
		}
		consumed := len(st.uotBuffer) - reader.Len()
		payload := bytes.Clone(st.uotBuffer[consumed : consumed+length])
		st.uotBuffer = st.uotBuffer[consumed+length:]
		packet := buf.FromBytes(payload)
		packetDestination := singbridge.ToDestination(destination, net.Network_UDP)
		packet.UDP = &packetDestination
		if err := st.link.Writer.WriteMultiBuffer(buf.MultiBuffer{packet}); err != nil {
			return err
		}
	}
}

func encodeUDPData(data buf.MultiBuffer, connect bool, defaultDestination *net.Destination) (buf.MultiBuffer, error) {
	var encoded buf.MultiBuffer
	for _, packet := range data {
		if packet == nil {
			continue
		}
		payload := packet.Bytes()
		destination := defaultDestination
		addressLength := 0
		if !connect {
			if packet.UDP != nil {
				destination = packet.UDP
			}
			if destination == nil {
				buf.ReleaseMulti(encoded)
				buf.ReleaseMulti(data)
				return nil, errors.New("anytls: UoT packet destination is missing")
			}
			addressLength = uot.AddrParser.AddrPortLen(singbridge.ToSocksaddr(*destination))
		}
		if len(payload) > maxFramePayload {
			buf.ReleaseMulti(encoded)
			buf.ReleaseMulti(data)
			return nil, errors.New("anytls: UoT packet is too large")
		}
		recordLength := addressLength + 2 + len(payload)
		record := buf.NewWithSize(int32(recordLength))
		if !connect {
			if err := uot.AddrParser.WriteAddrPort(record, singbridge.ToSocksaddr(*destination)); err != nil {
				record.Release()
				buf.ReleaseMulti(encoded)
				buf.ReleaseMulti(data)
				return nil, errors.New("anytls: encode UoT destination failed").Base(err)
			}
		}
		lengthBytes := record.Extend(2)
		binary.BigEndian.PutUint16(lengthBytes, uint16(len(payload)))
		if _, err := record.Write(payload); err != nil {
			record.Release()
			buf.ReleaseMulti(encoded)
			buf.ReleaseMulti(data)
			return nil, err
		}
		encoded = append(encoded, record)
	}
	buf.ReleaseMulti(data)
	return encoded, nil
}

func (s *session) pumpDownlink(sid uint32, link *transport.Link) {
	s.streamsMu.Lock()
	st := s.streams[sid]
	s.streamsMu.Unlock()
	defer func() {
		if s.finishStream(sid, nil) && !s.isClosed() {
			_ = s.sendFrame(newFrame(cmdFIN, sid))
		}
	}()

	for {
		mb, err := link.Reader.ReadMultiBuffer()
		if err != nil {
			break
		}
		if st != nil && st.isUDP {
			mb, err = encodeUDPData(mb, st.uotConnect, st.udpTarget)
			if err != nil {
				return
			}
		}

		if err := s.sendStreamData(sid, mb); err != nil {
			return
		}
	}
}

func (s *session) isClosed() bool {
	return s.closed.Load()
}

func (s *session) close(err error) {
	if !s.closed.CompareAndSwap(false, true) {
		return
	}
	if s.cancel != nil {
		s.cancel()
	}
	if s.conn != nil {
		_ = s.conn.Close()
	}

	s.streamsMu.Lock()
	streams := make([]*stream, 0, len(s.streams)+len(s.drainingStreams))
	for _, st := range s.streams {
		streams = append(streams, st)
	}
	for _, st := range s.drainingStreams {
		streams = append(streams, st)
	}
	s.streams = make(map[uint32]*stream)
	s.drainingStreams = make(map[uint32]*stream)
	if s.client != nil {
		s.activeStreams.Store(0)
	}
	s.streamsMu.Unlock()

	for _, st := range streams {
		st.close(err)
	}
	s.dieHookMu.Lock()
	hook := s.dieHook
	s.dieHook = nil
	s.dieHookMu.Unlock()
	if hook != nil {
		hook()
	}
}

func (s *session) attachStreamLink(st *stream, link *transport.Link) bool {
	s.streamsMu.Lock()
	defer s.streamsMu.Unlock()
	if s.isClosed() || s.streams[st.sid] != st {
		return false
	}
	st.link = link
	return true
}

func closeTransportLink(link *transport.Link) {
	if link == nil {
		return
	}
	common.Interrupt(link.Reader)
	common.Close(link.Writer)
}

func (s *session) reservePeerStreamID(sid uint32) error {
	s.streamsMu.Lock()
	defer s.streamsMu.Unlock()
	if sid == 0 {
		return errors.New("anytls: SYN stream ID must not be zero")
	}
	if sid <= s.lastPeerSID {
		return errors.New("anytls: SYN stream ID must increase, got ", sid, " after ", s.lastPeerSID)
	}
	s.lastPeerSID = sid
	return nil
}

func (s *session) finishStream(sid uint32, err error) bool {
	s.streamsMu.Lock()
	st := s.streams[sid]
	if st != nil {
		delete(s.streams, sid)
	}
	s.streamsMu.Unlock()

	if st == nil {
		return false
	}

	if s.client != nil {
		s.activeStreams.Add(-1)
	}
	st.close(err)
	return true
}

func (s *session) finishStreamAfterDelivery(sid uint32, err error) bool {
	s.streamsMu.Lock()
	st := s.streams[sid]
	if st != nil {
		delete(s.streams, sid)
		if s.drainingStreams == nil {
			s.drainingStreams = make(map[uint32]*stream)
		}
		s.drainingStreams[sid] = st
	}
	s.streamsMu.Unlock()

	if st == nil {
		return false
	}

	if s.client != nil {
		s.activeStreams.Add(-1)
	}
	st.setDeliveryDoneHook(func() {
		s.streamsMu.Lock()
		if s.drainingStreams[sid] == st {
			delete(s.drainingStreams, sid)
		}
		s.streamsMu.Unlock()
	})
	st.closeAfterDelivery(err)
	return true
}

func (s *session) sendFrame(f *frame) error {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	if err := s.fw.writeFrame(f); err != nil {
		return err
	}
	return s.fw.flush()
}

// nextPacketIndexLocked allocates the next session-wide packet index. The bool is
// deliberately separate from the index: packet index 0 is a valid padding rule,
// while a session past the stop value must send an unpadded packet.
func (s *session) nextPacketIndexLocked() (uint32, bool) {
	s.schemeMu.RLock()
	scheme := s.paddingScheme
	s.schemeMu.RUnlock()
	if scheme != nil && s.pktCounter.Load() < scheme.stop {
		return s.pktCounter.Add(1) - 1, true
	}
	return 0, false
}

func (s *session) nextPacketIndex() (uint32, bool) {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	return s.nextPacketIndexLocked()
}

func (s *session) writePacketLocked(frames buf.MultiBuffer) error {
	packetIndex, paddingEnabled := s.nextPacketIndexLocked()
	if paddingEnabled {
		return s.writePacketWithPadding(packetIndex, frames)
	}
	if err := s.fw.bw.WriteMultiBuffer(frames); err != nil {
		return err
	}
	return s.fw.flush()
}

func (s *session) writePacket(frames buf.MultiBuffer) error {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	return s.writePacketLocked(frames)
}

func (s *session) writeFramesLocked(sid uint32, data buf.MultiBuffer, packetIndex uint32, paddingEnabled bool) error {
	// Steady-state PSH frames bypass BufferedWriter. Feeding a large payload
	// through its 8 KiB staging buffer turns one AnyTLS frame into many small
	// TLS writes. Flush pending control data first, then batch complete frames
	// into bounded contiguous buffers while preserving their wire boundaries.
	if !paddingEnabled && s.conn != nil {
		if err := s.fw.flush(); err != nil {
			return err
		}
		return writePSHBatch(s.conn, sid, data)
	}

	for !data.IsEmpty() {
		var chunk buf.MultiBuffer
		data, chunk = buf.SplitSize(data, maxFramePayload)
		if paddingEnabled {
			b := buf.New()
			p := b.Extend(frameHeaderSize)
			p[0] = cmdPSH
			binary.BigEndian.PutUint32(p[1:5], sid)
			binary.BigEndian.PutUint16(p[5:7], uint16(chunk.Len()))
			merge, _ := buf.MergeMulti(buf.MultiBuffer{b}, chunk)
			if err := s.writePacketWithPadding(packetIndex, merge); err != nil {
				return err
			}
			continue
		}
		if err := s.fw.writeMultiBuffer(cmdPSH, sid, chunk); err != nil {
			return err
		}
		if err := s.fw.flush(); err != nil {
			return err
		}
	}
	return nil
}

const maxPSHBatchWireSize int32 = 128 * 1024

// writePSHBatch takes ownership of data. It preserves the 16-bit AnyTLS frame
// limit while grouping adjacent PSH frames into bounded connection writes.
func writePSHBatch(conn io.Writer, sid uint32, data buf.MultiBuffer) error {
	defer buf.ReleaseMulti(data)

	totalLength := data.Len()
	if totalLength <= 0 {
		return nil
	}

	// Sum of per-buffer ceilings is an upper bound on the number of frames
	// SplitSize can produce. It lets small writes use a small bytespool bucket
	// while the hard cap keeps large upstream batches bounded.
	frameCapacity := int32(0)
	for _, buffer := range data {
		if buffer != nil && !buffer.IsEmpty() {
			frameCapacity += (buffer.Len() + maxFramePayload - 1) / maxFramePayload
		}
	}
	wireCapacity := totalLength + frameCapacity*frameHeaderSize
	if wireCapacity > maxPSHBatchWireSize {
		wireCapacity = maxPSHBatchWireSize
	}
	wire := buf.NewWithSize(wireCapacity)
	defer wire.Release()

	for !data.IsEmpty() {
		var chunk buf.MultiBuffer
		data, chunk = buf.SplitSize(data, maxFramePayload)
		length := chunk.Len()
		if length <= 0 || length > maxFramePayload {
			buf.ReleaseMulti(chunk)
			return fmt.Errorf("anytls: invalid PSH frame payload length: %d", length)
		}

		frameLength := frameHeaderSize + length
		if !wire.IsEmpty() && wire.Len()+frameLength > wireCapacity {
			if err := writeFull(conn, wire.Bytes()); err != nil {
				buf.ReleaseMulti(chunk)
				return err
			}
			wire.Clear()
		}

		header := wire.Extend(frameHeaderSize)
		header[0] = cmdPSH
		binary.BigEndian.PutUint32(header[1:5], sid)
		binary.BigEndian.PutUint16(header[5:7], uint16(length))
		chunk.Copy(wire.Extend(length))
		buf.ReleaseMulti(chunk)
	}

	if wire.IsEmpty() {
		return nil
	}
	return writeFull(conn, wire.Bytes())
}

func (s *session) sendStreamData(sid uint32, data buf.MultiBuffer) error {
	defer buf.ReleaseMulti(data)
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	packetIndex, paddingEnabled := s.nextPacketIndexLocked()
	return s.writeFramesLocked(sid, data, packetIndex, paddingEnabled)
}

func validateIncomingFrame(isClient bool, cmd byte, sid uint32, length int) error {
	switch cmd {
	case cmdWaste:
		return nil
	case cmdSYN:
		if isClient {
			return fmt.Errorf("anytls: unexpected SYN from server")
		}
		if sid == 0 {
			return fmt.Errorf("anytls: SYN stream ID must not be zero")
		}
		if length != 0 {
			return fmt.Errorf("anytls: SYN body must be empty")
		}
	case cmdPSH:
		if sid == 0 {
			return fmt.Errorf("anytls: PSH stream ID must not be zero")
		}
		if length == 0 {
			return fmt.Errorf("anytls: PSH frame with empty payload, streamId=%d", sid)
		}
	case cmdFIN:
		if sid == 0 {
			return fmt.Errorf("anytls: FIN stream ID must not be zero")
		}
		if length != 0 {
			return fmt.Errorf("anytls: FIN body must be empty")
		}
	case cmdSettings:
		if isClient {
			return fmt.Errorf("anytls: unexpected cmdSettings from server")
		}
		if sid != 0 {
			return fmt.Errorf("anytls: Settings stream ID must be zero")
		}
		if length == 0 {
			return fmt.Errorf("anytls: Settings body must not be empty")
		}
	case cmdAlert:
		if !isClient {
			return fmt.Errorf("anytls: unexpected Alert from client")
		}
		if sid != 0 {
			return fmt.Errorf("anytls: Alert stream ID must be zero")
		}
	case cmdUpdatePaddingScheme:
		if !isClient {
			return fmt.Errorf("anytls: unexpected UpdatePaddingScheme from client")
		}
		if sid != 0 {
			return fmt.Errorf("anytls: UpdatePaddingScheme stream ID must be zero")
		}
		if length == 0 {
			return fmt.Errorf("anytls: empty padding update")
		}
	case cmdSYNACK:
		if !isClient {
			return fmt.Errorf("anytls: unexpected SYNACK from client")
		}
		if sid == 0 {
			return fmt.Errorf("anytls: SYNACK stream ID must not be zero")
		}
	case cmdHeartRequest, cmdHeartResponse:
		if sid != 0 {
			return fmt.Errorf("anytls: heartbeat stream ID must be zero")
		}
		if length != 0 {
			return fmt.Errorf("anytls: heartbeat body must be empty")
		}
	case cmdServerSettings:
		if !isClient {
			return fmt.Errorf("anytls: unexpected ServerSettings from client")
		}
		if sid != 0 {
			return fmt.Errorf("anytls: ServerSettings stream ID must be zero")
		}
		if length == 0 {
			return fmt.Errorf("anytls: ServerSettings body must not be empty")
		}
	}
	return nil
}

func (s *session) rejectIncomingFrame(length int, frameErr error) error {
	if length > 0 {
		if err := discardBytes(s.br, length); err != nil {
			return err
		}
	}
	if !s.isClient {
		_ = s.sendFrame(&frame{cmd: cmdAlert, sid: 0, data: []byte(frameErr.Error())})
	}
	return frameErr
}

func (s *session) readLoop(ctx context.Context) error {
	var head [7]byte
	for {
		_, err := io.ReadFull(s.br, head[:])
		if err != nil {
			if s.isClosed() {
				return nil
			}
			return err
		}

		cmd := head[0]
		sid := binary.BigEndian.Uint32(head[1:5])
		length := int(binary.BigEndian.Uint16(head[5:7]))
		//errors.LogDebug(ctx, "anytls: received frame cmd=", cmd, " streamId=", sid, " length=", length)
		if frameErr := validateIncomingFrame(s.isClient, cmd, sid, length); frameErr != nil {
			return s.rejectIncomingFrame(length, frameErr)
		}
		switch cmd {
		case cmdWaste:
			if length > 0 {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
			}
		case cmdSettings:
			if s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected cmdSettings from server")
			}
			text, err := readText(s.br, length)
			if err != nil {
				return err
			}
			if s.handshakeDoneValue() {
				return errors.New("anytls: duplicate settings")
			}
			settings, err := parseSettings(text)
			if err != nil {
				return err
			}
			if settings.version > 2 {
				s.setPeerVersion(2)
			} else {
				s.setPeerVersion(settings.version)
			}
			s.setClientPaddingMD5(settings.paddingMD5)
			if err := s.sendFrame(&frame{cmd: cmdServerSettings, sid: 0, data: []byte("v=2")}); err != nil {
				return err
			}
			if s.server != nil && s.server.paddingScheme != "" && s.clientPaddingMD5Value() != "" {
				sum := md5.Sum([]byte(s.server.paddingScheme))
				if strings.ToLower(hex.EncodeToString(sum[:])) != s.clientPaddingMD5Value() {
					if err := s.sendFrame(&frame{cmd: cmdUpdatePaddingScheme, sid: 0, data: []byte(s.server.paddingScheme)}); err != nil {
						return err
					}
				}
			}
			s.setHandshakeDone()
		case cmdHeartRequest:
			if length > 0 {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
			}
			if err := s.sendFrame(newFrame(cmdHeartResponse, 0)); err != nil {
				return err
			}
		case cmdHeartResponse:
			if length > 0 {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
			}
		case cmdSYN:
			if s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected SYN from server")
			} else {
				if !s.handshakeDoneValue() {
					alert := newFrame(cmdAlert, 0)
					alert.data = []byte("client did not send its settings")
					_ = s.sendFrame(alert)
					return errors.New("anytls: client did not send its settings")
				}
				if idErr := s.reservePeerStreamID(sid); idErr != nil {
					if length > 0 {
						if err := discardBytes(s.br, length); err != nil {
							return err
						}
					}
					return idErr
				}
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
					errors.LogWarning(ctx, "anytls: unexpected data in SYN, streamId=", sid)
					if err := s.sendFrame(&frame{cmd: cmdSYNACK, sid: sid, data: []byte("unexpected syn body")}); err != nil {
						return err
					}
					continue
				}
				s.streamsMu.Lock()
				if _, ok := s.streams[sid]; !ok {
					s.streams[sid] = newStream(sid, nil)
				}
				s.streamsMu.Unlock()
			}
		case cmdPSH:
			if length <= 0 {
				err := errors.New("anytls: PSH frame with empty payload, streamId=", sid)
				s.finishStream(sid, err)
				return err
			}
			s.streamsMu.Lock()
			st := s.streams[sid]
			s.streamsMu.Unlock()
			if st == nil {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
				// A locally closed stream may still have peer data in flight. The
				// reference implementations discard it without sacrificing the
				// multiplexed session.
				continue
			} else if st.isUDP && st.link == nil {
				if err := s.handleFirstUDPFrame(ctx, st, s.br, length); err != nil {
					return err
				}
				continue
			} else if st.link == nil {
				if err := s.handleNewStream(ctx, st, s.br, length); err != nil {
					return err
				}
				continue
			}
			if err := s.handlePSH(ctx, st, s.br, length); err != nil {
				return err
			}
		case cmdFIN:
			if length > 0 {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
			}
			s.finishStreamAfterDelivery(sid, nil)
		case cmdSYNACK:
			if !s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected SYNACK from client")
			}
			var rejected error
			if length > 0 {
				bodyText, err := readText(s.br, length)
				if err != nil {
					return err
				}
				rejected = errors.New(bodyText)
			}

			s.streamsMu.Lock()
			st := s.streams[sid]
			s.streamsMu.Unlock()
			if st == nil || !st.synAckReceived.CompareAndSwap(false, true) {
				continue
			}
			if rejected != nil {
				errors.LogWarning(ctx, "anytls: stream handshake rejected, streamId=", sid, " err=", rejected)
				if s.finishStream(sid, rejected) && !s.isClosed() {
					if err := s.sendFrame(newFrame(cmdFIN, sid)); err != nil {
						return err
					}
				}
			}
		case cmdServerSettings:
			if !s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected ServerSettings from client")
			}
			bodyText, err := readText(s.br, length)
			if err != nil {
				return err
			}
			if !s.markServerSettingsReceived() {
				return errors.New("anytls: duplicate ServerSettings")
			}
			settings, err := parseSettings(bodyText)
			if err != nil {
				return err
			}
			if settings.version > 2 {
				s.setPeerVersion(2)
			} else {
				s.setPeerVersion(settings.version)
			}
		case cmdUpdatePaddingScheme:
			if !s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected UpdatePaddingScheme from client")
			}
			if length > 0 {
				bodyText, err := readText(s.br, length)
				if err != nil {
					return err
				}
				scheme, perr := parsePaddingScheme(bodyText)
				if perr != nil {
					return errors.New("anytls: invalid padding update").Base(perr)
				}
				if s.client == nil {
					return errors.New("anytls: padding update has no client")
				}
				s.client.updatePaddingScheme(scheme)
			} else {
				return errors.New("anytls: empty padding update")
			}
		case cmdAlert:
			if !s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected Alert from client")
			}
			var bodyText string
			if length > 0 {
				bodyText, err = readText(s.br, length)
				if err != nil {
					return err
				}
			}
			alertText := "anytls: server alert"
			if bodyText != "" {
				alertText += ": " + bodyText
			}
			return errors.New(alertText)
		default:
			if length > 0 {
				if err := discardBytes(s.br, length); err != nil {
					return err
				}
			}
			errors.LogWarning(ctx, "anytls: unknown cmd=", cmd, " streamId=", sid)
			return errors.New("anytls: unknown cmd")
		}
	}
}
