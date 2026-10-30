package tls_test

import (
	"context"
	"crypto"
	gotls "crypto/tls"
	"crypto/x509"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/protocol/tls/cert"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/crypto/ocsp"
)

// This exercises the real OCSP watcher and crypto/tls certificate consumer.
// Run explicitly with -race while production fixes are deferred by the user.
func TestReviewOCSPRefreshDuringTLSHandshakes(t *testing.T) {
	if os.Getenv("BROCADE_RUN_OCSP_REVIEW") != "1" {
		t.Skip("explicit read-only OCSP concurrency audit")
	}
	issuer, _ := cert.MustGenerate(nil, cert.Authority(true), cert.KeyUsage(x509.KeyUsageCertSign|x509.KeyUsageDigitalSignature))
	issuerCert, err := x509.ParseCertificate(issuer.Certificate)
	if err != nil {
		t.Fatal(err)
	}
	key, err := x509.ParsePKCS8PrivateKey(issuer.PrivateKey)
	if err != nil {
		t.Fatal(err)
	}
	var requests, handshakes, stapled atomic.Int32
	var observedMu sync.Mutex
	observed := make(map[string]struct{})
	var leafCert *x509.Certificate
	responder := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, io.LimitReader(r.Body, 4096))
		revision := requests.Add(1)
		response, err := ocsp.CreateResponse(issuerCert, issuerCert, ocsp.Response{SerialNumber: leafCert.SerialNumber, Status: ocsp.Good, ThisUpdate: time.Now().Add(time.Duration(revision) * time.Second), NextUpdate: time.Now().Add(time.Hour)}, key.(crypto.Signer))
		if err != nil {
			http.Error(w, err.Error(), 500)
			return
		}
		w.Header().Set("Content-Type", "application/ocsp-response")
		_, _ = w.Write(response)
	}))
	defer responder.Close()
	leaf, _ := cert.MustGenerate(issuer, cert.DNSNames("localhost"), func(c *x509.Certificate) { c.OCSPServer = []string{responder.URL} })
	leafCert, err = x509.ParseCertificate(leaf.Certificate)
	if err != nil {
		t.Fatal(err)
	}
	entry := xraytls.ParseCertificate(leaf)
	issuerPEM, _ := issuer.ToPEM()
	entry.Certificate = append(entry.Certificate, issuerPEM...)
	entry.OcspStapling = 1
	config := (&xraytls.Config{Certificate: []*xraytls.Certificate{entry}}).GetTLSConfig()
	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	listener := gotls.NewListener(raw, config)
	var servers sync.WaitGroup
	servers.Add(1)
	go func() {
		defer servers.Done()
		for {
			c, err := listener.Accept()
			if err != nil {
				return
			}
			servers.Add(1)
			go func() {
				defer servers.Done()
				defer c.Close()
				_ = c.SetDeadline(time.Now().Add(time.Second))
				_ = c.(*gotls.Conn).Handshake()
			}()
		}
	}()
	pool := x509.NewCertPool()
	pool.AddCert(issuerCert)
	ctx, cancel := context.WithTimeout(context.Background(), 2500*time.Millisecond)
	defer cancel()
	var clients sync.WaitGroup
	errors := make(chan error, 4)
	for range 4 {
		clients.Add(1)
		go func() {
			defer clients.Done()
			for ctx.Err() == nil {
				c, err := gotls.DialWithDialer(&net.Dialer{Timeout: time.Second}, "tcp", listener.Addr().String(), &gotls.Config{RootCAs: pool, ServerName: "localhost"})
				if err != nil {
					errors <- err
					return
				}
				staple := c.ConnectionState().OCSPResponse
				if len(staple) > 0 {
					if _, err := ocsp.ParseResponse(staple, issuerCert); err != nil {
						_ = c.Close()
						errors <- err
						return
					}
					stapled.Add(1)
					observedMu.Lock()
					observed[string(staple)] = struct{}{}
					observedMu.Unlock()
				}
				handshakes.Add(1)
				_ = c.Close()
			}
		}()
	}
	clients.Wait()
	_ = listener.Close()
	servers.Wait()
	close(errors)
	for err := range errors {
		t.Error(err)
	}
	if requests.Load() < 2 || handshakes.Load() < 20 || stapled.Load() < 20 || len(observed) < 2 {
		t.Fatalf("fixture did not serve changing valid staples during handshakes: requests=%d handshakes=%d stapled=%d responses=%d", requests.Load(), handshakes.Load(), stapled.Load(), len(observed))
	}
	t.Logf("valid OCSP refreshes=%d, successful real TLS handshakes=%d, stapled=%d, different responses=%d", requests.Load(), handshakes.Load(), stapled.Load(), len(observed))
}
