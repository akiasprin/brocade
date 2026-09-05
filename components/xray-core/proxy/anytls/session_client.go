package anytls

import (
	"context"
	"encoding/binary"

	M "github.com/sagernet/sing/common/metadata"
	"github.com/sagernet/sing/common/uot"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/singbridge"
	"github.com/xtls/xray-core/transport"
)

var (
	errSessionClosed     = errors.New("anytls: session closed")
	errStreamIDExhausted = errors.New("anytls: stream ID space exhausted")
)

func (s *session) writePacketWithPadding(packetIndex uint32, frames buf.MultiBuffer) error {
	length := frames.Len()
	if length == 0 {
		return nil
	}
	b := frames
	defer func() {
		// The buffered writer owns buffers it accepts. Any remainder still held
		// by the planner must be released when a later write or flush fails.
		if b != nil {
			buf.ReleaseMulti(b)
		}
	}()
	s.schemeMu.RLock()
	scheme := s.paddingScheme
	s.schemeMu.RUnlock()
	if scheme == nil || packetIndex >= scheme.stop {
		if err := s.fw.bw.WriteMultiBuffer(frames); err != nil {
			return err
		}
		return s.fw.flush()
	}
	pktSizes := scheme.GenerateRecordPayloadSizes(packetIndex)
	if len(pktSizes) == 0 {
		if err := s.fw.bw.WriteMultiBuffer(frames); err != nil {
			return err
		}
		return s.fw.flush()
	}

	for _, targetsize := range pktSizes {
		size := targetsize
		remain := int(b.Len())
		if size == CheckMark {
			if b.IsEmpty() {
				break
			}
			continue
		}
		if size <= 7 || size > maxPaddingTargetSize {
			return errors.New("anytls: invalid padding scheme")
		}

		var data buf.MultiBuffer
		if remain > size {
			b, data = buf.SplitSize(b, int32(size))
			err := s.fw.bw.WriteMultiBuffer(data)
			if err != nil {
				return err
			}
			err = s.fw.flush()
			if err != nil {
				return err
			}
		} else if remain > 0 {
			paddingSize := size - remain
			if err := s.fw.bw.WriteMultiBuffer(b); err != nil {
				return err
			}
			b = nil
			if paddingSize >= 7 {
				if err := s.writeWasteFrames(paddingSize); err != nil {
					return err
				}
			} else if err := s.fw.flush(); err != nil {
				return err
			}
		} else {
			if err := s.writeWasteFrames(size); err != nil {
				return err
			}
		}
	}
	if !b.IsEmpty() {
		err := s.fw.bw.WriteMultiBuffer(b)
		if err != nil {
			return err
		}
		err = s.fw.flush()
		if err != nil {
			return err
		}
	}

	return nil
}

func (s *session) writeWasteFrames(total int) error {
	if total < 7 || total > maxPaddingTargetSize {
		return errors.New("anytls: invalid padding size")
	}
	const wasteFrameOverhead = 7
	maxWireSize := maxFramePayload + wasteFrameOverhead
	remaining := total
	for remaining > 0 {
		frameCount := (remaining + maxWireSize - 1) / maxWireSize
		minimumForRemainingFrames := wasteFrameOverhead * (frameCount - 1)
		wireSize := remaining - minimumForRemainingFrames
		if wireSize > maxWireSize {
			wireSize = maxWireSize
		}
		if wireSize < wasteFrameOverhead {
			return errors.New("anytls: invalid padding frame plan")
		}
		bodyLength := wireSize - wasteFrameOverhead

		s.fw.header[0] = cmdWaste
		binary.BigEndian.PutUint32(s.fw.header[1:5], 0)
		binary.BigEndian.PutUint16(s.fw.header[5:7], uint16(bodyLength))
		if _, err := s.fw.bw.Write(s.fw.header[:]); err != nil {
			return err
		}
		if bodyLength > 0 {
			body := buf.NewWithSize(int32(bodyLength))
			body.Extend(int32(bodyLength))
			if err := s.fw.bw.WriteMultiBuffer(buf.MultiBuffer{body}); err != nil {
				return err
			}
		}
		remaining -= wireSize
	}
	return s.fw.flush()
}

