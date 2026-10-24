package mtproto

import (
	"net"
	"strings"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/sourceaccess"
	"github.com/xtls/xray-core/common/sourceaccess/testutil"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/transport/internet/stat"
)

func TestSourceBanRejectsAuthenticatedMTProxyBeforeDirectoryOrUpstreamAccess(t *testing.T) {
	ctx, update := testutil.Open(t)
	update(sourceaccess.Policy{Revision: 2, Users: []sourceaccess.UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"192.0.2.1"}}}})
	secret := [16]byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	user := &protocol.MemoryUser{Email: "alice@tenant#i1", Account: &MemoryAccount{Secret: secret}}
	server := &Server{active: make(map[*protocol.MemoryUser]map[stat.Connection]struct{}), users: []*protocol.MemoryUser{user}, usersByEmail: map[string]*protocol.MemoryUser{user.Email: user}, policyManager: policy.DefaultManager{}, replaySeen: make(map[[32]byte]time.Time)}
	client, carrier := net.Pipe()
	defer client.Close()
	defer carrier.Close()
	done := make(chan error, 1)
	ctx = session.ContextWithInbound(ctx, &session.Inbound{})
	go func() {
		done <- server.Process(ctx, xnet.Network_TCP, testutil.PeerConn{Conn: carrier, IP: "192.0.2.1"}, nil)
	}()
	header, _, _ := testClientHeader(secret, [4]byte{0xee, 0xee, 0xee, 0xee}, 2)
	_ = client.SetWriteDeadline(time.Now().Add(time.Second))
	if _, err := client.Write(header); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if err == nil || !strings.Contains(err.Error(), "source IP access denied") {
			t.Fatalf("wrong denial: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("banned MTProxy authentication remained active")
	}
}
