package internet

import (
	"context"
	"errors"

	"github.com/apernet/quic-go"
	"github.com/apernet/quic-go/http3"
)

// WithQUICHandoff applies the generation admission barrier without stopping the
// packet socket that existing QUIC connections still need while paused.
func WithQUICHandoff(listener http3.QUICListener, gate PacketHandoffGate) http3.QUICListener {
	if gate == nil {
		return listener
	}
	return &quicHandoffListener{QUICListener: listener, gate: gate}
}

type quicHandoffListener struct {
	http3.QUICListener
	gate PacketHandoffGate
}

func (l *quicHandoffListener) Accept(ctx context.Context) (*quic.Conn, error) {
	for {
		acceptCtx, proceed := l.gate.BeforeAccept(ctx)
		if !proceed {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			return nil, quic.ErrServerClosed
		}
		conn, err := l.QUICListener.Accept(acceptCtx)
		l.gate.AfterAccept(err == nil)
		// A generation pause only interrupts this accept. Returning cancellation
		// to http3.Server would terminate its serving loop instead of resuming.
		if errors.Is(err, context.Canceled) && ctx.Err() == nil {
			continue
		}
		if err == nil {
			context.AfterFunc(conn.Context(), l.gate.FinishConnection)
		}
		return conn, err
	}
}

func (l *quicHandoffListener) Close() error {
	l.gate.Close()
	return l.QUICListener.Close()
}
