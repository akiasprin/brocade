// Package proxy contains all proxies used by Xray.
//
// To implement an inbound or outbound proxy, one needs to do the following:
// 1. Implement the interface(s) below.
// 2. Register a config creator through common.RegisterConfig.
package proxy

import (
	"bytes"
	"context"
	"crypto/rand"
	"io"
	"math/big"
	"runtime"
	"strconv"
	"sync"
	"time"

	"github.com/pires/go-proxyproto"
	"github.com/xtls/xray-core/app/dispatcher"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/features/stats"
	"github.com/xtls/xray-core/proxy/vless/encryption"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/finalmask"
	"github.com/xtls/xray-core/transport/internet/reality"
	"github.com/xtls/xray-core/transport/internet/stat"
	"github.com/xtls/xray-core/transport/internet/tls"
)

var (
	Tls13SupportedVersions  = []byte{0x00, 0x2b, 0x00, 0x02, 0x03, 0x04}
	TlsClientHandShakeStart = []byte{0x16, 0x03}
	TlsServerHandShakeStart = []byte{0x16, 0x03, 0x03}
	TlsApplicationDataStart = []byte{0x17, 0x03, 0x03}

	Tls13CipherSuiteDic = map[uint16]string{
		0x1301: "TLS_AES_128_GCM_SHA256",
		0x1302: "TLS_AES_256_GCM_SHA384",
		0x1303: "TLS_CHACHA20_POLY1305_SHA256",
		0x1304: "TLS_AES_128_CCM_SHA256",
		0x1305: "TLS_AES_128_CCM_8_SHA256",
	}
)

const (
	TlsHandshakeTypeClientHello byte = 0x01
	TlsHandshakeTypeServerHello byte = 0x02

	CommandPaddingContinue byte = 0x00
	CommandPaddingEnd      byte = 0x01
	CommandPaddingDirect   byte = 0x02
)

// An Inbound processes inbound connections.
type Inbound interface {
	// Network returns a list of networks that this inbound supports. Connections with not-supported networks will not be passed into Process().
	Network() []net.Network

	// Process processes a connection of given network. If necessary, the Inbound can dispatch the connection to an Outbound.
	Process(context.Context, net.Network, stat.Connection, routing.Dispatcher) error
}

// An Outbound process outbound connections.
type Outbound interface {
	// Process processes the given connection. The given dialer may be used to dial a system outbound connection.
	Process(context.Context, *transport.Link, internet.Dialer) error
}

// UserManager is the interface for Inbounds and Outbounds that can manage their users.
type UserManager interface {
	// AddUser adds a new user.
	AddUser(context.Context, *protocol.MemoryUser) error

	// RemoveUser removes a user by email.
	RemoveUser(context.Context, string) error

	// Get user by email.
	GetUser(context.Context, string) *protocol.MemoryUser

	// Get all users.
	GetUsers(context.Context) []*protocol.MemoryUser

	// Get users count.
	GetUsersCount(context.Context) int64
}

type GetInbound interface {
	GetInbound() Inbound
}

type GetOutbound interface {
	GetOutbound() Outbound
}

// TrafficState is used to track uplink and downlink of one connection
// It is used by XTLS to determine if switch to raw copy mode, It is used by Vision to calculate padding
type TrafficState struct {
	UserUUID               []byte
	NumberOfPacketToFilter int
	EnableXtls             bool
	IsTLS12orAbove         bool
	IsTLS                  bool
	Cipher                 uint16
	RemainingServerHello   int32
	Inbound                InboundState
	Outbound               OutboundState

	// TLS hello detection is fed by the independently running uplink and
	// downlink copy loops. Keep the small, bounded reassembly state here so a
	// record split across reads is classified exactly once without racing the
	// opposite direction.
	tlsFilterMu sync.Mutex
	tlsFilter   [2]tlsHelloFilterState
}

type tlsHelloFilterState struct {
	data []byte
	done bool
}

type tlsFilterSnapshot struct {
	filtering      bool
	isTLS          bool
	isTLS12orAbove bool
	enableXtls     bool
}

type InboundState struct {
	// reader link state
	WithinPaddingBuffers   bool
	UplinkReaderDirectCopy bool
	RemainingCommand       int32
	RemainingContent       int32
	RemainingPadding       int32
	CurrentCommand         int
	// write link state
	IsPadding                bool
	DownlinkWriterDirectCopy bool
}

type OutboundState struct {
	// reader link state
	WithinPaddingBuffers     bool
	DownlinkReaderDirectCopy bool
	RemainingCommand         int32
	RemainingContent         int32
	RemainingPadding         int32
	CurrentCommand           int
	// write link state
	IsPadding              bool
	UplinkWriterDirectCopy bool
}

func NewTrafficState(userUUID []byte) *TrafficState {
	return &TrafficState{
		UserUUID:               userUUID,
		NumberOfPacketToFilter: 8,
		EnableXtls:             false,
		IsTLS12orAbove:         false,
		IsTLS:                  false,
		Cipher:                 0,
		RemainingServerHello:   -1,
		Inbound: InboundState{
			WithinPaddingBuffers:     true,
			UplinkReaderDirectCopy:   false,
			RemainingCommand:         -1,
			RemainingContent:         -1,
			RemainingPadding:         -1,
			CurrentCommand:           0,
			IsPadding:                true,
			DownlinkWriterDirectCopy: false,
		},
		Outbound: OutboundState{
			WithinPaddingBuffers:     true,
			DownlinkReaderDirectCopy: false,
			RemainingCommand:         -1,
			RemainingContent:         -1,
			RemainingPadding:         -1,
			CurrentCommand:           0,
			IsPadding:                true,
			UplinkWriterDirectCopy:   false,
		},
	}
}

