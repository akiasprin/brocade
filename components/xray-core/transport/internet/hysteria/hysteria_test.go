package hysteria

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"net"
	"runtime"
	"sync/atomic"
	"testing"
	"time"

	"github.com/apernet/quic-go"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/transport/internet"
)

type failingQUICListener struct {
	err        error
	closeCount atomic.Int32
}

func (l *failingQUICListener) Accept(context.Context) (*quic.Conn, error) {
	return nil, l.err
}

func (*failingQUICListener) Addr() net.Addr {
	return &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)}
}

func (l *failingQUICListener) Close() error {
	l.closeCount.Add(1)
	return nil
}

type closeTrackingPacketConn struct {
	closeCount atomic.Int32
}

func (*closeTrackingPacketConn) ReadFrom([]byte) (int, net.Addr, error) {
	return 0, nil, net.ErrClosed
}

func (*closeTrackingPacketConn) WriteTo([]byte, net.Addr) (int, error) {
	return 0, net.ErrClosed
}

func (c *closeTrackingPacketConn) Close() error {
	c.closeCount.Add(1)
	return nil
}

func (*closeTrackingPacketConn) LocalAddr() net.Addr {
	return &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)}
}

func (*closeTrackingPacketConn) SetDeadline(time.Time) error      { return nil }
func (*closeTrackingPacketConn) SetReadDeadline(time.Time) error  { return nil }
func (*closeTrackingPacketConn) SetWriteDeadline(time.Time) error { return nil }

type closeTrackingHandoffGate struct {
	closed atomic.Bool
}

func (*closeTrackingHandoffGate) BeforeAccept(ctx context.Context) (context.Context, bool) {
	return ctx, true
}

func (*closeTrackingHandoffGate) AfterAccept(bool)      {}
func (*closeTrackingHandoffGate) StartConnection() bool { return true }
func (*closeTrackingHandoffGate) FinishConnection()     {}
func (g *closeTrackingHandoffGate) Close()              { g.closed.Store(true) }

var _ internet.PacketHandoffGate = (*closeTrackingHandoffGate)(nil)

func TestListenerClosesPacketSocketAfterAcceptLoopFailure(t *testing.T) {
	failure := errors.New("terminal packet read failure")
	listener := &failingQUICListener{err: failure}
	packetConn := &closeTrackingPacketConn{}
	handoff := &closeTrackingHandoffGate{}
	l := &Listener{listener: listener, pktConn: packetConn, handoff: handoff}

	l.keepAccepting()
	if !handoff.closed.Load() {
		t.Fatal("terminal accept failure left handoff listener advertised as open")
	}
	if listener.closeCount.Load() != 1 || packetConn.closeCount.Load() != 1 {
		t.Fatalf("terminal accept failure did not close listener resources exactly once: listener=%d packet=%d", listener.closeCount.Load(), packetConn.closeCount.Load())
	}
	if err := l.Close(); err != nil {
		t.Fatal(err)
	}
	if listener.closeCount.Load() != 1 || packetConn.closeCount.Load() != 1 {
		t.Fatal("listener close was not idempotent")
	}
}

