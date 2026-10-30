package internet_test

import (
	"context"
	"net"
	"sync/atomic"
	"testing"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/masque"
	"github.com/xtls/xray-core/transport/internet/splithttp"
	"github.com/xtls/xray-core/transport/internet/stat"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
)

func TestHTTP3ListenersRegisterHandoff(t *testing.T) {
	for _, network := range []string{"xhttp", "masque"} {
		t.Run(network, func(t *testing.T) {
			var calls atomic.Int32
			release, err := internet.RegisterPacketHandoffController(func(address string, _ *internet.SocketConfig, bind func(string) (net.PacketConn, error)) (net.PacketConn, internet.PacketHandoffGate, error) {
				calls.Add(1)
				c, err := bind(address)
				return c, nil, err
			})
			if err != nil {
				t.Fatal(err)
			}
			defer release()
			control, _, err := internet.ListenPacketWithHandoff(context.Background(), &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)}, nil)
			if err != nil {
				t.Fatal(err)
			}
			_ = control.Close()
			if calls.Load() != 1 {
				t.Fatal("positive control did not reach the registered controller")
			}
			calls.Store(0)
			certificate, _ := cert.MustGenerate(nil, cert.DNSNames("localhost"))
			settings := &internet.MemoryStreamConfig{ProtocolName: network, SecuritySettings: &xraytls.Config{Certificate: []*xraytls.Certificate{xraytls.ParseCertificate(certificate)}, NextProtocol: []string{"h3"}}}
			handler := func(c stat.Connection) { _ = c.Close() }
			var listener internet.Listener
			if network == "xhttp" {
				settings.ProtocolSettings = &splithttp.Config{Path: "/review", Mode: "stream-one"}
				listener, err = splithttp.ListenXH(context.Background(), xnet.LocalHostIP, 0, settings, handler)
			} else {
				settings.ProtocolSettings = &masque.Config{Path: "/.well-known/masque/ip/*/*/"}
				listener, err = masque.Listen(context.Background(), xnet.LocalHostIP, 0, settings, handler)
			}
			if err != nil {
				t.Fatal(err)
			}
			defer listener.Close()
			if got := calls.Load(); got != 1 {
				t.Fatalf("%s HTTP/3 UDP listener bypassed handoff controller: registrations=%d, want 1; native UDP positive control registered", network, got)
			}
		})
	}
}
