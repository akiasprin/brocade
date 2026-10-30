package probetrace

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"sync"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/transport"
)

var requestMagic = []byte("BRC-PROBE\x02")
var responseMagic = []byte("BRC-TRACE\x02")

type deadlineReader struct {
	reader   buf.TimeoutReader
	deadline time.Time
}

func (r *deadlineReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	left := time.Until(r.deadline)
	if left <= 0 {
		return nil, buf.ErrReadTimeout
	}
	return r.reader.ReadMultiBufferTimeout(left)
}
func timed(reader buf.Reader) buf.TimeoutReader {
	if r, ok := reader.(buf.TimeoutReader); ok {
		return r
	}
	return &interruptibleReader{TimeoutWrapperReader: &buf.TimeoutWrapperReader{Reader: reader}}
}

type interruptibleReader struct{ *buf.TimeoutWrapperReader }

func (r *interruptibleReader) Interrupt()   { common.Interrupt(r.Reader) }
func (r *interruptibleReader) Close() error { return common.Interrupt(r.Reader) }

// Prepare strips the private request envelope before routing/sniffing. Local SOCKS
// listeners create a distinct connection ID; remote authentication must own probe credentials.
func Prepare(ctx context.Context, destination net.Destination, link *transport.Link) (context.Context, error) {
	if FromContext(ctx) != nil || destination.Network != net.Network_TCP || destination.Address.Family().IsDomain() && destination.Address.Domain() == "v1.mux.cool" {
		return ctx, nil
	}
	if group := localGroup(ctx); group != "" {
		if !destination.Address.Family().IsDomain() || !Domain(destination.Address.Domain()) {
			return ctx, nil
		}
		state, err := newLocal(destination.Address.Domain(), group)
		if err != nil {
			return ctx, nil
		}
		return WithState(ctx, state), nil
	}
	if !probeUser(ctx) {
		return ctx, nil
	}
	original := link.Reader
	reader := timed(original)
	buffered := &buf.BufferedReader{Reader: &deadlineReader{reader: reader, deadline: time.Now().Add(2 * time.Second)}}
	header := make([]byte, len(requestMagic))
	if _, err := io.ReadFull(buffered, header); err != nil {
		common.Interrupt(original)
		return ctx, err
	}
	if string(header) != string(requestMagic) {
		// Probe credentials also serve family-capability diagnostics. An ordinary
		// TLS/HTTP stream carries no evidence and retains its original bytes.
		buffered.Reader = reader
		buffered.Buffer = append(buf.MergeBytes(nil, header), buffered.Buffer...)
		link.Reader = timed(buffered)
		return ctx, nil
	}
	rest := make([]byte, 17)
	if _, err := io.ReadFull(buffered, rest); err != nil {
		common.Interrupt(original)
		return ctx, err
	}
	header = append(header, rest...)
	domain := make([]byte, int(header[len(header)-1]))
	if _, err := io.ReadFull(buffered, domain); err != nil || !Domain(string(domain)) {
		common.Interrupt(original)
		return ctx, errors.New("invalid probe domain")
	}
	buffered.Reader = reader
	link.Reader = timed(buffered)
	state := &State{ID: hex.EncodeToString(header[len(requestMagic) : len(requestMagic)+16]), Domain: string(domain)}
	writer := &responseWriter{Writer: link.Writer, state: state}
	if inbound := session.InboundFromContext(ctx); inbound != nil && inbound.Conn != nil {
		writer.cancel = func() { _ = inbound.Conn.Close() }
	}
	state.emit = writer.emit
	link.Writer = writer
	return WithState(ctx, state), nil
}

type responseWriter struct {
	buf.Writer
	state  *State
	mu     sync.Mutex
	cancel func()
}

