//go:build linux

package internet_test

import (
	"context"
	gotls "crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/apernet/quic-go/http3"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/masque"
	"github.com/xtls/xray-core/transport/internet/splithttp"
	"github.com/xtls/xray-core/transport/internet/stat"
	"github.com/xtls/xray-core/transport/internet/tcp"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/net/http2"
)

func TestHTTP3HandoffPauseResumeAndLifetime(t *testing.T) {
	for _, network := range []string{"xhttp", "masque", "masque-mixed"} {
		t.Run(network, func(t *testing.T) {
			dir, err := os.MkdirTemp("", "h3gate-")
			if err != nil {
				t.Fatal(err)
			}
			defer os.RemoveAll(dir)
			control := filepath.Join(dir, "control.sock")
			generation := "http3-generation-0001"
			reservation, err := net.ListenPacket("udp4", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			addr := reservation.LocalAddr().(*net.UDPAddr)
			_ = reservation.Close()
			manifest := map[string]any{"version": 2, "scope": "whole-generation", "generation_id": generation, "control_path": control, "listeners": []any{map[string]any{"address": addr.String(), "network": "udp", "role": "public", "protocol": 17}}}
			if network == "masque-mixed" {
				manifest["listeners"] = append(manifest["listeners"].([]any), map[string]any{"address": addr.String(), "network": "tcp", "role": "public", "protocol": 0})
			}
			data, err := json.Marshal(manifest)
			if err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(dir, "manifest.json")
			if err := os.WriteFile(path, data, 0o600); err != nil {
				t.Fatal(err)
			}
			t.Setenv("XRAY_TCP_HANDOFF_MANIFEST", path)
			if err := tcp.InitHandoff(); err != nil {
				t.Fatal(err)
			}
			defer tcp.CloseHandoff()
			var epoch uint64
			rpc := func(operation string) struct {
				Ready, Paused bool
				PublicActive  uint64 `json:"public_active"`
				Accepted      uint64
			} {
				t.Helper()
				conn, err := net.DialTimeout("unixpacket", control, time.Second)
				if err != nil {
					t.Fatal(err)
				}
				defer conn.Close()
				_ = conn.SetDeadline(time.Now().Add(3 * time.Second))
				if operation != "status" {
					epoch++
				}
				request, _ := json.Marshal(map[string]any{"operation": operation, "generation_id": generation, "epoch": epoch, "intent": fmt.Sprintf("http3-operation-%020d", epoch)})
				if _, err := conn.Write(request); err != nil {
					t.Fatal(err)
				}
				buffer := make([]byte, 65536)
				n, err := conn.Read(buffer)
				if err != nil {
					t.Fatal(err)
				}
				var response struct {
					Error  string
					Status struct {
						Ready, Paused bool
						PublicActive  uint64 `json:"public_active"`
						Accepted      uint64
					}
				}
				if err := json.Unmarshal(buffer[:n], &response); err != nil || response.Error != "" {
					t.Fatalf("%s: err=%v response=%s", operation, err, buffer[:n])
				}
				return response.Status
			}
			certificate, _ := cert.MustGenerate(nil, cert.DNSNames("localhost"))
			settings := &internet.MemoryStreamConfig{ProtocolName: network, SecuritySettings: &xraytls.Config{Certificate: []*xraytls.Certificate{xraytls.ParseCertificate(certificate)}, NextProtocol: []string{"h3"}}}
			if network == "masque-mixed" {
				settings.ProtocolName = "masque"
				settings.SecuritySettings.(*xraytls.Config).NextProtocol = []string{"h2", "h3"}
			}
			handler := func(c stat.Connection) { _ = c.Close() }
			var listener internet.Listener
			if network == "xhttp" {
				settings.ProtocolSettings = &splithttp.Config{Path: "/review", Mode: "stream-one"}
				listener, err = splithttp.ListenXH(context.Background(), xnet.LocalHostIP, xnet.Port(addr.Port), settings, handler)
			} else {
				settings.ProtocolSettings = &masque.Config{Path: masque.DefaultPath}
				listener, err = masque.Listen(context.Background(), xnet.LocalHostIP, xnet.Port(addr.Port), settings, handler)
			}
			if err != nil {
				t.Fatal(err)
			}
			defer listener.Close()
			if err := tcp.MarkHandoffReady(); err != nil {
				t.Fatal(err)
			}
			if !rpc("seal").Ready || !rpc("resume").Ready {
				t.Fatal("HTTP/3 socket did not enter the sealed generation")
			}
			makeClient := func() *http3.Transport {
				c := &http3.Transport{TLSClientConfig: &gotls.Config{InsecureSkipVerify: true}}
				t.Cleanup(func() { _ = c.Close() })
				return c
			}
			request := func(c http.RoundTripper) error {
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
				defer cancel()
				r, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://"+addr.String()+"/not-review", nil)
				if err != nil {
					return err
				}
				response, err := c.RoundTrip(r)
				if err != nil {
					return err
				}
				defer response.Body.Close()
				_, _ = io.Copy(io.Discard, response.Body)
				if response.StatusCode != http.StatusNotFound {
					return fmt.Errorf("status %d", response.StatusCode)
				}
				return nil
			}
			if network == "masque-mixed" {
				h2 := &http2.Transport{TLSClientConfig: &gotls.Config{InsecureSkipVerify: true}}
				t.Cleanup(h2.CloseIdleConnections)
				if err := request(h2); err != nil {
					t.Fatal(err)
				}
				if status := rpc("status"); status.PublicActive != 1 {
					t.Fatalf("mixed MASQUE lost its H2 lifetime pin: %+v", status)
				}
				h2.CloseIdleConnections()
				deadline := time.Now().Add(2 * time.Second)
				for rpc("status").PublicActive != 0 {
					if time.Now().After(deadline) {
						t.Fatal("mixed MASQUE retained a closed H2 lifetime pin")
					}
					time.Sleep(10 * time.Millisecond)
				}
			}
			baselineAccepted := rpc("status").Accepted
			old := makeClient()
			if err := request(old); err != nil {
				t.Fatal(err)
			}
			if status := rpc("pause"); !status.Paused || status.PublicActive != 1 {
				t.Fatalf("pause lost the established QUIC lifetime: %+v", status)
			}
			if err := request(old); err != nil {
				t.Fatalf("pause interrupted the existing carrier: %v", err)
			}
			fresh := makeClient()
			done := make(chan error, 1)
			go func() { done <- request(fresh) }()
			select {
			case err := <-done:
				t.Fatalf("new HTTP/3 carrier completed during pause: %v", err)
			case <-time.After(100 * time.Millisecond):
			}
			rpc("resume")
			select {
			case err := <-done:
				if err != nil {
					t.Fatal(err)
				}
			case <-time.After(4 * time.Second):
				t.Fatal("HTTP/3 serving loop did not resume")
			}
			if status := rpc("status"); status.PublicActive != 2 || status.Accepted != baselineAccepted+2 {
				t.Fatalf("QUIC lifetime count is not per carrier: %+v", status)
			}
			_ = old.Close()
			_ = fresh.Close()
			deadline := time.Now().Add(2 * time.Second)
			for rpc("status").PublicActive != 0 {
				if time.Now().After(deadline) {
					t.Fatal("closed QUIC connections retained active lifetime pins")
				}
				time.Sleep(10 * time.Millisecond)
			}
			rpc("pause")
			closed := make(chan error, 1)
			go func() { closed <- listener.Close() }()
			select {
			case <-closed:
			case <-time.After(2 * time.Second):
				t.Fatal("closing a paused HTTP/3 listener blocked")
			}
			t.Log("sealed real UDP socket; pause preserves old carrier and blocks a new one; resume serves it; close releases both lifetime pins and paused accept")
		})
	}
}
