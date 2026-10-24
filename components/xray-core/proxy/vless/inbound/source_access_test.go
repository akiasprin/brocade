package inbound

import (
	"bytes"
	"context"
	"net"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/sourceaccess"
	"github.com/xtls/xray-core/common/sourceaccess/testutil"
	"github.com/xtls/xray-core/common/uuid"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/proxy/vless"
	"github.com/xtls/xray-core/proxy/vless/encoding"
)

func TestSourceBanCancelsVlessTCPAndMuxWithoutRevokingCredentials(t *testing.T) {
	ctx, update := testutil.Open(t)
	validator := new(vless.MemoryValidator)
	users := make(map[string]*protocol.MemoryUser)
	for _, name := range []string{"alice", "bob"} {
		user := &protocol.MemoryUser{Email: name + "@tenant#i1", Account: &vless.MemoryAccount{ID: protocol.NewID(uuid.New())}}
		if err := validator.Add(user); err != nil {
			t.Fatal(err)
		}
		users[name] = user
	}
	h := &Handler{validator: validator, policyManager: policy.DefaultManager{}}
	dispatch := &quotaSessionDispatcher{started: make(chan context.Context, 4)}
	open := func(name string, command protocol.RequestCommand) <-chan error {
		t.Helper()
		client, carrier := net.Pipe()
		done := make(chan error, 1)
		t.Cleanup(func() { _ = client.Close(); _ = carrier.Close() })
		inbound := session.ContextWithInbound(ctx, &session.Inbound{})
		go func() {
			done <- h.Process(inbound, xnet.Network_TCP, testutil.PeerConn{Conn: carrier, IP: "192.0.2.1"}, dispatch)
		}()
		var header bytes.Buffer
		if err := encoding.EncodeRequestHeader(&header, &protocol.RequestHeader{Version: encoding.Version, User: users[name], Command: command, Address: xnet.DomainAddress("example.test"), Port: 443}, &encoding.Addons{}); err != nil {
			t.Fatal(err)
		}
		_ = client.SetWriteDeadline(time.Now().Add(time.Second))
		if _, err := client.Write(header.Bytes()); err != nil {
			t.Fatal(err)
		}
		select {
		case <-dispatch.started:
		case err := <-done:
			t.Fatalf("unexpected authentication failure: %v", err)
		case <-time.After(time.Second):
			t.Fatal("authentication timed out")
		}
		return done
	}
	tcp := open("alice", protocol.RequestCommandTCP)
	mux := open("alice", protocol.RequestCommandMux)
	bob := open("bob", protocol.RequestCommandTCP)
	update(sourceaccess.Policy{Revision: 2, Users: []sourceaccess.UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"192.0.2.1"}}}})
	for _, done := range []<-chan error{tcp, mux} {
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Fatal("banned VLESS carrier survived")
		}
	}
	select {
	case err := <-bob:
		t.Fatalf("another user was closed: %v", err)
	default:
	}
	update(sourceaccess.Policy{Revision: 3})
	_ = open("alice", protocol.RequestCommandTCP)
	for _, user := range users {
		_ = h.RemoveUser(context.Background(), user.Email)
	}
}
