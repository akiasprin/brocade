package scenarios

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	stdnet "net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	statscmd "github.com/xtls/xray-core/app/stats/command"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

type frozenObject = map[string]any

// Explicit artifact tests never build a replacement or silently accept a skip.
func frozenBinary(t *testing.T) string {
	t.Helper()
	selected := os.Getenv("BROCADE_XRAY_BIN")
	if selected == "" {
		t.Skip("explicit release E2E requires BROCADE_XRAY_BIN")
	}
	path, err := filepath.Abs(selected)
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("artifact sha256=%x", sha256.Sum256(data))
	return path
}

var frozenNextPort atomic.Uint32

func frozenPort(t *testing.T) int {
	t.Helper()
	// Stay outside Linux's ephemeral source range. Releasing a bind(:0) can
	// otherwise let a new proxy dial steal a port reserved for the next listener.
	for range 10000 {
		port := 20000 + int(frozenNextPort.Add(1))
		if port >= 30000 {
			t.Fatal("frozen fixture listener range exhausted")
		}
		l, err := stdnet.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
		if err == nil {
			_ = l.Close()
			return port
		}
	}
	t.Fatal("no isolated fixture listener port available")
	return 0
}

func frozenWrite(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	temp := path + ".tmp"
	if err = os.WriteFile(temp, data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err = os.Rename(temp, path); err != nil {
		t.Fatal(err)
	}
}
func frozenStart(t *testing.T, config frozenObject, env ...string) (int, *exec.Cmd) {
	t.Helper()
	return frozenStartProcess(t, frozenPort(t), nil, config, env...)
}

func frozenStartProcess(t *testing.T, api int, files []*os.File, config frozenObject, env ...string) (int, *exec.Cmd) {
	t.Helper()
	services := []string{"StatsService", "HandlerService"}
	if requested, ok := config["api"].(frozenObject); ok {
		if custom, ok := requested["services"].([]string); ok {
			services = custom
		}
	}
	config["api"] = frozenObject{"tag": "api", "listen": fmt.Sprintf("127.0.0.1:%d", api), "services": services}
	if config["log"] == nil {
		config["log"] = frozenObject{"loglevel": "debug"}
	}
	if config["stats"] == nil {
		config["stats"] = frozenObject{}
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	frozenWrite(t, path, config)
	output := filepath.Join(dir, "xray.log")
	log, err := os.Create(output)
	if err != nil {
		t.Fatal(err)
	}
	binary := frozenBinary(t)
	cmd := exec.Command(binary, "run", "-config", path)
	for _, value := range env {
		if trace, ok := strings.CutPrefix(value, "BROCADE_E2E_SYSCALL_FILE="); ok {
			cmd = exec.Command("strace", "-D", "-f", "-v", "-s", "128", "-e", "trace=sendmsg,sendmmsg", "-o", trace, binary, "run", "-config", path)
		}
	}
	cmd.Env = append(os.Environ(), env...)
	cmd.ExtraFiles = files
	cmd.Stdout = log
	cmd.Stderr = log
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	t.Cleanup(func() {
		_ = cmd.Process.Signal(os.Interrupt)
		select {
		case <-done:
		case <-time.After(3 * time.Second):
			_ = cmd.Process.Kill()
			<-done
		}
		_ = log.Close()
		data, _ := os.ReadFile(output)
		if root := os.Getenv("BROCADE_E2E_OUTPUT"); root != "" {
			name := strings.NewReplacer("/", "_", " ", "_").Replace(t.Name()) + fmt.Sprintf("-%d.log", cmd.Process.Pid)
			if err := os.WriteFile(filepath.Join(root, name), data, 0o600); err != nil {
				t.Error(err)
			}
		}
		if t.Failed() {
			t.Logf("Xray log:\n%s", data)
		}
	})
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		select {
		case err := <-done:
			done <- err
			t.Fatalf("Xray exited before API readiness: %v", err)
		default:
		}
		c, err := stdnet.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", api), 50*time.Millisecond)
		if err == nil {
			_ = c.Close()
			return api, cmd
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("Xray API readiness deadline")
	return 0, nil
}
func frozenStats(t *testing.T, api int) statscmd.StatsServiceClient {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", api), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return statscmd.NewStatsServiceClient(c)
}
func frozenCounters(t *testing.T, client statscmd.StatsServiceClient, pattern string) map[string]int64 {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	reply, err := client.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: pattern})
	if err != nil {
		t.Fatal(err)
	}
	result := map[string]int64{}
	for _, s := range reply.Stat {
		result[s.Name] = s.Value
	}
	return result
}
func frozenEventually(t *testing.T, duration time.Duration, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(duration)
	for time.Now().Before(deadline) {
		if condition() {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("observable condition did not hold before deadline")
}
func frozenEcho(t *testing.T) int {
	t.Helper()
	l, err := stdnet.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = l.Close() })
	go func() {
		for {
			c, err := l.Accept()
			if err != nil {
				return
			}
			go func() { defer c.Close(); _ = c.SetDeadline(time.Now().Add(30 * time.Second)); _, _ = io.Copy(c, c) }()
		}
	}()
	return l.Addr().(*stdnet.TCPAddr).Port
}
func frozenRoundtrip(c stdnet.Conn, payload []byte) error {
	if err := c.SetDeadline(time.Now().Add(2 * time.Second)); err != nil {
		return err
	}
	if _, err := c.Write(payload); err != nil {
		return err
	}
	got := make([]byte, len(payload))
	_, err := io.ReadFull(c, got)
	if err == nil && !bytes.Equal(got, payload) {
		return fmt.Errorf("payload differs")
	}
	return err
}
func frozenDirect(t *testing.T, port int, host string) stdnet.Conn {
	t.Helper()
	c, err := stdnet.DialTimeout("tcp", fmt.Sprintf("%s:%d", host, port), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}
func frozenFreedom() []any {
	return []any{frozenObject{"protocol": "freedom", "settings": frozenObject{"finalRules": []any{frozenObject{"action": "allow", "ip": []string{"127.0.0.0/8", "::1/128"}}}}}}
}
func frozenPolicy() frozenObject {
	return frozenObject{"levels": frozenObject{"0": frozenObject{"statsUserUplink": true, "statsUserDownlink": true, "statsUserOnline": true}}}
}
func frozenDoko(port, target int) frozenObject {
	return frozenObject{"tag": "client", "listen": "127.0.0.1", "port": port, "protocol": "dokodemo-door", "settings": frozenObject{"address": "127.0.0.1", "port": target, "network": "tcp"}}
}
