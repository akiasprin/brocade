package mtproto

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type frozenMTBudget struct {
	mode   string
	calls  atomic.Int64
	bytes  atomic.Int64
	port   int
	target int
	env    []string
}

// The fixture refuses every positive payload request. A VLESS stream in the
// same release process proves the control file and endpoint actually work.
func frozenMTBudgetSetup(t *testing.T, dir string, config map[string]any) *frozenMTBudget {
	t.Helper()
	mode := os.Getenv("BROCADE_MT_TEST_BUDGET")
	if mode == "" {
		return nil
	}
	if mode != "machine" && mode != "user" {
		t.Fatal("invalid budget fixture mode")
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	probe := &frozenMTBudget{mode: mode, port: mtFrozenPort(t)}
	go func() {
		for {
			c, err := listener.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				_ = c.SetDeadline(time.Now().Add(15 * time.Second))
				reader := bufio.NewReader(c)
				for {
					line, err := reader.ReadString('\n')
					if err != nil {
						return
					}
					if mode == "user" {
						var request struct {
							Bytes int `json:"bytes"`
						}
						if json.Unmarshal([]byte(line), &request) != nil {
							return
						}
						if request.Bytes > 0 {
							probe.calls.Add(1)
						}
						_, _ = c.Write([]byte("STOP\n"))
					} else {
						var token string
						var n int
						if _, err := fmt.Sscanf(line, "%s %d", &token, &n); err != nil {
							return
						}
						if n > 0 {
							probe.calls.Add(1)
							_, _ = c.Write([]byte("ERR\n"))
						} else {
							_, _ = c.Write([]byte("OK 8000\n"))
						}
					}
				}
			}()
		}
	}()
	echo, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = echo.Close() })
	probe.target = echo.Addr().(*net.TCPAddr).Port
	go func() {
		for {
			c, err := echo.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				_ = c.SetDeadline(time.Now().Add(5 * time.Second))
				_, _ = io.Copy(c, io.TeeReader(c, mtBudgetCounter{&probe.bytes}))
			}()
		}
	}()
	config["outbounds"] = append(config["outbounds"].([]any), map[string]any{"tag": "budget-validation", "protocol": "freedom", "settings": map[string]any{"finalRules": []any{map[string]any{"action": "allow", "ip": []string{"127.0.0.0/8"}}}}})
	config["routing"] = map[string]any{"rules": []any{map[string]any{"type": "field", "inboundTag": []string{"budget-validation"}, "outboundTag": "budget-validation"}}}
	control := filepath.Join(dir, "budget.json")
	token := strings.Repeat("a", 32)
	if mode == "machine" {
		mtFrozenWrite(t, control, map[string]any{"enabled": true, "rate_bps": 8000, "endpoint": listener.Addr().String(), "token": token})
		probe.env = []string{"XRAY_BROCADE_MACHINE_LIMIT_FILE=" + control, "XRAY_BROCADE_MACHINE_LIMIT_REQUIRED=1"}
	} else {
		mtFrozenWrite(t, control, map[string]any{"endpoint": listener.Addr().String(), "token": token, "generation": 1})
		probe.env = []string{"XRAY_BROCADE_USER_LIMIT_FILE=" + control}
	}
	config["inbounds"] = append(config["inbounds"].([]any), map[string]any{"tag": "budget-validation", "listen": "127.0.0.1", "port": probe.port, "protocol": "vless", "settings": map[string]any{"decryption": "none", "clients": []any{map[string]any{"id": "01010101-0101-0101-0101-010101010101", "email": "alice@tenant#entry"}}}})
	return probe
}
func (p *frozenMTBudget) validate(t *testing.T) {
	t.Helper()
	if p == nil {
		return
	}
	c, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", p.port), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	// Machine budget failures retry for up to ten seconds before closing the
	// stream. Wait for that stream to finish so its retries cannot be counted
	// as MTProxy requests after the positive-control counter is reset.
	_ = c.SetDeadline(time.Now().Add(12 * time.Second))
	// Actual VLESS request with authenticated identity and nonempty payload.
	wire := append([]byte{0}, []byte(strings.Repeat("\x01", 16))...)
	wire = append(wire, 0, 1, byte(p.target>>8), byte(p.target), 1, 127, 0, 0, 1)
	wire = append(wire, []byte("budget-validation-payload")...)
	if _, err = c.Write(wire); err != nil {
		t.Fatal(err)
	}
	mtFrozenWait(t, func() bool { return p.calls.Load() > 0 })
	response, err := io.ReadAll(c)
	if err != nil {
		t.Fatalf("VLESS positive control did not close after budget denial: %v", err)
	}
	if p.bytes.Load() != 0 || len(response) > 2 {
		t.Fatalf("VLESS positive control forwarded denied payload: target bytes=%d response bytes=%d", p.bytes.Load(), len(response))
	}
	t.Logf("positive control: %s budget denies VLESS payload, requests=%d", p.mode, p.calls.Load())
	p.calls.Store(0)
}

type mtBudgetCounter struct{ bytes *atomic.Int64 }

func (c mtBudgetCounter) Write(p []byte) (int, error) {
	c.bytes.Add(int64(len(p)))
	return len(p), nil
}
func TestBrocadeFrozenMTProxyMustHonorMachineBudget(t *testing.T) {
	t.Setenv("BROCADE_MT_TEST_BUDGET", "machine")
	TestBrocadeFrozenMTProxyDirectoryRPCAccountingAndBan(t)
}
func TestBrocadeFrozenMTProxyMustHonorUserBudget(t *testing.T) {
	t.Setenv("BROCADE_MT_TEST_BUDGET", "user")
	TestBrocadeFrozenMTProxyDirectoryRPCAccountingAndBan(t)
}