func (s *TrafficState) tlsSnapshot() tlsFilterSnapshot {
	s.tlsFilterMu.Lock()
	defer s.tlsFilterMu.Unlock()
	return tlsFilterSnapshot{
		filtering:      s.NumberOfPacketToFilter > 0,
		isTLS:          s.IsTLS,
		isTLS12orAbove: s.IsTLS12orAbove,
		enableXtls:     s.EnableXtls,
	}
}

func (s *TrafficState) tlsFiltering() bool {
	s.tlsFilterMu.Lock()
	defer s.tlsFilterMu.Unlock()
	return s.NumberOfPacketToFilter > 0
}

// VisionReader is used to read xtls vision protocol
// Note Vision probably only make sense as the inner most layer of reader, since it need assess traffic state from origin proxy traffic
type VisionReader struct {
	buf.Reader
	trafficState *TrafficState
	ctx          context.Context
	isUplink     bool
	conn         net.Conn
	input        *bytes.Reader
	rawInput     *bytes.Buffer
	ob           *session.Outbound

	// internal
	directReadCounter stats.Counter
}

type visionBufferProvider interface {
	VisionBuffers() (*bytes.Reader, *bytes.Buffer)
}

// VisionBuffers resolves transport read-ahead without coupling VLESS to the
// private layout of crypto/tls, uTLS, REALITY, or VLESS encryption wrappers.
func VisionBuffers(conn net.Conn) (*bytes.Reader, *bytes.Buffer, bool) {
	provider, ok := conn.(visionBufferProvider)
	if !ok {
		return nil, nil, false
	}
	input, rawInput := provider.VisionBuffers()
	return input, rawInput, input != nil && rawInput != nil
}

func NewVisionReader(reader buf.Reader, trafficState *TrafficState, isUplink bool, ctx context.Context, conn net.Conn, input *bytes.Reader, rawInput *bytes.Buffer, ob *session.Outbound) *VisionReader {
	return &VisionReader{
		Reader:       reader,
		trafficState: trafficState,
		ctx:          ctx,
		isUplink:     isUplink,
		conn:         conn,
		input:        input,
		rawInput:     rawInput,
		ob:           ob,
	}
}

func (w *VisionReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	buffer, err := w.Reader.ReadMultiBuffer()
	if buffer.IsEmpty() {
		return buffer, err
	}

	var withinPaddingBuffers *bool
	var remainingContent *int32
	var remainingPadding *int32
	var currentCommand *int
	var switchToDirectCopy *bool
	if w.isUplink {
		withinPaddingBuffers = &w.trafficState.Inbound.WithinPaddingBuffers
		remainingContent = &w.trafficState.Inbound.RemainingContent
		remainingPadding = &w.trafficState.Inbound.RemainingPadding
		currentCommand = &w.trafficState.Inbound.CurrentCommand
		switchToDirectCopy = &w.trafficState.Inbound.UplinkReaderDirectCopy
	} else {
		withinPaddingBuffers = &w.trafficState.Outbound.WithinPaddingBuffers
		remainingContent = &w.trafficState.Outbound.RemainingContent
		remainingPadding = &w.trafficState.Outbound.RemainingPadding
		currentCommand = &w.trafficState.Outbound.CurrentCommand
		switchToDirectCopy = &w.trafficState.Outbound.DownlinkReaderDirectCopy
	}

	if *switchToDirectCopy {
		if w.directReadCounter != nil {
			w.directReadCounter.Add(int64(buffer.Len()))
		}
		return buffer, err
	}

	if *withinPaddingBuffers || w.trafficState.tlsFiltering() {
		mb2 := make(buf.MultiBuffer, 0, len(buffer))
		for _, b := range buffer {
			newbuffer := XtlsUnpadding(b, w.trafficState, w.isUplink, w.ctx)
			if newbuffer.Len() > 0 {
				mb2 = append(mb2, newbuffer)
			}
		}
		buffer = mb2
		if *remainingContent > 0 || *remainingPadding > 0 || *currentCommand == 0 {
			*withinPaddingBuffers = true
		} else if *currentCommand == 1 {
			*withinPaddingBuffers = false
		} else if *currentCommand == 2 {
			*withinPaddingBuffers = false
			*switchToDirectCopy = true
		} else {
			errors.LogDebug(w.ctx, "XtlsRead unknown command ", *currentCommand, buffer.Len())
		}
	}
	if w.trafficState.tlsFiltering() {
		XtlsFilterTls(buffer, w.trafficState, w.isUplink, w.ctx)
	}

	if *switchToDirectCopy {
		// XTLS Vision processes TLS-like conn's input and rawInput
		if inputBuffer, err := buf.ReadFrom(w.input); err == nil && !inputBuffer.IsEmpty() {
			buffer, _ = buf.MergeMulti(buffer, inputBuffer)
		}
		if rawInputBuffer, err := buf.ReadFrom(w.rawInput); err == nil && !rawInputBuffer.IsEmpty() {
			buffer, _ = buf.MergeMulti(buffer, rawInputBuffer)
		}
		*w.input = bytes.Reader{} // release memory
		w.input = nil
		*w.rawInput = bytes.Buffer{} // release memory
		w.rawInput = nil

		if inbound := session.InboundFromContext(w.ctx); inbound != nil && inbound.Conn != nil {
			// if w.isUplink && inbound.CanSpliceCopy.Load() == session.SpliceCopyWaiting { // TODO: enable uplink splice
			// 	inbound.CanSpliceCopy.CompareAndSwap(session.SpliceCopyWaiting, session.SpliceCopyDirect)
			// }
			if !w.isUplink && w.ob != nil { // ob need to be passed in due to context can have more than one ob
				w.ob.CanSpliceCopy.CompareAndSwap(session.SpliceCopyWaiting, session.SpliceCopyDirect)
			}
		}
		SuppressOuterCloseNotify(w.conn)
		readerConn, readCounter, _ := UnwrapRawConn(w.conn)
		w.directReadCounter = readCounter
		w.Reader = buf.NewReader(readerConn)
	}
	return buffer, err
}

