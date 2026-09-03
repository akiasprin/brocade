package anytls

import (
	"context"
	"encoding/binary"
	"time"

	M "github.com/sagernet/sing/common/metadata"
	"github.com/sagernet/sing/common/uot"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/singbridge"
	"github.com/xtls/xray-core/transport"
)

func (s *session) writePacketWithPadding(packetIndex uint32, frames buf.MultiBuffer) error {
	length := frames.Len()
	if length == 0 {
		return nil
	}
	s.schemeMu.RLock()
	scheme := s.paddingScheme
	s.schemeMu.RUnlock()
	pktSizes := scheme.GenerateRecordPayloadSizes(packetIndex)
	if scheme == nil || packetIndex >= scheme.stop || len(pktSizes) == 0 {
		err := s.fw.bw.WriteMultiBuffer(frames)
		if err != nil {
			buf.ReleaseMulti(frames)
			return err
		}
		return s.fw.flush()
	}

	b := frames
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
		return nil, errors.New("anytls: session closed")
	}

	actualDest := target
	if target.Network == net.Network_UDP {
		actualDest = net.Destination{
			Network: net.Network_TCP,
			Address: net.ParseAddress("sp.v2.udp-over-tcp.arpa"),
			Port:    0,
		}
	}

	sid := s.nextSID.Add(1) - 1
	st := newStream(sid, link)
	if target.Network == net.Network_UDP {
		st.isUDP = true
		st.uotConnect = true
		targetCopy := target
		st.udpTarget = &targetCopy
	}
	// SYNACK success is optional in AnyTLS v2. Only wait after the peer has
	// demonstrated support by returning a zero-length SYNACK; this keeps
	// stream creation compatible with sing-box/sing-anytls servers that do
	// not send success confirmations.
	waitForSynAck := sid >= 2 && s.peerVersion >= 2 && s.synAckSupported.Load() && target.Network != net.Network_UDP
	s.streamsMu.Lock()
	s.streams[st.sid] = st
	s.streamsMu.Unlock()
	s.activeStreams.Add(1)
	s.inIdlePool.Store(false)

	var ch chan error
	if waitForSynAck {
		ch = make(chan error, 1)
		s.synAckMu.Lock()
		s.synAckCh[sid] = ch
		s.synAckMu.Unlock()
		defer func() {
			s.synAckMu.Lock()
			delete(s.synAckCh, sid)
			s.synAckMu.Unlock()
		}()
	}

	var frames buf.MultiBuffer
	if !s.settingsSent {
		s.schemeMu.RLock()
		md5Value := s.paddingScheme.md5
		s.schemeMu.RUnlock()
		settingsFrame, err := (&frame{cmd: cmdSettings, sid: 0, data: []byte("v=2\nclient=" + clientMetadata() + "\npadding-md5=" + md5Value)}).toMultiBuffer()
		if err != nil {
			return nil, err
		}
		frames = append(frames, settingsFrame...)
		s.settingsSent = true
	}
	addrBuf := buf.New()
	if err := M.SocksaddrSerializer.WriteAddrPort(addrBuf, singbridge.ToSocksaddr(actualDest)); err != nil {
		addrBuf.Release()
		s.finishStream(sid, err)
		return nil, errors.New("anytls: write socks addr failed").Base(err)
	}
	synFrame, err := newFrame(cmdSYN, sid).toMultiBuffer()
	if err != nil {
		addrBuf.Release()
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
	writeErr := s.writePacketWithPadding(s.pktCounter.Add(1)-1, frames)
	s.writeMu.Unlock()
	if writeErr != nil {
		s.finishStream(sid, writeErr)
		return nil, errors.New("anytls: send session open packet failed").Base(writeErr)
	}

	if waitForSynAck {
		select {
		case serr := <-ch:
			if serr != nil {
				s.finishStream(sid, serr)
				return nil, errors.New("anytls: SYN rejected").Base(serr)
			}
		case sessErr := <-s.errCh:
			s.finishStream(sid, sessErr)
			return nil, sessErr
		case <-time.After(3 * time.Second):
			timeoutErr := errors.New("anytls: SYNACK timeout")
			s.close(timeoutErr)
			return nil, timeoutErr
		case <-ctx.Done():
			s.finishStream(sid, ctx.Err())
			return nil, ctx.Err()
		}
	}

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

		s.writeMu.Lock()
		err = s.writePacketWithPadding(s.pktCounter.Add(1)-1, UDPPSHframe)
		s.writeMu.Unlock()

		if err != nil {
			s.finishStream(sid, err)
			return nil, errors.New("anytls: send UoT request failed").Base(err)
		}
	}

	return st, nil
}

func (st *stream) pumpUplink(s *session) {
	defer func() {
		_ = s.sendFrame(newFrame(cmdFIN, st.sid))
		s.finishStream(st.sid, nil)
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
		var pktIndex uint32
		s.schemeMu.RLock()
		scheme := s.paddingScheme
		s.schemeMu.RUnlock()

		if scheme != nil && s.pktCounter.Load() < scheme.stop {
			pktIndex = s.pktCounter.Add(1) - 1
		} else {
			pktIndex = 0
		}

		if sendErr := s.sendStreamData(st.sid, mb, pktIndex); sendErr != nil {
			errors.LogDebug(context.Background(), "anytls: writePacketWithPadding error=", sendErr)
			_ = s.sendFrame(newFrame(cmdFIN, st.sid))
			s.close(sendErr)
			return
		}

	}
}
