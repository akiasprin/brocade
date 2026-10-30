package mtproto

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

func TestDirectoryReviewCacheExpiryFailureAndRecovery(t *testing.T) {
	var calls atomic.Int32
	var failing atomic.Bool
	var revision atomic.Int32
	revision.Store(1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		if failing.Load() {
			http.Error(w, "fixture unavailable", 503)
			return
		}
		if r.URL.Path == "/secret" {
			_, _ = w.Write(bytes.Repeat([]byte{byte(revision.Load())}, 128))
			return
		}
		fmt.Fprintf(w, "default 2;\nproxy_for 2 198.51.100.2:%d;\n", 8887+revision.Load())
	}))
	defer server.Close()
	directory := newOfficialProxyDirectory()
	directory.client = server.Client()
	directory.secretURL = server.URL + "/secret"
	directory.configURL = server.URL + "/config"
	now := time.Unix(1700000000, 0)
	directory.now = func() time.Time { return now }
	first, secret, err := directory.target(context.Background(), -4)
	if err != nil || first.port != 8888 || calls.Load() != 2 {
		t.Fatalf("initial directory/default DC: target=%+v calls=%d err=%v", first, calls.Load(), err)
	}
	secret[0] = 0xff
	now = now.Add(23 * time.Hour)
	_, retained, err := directory.target(context.Background(), 2)
	if err != nil || retained[0] != 1 || calls.Load() != 2 {
		t.Fatal("warm cache refetched or exposed mutable secret storage")
	}
	failing.Store(true)
	now = now.Add(2 * time.Hour)
	stale, _, err := directory.target(context.Background(), 2)
	if err != nil || stale != first || calls.Load() != 3 {
		t.Fatal("failed expiry refresh discarded last valid directory")
	}
	failing.Store(false)
	revision.Store(2)
	now = now.Add(time.Hour)
	fresh, key, err := directory.target(context.Background(), 2)
	if err != nil || fresh.port != 8889 || key[0] != 2 || calls.Load() != 5 {
		t.Fatalf("directory did not recover: %+v calls=%d err=%v", fresh, calls.Load(), err)
	}
}
func TestDirectoryReviewRejectsInvalidColdSnapshot(t *testing.T) {
	for name, secret := range map[string][]byte{"short": make([]byte, 31), "oversized": make([]byte, 257)} {
		t.Run(name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write(secret) }))
			defer server.Close()
			d := newOfficialProxyDirectory()
			d.secretURL = server.URL
			d.configURL = server.URL
			if _, _, err := d.target(context.Background(), 2); err == nil || d.snapshot != nil {
				t.Fatal("invalid cold directory was accepted")
			}
		})
	}
}
