package tcp

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"net"
	"os"
	"sync"

	"github.com/xtls/xray-core/transport/internet"
	"google.golang.org/protobuf/proto"
)

// ListenHandoffControl includes a direct API/metrics server in the same admission barrier as
// inbounds. Its connections are management traffic and do not participate in business drain.
// With no manifest, keep the normal direct-server listener behavior.
func ListenHandoffControl(ctx context.Context, address string) (net.Listener, error) {
	if os.Getenv("XRAY_TCP_HANDOFF_MANIFEST") == "" {
		return net.Listen("tcp", address)
	}
	addr, err := net.ResolveTCPAddr("tcp", address)
	if err != nil {
		return nil, err
	}
	listener, gate, err := openHandoffListener(addr.String(), false, nil, func() (net.Listener, error) {
		return internet.ListenSystem(ctx, addr, nil)
	})
	if err != nil || gate == nil {
		return listener, err
	}
	return &handoffStreamListener{Listener: listener, gate: gate}, nil
}

// ListenHandoffTransport preserves raw TCP connections for an HTTP transport.
// Its serving goroutine must call FinishHandoffConnection after the connection
// and its requests have finished; control listeners do not take this pin.
func ListenHandoffTransport(ctx context.Context, address string, config *internet.SocketConfig) (net.Listener, error) {
	addr, err := net.ResolveTCPAddr("tcp", address)
	if err != nil {
		return nil, err
	}
	listener, gate, err := openHandoffListener(addr.String(), false, config, func() (net.Listener, error) {
		return internet.ListenSystem(ctx, addr, config)
	})
	if err != nil || gate == nil {
		return listener, err
	}
	return &handoffStreamListener{Listener: listener, gate: gate, business: true}, nil
}

type handoffStreamListener struct {
	net.Listener
	gate     handoffGate
	business bool
}

func (l *handoffStreamListener) Accept() (net.Conn, error) {
	for {
		if !l.gate.beforeAccept() {
			return nil, net.ErrClosed
		}
		conn, err := l.Listener.Accept()
		l.gate.afterAccept(err == nil && l.business)
		if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
			continue
		}
		if err == nil && l.business {
			if err := l.gate.prepareConnection(conn); err != nil {
				_ = conn.Close()
				l.gate.finishConnection()
				return nil, err
			}
			l.gate.trackConnection(conn)
		}
		return conn, err
	}
}

func (l *handoffStreamListener) Close() error {
	l.gate.close()
	return l.Listener.Close()
}

// handoffGate controls admission to explicitly selected TCP listeners and counts the accepted
// connections still owned by their processing goroutines. It deliberately does not wrap a
// connection: preserving the concrete TCP type keeps Vision splice, kTLS and half-close intact.
// This selected-listener count alone is still not proof that the whole process can be retired.
type handoffGate interface {
	beforeAccept() bool
	afterAccept(bool)
	prepareConnection(net.Conn) error
	trackConnection(net.Conn)
	finishConnection()
	close()
}

var handoffConnections sync.Map

// FinishHandoffConnection releases the admission-time lifetime pin for a TCP connection.
// proxyman calls it from the actual connection-processing goroutine, after protocol handling and
// both copy directions have ended. Keeping the token out of a net.Conn wrapper preserves the
// concrete *net.TCPConn path needed by Vision raw splice and kTLS.
func FinishHandoffConnection(conn net.Conn) {
	if gate, ok := handoffConnections.LoadAndDelete(conn); ok {
		gate.(handoffGate).finishConnection()
	}
}

// The effective protobuf is the shared source of truth for socket settings.
// Nil and an empty message are equivalent; do not rebuild a subset of fields
// here because a newly introduced socket option must participate automatically.
func handoffSocketConfigDigest(config *internet.SocketConfig) (string, error) {
	if config == nil {
		config = &internet.SocketConfig{}
	}
	encoded, err := (proto.MarshalOptions{Deterministic: true}).Marshal(config)
	if err != nil {
		return "", err
	}
	digest := sha256.Sum256(encoded)
	return hex.EncodeToString(digest[:]), nil
}
