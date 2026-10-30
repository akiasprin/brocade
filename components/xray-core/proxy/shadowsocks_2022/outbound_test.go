package shadowsocks_2022

import (
	"context"
	"encoding/base64"
	"io"
	stdnet "net"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type failedPayloadReader struct{}

func (failedPayloadReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	return nil, io.ErrUnexpectedEOF
}
func (failedPayloadReader) ReadMultiBufferTimeout(time.Duration) (buf.MultiBuffer, error) {
	return nil, io.ErrUnexpectedEOF
}

type closeTrackedConn struct {
	net.Conn
	closed atomic.Bool
}

func (c *closeTrackedConn) Close() error { c.closed.Store(true); return c.Conn.Close() }

type fixedDialer struct {
	internet.Dialer
	conn stat.Connection
}

func (d fixedDialer) Dial(context.Context, net.Destination) (stat.Connection, error) {
	return d.conn, nil
}

func TestOutboundClosesServerOnEarlyPayloadError(t *testing.T) {
	instance, err := core.New(&core.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer instance.Close()
	ctx := context.WithValue(context.Background(), core.XrayKey(1), instance)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: net.TCPDestination(net.LocalHostIP, 443)}})
	outbound, err := NewClient(ctx, &ClientConfig{Address: net.NewIPOrDomain(net.LocalHostIP), Port: 443, Method: "2022-blake3-aes-128-gcm", Key: base64.StdEncoding.EncodeToString(make([]byte, 16))})
	if err != nil {
		t.Fatal(err)
	}
	local, remote := stdnet.Pipe()
	defer remote.Close()
	defer local.Close()
	conn := &closeTrackedConn{Conn: local}
	err = outbound.Process(ctx, &transport.Link{Reader: failedPayloadReader{}}, fixedDialer{conn: conn})
	if err == nil {
		t.Fatal("expected initial payload error")
	}
	if !conn.closed.Load() {
		t.Fatal("server connection leaked after payload failure")
	}
}
