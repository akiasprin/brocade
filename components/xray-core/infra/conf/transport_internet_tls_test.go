package conf_test

import (
	"context"
	"encoding/json"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	. "github.com/xtls/xray-core/infra/conf"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
)

func TestTLSCertificateReloadInterval(t *testing.T) {
	var config TLSCertConfig
	if err := json.Unmarshal([]byte(`{"reloadInterval":5}`), &config); err != nil {
		t.Fatal(err)
	}
	if config.ReloadInterval != 5 {
		t.Fatalf("reloadInterval = %d, want 5", config.ReloadInterval)
	}
}

func TestTLSExplicitAllowInsecure(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	defer server.Close()
	server.Config.ErrorLog = log.New(io.Discard, "", 0)

	for _, test := range []struct {
		name        string
		json        string
		wantSuccess bool
	}{
		{"default-verifies", `{}`, false},
		{"explicit-false-verifies", `{"allowInsecure":false}`, false},
		{"explicit-true-bypasses", `{"allowInsecure":true}`, true},
		{"explicit-pin-still-verifies", `{"allowInsecure":true,"pinnedPeerCertSha256":"` + strings.Repeat("00", 32) + `"}`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			config := TLSConfig{ServerName: "mismatched.example", Fingerprint: "chrome"}
			if err := json.Unmarshal([]byte(test.json), &config); err != nil {
				t.Fatal(err)
			}
			built, err := config.Build()
			if err != nil {
				t.Fatal(err)
			}
			tlsConfig := built.(*xraytls.Config).GetTLSConfig()
			for _, fingerprint := range []string{"", "chrome"} {
				t.Run("fingerprint="+fingerprint, func(t *testing.T) {
					raw, err := net.DialTimeout("tcp", server.Listener.Addr().String(), 2*time.Second)
					if err != nil {
						t.Fatal(err)
					}
					defer raw.Close()
					ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
					defer cancel()
					var conn interface{ HandshakeContext(context.Context) error }
					if fingerprint == "" {
						conn = xraytls.Client(raw, tlsConfig).(*xraytls.Conn)
					} else {
						conn = xraytls.UClient(raw, tlsConfig, xraytls.GetFingerprint(fingerprint)).(*xraytls.UConn)
					}
					err = conn.HandshakeContext(ctx)
					if (err == nil) != test.wantSuccess {
						t.Fatalf("TLS handshake error = %v, want success = %v", err, test.wantSuccess)
					}
				})
			}
		})
	}
}
