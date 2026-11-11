package mtproto

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/machinelimit"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/userlimit"
)

func mtBudgetContext(t *testing.T, reply string) (context.Context, context.CancelFunc, <-chan struct{}) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	requested := make(chan struct{}, 2)
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				_ = conn.SetDeadline(time.Now().Add(3 * time.Second))
				reader := bufio.NewReader(conn)
				for {
					if _, err := reader.ReadString('\n'); err != nil {
						return
					}
					requested <- struct{}{}
					if _, err := io.WriteString(conn, reply+"\n"); err != nil {
						return
					}
				}
			}()
		}
	}()
	control := filepath.Join(t.TempDir(), "user-budget.json")
	data, err := json.Marshal(map[string]any{"endpoint": listener.Addr().String(), "token": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "generation": 1})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(control, data, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(userlimit.Env, control)
	t.Setenv(machinelimit.Env, "")
	ctx := session.ContextWithInbound(context.Background(), &session.Inbound{Tag: "entry", User: &protocol.MemoryUser{Email: "alice@tenant#entry"}})
	ctx, cancel := context.WithCancel(machinelimit.NewContext(ctx))
	t.Cleanup(cancel)
	return ctx, cancel, requested
}

func TestBudgetConnectionRejectsPayloadInBothDirections(t *testing.T) {
	for _, direction := range []string{"read", "write"} {
		t.Run(direction, func(t *testing.T) {
			ctx, cancel, requested := mtBudgetContext(t, "STOP")
			server, client := net.Pipe()
			defer client.Close()
			_ = client.SetDeadline(time.Now().Add(time.Second))
			conn := &budgetConnection{Connection: server, ctx: ctx, cancel: cancel, controller: machinelimit.FromContext(ctx)}
			defer conn.Close()
			var n int
			var err error
			if direction == "read" {
				go func() { _, _ = client.Write([]byte("denied payload")) }()
				n, err = conn.Read(make([]byte, 64))
			} else {
				go func() { _, _ = io.Copy(io.Discard, client) }()
				n, err = conn.Write([]byte("denied payload"))
			}
			if n != 0 || err == nil {
				t.Fatalf("denied %s exposed %d bytes, error=%v", direction, n, err)
			}
			select {
			case <-requested:
			default:
				t.Fatal("payload did not reach the shared user budget")
			}
		})
	}
}

func TestBudgetConnectionCloseInterruptsPendingAdmission(t *testing.T) {
	ctx, cancel, requested := mtBudgetContext(t, "WAIT")
	server, client := net.Pipe()
	defer client.Close()
	conn := &budgetConnection{Connection: server, ctx: ctx, cancel: cancel, controller: machinelimit.FromContext(ctx)}
	defer conn.Close()
	done := make(chan error, 1)
	go func() { _, err := conn.Write([]byte("waiting payload")); done <- err }()
	select {
	case <-requested:
	case <-time.After(time.Second):
		t.Fatal("shared budget did not receive payload admission")
	}
	_ = conn.Close()
	select {
	case err := <-done:
		if err == nil || ctx.Err() == nil {
			t.Fatalf("closed connection completed pending admission: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("revocation left a pending budget admission blocked")
	}
}
