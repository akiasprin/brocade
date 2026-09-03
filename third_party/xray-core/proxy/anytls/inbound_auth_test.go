package anytls

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"net"
	"strings"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	sessionctx "github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/policy"
)

func newAuthTestServer(password string) *Server {
	sum := sha256.Sum256([]byte(password))
	user := &protocol.MemoryUser{Email: "auth-test", Level: 3, Account: &MemoryAccount{Password: password}}
	return &Server{
		policyManager: policy.DefaultManager{},
		users:         map[[32]byte]*protocol.MemoryUser{sum: user},
		usersByEmail:  map[string]*protocol.MemoryUser{user.Email: user},
	}
}

func runAuthProcess(t *testing.T, auth []byte, closeAfterAuth bool) error {
	t.Helper()
	clientConn, serverConn := net.Pipe()
	server := newAuthTestServer("correct-password")
	ctx := sessionctx.ContextWithInbound(context.Background(), &sessionctx.Inbound{})
	errCh := make(chan error, 1)
	go func() {
		errCh <- server.Process(ctx, xnet.Network_TCP, serverConn, &testDispatcher{})
	}()
	if _, err := clientConn.Write(auth); err != nil {
		clientConn.Close()
		t.Fatalf("write auth: %v", err)
	}
	if closeAfterAuth {
		_ = clientConn.Close()
	} else {
		_ = clientConn.SetReadDeadline(time.Now().Add(2 * time.Second))
	}
	select {
	case err := <-errCh:
		_ = clientConn.Close()
		return err
	case <-time.After(2 * time.Second):
		_ = clientConn.Close()
		t.Fatal("server auth process did not return")
		return nil
	}
}

func TestInboundAuthenticationBoundaries(t *testing.T) {
	correctHash := sha256.Sum256([]byte("correct-password"))
	tests := []struct {
		name     string
		auth     []byte
		wantText string
	}{
		{
			name:     "truncated-header",
			auth:     []byte("short"),
			wantText: "read auth",
		},
		{
			name: "wrong-password",
			auth: func() []byte {
				wrongHash := sha256.Sum256([]byte("wrong-password"))
				return append(append([]byte{}, wrongHash[:]...), 0, 0)
			}(),
			wantText: "invalid user",
		},
		{
			name: "truncated-padding",
			auth: func() []byte {
				data := append([]byte{}, correctHash[:]...)
				data = append(data, 0, 4)
				return append(data, 1, 2)
			}(),
			wantText: "read padding0",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := runAuthProcess(t, tt.auth, true)
			if err == nil || !strings.Contains(err.Error(), tt.wantText) {
				t.Fatalf("error = %v, want text %q", err, tt.wantText)
			}
		})
	}
}

func TestInboundAuthenticationAcceptsValidHeader(t *testing.T) {
	hash := sha256.Sum256([]byte("correct-password"))
	auth := append([]byte{}, hash[:]...)
	var paddingLen [2]byte
	binary.BigEndian.PutUint16(paddingLen[:], 3)
	auth = append(auth, paddingLen[:]...)
	auth = append(auth, 7, 8, 9)
	if err := runAuthProcess(t, auth, true); err == nil || !strings.Contains(err.Error(), "EOF") {
		t.Fatalf("valid auth result = %v, want session EOF", err)
	}
}