func (w *responseWriter) emit(result Result) error {
	encoded, err := json.Marshal(result)
	if err != nil {
		return err
	}
	if len(encoded) > MaxFrame {
		return errors.New("probe response envelope limit")
	}
	frame := append([]byte(nil), responseMagic...)
	id, _ := hex.DecodeString(result.ID)
	frame = append(frame, id...)
	frame = binary.BigEndian.AppendUint16(frame, uint16(len(encoded)))
	frame = append(frame, encoded...)
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.Writer.WriteMultiBuffer(buf.MergeBytes(nil, frame))
}
func (w *responseWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	w.state.Unknown()
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.Writer.WriteMultiBuffer(mb)
}
func (w *responseWriter) Close() error {
	w.state.Abort()
	w.mu.Lock()
	defer w.mu.Unlock()
	return common.Close(w.Writer)
}
func (w *responseWriter) Interrupt() {
	// Dispatch owns a logical pipe even when the authenticated transport is
	// multiplexed. Interrupt that pipe without closing other streams' session.
	if stream, ok := w.Writer.(common.Interruptible); ok {
		stream.Interrupt()
	} else if w.cancel != nil {
		w.cancel()
	} else {
		common.Interrupt(w.Writer)
	}
	w.state.Abort()
	w.mu.Lock()
	defer w.mu.Unlock()
	common.Interrupt(w.Writer)
}

// Relay wraps one authenticated diagnostic stream, independently of its public transport.
// Probe mux is bypassed by the caller so its lifecycle is that of this exact connection.
func Relay(state *State, link *transport.Link) *transport.Link {
	id, _ := hex.DecodeString(state.ID)
	header := append(append([]byte(nil), requestMagic...), id...)
	header = append(header, byte(len(state.Domain)))
	header = append(header, state.Domain...)
	reader := &buf.BufferedReader{Reader: link.Reader, Buffer: buf.MultiBuffer{buf.FromBytes(header)}}
	return &transport.Link{Reader: timed(reader), Writer: &collectorWriter{Writer: link.Writer, state: state}}
}

type collectorWriter struct {
	buf.Writer
	state   *State
	pending []byte
	done    bool
	mu      sync.Mutex
}

func (w *collectorWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.done {
		return w.Writer.WriteMultiBuffer(mb)
	}
	defer buf.ReleaseMulti(mb)
	headerSize := len(responseMagic) + 16 + 2
	for _, block := range mb {
		data := block.Bytes()
		for len(data) > 0 && !w.done {
			target := headerSize
			if len(w.pending) >= headerSize {
				target += int(binary.BigEndian.Uint16(w.pending[headerSize-2 : headerSize]))
			}
			take := target - len(w.pending)
			if take > len(data) {
				take = len(data)
			}
			w.pending = append(w.pending, data[:take]...)
			data = data[take:]
			if len(w.pending) < headerSize {
				continue
			}
			if string(w.pending[:len(responseMagic)]) != string(responseMagic) {
				return errors.New("invalid probe response envelope")
			}
			size := int(binary.BigEndian.Uint16(w.pending[headerSize-2 : headerSize]))
			if size == 0 || size > MaxFrame {
				return errors.New("probe response limit")
			}
			if len(w.pending) < headerSize+size {
				continue
			}
			var result Result
			if err := json.Unmarshal(w.pending[headerSize:], &result); err != nil || !result.Valid() || result.ID != w.state.ID || result.Domain != w.state.Domain || hex.EncodeToString(w.pending[len(responseMagic):len(responseMagic)+16]) != w.state.ID {
				// A bounded frame has a known payload boundary. Losing optional
				// evidence must not discard an independently verified TLS response.
				w.state.Set(Result{Error: "invalid_evidence"})
			} else {
				w.state.Set(result)
			}
			w.pending = nil
			w.done = true
		}
		if len(data) > 0 {
			if err := w.Writer.WriteMultiBuffer(buf.MergeBytes(nil, data)); err != nil {
				return err
			}
		}
	}
	return nil
}
func (w *collectorWriter) Close() error { w.state.Abort(); return common.Close(w.Writer) }
func (w *collectorWriter) Interrupt()   { w.state.Abort(); common.Interrupt(w.Writer) }
