package anytls

import (
	"context"
	"crypto/sha256"
	"net"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	sessionctx "github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/sourceaccess"
	"github.com/xtls/xray-core/common/sourceaccess/testutil"
)

func TestSourceBanClosesAuthenticatedAnyTLSWithoutRevokingOtherSources(t *testing.T) {
	ctx, update := testutil.Open(t)
	server := newAuthTestServer("correct-password")
	for _, user := range server.users {
		user.Email = "alice@tenant#i1"
		server.usersByEmail = map[string]*protocol.MemoryUser{user.Email: user}
	}
	open := func(ip string) (net.Conn, <-chan error) {
		t.Helper()
		client, carrier := net.Pipe()
		done := make(chan error, 1)
		t.Cleanup(func() { _ = client.Close(); _ = carrier.Close() })
		inbound := sessionctx.ContextWithInbound(ctx, &sessionctx.Inbound{})
		go func() {
			done <- server.Process(inbound, xnet.Network_TCP, testutil.PeerConn{Conn: carrier, IP: ip}, &testDispatcher{})
		}()
		hash := sha256.Sum256([]byte("correct-password"))
		_ = client.SetWriteDeadline(time.Now().Add(time.Second))
		if _, err := client.Write(append(append([]byte{}, hash[:]...), 0, 0)); err != nil {
			t.Fatal(err)
		}
		return client, done
	}
	_, closed := open("192.0.2.1")
	_, other := open("192.0.2.2")
	update(sourceaccess.Policy{Revision: 2, Users: []sourceaccess.UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"192.0.2.1"}}}})
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("banned AnyTLS carrier survived")
	}
	select {
	case err := <-other:
		t.Fatalf("another source closed: %v", err)
	default:
	}
	if err := server.RemoveUser(context.Background(), "alice@tenant#i1"); err != nil {
		t.Fatal(err)
	}
}