// VisionWriter is used to write xtls vision protocol
// Note Vision probably only make sense as the inner most layer of writer, since it need assess traffic state from origin proxy traffic
type VisionWriter struct {
	buf.Writer
	trafficState *TrafficState
	ctx          context.Context
	isUplink     bool
	conn         net.Conn
	ob           *session.Outbound

	// internal
	writeOnceUserUUID  []byte
	directWriteCounter stats.Counter
	directWrite        bool
	tlsRecordMode      uint8
	tlsRecordHeader    [6]byte
	tlsRecordHeaderLen int
	tlsRecordRemaining int
	tlsRecordIsAppData bool

	testseed []uint32
}

func NewVisionWriter(writer buf.Writer, trafficState *TrafficState, isUplink bool, ctx context.Context, conn net.Conn, ob *session.Outbound, testseed []uint32) *VisionWriter {
	w := make([]byte, len(trafficState.UserUUID))
	copy(w, trafficState.UserUUID)
	if len(testseed) < 4 {
		testseed = []uint32{900, 500, 900, 256}
	}
	return &VisionWriter{
		Writer:            writer,
		trafficState:      trafficState,
		ctx:               ctx,
		writeOnceUserUUID: w,
		isUplink:          isUplink,
		conn:              conn,
		ob:                ob,
		testseed:          testseed,
	}
}

func (w *VisionWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	var isPadding *bool
	var switchToDirectCopy *bool
	if w.isUplink {
		isPadding = &w.trafficState.Outbound.IsPadding
		switchToDirectCopy = &w.trafficState.Outbound.UplinkWriterDirectCopy
	} else {
		isPadding = &w.trafficState.Inbound.IsPadding
		switchToDirectCopy = &w.trafficState.Inbound.DownlinkWriterDirectCopy
	}

	var spliceReadyInbound *session.Inbound
	if *switchToDirectCopy {
		spliceReadyInbound = w.activateDirectWrite(switchToDirectCopy)
	}

	if w.trafficState.tlsFiltering() {
		XtlsFilterTls(mb, w.trafficState, w.isUplink, w.ctx)
	}

	if *isPadding {
		if len(mb) == 1 && mb[0] == nil {
			mb[0] = XtlsPadding(nil, CommandPaddingContinue, &w.writeOnceUserUUID, true, w.ctx, w.testseed) // we do a long padding to hide vless header
		} else {
			handled, fallback, err := w.writeTLSRecords(mb, isPadding, switchToDirectCopy)
			if handled {
				return err
			}
			mb = fallback
			snapshot := w.trafficState.tlsSnapshot()
			isComplete := IsCompleteRecord(mb)
			mb = ReshapeMultiBuffer(w.ctx, mb)
			longPadding := snapshot.isTLS
			for i, b := range mb {
				if snapshot.isTLS && b.Len() >= 6 && bytes.Equal(TlsApplicationDataStart, b.BytesTo(3)) && isComplete {
					if snapshot.enableXtls {
						*switchToDirectCopy = true
					}
					var command byte = CommandPaddingContinue
					if i == len(mb)-1 {
						command = CommandPaddingEnd
						if snapshot.enableXtls {
							command = CommandPaddingDirect
						}
					}
					mb[i] = XtlsPadding(b, command, &w.writeOnceUserUUID, true, w.ctx, w.testseed)
					*isPadding = false // padding going to end
					longPadding = false
					continue
				} else if !snapshot.isTLS12orAbove && !snapshot.filtering { // For compatibility with earlier vision receiver, finish padding once classification is conclusive.
					*isPadding = false
					mb[i] = XtlsPadding(b, CommandPaddingEnd, &w.writeOnceUserUUID, longPadding, w.ctx, w.testseed)
					break
				}
				var command byte = CommandPaddingContinue
				if i == len(mb)-1 && !*isPadding {
					command = CommandPaddingEnd
					if snapshot.enableXtls {
						command = CommandPaddingDirect
					}
				}
				mb[i] = XtlsPadding(b, command, &w.writeOnceUserUUID, longPadding, w.ctx, w.testseed)
			}
		}
	}
	return w.writeOutput(mb, spliceReadyInbound)
}

const (
	tlsRecordModeUnknown uint8 = iota
	tlsRecordModeTLS
	tlsRecordModePassthrough
	maxTLSRecordPayload = (1 << 14) + 2048
)

