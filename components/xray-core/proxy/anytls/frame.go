package anytls

import (
	"encoding/binary"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
)

const (
	frameHeaderSize = 7

	// cmds
	cmdWaste               = 0  // Paddings
	cmdSYN                 = 1  // stream open
	cmdPSH                 = 2  // data push
	cmdFIN                 = 3  // stream close, a.k.a EOF mark
	cmdSettings            = 4  // Settings (Client send to Server)
	cmdAlert               = 5  // Alert
	cmdUpdatePaddingScheme = 6  // update padding scheme
	cmdSYNACK              = 7  // Server reports to the client that the stream has been opened
	cmdHeartRequest        = 8  // Keep alive command
	cmdHeartResponse       = 9  // Keep alive command
	cmdServerSettings      = 10 // Settings (Server send to client)
)

type frame struct {
	cmd  byte
	sid  uint32
	data []byte
}

func newFrame(cmd byte, sid uint32) *frame {
	return &frame{cmd: cmd, sid: sid}
}

func (f *frame) toMultiBuffer() (buf.MultiBuffer, error) {
	if f == nil || len(f.data) > maxFramePayload {
		return nil, errors.New("anytls: frame payload too large")
	}
	hdr := buf.New()
	hdrb := hdr.Extend(frameHeaderSize)
	hdrb[0] = f.cmd
	binary.BigEndian.PutUint32(hdrb[1:5], f.sid)
	length := len(f.data)
	binary.BigEndian.PutUint16(hdrb[5:7], uint16(length))
	var mb buf.MultiBuffer
	if length > 0 {
		body := buf.NewWithSize(int32(length))
		bodyb := body.Extend(int32(length))
		copy(bodyb, f.data)
		mb = buf.MultiBuffer{hdr, body}
	} else {
		mb = buf.MultiBuffer{hdr}
	}

	return mb, nil
}

func (f *frame) toMultiBufferWithBody(body *buf.Buffer) (buf.MultiBuffer, error) {
	if f == nil {
		if body != nil {
			body.Release()
		}
		return nil, errors.New("anytls: nil frame")
	}
	hdr := buf.New()
	hdrb := hdr.Extend(frameHeaderSize)
	hdrb[0] = f.cmd
	binary.BigEndian.PutUint32(hdrb[1:5], f.sid)
	if body == nil {
		binary.BigEndian.PutUint16(hdrb[5:7], 0)
		return buf.MultiBuffer{hdr}, nil
	}
	length := int(body.Len())
	if length > maxFramePayload {
		hdr.Release()
		body.Release()
		return nil, errors.New("anytls: frame payload too large")
	}
	binary.BigEndian.PutUint16(hdrb[5:7], uint16(length))
	if length == 0 {
		body.Release()
		return buf.MultiBuffer{hdr}, nil
	}
	return buf.MultiBuffer{hdr, body}, nil
}

type frameWriter struct {
	bw     *buf.BufferedWriter
	header [frameHeaderSize]byte
}

func newFrameWriter(bw *buf.BufferedWriter) *frameWriter {
	return &frameWriter{bw: bw}
}

const maxFramePayload = 0xffff

func (w *frameWriter) writeFrame(f *frame) error {
	if f == nil {
		return nil
	}
	if len(f.data) > maxFramePayload {
		return errors.New("anytls: frame payload too large")
	}
	w.header[0] = f.cmd
	binary.BigEndian.PutUint32(w.header[1:5], f.sid)
	binary.BigEndian.PutUint16(w.header[5:7], uint16(len(f.data)))

	if _, err := w.bw.Write(w.header[:]); err != nil {
		return err
	}

	if len(f.data) > 0 {
		_, err := w.bw.Write(f.data)
		return err
	}

	return nil
}

func (w *frameWriter) writeMultiBuffer(cmd byte, sid uint32, mb buf.MultiBuffer) error {
	if mb.IsEmpty() {
		return nil
	}
	if mb.Len() > maxFramePayload {
		return errors.New("anytls: frame payload too large")
	}
	w.header[0] = cmd
	binary.BigEndian.PutUint32(w.header[1:5], sid)
	binary.BigEndian.PutUint16(w.header[5:7], uint16(mb.Len()))

	if _, err := w.bw.Write(w.header[:]); err != nil {
		return err
	}

	return w.bw.WriteMultiBuffer(mb)
}

func (w *frameWriter) flush() error {
	return w.bw.Flush()
}
