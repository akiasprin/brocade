package anytls

import (
	"context"
	"crypto/sha256"
	"testing"

	"github.com/xtls/xray-core/common/protocol"
)

func testMemoryUser(email, password string) *protocol.MemoryUser {
	return &protocol.MemoryUser{Email: email, Account: &MemoryAccount{Password: password}}
}

func TestServerUserManagement(t *testing.T) {
	server := &Server{
		users:        make(map[[32]byte]*protocol.MemoryUser),
		usersByEmail: make(map[string]*protocol.MemoryUser),
	}
	ctx := context.Background()

	if err := server.AddUser(ctx, nil); err == nil {
		t.Fatal("AddUser(nil) unexpectedly succeeded")
	}
	if err := server.AddUser(ctx, testMemoryUser("alice", "first")); err != nil {
		t.Fatal(err)
	}
	if server.GetUsersCount(ctx) != 1 || server.GetUser(ctx, "alice") == nil {
		t.Fatal("user was not added")
	}
	users := server.GetUsers(ctx)
	if len(users) != 1 || users[0].Email != "alice" {
		t.Fatalf("GetUsers = %v, want alice", users)
	}

	if err := server.AddUser(ctx, testMemoryUser("alice", "second")); err != nil {
		t.Fatal(err)
	}
	if server.GetUsersCount(ctx) != 1 {
		t.Fatalf("user count after replacement = %d, want 1", server.GetUsersCount(ctx))
	}
	if _, ok := server.users[sha256ForTest("first")]; ok {
		t.Fatal("old password hash remained after replacement")
	}
	if _, ok := server.users[sha256ForTest("second")]; !ok {
		t.Fatal("new password hash was not registered")
	}

	if err := server.RemoveUser(ctx, ""); err == nil {
		t.Fatal("RemoveUser(empty email) unexpectedly succeeded")
	}
	if err := server.RemoveUser(ctx, "alice"); err != nil {
		t.Fatal(err)
	}
	if server.GetUsersCount(ctx) != 0 || server.GetUser(ctx, "alice") != nil {
		t.Fatal("user was not removed")
	}
	if server.GetUser(ctx, "") != nil {
		t.Fatal("empty email unexpectedly returned a user")
	}
}

func TestServerUserReplacementAndRemovalCloseActiveSessions(t *testing.T) {
	server := &Server{
		users:          make(map[[32]byte]*protocol.MemoryUser),
		usersByEmail:   make(map[string]*protocol.MemoryUser),
		activeSessions: make(map[*protocol.MemoryUser]map[*session]struct{}),
	}
	ctx := context.Background()
	alice := testMemoryUser("alice", "first")
	bob := testMemoryUser("bob", "second")
	if err := server.AddUser(ctx, alice); err != nil {
		t.Fatal(err)
	}
	if err := server.AddUser(ctx, bob); err != nil {
		t.Fatal(err)
	}

	aliceSession := &session{streams: make(map[uint32]*stream)}
	bobSession := &session{streams: make(map[uint32]*stream)}
	if got := server.registerSession(sha256ForTest("first"), aliceSession); got != alice {
		t.Fatal("alice session was not registered")
	}
	if got := server.registerSession(sha256ForTest("second"), bobSession); got != bob {
		t.Fatal("bob session was not registered")
	}

	if err := server.AddUser(ctx, testMemoryUser("alice", "replacement")); err != nil {
		t.Fatal(err)
	}
	if !aliceSession.isClosed() {
		t.Fatal("replacing alice did not close her active session")
	}
	if bobSession.isClosed() {
		t.Fatal("replacing alice closed bob's active session")
	}

	if err := server.RemoveUser(ctx, "bob"); err != nil {
		t.Fatal(err)
	}
	if !bobSession.isClosed() {
		t.Fatal("removing bob did not close his active session")
	}
}

func TestMemoryAccountValueSemantics(t *testing.T) {
	account := &MemoryAccount{Password: "secret"}
	if !account.Equals(&MemoryAccount{Password: "secret"}) {
		t.Fatal("equal memory accounts were not equal")
	}
	if account.Equals(&MemoryAccount{Password: "other"}) {
		t.Fatal("different memory accounts were equal")
	}
	if account.Equals(nil) {
		t.Fatal("memory account unexpectedly equals nil")
	}
	protoAccount, ok := account.ToProto().(*Account)
	if !ok || protoAccount.Password != "secret" {
		t.Fatalf("ToProto = %v, want secret account", protoAccount)
	}
	if account.ToProto() == account.ToProto() {
		t.Fatal("ToProto reused mutable account instance")
	}
}

func sha256ForTest(password string) [32]byte {
	return sha256.Sum256([]byte(password))
}