func (w *VisionWriter) activateDirectWrite(switchToDirectCopy *bool) *session.Inbound {
	var spliceReadyInbound *session.Inbound
	if inbound := session.InboundFromContext(w.ctx); inbound != nil {
		if !w.isUplink && inbound.CanSpliceCopy.Load() == session.SpliceCopyWaiting {
			spliceReadyInbound = inbound
		}
		// Uplink splice is intentionally not enabled yet. The writer still has to
		// unwrap the outer TLS connection when the peer sends CommandPaddingDirect.
	}
	SuppressOuterCloseNotify(w.conn)
	rawConn, _, writerCounter := UnwrapRawConn(w.conn)
	w.Writer = buf.NewWriter(rawConn)
	w.directWriteCounter = writerCounter
	w.directWrite = true
	*switchToDirectCopy = false
	return spliceReadyInbound
}

func (w *VisionWriter) writeOutput(mb buf.MultiBuffer, spliceReadyInbound *session.Inbound) error {
	bytesWritten := int64(mb.Len())
	if bytesWritten > 0 && w.directWriteCounter != nil {
		w.directWriteCounter.Add(bytesWritten)
	}
	if err := w.Writer.WriteMultiBuffer(mb); err != nil {
		return err
	}
	if !w.isUplink && w.directWrite && bytesWritten > 0 {
		if inbound := session.InboundFromContext(w.ctx); inbound != nil && inbound.SpliceMetrics != nil {
			inbound.SpliceMetrics.AddDirectBytes(bytesWritten)
		}
	}
	if spliceReadyInbound != nil && bytesWritten > 0 && spliceReadyInbound.CanSpliceCopy.CompareAndSwap(session.SpliceCopyWaiting, session.SpliceCopyDirect) {
		// Enable splice only after this write has completed to avoid racing a
		// concurrent raw write to the same TCP connection.
		if spliceReadyInbound.SpliceMetrics != nil {
			spliceReadyInbound.SpliceMetrics.MarkDirect()
		}
	}
	return nil
}

// writeTLSRecords tracks TLS record boundaries without delaying short writes.
// Header fragments are padded and sent immediately; only six bytes of parser
// state and the current record's remaining length survive across calls. The
// block carrying the final byte of the first application-data record ends
// padding, so direct mode starts at an exact TLS record boundary regardless of
// how Read split the stream.
func (w *VisionWriter) writeTLSRecords(mb buf.MultiBuffer, isPadding, switchToDirectCopy *bool) (bool, buf.MultiBuffer, error) {
	if w.tlsRecordMode == tlsRecordModePassthrough {
		return false, mb, nil
	}

	snapshot := w.trafficState.tlsSnapshot()
	var padded buf.MultiBuffer
	content := buf.New()
	flush := func(command byte) {
		if content.IsEmpty() {
			return
		}
		longPadding := snapshot.isTLS && !w.tlsRecordIsAppData
		padded = append(padded, XtlsPadding(content, command, &w.writeOnceUserUUID, longPadding, w.ctx, w.testseed))
		content = buf.New()
	}
	defer func() {
		if content != nil {
			content.Release()
		}
	}()

	for bufferIndex, input := range mb {
		if input == nil {
			continue
		}
		data := input.Bytes()
		consumed := 0
		for consumed < len(data) {
			if content.Len() >= buf.Size-21 {
				flush(CommandPaddingContinue)
			}

			if w.tlsRecordMode == tlsRecordModeUnknown {
				needed := len(w.tlsRecordHeader) - w.tlsRecordHeaderLen
				take := minInt(needed, len(data)-consumed, int(buf.Size-21-content.Len()))
				copy(w.tlsRecordHeader[w.tlsRecordHeaderLen:], data[consumed:consumed+take])
				_, _ = content.Write(data[consumed : consumed+take])
				w.tlsRecordHeaderLen += take
				consumed += take
				if w.tlsRecordHeaderLen < len(w.tlsRecordHeader) {
					continue
				}

				recordSize, valid := tlsRecordSize(w.tlsRecordHeader[:])
				expectedHello := TlsHandshakeTypeServerHello
				if w.isUplink {
					expectedHello = TlsHandshakeTypeClientHello
				}
				if !valid || w.tlsRecordHeader[0] != 0x16 || w.tlsRecordHeader[5] != expectedHello {
					w.tlsRecordMode = tlsRecordModePassthrough
					flush(CommandPaddingContinue)
					if err := w.writeOutput(padded, nil); err != nil {
						input.Advance(int32(consumed))
						buf.ReleaseMulti(mb[bufferIndex:])
						return true, nil, err
					}
					input.Advance(int32(consumed))
					if input.IsEmpty() {
						input.Release()
						mb[bufferIndex] = nil
						return false, mb[bufferIndex+1:], nil
					}
					return false, mb[bufferIndex:], nil
				}

				w.tlsRecordMode = tlsRecordModeTLS
				w.tlsRecordRemaining = recordSize - len(w.tlsRecordHeader)
				w.tlsRecordHeaderLen = 0
				if w.tlsRecordRemaining == 0 {
					w.tlsRecordIsAppData = false
				}
				continue
			}

			if w.tlsRecordRemaining == 0 {
				needed := 5 - w.tlsRecordHeaderLen
				take := minInt(needed, len(data)-consumed, int(buf.Size-21-content.Len()))
				copy(w.tlsRecordHeader[w.tlsRecordHeaderLen:], data[consumed:consumed+take])
				_, _ = content.Write(data[consumed : consumed+take])
				w.tlsRecordHeaderLen += take
				consumed += take
				if w.tlsRecordHeaderLen < 5 {
					continue
				}

				recordSize, valid := tlsRecordSize(w.tlsRecordHeader[:5])
				if !valid {
					w.tlsRecordMode = tlsRecordModePassthrough
					flush(CommandPaddingContinue)
					if err := w.writeOutput(padded, nil); err != nil {
						input.Advance(int32(consumed))
						buf.ReleaseMulti(mb[bufferIndex:])
						return true, nil, err
					}
					input.Advance(int32(consumed))
					if input.IsEmpty() {
						input.Release()
						mb[bufferIndex] = nil
						return false, mb[bufferIndex+1:], nil
					}
					return false, mb[bufferIndex:], nil
				}
				w.tlsRecordRemaining = recordSize - 5
				w.tlsRecordIsAppData = snapshot.isTLS && bytes.Equal(w.tlsRecordHeader[:3], TlsApplicationDataStart)
				w.tlsRecordHeaderLen = 0
			}

			take := minInt(w.tlsRecordRemaining, len(data)-consumed, int(buf.Size-21-content.Len()))
			_, _ = content.Write(data[consumed : consumed+take])
			w.tlsRecordRemaining -= take
			consumed += take
			if w.tlsRecordRemaining != 0 || !w.tlsRecordIsAppData {
				continue
			}

			command := CommandPaddingEnd
			if snapshot.enableXtls {
				command = CommandPaddingDirect
				*switchToDirectCopy = true
			}
			flush(command)
			*isPadding = false
			if err := w.writeOutput(padded, nil); err != nil {
				input.Advance(int32(consumed))
				buf.ReleaseMulti(mb[bufferIndex:])
				return true, nil, err
			}
			padded = nil

			input.Advance(int32(consumed))
			var remainder buf.MultiBuffer
			if input.IsEmpty() {
				input.Release()
				mb[bufferIndex] = nil
				remainder = mb[bufferIndex+1:]
			} else {
				remainder = mb[bufferIndex:]
			}
			if remainder.IsEmpty() {
				return true, nil, nil
			}
			var spliceReadyInbound *session.Inbound
			if snapshot.enableXtls {
				spliceReadyInbound = w.activateDirectWrite(switchToDirectCopy)
			}
			return true, nil, w.writeOutput(remainder, spliceReadyInbound)
		}
		input.Release()
		mb[bufferIndex] = nil
	}

	flush(CommandPaddingContinue)
	return true, nil, w.writeOutput(padded, nil)
}

