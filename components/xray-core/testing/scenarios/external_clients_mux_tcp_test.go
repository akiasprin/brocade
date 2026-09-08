package scenarios

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
	"time"

	"golang.org/x/net/proxy"
	"golang.org/x/sync/errgroup"
)

// Use actual HTTP uploads/downloads through each client's SOCKS CONNECT path.
// Exact body comparison detects truncation and cross-stream contamination.
func TestExternalClientsMuxTCPDataPlane(t *testing.T) {
	if testing.Short() {
		t.Skip("real external-client process test")
	}
	for _, mode := range []externalMuxMode{{name: "shared-mux", xudpConcurrency: 0}, {name: "dedicated-xudp", xudpConcurrency: 1}} {
		for _, protocol := range []string{"AnyTLS", "VLESS", "Hysteria2"} {
			for _, kind := range []externalClientKind{externalSingBox, externalMihomo} {
				t.Run(mode.name+"/"+protocol+"/"+string(kind), func(t *testing.T) {
					if externalClientBinary(kind) == "" {
						t.Skip("external client binary is not configured")
					}
					topology := startExternalUDPTopology(t, protocol, mode)
					client := startExternalClient(t, kind, topology.ingress)
					server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
						defer r.Body.Close()
						body, err := io.ReadAll(io.LimitReader(r.Body, 2<<20))
						if err != nil {
							http.Error(w, err.Error(), 400)
							return
						}
						_, _ = w.Write(body)
					}))
					t.Cleanup(server.Close)
					dialer, err := proxy.SOCKS5("tcp", fmt.Sprintf("127.0.0.1:%d", client.socksPort), nil, &net.Dialer{Timeout: 5 * time.Second})
					if err != nil {
						t.Fatal(err)
					}
					transport := &http.Transport{DialContext: dialer.(proxy.ContextDialer).DialContext, DisableKeepAlives: true}
					t.Cleanup(transport.CloseIdleConnections)
					httpClient := &http.Client{Transport: transport, Timeout: 10 * time.Second}
					request := func(id, size int) error {
						body := bytes.Repeat([]byte(fmt.Sprintf("stream-%06d/", id)), size/14+1)[:size]
						req, err := http.NewRequestWithContext(context.Background(), "POST", server.URL, bytes.NewReader(body))
						if err != nil {
							return err
						}
						response, err := httpClient.Do(req)
						if err != nil {
							return err
						}
						defer response.Body.Close()
						got, err := io.ReadAll(response.Body)
						if err != nil {
							return err
						}
						if response.StatusCode != 200 || !bytes.Equal(got, body) {
							return fmt.Errorf("stream %d: status=%d received=%d expected=%d", id, response.StatusCode, len(got), len(body))
						}
						return nil
					}
					check := func(err error) {
						t.Helper()
						if err != nil {
							log, _ := os.ReadFile(client.logPath)
							t.Fatalf("TCP E2E: %v\n%s", err, log)
						}
					}
					// More fresh TCP streams than the configured eight-request worker limit.
					for i := 0; i < 40; i++ {
						check(request(i, 4096))
					}
					var group errgroup.Group
					for i := 0; i < 12; i++ {
						id := i
						group.Go(func() error { return request(100+id, 1<<20) })
					}
					// UDP and TCP run concurrently on the same real client and ingress.
					dns := startDNSUDPServer(t, [4]byte{192, 0, 2, 30})
					group.Go(func() error { return runConcurrentDNSWorkload(client.socksPort, []*dnsUDPServer{dns}, 4, 16) })
					check(group.Wait())
					topology.hopProxy.setDelay(15 * time.Millisecond)
					check(request(200, 64<<10))
					topology.hopProxy.closeConnections(true)
					check(retryDNSWorkload(8*time.Second, func() error { return request(201, 64<<10) }))
					t.Log("passed: 40 fresh streams, 12 concurrent 1 MiB uploads/downloads, mixed UDP/TCP, delayed hop, new request after RST")
				})
			}
		}
	}
}
