package anytls

import (
	"context"
	"crypto/sha256"
	"sync"
	"testing"

	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/core"
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
	if err := server.AddUser(ctx, testMemoryUser("", "password")); err == nil {
		t.Fatal("AddUser with empty email unexpectedly succeeded")
	}
	if err := server.AddUser(ctx, testMemoryUser("empty-password", "")); err == nil {
		t.Fatal("AddUser with empty password unexpectedly succeeded")
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

	if err := server.AddUser(ctx, testMemoryUser("alice", "second")); err == nil {
		t.Fatal("duplicate email unexpectedly replaced the user")
	}
	if err := server.AddUser(ctx, testMemoryUser("bob", "first")); err == nil {
		t.Fatal("duplicate password unexpectedly replaced the authentication index")
	}
	if server.GetUsersCount(ctx) != 1 {
		t.Fatalf("user count after rejected duplicates = %d, want 1", server.GetUsersCount(ctx))
	}
	if server.users[sha256ForTest("first")] != server.GetUser(ctx, "alice") {
		t.Fatal("rejected duplicate changed the original user indexes")
	}
	if _, ok := server.users[sha256ForTest("second")]; ok {
		t.Fatal("rejected duplicate email left its password hash behind")
	}
	if server.GetUser(ctx, "bob") != nil {
		t.Fatal("rejected duplicate password left its email behind")
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

func TestServerRejectedReplacementPreservesSessionAndRemovalClosesIt(t *testing.T) {
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

	if err := server.AddUser(ctx, testMemoryUser("alice", "replacement")); err == nil {
		t.Fatal("duplicate email unexpectedly replaced alice")
	}
	if aliceSession.isClosed() {
		t.Fatal("rejected replacement closed alice's active session")
	}
	if bobSession.isClosed() {
		t.Fatal("rejected replacement closed bob's active session")
	}
	if err := server.RemoveUser(ctx, "alice"); err != nil {
		t.Fatal(err)
	}
	if !aliceSession.isClosed() {
		t.Fatal("removing alice did not close her active session")
	}
	if bobSession.isClosed() {
		t.Fatal("removing alice closed bob's active session")
	}

	if err := server.RemoveUser(ctx, "bob"); err != nil {
		t.Fatal(err)
	}
	if !bobSession.isClosed() {
		t.Fatal("removing bob did not close his active session")
	}
}

func TestServerConcurrentAddMaintainsOneToOneIndexes(t *testing.T) {
	tests := []struct {
		name string
		user func(int) *protocol.MemoryUser
	}{
		{
			name: "same-email",
			user: func(i int) *protocol.MemoryUser {
				return testMemoryUser("shared", string(rune('a'+i)))
			},
		},
		{
			name: "same-password",
			user: func(i int) *protocol.MemoryUser {
				return testMemoryUser(string(rune('a'+i)), "shared")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := &Server{}
			const attempts = 16
			start := make(chan struct{})
			results := make(chan error, attempts)
			var wg sync.WaitGroup
			for i := range attempts {
				wg.Add(1)
				go func() {
					defer wg.Done()
					<-start
					results <- server.AddUser(context.Background(), tt.user(i))
				}()
			}
			close(start)
			wg.Wait()
			close(results)

			successes := 0
			for err := range results {
				if err == nil {
					successes++
				}
			}
			if successes != 1 {
				t.Fatalf("successful adds = %d, want 1", successes)
			}
			if len(server.users) != 1 || len(server.usersByEmail) != 1 {
				t.Fatalf("index sizes = password:%d email:%d, want 1:1", len(server.users), len(server.usersByEmail))
			}
			for email, user := range server.usersByEmail {
				account := user.Account.(*MemoryAccount)
				if user.Email != email || server.users[sha256ForTest(account.Password)] != user {
					t.Fatal("email and password indexes refer to different users")
				}
			}
		})
	}
}

func TestNewServerRejectsInvalidStaticUsers(t *testing.T) {
	instance, err := core.New(&core.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer instance.Close()
	ctx := context.WithValue(context.Background(), core.XrayKey(1), instance)

	tests := []struct {
		name  string
		users []*protocol.MemoryUser
	}{
		{
			name:  "duplicate-email",
			users: []*protocol.MemoryUser{testMemoryUser("alice", "first"), testMemoryUser("alice", "second")},
		},
		{
			name:  "duplicate-password",
			users: []*protocol.MemoryUser{testMemoryUser("alice", "shared"), testMemoryUser("bob", "shared")},
		},
		{
			name:  "empty-email",
			users: []*protocol.MemoryUser{testMemoryUser("", "password")},
		},
		{
			name:  "empty-password",
			users: []*protocol.MemoryUser{testMemoryUser("alice", "")},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			users := make([]*protocol.User, 0, len(tt.users))
			for _, user := range tt.users {
				users = append(users, protocol.ToProtoUser(user))
			}
			if _, err := NewServer(ctx, &ServerConfig{Users: users}); err == nil {
				t.Fatal("NewServer unexpectedly accepted invalid static users")
			}
		})
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

func TestAccountRejectsEmptyPassword(t *testing.T) {
	for _, account := range []*Account{nil, {}} {
		if _, err := account.AsAccount(); err == nil {
			t.Fatalf("AsAccount(%v) unexpectedly accepted an empty password", account)
		}
	}
	if _, err := protocol.ToProtoUser(testMemoryUser("client", "")).ToMemoryUser(); err == nil {
		t.Fatal("protobuf account decoding unexpectedly accepted an empty password")
	}

	account, err := (&Account{Password: "secret"}).AsAccount()
	if err != nil {
		t.Fatal(err)
	}
	if memory, ok := account.(*MemoryAccount); !ok || memory.Password != "secret" {
		t.Fatalf("AsAccount = %#v, want MemoryAccount with the configured password", account)
	}
}

func sha256ForTest(password string) [32]byte {
	return sha256.Sum256([]byte(password))
}