func tlsRecordSize(header []byte) (int, bool) {
	if len(header) < 5 || header[0] < 0x14 || header[0] > 0x17 || header[1] != 0x03 {
		return 0, false
	}
	payload := int(header[3])<<8 | int(header[4])
	if payload <= 0 || payload > maxTLSRecordPayload {
		return 0, false
	}
	return payload + 5, true
}

func minInt(values ...int) int {
	minimum := values[0]
	for _, value := range values[1:] {
		if value < minimum {
			minimum = value
		}
	}
	return minimum
}

// IsCompleteRecord Is complete tls data record
func IsCompleteRecord(buffer buf.MultiBuffer) bool {
	cursor := newMultiBufferCursor(buffer)
	for cursor.remaining > 0 {
		contentType, ok := cursor.readByte()
		if !ok || contentType != 0x17 {
			return false
		}
		major, ok := cursor.readByte()
		if !ok || major != 0x03 {
			return false
		}
		minor, ok := cursor.readByte()
		if !ok || minor != 0x03 {
			return false
		}
		high, ok := cursor.readByte()
		if !ok {
			return false
		}
		low, ok := cursor.readByte()
		if !ok {
			return false
		}
		recordLen := int(high)<<8 | int(low)
		if recordLen == 0 || !cursor.skip(recordLen) {
			return false
		}
	}
	return true
}

type multiBufferCursor struct {
	buffers   buf.MultiBuffer
	buffer    int
	offset    int
	remaining int
}

func newMultiBufferCursor(buffers buf.MultiBuffer) multiBufferCursor {
	remaining := 0
	for _, buffer := range buffers {
		if buffer != nil {
			remaining += int(buffer.Len())
		}
	}
	return multiBufferCursor{buffers: buffers, remaining: remaining}
}

func (c *multiBufferCursor) readByte() (byte, bool) {
	for c.buffer < len(c.buffers) {
		buffer := c.buffers[c.buffer]
		if buffer == nil || c.offset >= int(buffer.Len()) {
			c.buffer++
			c.offset = 0
			continue
		}
		value := buffer.Byte(int32(c.offset))
		c.offset++
		c.remaining--
		return value, true
	}
	return 0, false
}

func (c *multiBufferCursor) skip(bytes int) bool {
	if bytes > c.remaining {
		return false
	}
	c.remaining -= bytes
	for bytes > 0 {
		buffer := c.buffers[c.buffer]
		if buffer == nil || c.offset >= int(buffer.Len()) {
			c.buffer++
			c.offset = 0
			continue
		}
		available := int(buffer.Len()) - c.offset
		if available > bytes {
			c.offset += bytes
			return true
		}
		bytes -= available
		c.buffer++
		c.offset = 0
	}
	return true
}

