//go:build linux

package tcp

import (
	"context"
	gotls "crypto/tls"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/apernet/quic-go"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/finalmask"
	"github.com/xtls/xray-core/transport/internet/finalmask/salamander"
	"github.com/xtls/xray-core/transport/internet/hysteria"
	"github.com/xtls/xray-core/transport/internet/stat"
	xtls "github.com/xtls/xray-core/transport/internet/tls"
	"github.com/xtls/xray-core/transport/internet/udp"
	"golang.org/x/sys/unix"
)

func TestWholeGenerationSalamanderListeners(t *testing.T) {
	for _, transport := range []string{"udp", "hysteria"} {
		t.Run(transport, func(t *testing.T) {
			reserved, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
			if err != nil {
				t.Fatal(err)
			}
			addr := reserved.LocalAddr().(*net.UDPAddr)
			reserved.Close()
			dir := t.TempDir()
			if err := os.Chmod(dir, 0700); err != nil {
				t.Fatal(err)
			}
			control := handoffTestControl(t, handoffManifest{
				Version: 2, Scope: "whole-generation", GenerationID: "masked-startup-0001",
				ControlPath: filepath.Join(dir, "control.sock"),
				Listeners:   []handoffEndpoint{{Address: addr.String(), BindAddress: addr.String(), Network: "udp", Role: "public", Protocol: unix.IPPROTO_UDP}},
			})
			defaultBinds := 0
			mask := finalmask.NewFinalMask(nil, []finalmask.UDPMask{&salamander.Config{Password: "loopback-mask-test"}}, nil, nil,
				func(context.Context, xnet.Destination) (net.PacketConn, net.Addr, error) {
					conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
					return conn, addr, err
				},
				func(ctx context.Context, addr net.Addr) (net.PacketConn, error) {
					defaultBinds++
					return internet.ListenSystemPacket(ctx, addr, nil)
				})
			settings := &internet.MemoryStreamConfig{FinalMask: mask}
			var hub *udp.Hub
			if transport == "udp" {
				hub, err = udp.ListenUDP(context.Background(), xnet.LocalHostIP, xnet.Port(addr.Port), settings, udp.HubCapacity(1))
				if err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() { hub.Close() })
				if hub.Handoff() == nil {
					t.Fatal("masked UDP listener lost generation admission")
				}
			} else {
				certificate, _ := cert.MustGenerate(nil)
				certificatePEM, keyPEM := certificate.ToPEM()
				settings.SecuritySettings = &xtls.Config{Certificate: []*xtls.Certificate{{Certificate: certificatePEM, Key: keyPEM}}}
				settings.ProtocolSettings = &hysteria.Config{Auth: "loopback-auth-test"}
				settings.QuicParams = &internet.QuicParams{Congestion: "reno", DisablePathMtuDiscovery: true}
				listener, listenErr := hysteria.Listen(context.Background(), xnet.LocalHostIP, xnet.Port(addr.Port), settings, func(conn stat.Connection) { conn.Close() })
				if listenErr != nil {
					t.Fatal(listenErr)
				}
				t.Cleanup(func() { listener.Close() })
			}
			if defaultBinds != 0 {
				t.Fatal("generation listener bypassed its raw socket controller")
			}
			if err := control.markReady(); err != nil {
				t.Fatal(err)
			}
			if err := control.seal(); err != nil {
				t.Fatal(err)
			}
			status := control.status()
			if !status.Ready || len(status.Listeners) != 1 || status.Listeners[0].SocketInode == 0 || status.Listeners[0].SocketCookie == 0 {
				t.Fatalf("masked socket identity was not verified: %+v", status)
			}
			if err := control.resume(); err != nil {
				t.Fatal(err)
			}
			client, err := mask.DialUDP(context.Background(), xnet.DestinationFromAddr(addr))
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { client.Close() })
			if err := client.SetDeadline(time.Now().Add(3 * time.Second)); err != nil {
				t.Fatal(err)
			}
			if hub != nil {
				payload := "masked UDP round trip"
				if _, err := client.Write([]byte(payload)); err != nil {
					t.Fatal(err)
				}
				select {
				case packet := <-hub.Receive():
					if packet == nil {
						t.Fatal("masked UDP listener stopped")
					}
					defer packet.Payload.Release()
					if string(packet.Payload.Bytes()) != payload {
						t.Fatalf("mask did not decode payload: %q", packet.Payload.Bytes())
					}
					if _, err := hub.WriteTo(packet.Payload.Bytes(), packet.Source); err != nil {
						t.Fatal(err)
					}
				case <-time.After(3 * time.Second):
					t.Fatal("masked UDP receive timed out")
				}
				buffer := make([]byte, 128)
				n, err := client.Read(buffer)
				if err != nil || string(buffer[:n]) != payload {
					t.Fatalf("masked UDP response: %q, %v", buffer[:n], err)
				}
			} else {
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
				defer cancel()
				// The certificate is generated solely for this loopback fixture.
				conn, err := quic.Dial(ctx, client.(*xnet.PacketConnWrapper).PacketConn, addr, &gotls.Config{InsecureSkipVerify: true, NextProtos: []string{"h3"}}, &quic.Config{EnableDatagrams: true})
				if err != nil {
					t.Fatal(err)
				}
				defer conn.CloseWithError(0, "test complete")
			}
			if err := control.pause(context.Background()); err != nil {
				t.Fatal(err)
			}
			if err := control.dropUDP(); err != nil {
				t.Fatal(err)
			}
		})
	}
}
