package tcp

import (
	"context"
	gotls "crypto/tls"
	"strings"
	"time"

	goreality "github.com/xtls/reality"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/reality"
	"github.com/xtls/xray-core/transport/internet/stat"
	"github.com/xtls/xray-core/transport/internet/tls"
)

// Listener is an internet.Listener that listens for TCP connections.
type Listener struct {
	listener      net.Listener
	tlsConfig     *gotls.Config
	realityConfig *goreality.Config
	realityGuard  *reality.FallbackProtection
	authConfig    internet.ConnectionAuthenticator
	config        *Config
	addConn       internet.ConnHandler
	handoff       handoffGate
}

// ListenTCP creates a new Listener based on configurations.
func ListenTCP(ctx context.Context, address net.Address, port net.Port, streamSettings *internet.MemoryStreamConfig, handler internet.ConnHandler) (internet.Listener, error) {
	l := &Listener{
		addConn: handler,
	}
	tcpSettings := streamSettings.ProtocolSettings.(*Config)
	l.config = tcpSettings
	if l.config != nil {
		if streamSettings.SocketSettings == nil {
			streamSettings.SocketSettings = &internet.SocketConfig{}
		}
		streamSettings.SocketSettings.AcceptProxyProtocol = l.config.AcceptProxyProtocol || streamSettings.SocketSettings.AcceptProxyProtocol
	}
	var listener net.Listener
	var err error
	var addr net.Addr
	if port == net.Port(0) { // unix
		addr = &net.UnixAddr{Name: address.Domain(), Net: "unix"}
	} else { // tcp
		addr = &net.TCPAddr{IP: address.IP(), Port: int(port)}
	}
	if port == 0 || streamSettings.FinalMask != nil {
		if streamSettings.FinalMask != nil {
			listener, err = streamSettings.FinalMask.Listen(ctx, addr)
		} else {
			listener, err = internet.ListenSystem(ctx, addr, streamSettings.SocketSettings)
		}
	} else {
		unsupported := tcpSettings.HeaderSettings != nil || (streamSettings.SocketSettings != nil && streamSettings.SocketSettings.AcceptProxyProtocol)
		listener, l.handoff, err = openHandoffListener(addr.String(), unsupported, streamSettings.SocketSettings, func() (net.Listener, error) { return internet.ListenSystem(ctx, addr, streamSettings.SocketSettings) })
	}
	if err != nil {
		return nil, errors.New("failed to listen ", addr.Network(), " on ", address, ":", port).Base(err)
	}
	errors.LogInfo(ctx, "listening ", addr.Network(), " on ", address, ":", port)

	if streamSettings.SocketSettings != nil && streamSettings.SocketSettings.AcceptProxyProtocol {
		errors.LogWarning(ctx, "accepting PROXY protocol")
	}

	l.listener = listener

	if config := tls.ConfigFromStreamSettings(streamSettings); config != nil {
		l.tlsConfig = config.GetTLSConfig()
	}
	if config := reality.ConfigFromStreamSettings(streamSettings); config != nil {
		l.realityConfig = config.GetREALITYConfig()
		l.realityGuard = reality.DefaultFallbackProtection()
		go goreality.DetectPostHandshakeRecordsLens(l.realityConfig)
	}

	if tcpSettings.HeaderSettings != nil {
		headerConfig, err := tcpSettings.HeaderSettings.GetInstance()
		if err != nil {
			return nil, errors.New("invalid header settings").Base(err)
		}
		auth, err := internet.CreateConnectionAuthenticator(headerConfig)
		if err != nil {
			return nil, errors.New("invalid header settings.").Base(err)
		}
		l.authConfig = auth
	}

	go l.keepAccepting()
	return l, nil
}

func (v *Listener) keepAccepting() {
	for {
		if v.handoff != nil && !v.handoff.beforeAccept() {
			return
		}
		conn, err := v.listener.Accept()
		if v.handoff != nil {
			v.handoff.afterAccept(err == nil)
			if netErr, ok := err.(net.Error); ok && netErr.Timeout() {
				continue
			}
		}
		if err != nil {
			errStr := err.Error()
			if strings.Contains(errStr, "closed") {
				break
			}
			errors.LogWarningInner(context.Background(), err, "failed to accepted raw connections")
			if strings.Contains(errStr, "too many") {
				time.Sleep(time.Millisecond * 500)
			}
			continue
		}
		if v.handoff != nil {
			if err := v.handoff.prepareConnection(conn); err != nil {
				v.handoff.finishConnection()
				conn.Close()
				errors.LogWarningInner(context.Background(), err, "failed to restore inherited TCP connection options")
				continue
			}
		}

		go func() {
			if v.tlsConfig != nil {
				conn = tls.Server(conn, v.tlsConfig)
			} else if v.realityConfig != nil {
				if conn, err = reality.Server(conn, v.realityConfig, v.realityGuard); err != nil {
					if v.handoff != nil {
						v.handoff.finishConnection()
					}
					errors.LogInfo(context.Background(), err.Error())
					return
				}
			}
			if v.authConfig != nil {
				conn = v.authConfig.Server(conn)
			}
			if v.handoff != nil {
				v.handoff.trackConnection(conn)
			}
			v.addConn(stat.Connection(conn))
		}()
	}
}

// Addr implements internet.Listener.Addr.
func (v *Listener) Addr() net.Addr {
	return v.listener.Addr()
}

// Close implements internet.Listener.Close.
func (v *Listener) Close() error {
	if v.handoff != nil {
		v.handoff.close()
	}
	return v.listener.Close()
}

func init() {
	common.Must(internet.RegisterTransportListener(protocolName, ListenTCP))
}