// ReshapeMultiBuffer prepare multi buffer for padding structure (max 21 bytes)
func ReshapeMultiBuffer(ctx context.Context, buffer buf.MultiBuffer) buf.MultiBuffer {
	needReshape := 0
	for _, b := range buffer {
		if b.Len() >= buf.Size-21 {
			needReshape += 1
		}
	}
	if needReshape == 0 {
		return buffer
	}
	mb2 := make(buf.MultiBuffer, 0, len(buffer)+needReshape)
	toPrint := ""
	for i, buffer1 := range buffer {
		if buffer1.Len() >= buf.Size-21 {
			index := int32(bytes.LastIndex(buffer1.Bytes(), TlsApplicationDataStart))
			if index < 21 || index > buf.Size-21 {
				index = buf.Size / 2
			}
			buffer2 := buf.New()
			buffer2.Write(buffer1.BytesFrom(index))
			buffer1.Resize(0, index)
			mb2 = append(mb2, buffer1, buffer2)
			toPrint += " " + strconv.Itoa(int(buffer1.Len())) + " " + strconv.Itoa(int(buffer2.Len()))
		} else {
			mb2 = append(mb2, buffer1)
			toPrint += " " + strconv.Itoa(int(buffer1.Len()))
		}
		buffer[i] = nil
	}
	buffer = buffer[:0]
	errors.LogDebug(ctx, "ReshapeMultiBuffer ", toPrint)
	return mb2
}

// XtlsPadding add padding to eliminate length signature during tls handshake
func XtlsPadding(b *buf.Buffer, command byte, userUUID *[]byte, longPadding bool, ctx context.Context, testseed []uint32) *buf.Buffer {
	var contentLen int32 = 0
	var paddingLen int32 = 0
	if b != nil {
		contentLen = b.Len()
	}
	if contentLen < int32(testseed[0]) && longPadding {
		l, err := rand.Int(rand.Reader, big.NewInt(int64(testseed[1])))
		if err != nil {
			errors.LogDebugInner(ctx, err, "failed to generate padding")
		}
		paddingLen = int32(l.Int64()) + int32(testseed[2]) - contentLen
	} else {
		l, err := rand.Int(rand.Reader, big.NewInt(int64(testseed[3])))
		if err != nil {
			errors.LogDebugInner(ctx, err, "failed to generate padding")
		}
		paddingLen = int32(l.Int64())
	}
	if paddingLen > buf.Size-21-contentLen {
		paddingLen = buf.Size - 21 - contentLen
	}
	newbuffer := buf.New()
	if userUUID != nil {
		newbuffer.Write(*userUUID)
		*userUUID = nil
	}
	newbuffer.Write([]byte{command, byte(contentLen >> 8), byte(contentLen), byte(paddingLen >> 8), byte(paddingLen)})
	if b != nil {
		newbuffer.Write(b.Bytes())
		b.Release()
		b = nil
	}
	newbuffer.Extend(paddingLen)
	errors.LogDebug(ctx, "XtlsPadding ", contentLen, " ", paddingLen, " ", command)
	return newbuffer
}

// XtlsUnpadding remove padding and parse command
func XtlsUnpadding(b *buf.Buffer, s *TrafficState, isUplink bool, ctx context.Context) *buf.Buffer {
	var remainingCommand *int32
	var remainingContent *int32
	var remainingPadding *int32
	var currentCommand *int
	if isUplink {
		remainingCommand = &s.Inbound.RemainingCommand
		remainingContent = &s.Inbound.RemainingContent
		remainingPadding = &s.Inbound.RemainingPadding
		currentCommand = &s.Inbound.CurrentCommand
	} else {
		remainingCommand = &s.Outbound.RemainingCommand
		remainingContent = &s.Outbound.RemainingContent
		remainingPadding = &s.Outbound.RemainingPadding
		currentCommand = &s.Outbound.CurrentCommand
	}
	if *remainingCommand == -1 && *remainingContent == -1 && *remainingPadding == -1 { // initial state
		if b.Len() >= 21 && bytes.Equal(s.UserUUID, b.BytesTo(16)) {
			b.Advance(16)
			*remainingCommand = 5
		} else {
			return b
		}
	}
	newbuffer := buf.New()
	for b.Len() > 0 {
		if *remainingCommand > 0 {
			data, err := b.ReadByte()
			if err != nil {
				return newbuffer
			}
			switch *remainingCommand {
			case 5:
				*currentCommand = int(data)
			case 4:
				*remainingContent = int32(data) << 8
			case 3:
				*remainingContent = *remainingContent | int32(data)
			case 2:
				*remainingPadding = int32(data) << 8
			case 1:
				*remainingPadding = *remainingPadding | int32(data)
				errors.LogDebug(ctx, "Xtls Unpadding new block, content ", *remainingContent, " padding ", *remainingPadding, " command ", *currentCommand)
			}
			*remainingCommand--
		} else if *remainingContent > 0 {
			len := *remainingContent
			if b.Len() < len {
				len = b.Len()
			}
			data, err := b.ReadBytes(len)
			if err != nil {
				return newbuffer
			}
			newbuffer.Write(data)
			*remainingContent -= len
		} else { // remainingPadding > 0
			len := *remainingPadding
			if b.Len() < len {
				len = b.Len()
			}
			b.Advance(len)
			*remainingPadding -= len
		}
		if *remainingCommand <= 0 && *remainingContent <= 0 && *remainingPadding <= 0 { // this block done
			if *currentCommand == 0 {
				*remainingCommand = 5
			} else {
				*remainingCommand = -1 // set to initial state
				*remainingContent = -1
				*remainingPadding = -1
				if b.Len() > 0 { // shouldn't happen
					newbuffer.Write(b.Bytes())
				}
				break
			}
		}
	}
	b.Release()
	b = nil
	return newbuffer
}

