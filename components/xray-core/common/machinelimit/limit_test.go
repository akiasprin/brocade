package machinelimit

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestMachineLimitSharesBudgetAndReloadsExistingController(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	var mu sync.Mutex
	var charged int
	stop := make(chan struct{})
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				reader := bufio.NewReader(conn)
				for {
					line, err := reader.ReadString('\n')
					if err != nil {
						return
					}
					var token string
					var n int
					if _, err = fmt.Sscanf(line, "%s %d", &token, &n); err != nil {
						return
					}
					if token != "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
						return
					}
					if len(strings.Fields(line)) != 2 {
						t.Error("legacy Agent control must not enable progress frames")
						return
					}
					mu.Lock()
					charged += n
					mu.Unlock()
					_, _ = fmt.Fprint(conn, "OK 80000\n")
					select {
					case <-stop:
						return
					default:
					}
				}
			}()
		}
	}()
	defer close(stop)
	path := filepath.Join(t.TempDir(), "control.json")
	cfg := config{Enabled: false, Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
	write := func() {
		data, _ := json.Marshal(cfg)
		if err := os.WriteFile(path, data, 0600); err != nil {
			t.Fatal(err)
		}
	}
	write()
	first, second := New(path), New(path)
	if err := first.Wait(context.Background(), 200); err != nil {
		t.Fatal(err)
	}
	cfg.Enabled = true
	cfg.Rate = 80000
	write()
	// Snapshot forces the same hot reload used by the Agent's readback.
	if _, err := first.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := second.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := first.Wait(context.Background(), 100); err != nil {
		t.Fatal(err)
	}
	if err := second.Wait(context.Background(), 300); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	got := charged
	mu.Unlock()
	if got != 400 {
		t.Fatalf("shared budget saw %d bytes, want 400", got)
	}
	cfg.Enabled = false
	cfg.Rate = 0
	write()
	if _, err := first.Snapshot(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := first.Wait(context.Background(), 1000); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	defer mu.Unlock()
	if charged != 400 {
		t.Fatal("disabled limit still charged budget")
	}
}

func TestMachineLimitRejectsRemoteEndpointsAndCancelsBlockedBudget(t *testing.T) {
	path := filepath.Join(t.TempDir(), "control.json")
	data, _ := json.Marshal(config{Enabled: true, Rate: 80000, Endpoint: "192.0.2.1:1234", Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
	_ = os.WriteFile(path, data, 0600)
	if err := New(path).Wait(context.Background(), 10); err == nil {
		t.Fatal("accepted remote budget")
	}
	listener, _ := net.Listen("tcp", "127.0.0.1:0")
	defer listener.Close()
	done := make(chan struct{})
	go func() {
		conn, err := listener.Accept()
		if err == nil {
			defer conn.Close()
			<-done
		}
	}()
	defer close(done)
	data, _ = json.Marshal(config{Enabled: true, Rate: 80000, Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
	_ = os.WriteFile(path, data, 0600)
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Millisecond)
	defer cancel()
	started := time.Now()
	if err := New(path).Wait(ctx, 10); err == nil {
		t.Fatal("blocked budget did not cancel")
	}
	if time.Since(started) > time.Second {
		t.Fatal("cancellation exceeded bound")
	}
}

func TestMachineLimitInactiveControlFailureDoesNotBreakUncappedTraffic(t *testing.T) {
	path := filepath.Join(t.TempDir(), "control.json")
	c := New(path)
	c.required = false
	if _, err := c.load(true); err != nil {
		t.Fatal("an explicitly uncapped launch must survive a missing optional budget", err)
	}
	data, _ := json.Marshal(config{Enabled: true, Rate: 1000, Endpoint: "127.0.0.1:1", Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := c.load(true); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("invalid"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := c.load(true); err == nil {
		t.Fatal("an activated cap must not fail open after control corruption")
	}
}

func TestMachineLimitLowRateDatagramUsesSmallBudgetRequests(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	counts := make(chan []int, 1)
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			counts <- nil
			return
		}
		defer conn.Close()
		reader := bufio.NewReader(conn)
		var got []int
		for total := 0; total < 32; {
			line, err := reader.ReadString('\n')
			if err != nil {
				break
			}
			var token string
			var n int
			if _, err = fmt.Sscanf(line, "%s %d", &token, &n); err != nil {
				break
			}
			got = append(got, n)
			total += n
			_, _ = fmt.Fprint(conn, "OK 1000\n")
		}
		counts <- got
	}()
	path := filepath.Join(t.TempDir(), "control.json")
	data, _ := json.Marshal(config{Enabled: true, Rate: 1000, Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := New(path).Wait(ctx, 32); err != nil {
		t.Fatal(err)
	}
	got := <-counts
	if len(got) != 3 || got[0] != 12 || got[1] != 12 || got[2] != 8 {
		t.Fatalf("datagram must preserve bytes across fair budget slices: %v", got)
	}
}

func TestMachineLimitProgressSurvivesLongRateReduction(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	done := make(chan struct{})
	defer close(done)
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		line, err := bufio.NewReader(conn).ReadString('\n')
		if err != nil || !strings.HasSuffix(line, " progress-v1\n") {
			return
		}
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for i := 0; i < 11; i++ {
			select {
			case <-ticker.C:
				if _, err := fmt.Fprint(conn, "WAIT\n"); err != nil {
					return
				}
			case <-done:
				return
			}
		}
		_, _ = fmt.Fprint(conn, "OK 1000\n")
	}()
	path := filepath.Join(t.TempDir(), "control.json")
	data, _ := json.Marshal(config{Enabled: true, Rate: 100_000_000, Progress: true, Endpoint: listener.Addr().String(), Token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := New(path).Wait(ctx, 1000); err != nil {
		t.Fatal("healthy throttling progress must not hit the outage timeout", err)
	}
}
