package userlimit

import (
	"bufio"
	"context"
	"encoding/json"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"net"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

func TestAuthenticatedIdentityIsNotTheClientIPOrRelayIdentity(t *testing.T) {
	for _, item := range []struct{ tag, email, want string }{{"entry", "alice@platform#in1", "alice@platform#in1"}, {"entry", "probe#in1", ""}, {"hop", "hop-entry", ""}, {"fwd:42", "", "fwd:42"}, {"api", "alice@platform#in1", ""}} {
		ctx := session.ContextWithInbound(context.Background(), &session.Inbound{Tag: item.tag, User: &protocol.MemoryUser{Email: item.email}})
		if got := Source(ctx); got != item.want {
			t.Fatalf("source %q: got %q want %q", item.tag, got, item.want)
		}
	}
}
func TestExistingContextsAndConcurrentGenerationsUseUserBudgetAndCancel(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	dir := t.TempDir()
	file := filepath.Join(dir, "control.json")
	cfg := control{Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
	data, _ := json.Marshal(cfg)
	if err = os.WriteFile(file, data, 0600); err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	counts := map[string]int{}
	go func() {
		for {
			conn, e := listener.Accept()
			if e != nil {
				return
			}
			go func() {
				defer conn.Close()
				reader := bufio.NewReader(conn)
				for {
					line, e := reader.ReadBytes('\n')
					if e != nil {
						return
					}
					var request struct {
						Token, Source string
						Bytes         int
					}
					if json.Unmarshal(line, &request) != nil || request.Token != cfg.Token {
						return
					}
					mu.Lock()
					counts[request.Source] += request.Bytes
					mu.Unlock()
					if request.Source == "blocked@platform#entry" {
						for {
							time.Sleep(10 * time.Millisecond)
							if _, e = conn.Write([]byte("WAIT\n")); e != nil {
								return
							}
						}
					}
					if _, e = conn.Write([]byte("OK\n")); e != nil {
						return
					}
				}
			}()
		}
	}()
	var wg sync.WaitGroup
	for range 2 {
		wg.Go(func() {
			ctx := context.WithValue(context.Background(), contextKey{}, New(file))
			ctx = session.ContextWithInbound(ctx, &session.Inbound{Tag: "entry", User: &protocol.MemoryUser{Email: "alice@platform#entry"}})
			if err := Wait(ctx, 32768); err != nil {
				t.Error(err)
			}
		})
	}
	wg.Wait()
	mu.Lock()
	charged := counts["alice@platform#entry"]
	mu.Unlock()
	if charged != 65536 {
		t.Fatalf("charged %d want 65536", charged)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 80*time.Millisecond)
	defer cancel()
	ctx = context.WithValue(ctx, contextKey{}, New(file))
	ctx = session.ContextWithInbound(ctx, &session.Inbound{User: &protocol.MemoryUser{Email: "blocked@platform#entry"}})
	started := time.Now()
	if Wait(ctx, 1) == nil {
		t.Fatal("blocked request ignored cancellation")
	}
	if time.Since(started) > time.Second {
		t.Fatal("cancellation did not close the local request")
	}
	if err = os.WriteFile(file, []byte(`{"endpoint":"203.0.113.1:80","token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if New(file).Ready() {
		t.Fatal("accepted a remote privileged budget endpoint")
	}
}
func TestControlFailureCannotReleaseAnAuthenticatedUser(t *testing.T) {
	ctx := context.WithValue(context.Background(), contextKey{}, New(filepath.Join(t.TempDir(), "missing")))
	ctx = session.ContextWithInbound(ctx, &session.Inbound{User: &protocol.MemoryUser{Email: "alice@platform#entry"}})
	if Wait(ctx, 100) == nil {
		t.Fatal("missing control bypassed the budget")
	}
}

func TestUnrestrictedCacheIsInvalidatedByPublishedPolicyGeneration(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	var mu sync.Mutex
	calls := 0
	stopped := false
	go func() {
		for {
			conn, e := listener.Accept()
			if e != nil {
				return
			}
			go func() {
				defer conn.Close()
				reader := bufio.NewReader(conn)
				for {
					if _, e := reader.ReadBytes('\n'); e != nil {
						return
					}
					mu.Lock()
					calls++
					stop := stopped
					mu.Unlock()
					if stop {
						_, _ = conn.Write([]byte("STOP\n"))
					} else {
						_, _ = conn.Write([]byte("PASS\n"))
					}
				}
			}()
		}
	}()
	file := filepath.Join(t.TempDir(), "control.json")
	cfg := control{Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
	data, _ := json.Marshal(cfg)
	if err = os.WriteFile(file, data, 0600); err != nil {
		t.Fatal(err)
	}
	ctx := context.WithValue(context.Background(), contextKey{}, New(file))
	ctx = session.ContextWithInbound(ctx, &session.Inbound{User: &protocol.MemoryUser{Email: "alice@platform#entry"}})
	if err = Wait(ctx, 1024*1024); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	count := calls
	stopped = true
	mu.Unlock()
	if count != 1 {
		t.Fatalf("unrestricted payload performed %d budget requests", count)
	}
	cfg.Generation = 1
	data, _ = json.Marshal(cfg)
	if err = os.WriteFile(file, data, 0600); err != nil {
		t.Fatal(err)
	}
	time.Sleep(300 * time.Millisecond)
	if Wait(ctx, 1) == nil {
		t.Fatal("old unrestricted cache bypassed the newly published suspension")
	}
}