// XtlsFilterTls recognizes the first TLS hello in one traffic direction. The
// copy loops may split a record at any byte and run concurrently, so detection
// is protected by TrafficState and reassembles at most one bounded TLS record.
func XtlsFilterTls(buffer buf.MultiBuffer, trafficState *TrafficState, isUplink bool, ctx context.Context) {
	trafficState.tlsFilterMu.Lock()
	defer trafficState.tlsFilterMu.Unlock()
	if trafficState.NumberOfPacketToFilter <= 0 {
		return
	}

	direction := 0
	expectedHello := TlsHandshakeTypeClientHello
	if !isUplink {
		direction = 1
		expectedHello = TlsHandshakeTypeServerHello
	}
	filter := &trafficState.tlsFilter[direction]
	if filter.done {
		return
	}

	for _, b := range buffer {
		if b == nil || b.IsEmpty() || len(filter.data) >= maxTLSRecordPayload+5 {
			continue
		}
		remaining := maxTLSRecordPayload + 5 - len(filter.data)
		data := b.Bytes()
		if len(data) > remaining {
			data = data[:remaining]
		}
		filter.data = append(filter.data, data...)
	}
	if len(filter.data) < 6 {
		return
	}

	recordSize, valid := tlsRecordSize(filter.data)
	if !valid || filter.data[0] != 0x16 || filter.data[5] != expectedHello {
		filter.data = nil
		filter.done = true
		// A connection starts in the uplink direction. If its first payload is
		// not a ClientHello, continuing to scan arbitrary application chunks only
		// makes padding duration depend on read fragmentation.
		if isUplink || trafficState.IsTLS {
			trafficState.NumberOfPacketToFilter = 0
		}
		return
	}

	trafficState.IsTLS = true
	if isUplink {
		filter.data = nil
		filter.done = true
		errors.LogDebug(ctx, "XtlsFilterTls found tls client hello! ", buffer.Len())
		return
	}

	trafficState.IsTLS12orAbove = true
	trafficState.RemainingServerHello = int32(recordSize)
	if len(filter.data) < recordSize {
		return
	}
	hello := filter.data[:recordSize]
	if len(hello) > 45 {
		sessionIDLength := int(hello[43])
		cipherOffset := 44 + sessionIDLength
		if cipherOffset+2 <= len(hello) {
			trafficState.Cipher = uint16(hello[cipherOffset])<<8 | uint16(hello[cipherOffset+1])
		}
	}

	if bytes.Contains(hello, Tls13SupportedVersions) {
		cipherName, ok := Tls13CipherSuiteDic[trafficState.Cipher]
		if !ok {
			cipherName = "Old cipher: " + strconv.FormatUint(uint64(trafficState.Cipher), 16)
		} else if cipherName != "TLS_AES_128_CCM_8_SHA256" {
			trafficState.EnableXtls = true
		}
		errors.LogDebug(ctx, "XtlsFilterTls found tls 1.3! ", recordSize, " ", cipherName)
	} else {
		errors.LogDebug(ctx, "XtlsFilterTls found tls 1.2! ", recordSize)
	}
	trafficState.RemainingServerHello = 0
	trafficState.NumberOfPacketToFilter = 0
	filter.data = nil
	filter.done = true
}

type closeNotifySuppressor interface {
	SuppressCloseNotify()
}

// SuppressOuterCloseNotify transfers connection shutdown to the raw transport
// after Vision has bypassed the outer TLS record layer. Closing that abandoned
// TLS state would otherwise inject an outer close_notify into the inner stream.
func SuppressOuterCloseNotify(conn net.Conn) {
	if suppressor, ok := stat.TryUnwrapStatsConn(conn).(closeNotifySuppressor); ok {
		suppressor.SuppressCloseNotify()
	}
}

// UnwrapRawConn support unwrap encryption, stats, mask wrappers, tls, utls, reality, proxyproto, uds-wrapper conn and get raw tcp/uds conn from it
func UnwrapRawConn(conn net.Conn) (net.Conn, stats.Counter, stats.Counter) {
	var readCounter, writerCounter stats.Counter
	if conn != nil {
		isEncryption := false
		if commonConn, ok := conn.(*encryption.CommonConn); ok {
			conn = commonConn.Conn
			isEncryption = true
		}
		if xorConn, ok := conn.(*encryption.XorConn); ok {
			return xorConn, nil, nil // full-random xorConn should not be penetrated
		}
		if statConn, ok := conn.(*stat.CounterConnection); ok {
			conn = statConn.Connection
			readCounter = statConn.ReadCounter
			writerCounter = statConn.WriteCounter
		}

		if !isEncryption { // avoids double penetration
			if xc, ok := conn.(*tls.Conn); ok {
				conn = xc.NetConn()
			} else if utlsConn, ok := conn.(*tls.UConn); ok {
				conn = utlsConn.NetConn()
			} else if realityConn, ok := conn.(*reality.Conn); ok {
				conn = realityConn.NetConn()
			} else if realityUConn, ok := conn.(*reality.UConn); ok {
				conn = realityUConn.NetConn()
			}
		}

		conn = finalmask.UnwrapTcpMask(conn)

		if pc, ok := conn.(*proxyproto.Conn); ok {
			conn = pc.Raw()
			// 8192 > 4096, there is no need to process pc's bufReader
		}
		if uc, ok := conn.(*internet.UnixConnWrapper); ok {
			conn = uc.UnixConn
		}
	}
	return conn, readCounter, writerCounter
}

