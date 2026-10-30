package mtproto

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/sha256"
	"crypto/tls"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// This peer implements Telegram's published nonce/CBC/framing exchange. The
// actual release Xray performs its normal HTTPS directory fetch, admission,
// middle RPC and user/site accounting. No production test hook is required.
func frozenMiddlePeer(c net.Conn, secret []byte, requests *atomic.Int32) error {
	defer c.Close()
	_ = c.SetDeadline(time.Now().Add(20 * time.Second))
	seq, nonce, err := readPlainRPCFrame(c)
	if err != nil {
		return err
	}
	if seq != -2 || len(nonce) != 32 || binary.LittleEndian.Uint32(nonce) != rpcNonce {
		return fmt.Errorf("invalid nonce")
	}
	var clientNonce, serverNonce [16]byte
	copy(clientNonce[:], nonce[16:])
	for i := range serverNonce {
		serverNonce[i] = byte(160 + i)
	}
	response := append([]byte(nil), nonce...)
	copy(response[16:], serverNonce[:])
	if err := writeAll(c, rpcFrame(-2, response)); err != nil {
		return err
	}
	writeKey, writeIV, readKey, readIV, err := middleRPCKeys(serverNonce, clientNonce, int32(binary.LittleEndian.Uint32(nonce[12:16])), c.LocalAddr().(*net.TCPAddr), c.RemoteAddr().(*net.TCPAddr), secret)
	if err != nil {
		return err
	}
	decode, _ := aes.NewCipher(writeKey)
	encode, _ := aes.NewCipher(readKey)
	rpc := &middleRPC{conn: c, outSeq: -1, inSeq: -1, decode: cipher.NewCBCDecrypter(decode, writeIV), encode: cipher.NewCBCEncrypter(encode, readIV)}
	_, handshake, err := rpc.readPacket()
	if err != nil {
		return err
	}
	if len(handshake) != 32 || binary.LittleEndian.Uint32(handshake) != rpcHandshake {
		return fmt.Errorf("invalid encrypted handshake")
	}
	answer := make([]byte, 32)
	binary.LittleEndian.PutUint32(answer, rpcHandshake)
	copy(answer[8:20], handshake[20:32])
	copy(answer[20:32], handshake[8:20])
	if err := rpc.writePacket(answer); err != nil {
		return err
	}
	for {
		_, request, err := rpc.readPacket()
		if err != nil {
			return err
		}
		if len(request) < 112 || binary.LittleEndian.Uint32(request) != rpcProxyRequest {
			return fmt.Errorf("invalid proxy request")
		}
		requests.Add(1)
		reply := make([]byte, 16, len(request)-40)
		binary.LittleEndian.PutUint32(reply, rpcProxyAnswer)
		copy(reply[8:16], request[8:16])
		reply = append(reply, request[56:]...)
		if err := rpc.writePacket(reply); err != nil {
			return err
		}
	}
}
func mtFrozenPort(t *testing.T) int {
	t.Helper()
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}
func mtFrozenWrite(t *testing.T, path string, v any) {
	t.Helper()
	data, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(path+".tmp", data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err = os.Rename(path+".tmp", path); err != nil {
		t.Fatal(err)
	}
}
func mtFrozenWait(t *testing.T, f func() bool) {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if f() {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("MTProxy observable deadline")
}
func TestBrocadeFrozenMTProxyDirectoryRPCAccountingAndBan(t *testing.T) {
	artifact := os.Getenv("BROCADE_XRAY_BIN")
	if artifact == "" {
		t.Skip("explicit artifact E2E requires BROCADE_XRAY_BIN")
	}
	artifact, err := filepath.Abs(artifact)
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(artifact)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("artifact sha256=%x", sha256.Sum256(data))
	// The isolated runner assigns this documentation address in its own network
	// namespace, preserving the real non-NAT socket identity required by MTProxy.
	middle, err := net.Listen("tcp", "198.51.100.2:0")
	if err != nil {
		t.Fatalf("isolated documentation address missing: %v", err)
	}
	defer middle.Close()
	secret := bytes.Repeat([]byte{0x5a}, 128)
	var requests, connections, directoryGets atomic.Int32
	go func() {
		for {
			c, err := middle.Accept()
			if err != nil {
				return
			}
			connections.Add(1)
			go func() { _ = frozenMiddlePeer(c, secret, &requests) }()
		}
	}()
	directory := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		directoryGets.Add(1)
		switch r.URL.Path {
		case "/getProxySecret":
			_, _ = w.Write(secret)
		case "/getProxyConfig":
			fmt.Fprintf(w, "default 2;\nproxy_for 2 %s;\n", middle.Addr())
		default:
			http.Error(w, "unexpected path", 404)
		}
	}))
	certificate, _ := cert.MustGenerate(nil, cert.DNSNames("core.telegram.org"), cert.Authority(true))
	pem, key := certificate.ToPEM()
	pair, err := tls.X509KeyPair(pem, key)
	if err != nil {
		t.Fatal(err)
	}
	directory.TLS = &tls.Config{Certificates: []tls.Certificate{pair}, MinVersion: tls.VersionTLS12}
	directory.StartTLS()
	defer directory.Close()
	proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodConnect || r.Host != "core.telegram.org:443" {
			http.Error(w, "unexpected directory destination", 403)
			return
		}
		upstream, err := net.DialTimeout("tcp", directory.Listener.Addr().String(), time.Second)
		if err != nil {
			http.Error(w, "directory unavailable", 502)
			return
		}
		client, _, err := w.(http.Hijacker).Hijack()
		if err != nil {
			_ = upstream.Close()
			return
		}
		_ = client.SetDeadline(time.Now().Add(20 * time.Second))
		_ = upstream.SetDeadline(time.Now().Add(20 * time.Second))
		_, _ = client.Write([]byte("HTTP/1.1 200 Connection Established\r\n\r\n"))
		go func() { _, _ = io.Copy(upstream, client); _ = upstream.Close() }()
		go func() { _, _ = io.Copy(client, upstream); _ = client.Close() }()
	}))
	defer proxy.Close()
	dir := t.TempDir()
	ca := filepath.Join(dir, "ca.pem")
	if err = os.WriteFile(ca, pem, 0o600); err != nil {
		t.Fatal(err)
	}
	policy := filepath.Join(dir, "source.json")
	mtFrozenWrite(t, policy, map[string]any{"revision": 1, "node_ips": []string{}, "users": []any{}})
	ingress, api := mtFrozenPort(t), mtFrozenPort(t)
	config := map[string]any{"log": map[string]any{"loglevel": "debug"}, "api": map[string]any{"tag": "api", "listen": fmt.Sprintf("127.0.0.1:%d", api), "services": []string{"StatsService", "HandlerService"}}, "stats": map[string]any{}, "policy": map[string]any{"levels": map[string]any{"0": map[string]any{"statsUserUplink": true, "statsUserDownlink": true, "statsUserOnline": true}}}, "inbounds": []any{map[string]any{"tag": "mt-entry", "listen": "127.0.0.1", "port": ingress, "protocol": "mtproto", "settings": map[string]any{"users": []any{map[string]any{"secret": "00010203-0405-0607-0809-0a0b0c0d0e0f", "email": "alice@tenant#entry"}}}}}, "outbounds": []any{map[string]any{"protocol": "blackhole"}}}
	if os.Getenv("BROCADE_MT_TEST_LIFECYCLE") == "1" {
		settings := config["inbounds"].([]any)[0].(map[string]any)["settings"].(map[string]any)
		settings["users"] = append(settings["users"].([]any), map[string]any{"secret": "20212223-2425-2627-2829-2a2b2c2d2e2f", "email": "bob@tenant#entry"})
	}
	budget := frozenMTBudgetSetup(t, dir, config)
	path := filepath.Join(dir, "xray.json")
	mtFrozenWrite(t, path, config)
	logPath := filepath.Join(dir, "xray.log")
	log, err := os.Create(logPath)
	if err != nil {
		t.Fatal(err)
	}
	process := exec.Command(artifact, "run", "-config", path)
	process.Env = append(os.Environ(), "HTTPS_PROXY="+proxy.URL, "HTTP_PROXY="+proxy.URL, "NO_PROXY=", "SSL_CERT_FILE="+ca, "XRAY_BROCADE_SOURCE_ACCESS_FILE="+policy, "XRAY_BROCADE_SITE_USAGE_ENABLED=1", "XRAY_BROCADE_SITE_ACCOUNTING_FILE="+filepath.Join(dir, "sites.bin"))
	if budget != nil {
		process.Env = append(process.Env, budget.env...)
	}
	process.Stdout = log
	process.Stderr = log
	if err = process.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- process.Wait() }()
	defer func() {
		_ = process.Process.Signal(os.Interrupt)
		select {
		case <-done:
		case <-time.After(3 * time.Second):
			_ = process.Process.Kill()
			<-done
		}
		_ = log.Close()
		if t.Failed() {
			data, _ := os.ReadFile(logPath)
			t.Logf("Xray log:\n%s", data)
		}
	}()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	channel, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", api), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	defer channel.Close()
	stats := statscmd.NewStatsServiceClient(channel)
	if budget != nil {
		budget.validate(t)
	}
	counters := func(pattern string) map[string]int64 {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		r, err := stats.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: pattern})
		if err != nil {
			t.Fatal(err)
		}
		values := map[string]int64{}
		for _, s := range r.Stat {
			values[s.Name] = s.Value
		}
		return values
	}
	dial := func() net.Conn {
		c, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", ingress), time.Second)
		if err != nil {
			t.Fatal(err)
		}
		_ = c.SetDeadline(time.Now().Add(5 * time.Second))
		t.Cleanup(func() { _ = c.Close() })
		return c
	}
	var userSecret [16]byte
	for i := range userSecret {
		userSecret[i] = byte(i)
	}
	// A valid obfuscated packet with an unknown secret must never reach directory/RPC.
	invalid := dial()
	bad, _, _ := testClientHeader([16]byte{0x88}, [4]byte{0xdd, 0xdd, 0xdd, 0xdd}, 2)
	_, _ = invalid.Write(bad)
	mtFrozenRequireClosed(t, invalid, "wrong secret")
	if directoryGets.Load() != 0 || connections.Load() != 0 {
		t.Fatal("unauthenticated client reached upstream")
	}
	c := dial()
	header, encrypt, decrypt := testClientHeader(userSecret, [4]byte{0xdd, 0xdd, 0xdd, 0xdd}, 2)
	_, _ = c.Write(header)
	payload := bytes.Repeat([]byte{0x31}, 56)
	plain := binaryLittleFrame(payload)
	encoded := make([]byte, len(plain))
	encrypt.XORKeyStream(encoded, plain)
	_, _ = c.Write(encoded)
	reader := &cipher.StreamReader{S: decrypt, R: c}
	var size [4]byte
	if _, err := io.ReadFull(reader, size[:]); err != nil {
		if budget != nil && budget.calls.Load() > 0 {
			if requests.Load() != 0 {
				t.Fatal("MTProxy forwarded an uplink packet before budget denial")
			}
			t.Logf("%s budget stopped MTProxy payload", budget.mode)
			return
		}
		t.Fatal(err)
	}
	n := int(binary.LittleEndian.Uint32(size[:]))
	if n < 56 || n > 59 {
		t.Fatalf("response length=%d", n)
	}
	response := make([]byte, n)
	if _, err := io.ReadFull(reader, response); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(response[:56], payload) {
		t.Fatal("middle RPC changed payload")
	}
	if budget != nil {
		t.Fatalf("MTProxy bypassed rejecting %s budget: complete encrypted payload arrived; budget payload requests=%d", budget.mode, budget.calls.Load())
	}
	mtFrozenWait(t, func() bool {
		v := counters("user>>>")
		return v["user>>>alice@tenant#entry>>>traffic>>>uplink"] == 60 && v["user>>>alice@tenant#entry>>>traffic>>>downlink"] == int64(n+4)
	})
	mtFrozenWait(t, func() bool {
		v := counters("site>>>")
		return v["site>>>alice@tenant#entry>>>telegram>>>traffic>>>uplink"] == 60 && v["site>>>alice@tenant#entry>>>telegram>>>traffic>>>downlink"] == int64(n+4)
	})
	if requests.Load() != 1 || directoryGets.Load() != 2 {
		t.Fatalf("directory/RPC exchange missing: requests=%d fetches=%d", requests.Load(), directoryGets.Load())
	}
	replay := dial()
	_, _ = replay.Write(header)
	mtFrozenRequireClosed(t, replay, "replayed authentication")
	if connections.Load() != 1 {
		t.Fatal("replay reached middle proxy")
	}
	if os.Getenv("BROCADE_MT_TEST_LIFECYCLE") == "1" {
		frozenMTRevocation(t, channel, dial, c, stats)
		if directoryGets.Load() != 2 {
			t.Fatal("new sessions bypassed the 24-hour directory cache")
		}
		return
	}
	mtFrozenWrite(t, policy, map[string]any{"revision": 2, "node_ips": []string{}, "users": []any{map[string]any{"tenant_id": "tenant", "user_id": "alice", "ips": []string{"127.0.0.1"}}}})
	mtFrozenWait(t, func() bool {
		return counters("brocade>>>source_access>>>revision")["brocade>>>source_access>>>revision"] == 2
	})
	mtFrozenRequireClosed(t, c, "source ban")
	t.Logf("HTTPS directory fetched normally; encrypted nonce/RPC and exact response; billing/site=%d/%d; wrong secret/replay rejected; source ban closes carrier", 60, n+4)
}
func binaryLittleFrame(payload []byte) []byte {
	return append(binary.LittleEndian.AppendUint32(nil, uint32(len(payload))), payload...)
}

func mtFrozenRequireClosed(t *testing.T, c net.Conn, reason string) {
	t.Helper()
	var one [1]byte
	_, err := c.Read(one[:])
	if err == nil {
		t.Fatalf("%s did not close carrier", reason)
	}
	if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
		t.Fatalf("%s timed out without proving carrier closure: %v", reason, err)
	}
}
