package anytls

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/binary"
	"encoding/hex"
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

	streamsMu sync.Mutex
	streams   map[uint32]*stream

	peerVersion     byte
	errCh           chan error
	closed          atomic.Bool
	synAckSupported atomic.Bool
	seq             uint64

	server           *Server
	dispatcher       routing.Dispatcher
	handshakeDone    bool
	clientPaddingMD5 string

	client       *Client
	nextSID      atomic.Uint32
	pktCounter   atomic.Uint32
	settingsSent bool

	schemeMu      sync.RWMutex
	paddingScheme *paddingScheme

	synAckMu sync.Mutex
	synAckCh map[uint32]chan error

	activeStreams atomic.Int32
	idleSinceNano atomic.Int64
	inIdlePool    atomic.Bool
	dieHook       func()
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

	if err := st.link.Writer.WriteMultiBuffer(body); err != nil {
		return err
	}
	return nil
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
	if strings.Contains(dest.Address.String(), "udp-over-tcp.arpa") {
		st.isUDP = true
		if err := s.sendFrame(newFrame(cmdSYNACK, st.sid)); err != nil {
			errors.LogWarning(ctx, "anytls: UDP SYNACK send error, streamId=", st.sid, " err=", err)
			return err
		}
		return nil
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
	st.link = l

	if err := s.sendFrame(newFrame(cmdSYNACK, st.sid)); err != nil {
		errors.LogWarning(ctx, "anytls: new stream SYNACK send error, streamId=", st.sid, " err=", err)
		return err
	}

	if bodyReader.Len() > 0 {
		initial, err := io.ReadAll(bodyReader)
		if err != nil {
			return err
		}
		if err := st.link.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(initial)}); err != nil {
			return err
		}
	}
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

		link, err := s.dispatcher.Dispatch(ctx, requestDest)
		if err != nil {
			errors.LogWarning(ctx, "anytls: UDP dispatcher error, streamId=", st.sid, " err=", err)
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
			s.finishStream(st.sid, nil)
			return nil
		}

		st.link = link
		st.uotConnect = request.IsConnect
		st.udpTarget = &requestDest
		if bodyReader.Len() > 0 {
			initial, err := io.ReadAll(bodyReader)
			if err != nil {
				return err
			}
			if err := s.handleUDPData(st, initial); err != nil {
				return err
			}
		}

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
			if err := st.link.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
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
		recordLength := addressLength + 2 + len(payload)
		if recordLength > maxFramePayload {
			buf.ReleaseMulti(encoded)
			buf.ReleaseMulti(data)
			return nil, errors.New("anytls: UoT packet is too large")
		}
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
		s.streamsMu.Lock()
		st := s.streams[sid]
		delete(s.streams, sid)
		s.streamsMu.Unlock()
		if st != nil && st.link != nil {
			common.Close(st.link.Writer)
			common.Close(st.link.Reader)
		}
		if !s.isClosed() {
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

		if err := s.sendStreamData(sid, mb, s.nextPacketIndex()); err != nil {
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
	if err != nil {
		select {
		case s.errCh <- err:
		default:
		}
	}
	_ = s.conn.Close()

	s.streamsMu.Lock()
	streams := make([]*stream, 0, len(s.streams))
	for _, st := range s.streams {
		streams = append(streams, st)
	}
	s.streams = make(map[uint32]*stream)
	s.streamsMu.Unlock()

	for _, st := range streams {
		st.close(err)
	}
	if s.dieHook != nil {
		s.dieHook()
	}
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

func (s *session) sendFrame(f *frame) error {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	if err := s.fw.writeFrame(f); err != nil {
		return err
	}
	return s.fw.flush()
}

// Packet indexes are session-wide because padding rules describe records, not individual
// streams. Both directions use the same counter shape: once the configured stop value is
// reached, a zero index keeps the wire unpadded without advancing the counter forever.
func (s *session) nextPacketIndex() uint32 {
	s.schemeMu.RLock()
	scheme := s.paddingScheme
	s.schemeMu.RUnlock()
	if scheme != nil && s.pktCounter.Load() < scheme.stop {
		return s.pktCounter.Add(1) - 1
	}
	return 0
}

func (s *session) sendStreamData(sid uint32, data buf.MultiBuffer, packetIndex uint32) error {
	defer buf.ReleaseMulti(data)
	for !data.IsEmpty() {
		var chunk buf.MultiBuffer
		data, chunk = buf.SplitSize(data, maxFramePayload)
		if packetIndex > 0 {
			b := buf.New()
			p := b.Extend(7)
			p[0] = cmdPSH
			binary.BigEndian.PutUint32(p[1:5], sid)
			binary.BigEndian.PutUint16(p[5:7], uint16(chunk.Len()))
			merge, _ := buf.MergeMulti(buf.MultiBuffer{b}, chunk)
			s.writeMu.Lock()
			if err := s.writePacketWithPadding(packetIndex, merge); err != nil {
				return err
			}
			s.writeMu.Unlock()
		} else {
			s.writeMu.Lock()
			err := s.fw.writeMultiBuffer(cmdPSH, sid, chunk)
			if err == nil {
				err = s.fw.flush()
			}
			s.writeMu.Unlock()
			if err != nil {
				buf.ReleaseMulti(data)
				return err
			}
		}

	}
	return nil
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
			if s.handshakeDone {
				return errors.New("anytls: duplicate settings")
			}
			settings, err := parseSettings(text)
			if err != nil {
				return err
			}
			if settings.version > 2 {
				s.peerVersion = 2
			} else {
				s.peerVersion = settings.version
			}
			s.clientPaddingMD5 = settings.paddingMD5
			if err := s.sendFrame(&frame{cmd: cmdServerSettings, sid: 0, data: []byte("v=2")}); err != nil {
				return err
			}
			if s.server != nil && s.server.paddingScheme != "" && s.clientPaddingMD5 != "" {
				sum := md5.Sum([]byte(s.server.paddingScheme))
				if strings.ToLower(hex.EncodeToString(sum[:])) != s.clientPaddingMD5 {
					if err := s.sendFrame(&frame{cmd: cmdUpdatePaddingScheme, sid: 0, data: []byte(s.server.paddingScheme)}); err != nil {
						return err
					}
				}
			}
			s.handshakeDone = true
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
				if !s.handshakeDone {
					alert := newFrame(cmdAlert, 0)
					alert.data = []byte("client did not send its settings")
					_ = s.sendFrame(alert)
					return errors.New("anytls: client did not send its settings")
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
					s.streams[sid] = &stream{sid: sid}
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
				err := errors.New("anytls: received PSH for unknown stream, streamId=", sid)
				return err
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
			} else if st.isUDP {
				body := make([]byte, length)
				if _, err := io.ReadFull(s.br, body); err != nil {
					return err
				}
				if err := s.handleUDPData(st, body); err != nil {
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
			s.finishStream(sid, nil)
		case cmdSYNACK:
			if !s.isClient {
				if length > 0 {
					if err := discardBytes(s.br, length); err != nil {
						return err
					}
				}
				return errors.New("anytls: unexpected SYNACK from client")
			}
			if length == 0 {
				// A zero-length SYNACK is an optional success confirmation. Record
				// it so later streams can wait for prompt dispatcher rejections,
				// while remaining compatible with clients such as sing-box that do
				// not send success confirmations at all.
				s.synAckSupported.Store(true)
			}
			s.synAckMu.Lock()
			ch := s.synAckCh[sid]
			s.synAckMu.Unlock()
			if length == 0 {
				if ch != nil {
					ch <- nil
				}
			} else {
				bodyText, err := readText(s.br, length)
				if err != nil {
					return err
				}
				errors.LogWarning(ctx, "anytls: stream handshake rejected, streamId=", sid, " err=", bodyText)
				rejected := errors.New(bodyText)
				if s.finishStream(sid, rejected) && !s.isClosed() {
					if err := s.sendFrame(newFrame(cmdFIN, sid)); err != nil {
						return err
					}
				}
				if ch != nil {
					ch <- rejected
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
			settings, err := parseSettings(bodyText)
			if err != nil {
				return err
			}
			if settings.version > 2 {
				s.peerVersion = 2
			} else {
				s.peerVersion = settings.version
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
				s.schemeMu.Lock()
				s.paddingScheme = scheme
				s.schemeMu.Unlock()
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