// CopyRawConnIfExist use the most efficient copy method.
// - If caller don't want to turn on splice, do not pass in both reader conn and writer conn
// - writer are from *transport.Link
func CopyRawConnIfExist(ctx context.Context, readerConn net.Conn, writerConn net.Conn, writer buf.Writer, timer *signal.ActivityTimer, inTimer *signal.ActivityTimer) error {
	readerConn, readCounter, _ := UnwrapRawConn(readerConn)
	writerConn, _, writeCounter := UnwrapRawConn(writerConn)
	reader := buf.NewReader(readerConn)
	inbound := session.InboundFromContext(ctx)
	if runtime.GOOS != "linux" && runtime.GOOS != "android" {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedUnsupportedTransport)
		return readV(ctx, reader, writer, timer, readCounter)
	}
	tc, ok := writerConn.(*net.TCPConn)
	if !ok || readerConn == nil || writerConn == nil {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedRawConnectionUnavailable)
		return readV(ctx, reader, writer, timer, readCounter)
	}
	if inbound == nil || inbound.CanSpliceCopy.Load() == session.SpliceCopyDisabled {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedInboundIneligible)
		return readV(ctx, reader, writer, timer, readCounter)
	}
	outbounds := session.OutboundsFromContext(ctx)
	if len(outbounds) == 0 {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedMissingOutbound)
		return readV(ctx, reader, writer, timer, readCounter)
	}
	for _, ob := range outbounds {
		if ob.CanSpliceCopy.Load() == session.SpliceCopyDisabled {
			SetSpliceNotUsedReason(inbound, session.SpliceNotUsedOutboundIneligible)
			return readV(ctx, reader, writer, timer, readCounter)
		}
	}

	for {
		var splice = inbound.CanSpliceCopy.Load() == session.SpliceCopyDirect
		for _, ob := range outbounds {
			if ob.CanSpliceCopy.Load() != session.SpliceCopyDirect {
				splice = false
			}
		}
		if splice && inbound.CanSpliceCopy.CompareAndSwap(session.SpliceCopyDirect, session.SpliceCopySplicing) {
			errors.LogDebug(ctx, "CopyRawConn splice")
			if inbound.SpliceMetrics != nil {
				inbound.SpliceMetrics.MarkSplice()
			}
			statWriter, _ := writer.(*dispatcher.SizeStatWriter)
			//runtime.Gosched() // necessary
			timer.SetTimeout(24 * time.Hour) // prevent leak, just in case
			if inTimer != nil {
				inTimer.SetTimeout(24 * time.Hour)
			}
			for {
				limited := &io.LimitedReader{R: readerConn, N: spliceAccountingChunk}
				written, err := tc.ReadFrom(limited)
				if readCounter != nil {
					readCounter.Add(written) // outbound stats
				}
				if writeCounter != nil {
					writeCounter.Add(written) // inbound stats
				}
				if statWriter != nil {
					statWriter.Counter.Add(written) // user stats
				}
				if inbound.SpliceMetrics != nil {
					inbound.SpliceMetrics.AddSpliceBytes(written)
				}
				if err != nil && errors.Cause(err) != io.EOF {
					return err
				}
				if err != nil || written < spliceAccountingChunk {
					return nil
				}
			}
		}
		buffer, err := reader.ReadMultiBuffer()
		if !buffer.IsEmpty() {
			if readCounter != nil {
				readCounter.Add(int64(buffer.Len()))
			}
			timer.Update()
			if werr := writer.WriteMultiBuffer(buffer); werr != nil {
				setSpliceEndedReason(inbound)
				return werr
			}
		}
		if err != nil {
			setSpliceEndedReason(inbound)
			if errors.Cause(err) == io.EOF {
				return nil
			}
			return err
		}
	}
}

// spliceAccountingChunk keeps byte counters observable during long downloads
// while amortizing the accounting work over large zero-copy transfers.
const spliceAccountingChunk int64 = 64 << 20

// SetSpliceNotUsedReason records why a protocol connection did not enter the
// raw splice path. It is a no-op for protocols without splice metrics.
func SetSpliceNotUsedReason(inbound *session.Inbound, reason session.SpliceNotUsedReason) {
	if inbound != nil && inbound.SpliceMetrics != nil {
		inbound.SpliceMetrics.SetNotSplicedReason(reason)
	}
}

func setSpliceEndedReason(inbound *session.Inbound) {
	if inbound == nil {
		return
	}
	if inbound.CanSpliceCopy.Load() == session.SpliceCopyDirect {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedEndedBeforeSplice)
	} else {
		SetSpliceNotUsedReason(inbound, session.SpliceNotUsedEndedBeforeDirect)
	}
}

func readV(ctx context.Context, reader buf.Reader, writer buf.Writer, timer signal.ActivityUpdater, readCounter stats.Counter) error {
	errors.LogDebug(ctx, "CopyRawConn (maybe) readv")
	if err := buf.Copy(reader, writer, buf.UpdateActivity(timer), buf.AddToStatCounter(readCounter)); err != nil {
		return errors.New("failed to process response").Base(err)
	}
	return nil
}

func IsRAWTransportWithoutSecurity(conn stat.Connection) bool {
	iConn := stat.TryUnwrapStatsConn(conn)
	iConn = finalmask.UnwrapTcpMask(iConn)
	_, ok1 := iConn.(*proxyproto.Conn)
	_, ok2 := iConn.(*net.TCPConn)
	_, ok3 := iConn.(*internet.UnixConnWrapper)
	return ok1 || ok2 || ok3
}