func (s *session) openStream(ctx context.Context, target net.Destination, link *transport.Link) (*stream, error) {
	if s.isClosed() {
		return nil, errSessionClosed
	}

	actualDest := target
	if target.Network == net.Network_UDP {
		actualDest = net.Destination{
			Network: net.Network_TCP,
			Address: net.ParseAddress("sp.v2.udp-over-tcp.arpa"),
			Port:    0,
		}
	}

	// ID assignment and the corresponding SYN write must stay in one order.
	// Allocating before serialization lets concurrent callers put SID 2 on the
	// wire before SID 1 even though both counters are individually atomic.
	s.openMu.Lock()
	openLocked := true
	defer func() {
		if openLocked {
			s.openMu.Unlock()
		}
	}()
	if s.isClosed() {
		return nil, errSessionClosed
	}
	sid := s.nextSID.Load()
	if sid == 0 {
		return nil, errStreamIDExhausted
	}
	s.nextSID.Store(sid + 1)
	st := newStream(sid, link)
	if target.Network == net.Network_UDP {
		st.isUDP = true
		st.uotConnect = true
		targetCopy := target
		st.udpTarget = &targetCopy
	}
	// Stream opening is deliberately optimistic. A successful SYNACK is only
	// advisory: the server dispatcher returns before the target dial completes,
	// so waiting for it would delay the first uplink payload by a full session
	// RTT without proving that the destination is reachable. Rejections remain
	// asynchronous and close only this stream in the session read loop.
	s.streamsMu.Lock()
	// close marks the session before it takes streamsMu. Registering and incrementing under the
	// same lock lets close either reject this stream or clear both pieces of state afterwards;
	// neither the map entry nor activeStreams can reappear after close has reset them.
	if s.isClosed() {
		s.streamsMu.Unlock()
		return nil, errSessionClosed
	}
	s.streams[st.sid] = st
	s.activeStreams.Add(1)
	s.inIdlePool.Store(false)
	s.streamsMu.Unlock()

	var frames buf.MultiBuffer
	addrBuf := buf.New()
	if err := M.SocksaddrSerializer.WriteAddrPort(addrBuf, singbridge.ToSocksaddr(actualDest)); err != nil {
		addrBuf.Release()
		s.finishStream(sid, err)
		return nil, errors.New("anytls: write socks addr failed").Base(err)
	}
	synFrame, err := newFrame(cmdSYN, sid).toMultiBuffer()
	if err != nil {
		addrBuf.Release()
		s.finishStream(sid, err)
		return nil, err
	}
	frames = append(frames, synFrame...)
	addrFrame, err := (&frame{cmd: cmdPSH, sid: sid}).toMultiBufferWithBody(addrBuf)
	if err != nil {
		s.finishStream(sid, err)
		return nil, err
	}
	frames = append(frames, addrFrame...)

	s.writeMu.Lock()
	if !s.settingsSent {
		s.schemeMu.RLock()
		md5Value := ""
		if s.paddingScheme != nil {
			md5Value = s.paddingScheme.md5
		}
		s.schemeMu.RUnlock()
		settingsFrame, err := (&frame{cmd: cmdSettings, sid: 0, data: []byte("v=2\nclient=" + clientMetadata() + "\npadding-md5=" + md5Value)}).toMultiBuffer()
		if err != nil {
			s.writeMu.Unlock()
			s.finishStream(sid, err)
			return nil, err
		}
		frames = append(settingsFrame, frames...)
		s.settingsSent = true
	}
	writeErr := s.writePacketLocked(frames)
	s.writeMu.Unlock()
	if writeErr != nil {
		s.finishStream(sid, writeErr)
		return nil, errors.New("anytls: send session open packet failed").Base(writeErr)
	}
	openLocked = false
	s.openMu.Unlock()

	if target.Network == net.Network_UDP {
		reqBuf := buf.New()
		err := uot.WriteRequest(reqBuf, uot.Request{
			IsConnect:   true,
			Destination: singbridge.ToSocksaddr(target),
		})
		if err != nil {
			reqBuf.Release()
			s.finishStream(sid, err)
			return nil, errors.New("anytls: write UoT request failed").Base(err)
		}
		UDPPSHframe, err := (&frame{cmd: cmdPSH, sid: sid}).toMultiBufferWithBody(reqBuf)
		if err != nil {
			s.finishStream(sid, err)
			return nil, err
		}

		err = s.writePacket(UDPPSHframe)

		if err != nil {
			s.finishStream(sid, err)
			return nil, errors.New("anytls: send UoT request failed").Base(err)
		}
	}

	s.startStreamDelivery(st)
	return st, nil
}

func (st *stream) pumpUplink(s *session) {
	defer func() {
		if s.finishStream(st.sid, nil) && !s.isClosed() {
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
		}
	}()
	for {
		mb, err := st.link.Reader.ReadMultiBuffer()
		if err != nil {
			break
		}
		if st.isUDP {
			mb, err = encodeUDPData(mb, st.uotConnect, st.udpTarget)
			if err != nil {
				errors.LogDebug(context.Background(), "anytls: encode UoT packet error=", err)
				_ = s.sendFrame(newFrame(cmdFIN, st.sid))
				s.close(err)
				return
			}
		}
		if sendErr := s.sendStreamData(st.sid, mb); sendErr != nil {
			errors.LogDebug(context.Background(), "anytls: writePacketWithPadding error=", sendErr)
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
			s.close(sendErr)
			return
		}

	}
}
