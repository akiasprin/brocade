package reality

import (
	"context"
	"crypto/ecdh"
	stderrors "errors"
	gonet "net"
	"testing"
	"time"

	realitytls "github.com/xtls/reality"
)

func TestFallbackSourceKeyGroupsIPv6Prefix(t *testing.T) {
	first := fallbackSourceKey(&gonet.TCPAddr{IP: gonet.ParseIP("2001:db8:1:2::1"), Port: 443})
	second := fallbackSourceKey(&gonet.TCPAddr{IP: gonet.ParseIP("2001:db8:1:2::ffff"), Port: 8443})
	other := fallbackSourceKey(&gonet.TCPAddr{IP: gonet.ParseIP("2001:db8:1:3::1"), Port: 443})
	if first != second {
		t.Fatal("addresses in one IPv6 /64 received different protection keys")
	}
	if first == other {
		t.Fatal("addresses in different IPv6 /64s received one protection key")
	}
}

func TestFallbackProtectionBoundsSourceAndTotal(t *testing.T) {
	protection := NewFallbackProtection(FallbackProtectionOptions{
		IdleTimeout:         time.Second,
		MaxPending:          2,
		MaxPendingPerSource: 1,
	})
	firstSource := &gonet.TCPAddr{IP: gonet.ParseIP("192.0.2.1"), Port: 1000}
	secondSource := &gonet.TCPAddr{IP: gonet.ParseIP("192.0.2.2"), Port: 1000}

	releaseFirst, err := protection.acquire(firstSource)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := protection.acquire(firstSource); !stderrors.Is(err, ErrTooManyPendingFallbacksForSource) {
		t.Fatalf("same-source admission error = %v", err)
	}
	releaseSecond, err := protection.acquire(secondSource)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := protection.acquire(&gonet.TCPAddr{IP: gonet.ParseIP("192.0.2.3"), Port: 1000}); !stderrors.Is(err, ErrTooManyPendingFallbacks) {
		t.Fatalf("global admission error = %v", err)
	}
	releaseFirst()
	releaseSecond()
	if release, err := protection.acquire(firstSource); err != nil {
		t.Fatalf("released source remained blocked: %v", err)
	} else {
		release()
	}
}

func TestFallbackIdleConnectionClosesSilentPeer(t *testing.T) {
	client, server := gonet.Pipe()
	defer client.Close()
	defer server.Close()
	guarded, err := newFallbackIdleConn(server, 30*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	_, err = guarded.Read(make([]byte, 1))
	if timeout, ok := err.(gonet.Error); !ok || !timeout.Timeout() {
		t.Fatalf("idle read error = %v", err)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("idle connection took %s to close", elapsed)
	}
}

func TestSuccessfulHandshakeCanDisableFallbackDeadline(t *testing.T) {
	client, server := gonet.Pipe()
	defer client.Close()
	defer server.Close()
	guarded, err := newFallbackIdleConn(server, 20*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if err := guarded.disable(); err != nil {
		t.Fatal(err)
	}
	time.Sleep(40 * time.Millisecond)
	written := make(chan error, 1)
	go func() {
		_, err := client.Write([]byte{1})
		written <- err
	}()
	buffer := make([]byte, 1)
	if _, err := guarded.Read(buffer); err != nil {
		t.Fatalf("disabled fallback deadline remained active: %v", err)
	}
	if err := <-written; err != nil {
		t.Fatal(err)
	}
}

func TestRealityServerBoundsSilentFallback(t *testing.T) {
	client, server := gonet.Pipe()
	targetClient, targetServer := gonet.Pipe()
	defer client.Close()
	defer targetClient.Close()
	config := &realitytls.Config{
		DialContext: func(_ context.Context, _, _ string) (gonet.Conn, error) {
			return targetServer, nil
		},
		Type: "tcp",
		Dest: "cover.invalid:443",
		PrivateKey: func() *ecdh.PrivateKey {
			key, err := ecdh.X25519().NewPrivateKey(make([]byte, 32))
			if err != nil {
				t.Fatal(err)
			}
			return key
		}(),
		ServerNames: map[string]bool{"cover.invalid": true},
		ShortIds:    map[[8]byte]bool{{}: true},
	}
	protection := NewFallbackProtection(FallbackProtectionOptions{
		IdleTimeout:         30 * time.Millisecond,
		MaxPending:          2,
		MaxPendingPerSource: 1,
	})
	done := make(chan error, 1)
	go func() {
		_, err := serverWithFallbackProtection(server, config, protection)
		done <- err
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("silent fallback unexpectedly completed")
		}
	case <-time.After(time.Second):
		t.Fatal("silent fallback outlived its idle timeout")
	}
}
