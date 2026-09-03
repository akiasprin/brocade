package anytls

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"io"
	"net"
	"net/http"
	"strings"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	sessionctx "github.com/xtls/xray-core/common/session"
)

func TestMasquerade404Response(t *testing.T) {
	m, err := newMasquerade(&Masquerade{Type: "404"})
	if err != nil {
		t.Fatal(err)
	}

	var wire bytes.Buffer
	if err := m.write(&wire); err != nil {
		t.Fatal(err)
	}
	response, err := http.ReadResponse(bufio.NewReader(bytes.NewReader(wire.Bytes())), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", response.StatusCode, http.StatusNotFound)
	}
	if string(body) != "404 page not found\n" {
		t.Fatalf("body = %q", body)
	}
	if response.Header.Get("X-Content-Type-Options") != "nosniff" {
		t.Fatalf("missing nosniff header: %v", response.Header)
	}
}

func TestMasqueradeDefaultsTo404(t *testing.T) {
	for _, config := range []*Masquerade{nil, &Masquerade{}} {
		m, err := newMasquerade(config)
		if err != nil {
			t.Fatal(err)
		}
		var wire bytes.Buffer
		if err := m.write(&wire); err != nil {
			t.Fatal(err)
		}
		response, err := http.ReadResponse(bufio.NewReader(bytes.NewReader(wire.Bytes())), nil)
		if err != nil {
			t.Fatal(err)
		}
		if response.StatusCode != http.StatusNotFound {
			response.Body.Close()
			t.Fatalf("status = %d, want %d", response.StatusCode, http.StatusNotFound)
		}
		response.Body.Close()
	}
}

func TestMasqueradeStringResponse(t *testing.T) {
	m, err := newMasquerade(&Masquerade{
		Type:       "string",
		Content:    "Forbidden",
		Headers:    map[string]string{"Content-Type": "text/plain", "X-Brocade": "anytls"},
		StatusCode: http.StatusForbidden,
	})
	if err != nil {
		t.Fatal(err)
	}

	var wire bytes.Buffer
	if err := m.write(&wire); err != nil {
		t.Fatal(err)
	}
	response, err := http.ReadResponse(bufio.NewReader(bytes.NewReader(wire.Bytes())), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusForbidden || string(body) != "Forbidden" {
		t.Fatalf("response = %d %q", response.StatusCode, body)
	}
	if response.Header.Get("X-Brocade") != "anytls" {
		t.Fatalf("headers = %v", response.Header)
	}
}

func TestMasqueradeRejectsInvalidConfiguration(t *testing.T) {
	tests := []struct {
		name   string
		config *Masquerade
	}{
		{name: "unknown type", config: &Masquerade{Type: "proxy"}},
		{name: "invalid status", config: &Masquerade{Type: "string", StatusCode: 4030}},
		{name: "invalid header name", config: &Masquerade{Type: "string", Headers: map[string]string{"Bad Header": "value"}}},
		{name: "header injection", config: &Masquerade{Type: "string", Headers: map[string]string{"X-Test": "a\r\nb"}}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if err := ValidateMasquerade(tt.config); err == nil {
				t.Fatal("configuration unexpectedly accepted")
			}
		})
	}
}

func TestInboundInvalidAuthenticationWritesMasquerade(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	server := newAuthTestServer("correct-password")
	ctx := sessionctx.ContextWithInbound(context.Background(), &sessionctx.Inbound{})
	errCh := make(chan error, 1)
	go func() {
		errCh <- server.Process(ctx, xnet.Network_TCP, serverConn, &testDispatcher{})
	}()

	responseCh := make(chan []byte, 1)
	go func() {
		_ = clientConn.SetReadDeadline(time.Now().Add(2 * time.Second))
		response, _ := io.ReadAll(clientConn)
		responseCh <- response
	}()

	wrongHash := sha256.Sum256([]byte("wrong-password"))
	auth := append([]byte{}, wrongHash[:]...)
	auth = append(auth, 0, 0)
	if _, err := clientConn.Write(auth); err != nil {
		t.Fatal(err)
	}
	if err := <-errCh; err == nil || !strings.Contains(err.Error(), "invalid user") {
		t.Fatalf("process error = %v", err)
	}
	_ = serverConn.Close()
	response := <-responseCh
	if !bytes.HasPrefix(response, []byte("HTTP/1.1 404 Not Found\r\n")) {
		t.Fatalf("response = %q", response)
	}
	_ = clientConn.Close()
}

func TestInboundAuthenticatedProtocolErrorDoesNotWriteMasquerade(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	server := newAuthTestServer("correct-password")
	server.masquerade, _ = newMasquerade(&Masquerade{Type: "404"})
	ctx := sessionctx.ContextWithInbound(context.Background(), &sessionctx.Inbound{})
	errCh := make(chan error, 1)
	go func() {
		errCh <- server.Process(ctx, xnet.Network_TCP, serverConn, &testDispatcher{})
	}()

	hash := sha256.Sum256([]byte("correct-password"))
	auth := append([]byte{}, hash[:]...)
	auth = append(auth, 0, 0)
	if _, err := clientConn.Write(append(auth, marshalTestFrame(0xff, 0, nil)...)); err != nil {
		t.Fatal(err)
	}
	if err := <-errCh; err == nil || !strings.Contains(err.Error(), "unknown cmd") {
		t.Fatalf("process error = %v", err)
	}

	_ = serverConn.Close()
	_ = clientConn.SetReadDeadline(time.Now().Add(2 * time.Second))
	response, err := io.ReadAll(clientConn)
	if err != nil && !errors.Is(err, net.ErrClosed) {
		t.Fatal(err)
	}
	if len(response) != 0 {
		t.Fatalf("authenticated protocol error wrote fallback response: %q", response)
	}
	_ = clientConn.Close()
}