func TestDatagram(t *testing.T) {
	run := func() (addr net.Addr, recv chan int64, cancel func()) {
		cert, _ := cert.MustGenerate(nil)
		Certificate := [][]byte{cert.Certificate}
		PrivateKey := common.Must2(x509.ParsePKCS8PrivateKey(cert.PrivateKey))

		tlsConf := &tls.Config{
			Certificates: []tls.Certificate{
				{
					Certificate: Certificate,
					PrivateKey:  PrivateKey,
				},
			},
			NextProtos: []string{"h3"},
		}

		quicConf := &quic.Config{
			InitialStreamReceiveWindow:     8388608,
			MaxStreamReceiveWindow:         8388608,
			InitialConnectionReceiveWindow: 8388608 * 5 / 2,
			MaxConnectionReceiveWindow:     8388608 * 5 / 2,
			MaxIdleTimeout:                 30 * time.Second,
			MaxIncomingStreams:             1024,
			DisablePathMTUDiscovery:        runtime.GOOS != "linux" && runtime.GOOS != "windows" && runtime.GOOS != "darwin",
			EnableDatagrams:                true,
			MaxDatagramFrameSize:           MaxDatagramFrameSize,
			AssumePeerMaxDatagramFrameSize: MaxDatagramFrameSize,
			DisablePathManager:             true,
		}

		pktConn := common.Must2(net.ListenPacket("udp", "127.0.0.1:0"))
		tr := &quic.Transport{Conn: pktConn}
		l := common.Must2(tr.Listen(tlsConf, quicConf))

		recv = make(chan int64)
		ctx, cancel := context.WithCancel(context.Background())

		go func() {
			defer pktConn.Close()
			defer tr.Close()
			defer l.Close()
			defer close(recv)

			var buf [1500]byte
			for {
				conn, err := l.Accept(ctx)
				if err != nil {
					if !errors.Is(err, context.Canceled) {
						t.Error(err)
					}
					break
				}
				err = conn.SendDatagram(buf[:])
				var qErr *quic.DatagramTooLargeError
				if !errors.As(err, &qErr) {
					t.Error(err)
				}
				recv <- qErr.MaxDatagramPayloadSize
				defer conn.CloseWithError(0, "")
			}
		}()

		return l.Addr(), recv, cancel
	}

	addr, recv, cancel := run()

	t.Run("With ChromeParrot", func(t *testing.T) {
		tlsConf := &tls.Config{
			InsecureSkipVerify: true,
		}

		quicConf := &quic.Config{
			InitialStreamReceiveWindow:     8388608,
			MaxStreamReceiveWindow:         8388608,
			InitialConnectionReceiveWindow: 8388608 * 5 / 2,
			MaxConnectionReceiveWindow:     8388608 * 5 / 2,
			MaxIdleTimeout:                 30 * time.Second,
			KeepAlivePeriod:                10 * time.Second,
			DisablePathMTUDiscovery:        runtime.GOOS != "linux" && runtime.GOOS != "windows" && runtime.GOOS != "darwin",
			ChromeParrot:                   true,
			EnableDatagrams:                true,
			MaxDatagramFrameSize:           MaxDatagramFrameSize,
			OmitMaxDatagramFrameSize:       true,
			DisablePathManager:             true,
		}

		pktConn := common.Must2(net.ListenPacket("udp", "127.0.0.1:0"))
		tr := &quic.Transport{Conn: pktConn, ConnectionIDGenerator: quic.ZeroLengthConnectionIDGenerator{}}
		conn := common.Must2(tr.DialEarly(context.Background(), addr, tlsConf, quicConf))

		defer pktConn.Close()
		defer tr.Close()
		defer conn.CloseWithError(0, "")

		var buf [1500]byte
		err := conn.SendDatagram(buf[:])
		var qErr *quic.DatagramTooLargeError
		if !errors.As(err, &qErr) || qErr.MaxDatagramPayloadSize != 1197 {
			t.Error(err)
		}
		if server := <-recv; server != 1243 {
			t.Error(server)
		}
	})

	t.Run("Without ChromeParrot", func(t *testing.T) {
		tlsConf := &tls.Config{
			InsecureSkipVerify: true,
			NextProtos:         []string{"h3"},
		}

		quicConf := &quic.Config{
			InitialStreamReceiveWindow:     8388608,
			MaxStreamReceiveWindow:         8388608,
			InitialConnectionReceiveWindow: 8388608 * 5 / 2,
			MaxConnectionReceiveWindow:     8388608 * 5 / 2,
			MaxIdleTimeout:                 30 * time.Second,
			KeepAlivePeriod:                10 * time.Second,
			DisablePathMTUDiscovery:        runtime.GOOS != "linux" && runtime.GOOS != "windows" && runtime.GOOS != "darwin",
			ChromeParrot:                   false,
			EnableDatagrams:                true,
			MaxDatagramFrameSize:           MaxDatagramFrameSize,
			OmitMaxDatagramFrameSize:       true,
			DisablePathManager:             true,
		}

		pktConn := common.Must2(net.ListenPacket("udp", "127.0.0.1:0"))
		tr := &quic.Transport{Conn: pktConn}
		conn := common.Must2(tr.DialEarly(context.Background(), addr, tlsConf, quicConf))

		defer pktConn.Close()
		defer tr.Close()
		defer conn.CloseWithError(0, "")

		var buf [1500]byte
		err := conn.SendDatagram(buf[:])
		var qErr *quic.DatagramTooLargeError
		if !errors.As(err, &qErr) || qErr.MaxDatagramPayloadSize != 1197 {
			t.Error(err)
		}
		if server := <-recv; server != 1197 {
			t.Error(server)
		}
	})

	cancel()
}
