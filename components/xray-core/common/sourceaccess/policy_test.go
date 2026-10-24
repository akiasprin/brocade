package sourceaccess

import (
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func manager(t *testing.T) *Manager {
	t.Helper()
	path := filepath.Join(t.TempDir(), "policy.json")
	data, _ := json.Marshal(Policy{Revision: 1})
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	m, err := New(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = m.Close() })
	return m
}
func peer(ip string) net.Addr { return &net.TCPAddr{IP: net.ParseIP(ip), Port: 12345} }

func TestUserAndNodeBansCloseOnlyMatchingCarriersAndUnbanAllowsNewOnes(t *testing.T) {
	m := manager(t)
	ctx := Context(context.Background(), m)
	var a, b, other atomic.Int32
	for _, session := range []struct {
		user, ip string
		closed   *atomic.Int32
	}{
		{"alice@tenant#i1", "192.0.2.1", &a}, {"alice@tenant#i2", "192.0.2.2", &other}, {"bob@tenant#i1", "192.0.2.1", &b},
	} {
		release, err := Register(ctx, session.user, peer(session.ip), func() { session.closed.Add(1) })
		if err != nil {
			t.Fatal(err)
		}
		defer release()
	}
	policy := Policy{Revision: 2, Users: []UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"::ffff:192.0.2.1"}}}}
	if err := m.apply(policy); err != nil {
		t.Fatal(err)
	}
	if a.Load() != 1 || b.Load() != 0 || other.Load() != 0 {
		t.Fatal("user ban closed unrelated carrier")
	}
	if _, err := Register(ctx, "alice@tenant#new-ingress", peer("192.0.2.1"), func() {}); err == nil {
		t.Fatal("new grant bypassed a user ban")
	}
	policy.Revision = 3
	policy.NodeIPs = []string{"192.0.2.1"}
	if err := m.apply(policy); err != nil {
		t.Fatal(err)
	}
	if b.Load() != 1 || other.Load() != 0 {
		t.Fatal("node ban did not isolate source")
	}
	policy.Revision = 4
	policy.Users = nil
	if err := m.apply(policy); err != nil {
		t.Fatal(err)
	}
	if _, err := Register(ctx, "alice@tenant#i1", peer("192.0.2.1"), func() {}); err == nil {
		t.Fatal("user unban bypassed node ban")
	}
	policy.Revision = 5
	policy.NodeIPs = nil
	if err := m.apply(policy); err != nil {
		t.Fatal(err)
	}
	release, err := Register(ctx, "alice@tenant#i1", peer("192.0.2.1"), func() {})
	if err != nil {
		t.Fatal(err)
	}
	release()
	if a.Load() != 1 || b.Load() != 1 {
		t.Fatal("unban revived or closed an old carrier twice")
	}
}

func TestAuthenticationRacingWithBanCannotAttachAfterRevocation(t *testing.T) {
	m := manager(t)
	ctx := Context(context.Background(), m)
	var wg sync.WaitGroup
	var admitted, closed atomic.Int32
	for range 100 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			release, err := Register(ctx, "alice@tenant#i1", peer("2001:db8::1"), func() { closed.Add(1) })
			if err == nil {
				admitted.Add(1)
				_ = release
			}
		}()
	}
	if err := m.apply(Policy{Revision: 2, Users: []UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"2001:db8::1"}}}}); err != nil {
		t.Fatal(err)
	}
	wg.Wait()
	if admitted.Load() != closed.Load() {
		t.Fatalf("surviving banned sessions: admitted=%d closed=%d", admitted.Load(), closed.Load())
	}
}

func TestLiveFileReloadClosesIdleSessionAndRetainsDenialsOnCorruption(t *testing.T) {
	m := manager(t)
	ctx := Context(context.Background(), m)
	closed := make(chan struct{}, 1)
	release, err := Register(ctx, "alice@tenant#i1", peer("192.0.2.1"), func() { closed <- struct{}{} })
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	if err := m.Start(); err != nil {
		t.Fatal(err)
	}
	data, _ := json.Marshal(Policy{Revision: 2, NodeIPs: []string{"192.0.2.1"}})
	tmp := m.path + ".new"
	if err := os.WriteFile(tmp, data, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, m.path); err != nil {
		t.Fatal(err)
	}
	select {
	case <-closed:
	case <-time.After(3 * time.Second):
		t.Fatal("idle carrier survived live ban")
	}
	if err := os.WriteFile(m.path, []byte("bad"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := m.reload(); err == nil {
		t.Fatal("invalid snapshot accepted")
	}
	if _, err := Register(ctx, "alice@tenant#i1", peer("192.0.2.1"), func() {}); err == nil {
		t.Fatal("invalid snapshot released ban")
	}
	if err := m.apply(Policy{Revision: 1}); err == nil {
		t.Fatal("regressing snapshot accepted")
	}
}

func TestPolicyRejectsMalformedAndOversizedSnapshots(t *testing.T) {
	m := manager(t)
	for _, policy := range []Policy{
		{Revision: -1}, {Revision: 2, NodeIPs: []string{"192.0.2.0/24"}},
		{Revision: 2, NodeIPs: []string{"fe80::1%eth0"}},
		{Revision: 2, Users: []UserBlock{{Tenant: "tenant#other", User: "alice", IPs: []string{"192.0.2.1"}}}},
	} {
		if m.apply(policy) == nil {
			t.Fatalf("accepted malformed policy: %#v", policy)
		}
	}
	policy := Policy{Revision: 2, NodeIPs: make([]string, MaxEntries+1)}
	for i := range policy.NodeIPs {
		policy.NodeIPs[i] = "192.0.2.1"
	}
	if m.apply(policy) == nil {
		t.Fatal("accepted oversized policy")
	}
}

func TestLivePeerMigrationIsRecheckedWithoutAPolicyChange(t *testing.T) {
	m := manager(t)
	ctx := Context(context.Background(), m)
	if err := m.apply(Policy{Revision: 2, NodeIPs: []string{"192.0.2.1"}}); err != nil {
		t.Fatal(err)
	}
	var current atomic.Pointer[net.UDPAddr]
	current.Store(&net.UDPAddr{IP: net.ParseIP("192.0.2.2"), Port: 443})
	stopped := make(chan struct{}, 1)
	release, err := RegisterPeer(ctx, "alice@tenant#i1", func() net.Addr { return current.Load() }, func() { stopped <- struct{}{} })
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	if err := m.Start(); err != nil {
		t.Fatal(err)
	}
	current.Store(&net.UDPAddr{IP: net.ParseIP("192.0.2.1"), Port: 443})
	select {
	case <-stopped:
	case <-time.After(3 * time.Second):
		t.Fatal("migrated banned peer remained admitted")
	}
}

func TestPeerIPv6InterfaceZoneDoesNotRejectOtherAddresses(t *testing.T) {
	m := manager(t)
	ctx := Context(context.Background(), m)
	release, err := Register(ctx, "alice@tenant#i1", &net.UDPAddr{IP: net.ParseIP("fe80::2"), Zone: "eth0", Port: 443}, func() {})
	if err != nil {
		t.Fatal(err)
	}
	release()
}
