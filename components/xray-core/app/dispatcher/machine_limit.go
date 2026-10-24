package dispatcher

import (
	"context"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/machinelimit"
	"github.com/xtls/xray-core/common/session"
)

func machineController(ctx context.Context) *machinelimit.Controller {
	if inbound := session.InboundFromContext(ctx); inbound != nil && inbound.Tag == "api" {
		return nil
	}
	if outbounds := session.OutboundsFromContext(ctx); len(outbounds) > 0 {
		target := outbounds[len(outbounds)-1].OriginalTarget
		if target.Address != nil && target.Address.Family().IsDomain() && target.Address.Domain() == "v1.mux.cool" {
			return nil
		}
	}
	return machinelimit.FromContext(ctx)
}
func machineWriter(ctx context.Context, w buf.Writer) buf.Writer {
	c := machineController(ctx)
	if c == nil {
		return w
	}
	ctx, cancel := context.WithCancel(ctx)
	return &limitedWriter{ctx: ctx, cancel: cancel, controller: c, writer: w}
}

type limitedWriter struct {
	ctx        context.Context
	cancel     context.CancelFunc
	controller *machinelimit.Controller
	writer     buf.Writer
}

func (w *limitedWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	size, err := w.controller.ChunkSize()
	if err != nil {
		buf.ReleaseMulti(mb)
		return err
	}
	if size == 1<<20 {
		return w.writer.WriteMultiBuffer(mb)
	}
	for len(mb) > 0 {
		size, err := w.controller.ChunkSize()
		if err != nil {
			buf.ReleaseMulti(mb)
			return err
		}
		// UDP datagrams remain atomic; stream buffers can be split without losing framing metadata.
		if mb[0].UDP != nil {
			size = int(mb[0].Len())
		}
		var part buf.MultiBuffer
		if mb[0].UDP != nil {
			var b *buf.Buffer
			mb, b = buf.SplitFirst(mb)
			part = buf.MultiBuffer{b}
		} else {
			mb, part = buf.SplitSize(mb, int32(size))
		}
		if err = w.controller.Wait(w.ctx, int(part.Len())); err != nil {
			buf.ReleaseMulti(part)
			buf.ReleaseMulti(mb)
			return err
		}
		if err = w.writer.WriteMultiBuffer(part); err != nil {
			buf.ReleaseMulti(mb)
			return err
		}
	}
	return nil
}
func (w *limitedWriter) Close() error { w.cancel(); return common.Close(w.writer) }
func (w *limitedWriter) Interrupt()   { w.cancel(); common.Interrupt(w.writer) }

type limitedReader struct {
	ctx        context.Context
	cancel     context.CancelFunc
	controller *machinelimit.Controller
	reader     buf.Reader
}

func (r *limitedReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	mb, err := r.reader.ReadMultiBuffer()
	if e := r.controller.Wait(r.ctx, int(mb.Len())); e != nil {
		buf.ReleaseMulti(mb)
		return nil, e
	}
	return mb, err
}
func (r *limitedReader) Interrupt() { r.cancel(); common.Interrupt(r.reader) }
