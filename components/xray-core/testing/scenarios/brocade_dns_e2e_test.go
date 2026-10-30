package scenarios

import (
	"encoding/json"
	"fmt"
	stdnet "net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	mdns "github.com/miekg/dns"
)

type frozenDNS struct {
	port  int
	sends atomic.Int32
	a     atomic.Int32
	aaaa  atomic.Int32
	mode  atomic.Int32
	first chan struct{}
	once  sync.Once
}

func frozenResolver(t *testing.T, tcp bool, handler func(*frozenDNS, *mdns.Msg) *mdns.Msg) *frozenDNS {
	t.Helper()
	f := &frozenDNS{first: make(chan struct{})}
	server := &mdns.Server{}
	if tcp {
		l, err := stdnet.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		f.port = l.Addr().(*stdnet.TCPAddr).Port
		server.Listener = l
		server.Net = "tcp"
	} else {
		p, err := stdnet.ListenPacket("udp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		f.port = p.LocalAddr().(*stdnet.UDPAddr).Port
		server.PacketConn = p
		server.Net = "udp"
	}
	server.Handler = mdns.HandlerFunc(func(w mdns.ResponseWriter, q *mdns.Msg) {
		f.sends.Add(1)
		if q.Question[0].Qtype == mdns.TypeA {
			f.a.Add(1)
		}
		if q.Question[0].Qtype == mdns.TypeAAAA {
			f.aaaa.Add(1)
		}
		f.once.Do(func() { close(f.first) })
		reply := handler(f, q)
		if reply != nil {
			_ = w.WriteMsg(reply)
		} else if tcp {
			_ = w.Close()
		}
	})
	go func() { _ = server.ActivateAndServe() }()
	t.Cleanup(func() { _ = server.Shutdown() })
	return f
}
func frozenDNSReply(q *mdns.Msg, ttl uint32) *mdns.Msg {
	r := new(mdns.Msg)
	r.SetReply(q)
	if q.Question[0].Qtype == mdns.TypeA {
		r.Answer = []mdns.RR{&mdns.A{Hdr: mdns.RR_Header{Name: q.Question[0].Name, Rrtype: mdns.TypeA, Class: mdns.ClassINET, Ttl: ttl}, A: stdnet.ParseIP("127.0.0.1")}}
	}
	return r
}
func frozenDNSConfig(t *testing.T, resolver *frozenDNS, tcp bool, target int, domain string, trace bool, stale bool) (frozenObject, int, string) {
	t.Helper()
	port := frozenPort(t)
	in := frozenDoko(port, target)
	in["settings"].(frozenObject)["address"] = domain
	tag := "ordinary"
	if trace {
		tag = "probe:request:dns-e2e"
	}
	in["tag"] = tag
	address := "127.0.0.1"
	if tcp {
		address = fmt.Sprintf("tcp://127.0.0.1:%d", resolver.port)
	}
	config := frozenObject{"inbounds": []any{in}, "dns": frozenObject{"disableFallback": true, "enableParallelQuery": true, "serveStale": stale, "servers": []any{frozenObject{"address": address, "port": resolver.port, "domains": []string{"full:" + domain}, "tag": "dns:rule:e2e", "queryStrategy": "UseIPv4", "skipFallback": true, "timeoutMs": 4000}}}, "outbounds": []any{frozenObject{"tag": "out:forward", "protocol": "freedom", "settings": frozenObject{"domainStrategy": "UseIPv4", "finalRules": []any{frozenObject{"action": "allow", "ip": []string{"127.0.0.0/8"}}}}}}}
	return config, port, tag
}
func TestBrocadeFrozenDNSRetryCacheAndProtocol(t *testing.T) {
	frozenBinary(t)
	for _, tcp := range []bool{false, true} {
		t.Run(map[bool]string{false: "udp", true: "tcp"}[tcp], func(t *testing.T) {
			resolver := frozenResolver(t, tcp, func(f *frozenDNS, q *mdns.Msg) *mdns.Msg {
				if f.sends.Load() <= 2 {
					return nil
				}
				return frozenDNSReply(q, 30)
			})
			echo := frozenEcho(t)
			config, port, _ := frozenDNSConfig(t, resolver, tcp, echo, "retry.example", false, false)
			frozenStart(t, config)
			started := time.Now()
			if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("retry-proof")); err != nil {
				t.Fatal(err)
			}
			if resolver.sends.Load() != 3 {
				t.Fatalf("want third-send success, got %d", resolver.sends.Load())
			}
			before := resolver.sends.Load()
			if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("cached-proof")); err != nil {
				t.Fatal(err)
			}
			if resolver.sends.Load() != before {
				t.Fatal("positive TTL cache was bypassed")
			}
			t.Logf("%s: first two sends fail, third succeeds, exact payload, positive cache; elapsed=%s", map[bool]string{false: "UDP", true: "TCP"}[tcp], time.Since(started))
		})
	}
}
func frozenTraceEnv(t *testing.T) ([]string, string) {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "trace.jsonl")
	if err := os.WriteFile(path, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	id := filepath.Join(dir, "node")
	if err := os.WriteFile(id, []byte("e2e-exit"), 0o600); err != nil {
		t.Fatal(err)
	}
	return []string{"XRAY_BROCADE_PROBE_FILE=" + path, "XRAY_BROCADE_PROBE_IDENTITY_FILE=" + id, "XRAY_BROCADE_PROBE_PROFILE=" + `{"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","strategies":{"out:forward":"UseIPv6"}}`}, path
}
func frozenTrace(t *testing.T, path string) map[string]any {
	t.Helper()
	var result map[string]any
	frozenEventually(t, 2*time.Second, func() bool {
		data, _ := os.ReadFile(path)
		for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
			var r map[string]any
			if json.Unmarshal([]byte(line), &r) == nil && r["complete"] == true {
				result = r
			}
		}
		return result != nil
	})
	return result
}
func TestBrocadeFrozenDNSColdWarmDiagnosticsAndBudget(t *testing.T) {
	frozenBinary(t)
	for _, mode := range []string{"cold", "warm", "timeout"} {
		t.Run(mode, func(t *testing.T) {
			resolver := frozenResolver(t, false, func(f *frozenDNS, q *mdns.Msg) *mdns.Msg {
				if mode == "timeout" {
					return nil
				}
				time.Sleep(60 * time.Millisecond)
				return frozenDNSReply(q, 30)
			})
			config, port, _ := frozenDNSConfig(t, resolver, false, frozenEcho(t), "diagnostic.example", true, false)
			config["outbounds"].([]any)[0].(frozenObject)["settings"].(frozenObject)["domainStrategy"] = "ForceIPv6"
			if mode == "warm" {
				warm := frozenDoko(frozenPort(t), config["inbounds"].([]any)[0].(frozenObject)["settings"].(frozenObject)["port"].(int))
				warm["tag"] = "warm"
				warm["settings"].(frozenObject)["address"] = "diagnostic.example"
				config["inbounds"] = append(config["inbounds"].([]any), warm)
				config["outbounds"] = append(config["outbounds"].([]any), frozenObject{"tag": "warm", "protocol": "freedom", "settings": frozenObject{"domainStrategy": "UseIPv4", "finalRules": []any{frozenObject{"action": "allow", "ip": []string{"127.0.0.0/8"}}}}})
				config["routing"] = frozenObject{"rules": []any{frozenObject{"type": "field", "inboundTag": []string{"warm"}, "outboundTag": "warm"}}}
			}
			env, path := frozenTraceEnv(t)
			frozenStart(t, config, env...)
			if mode == "warm" {
				w := config["inbounds"].([]any)[1].(frozenObject)["port"].(int)
				if err := frozenRoundtrip(frozenDirect(t, w, "127.0.0.1"), []byte("warm")); err != nil {
					t.Fatal(err)
				}
			}
			c := frozenDirect(t, port, "127.0.0.1")
			_ = c.SetDeadline(time.Now().Add(6 * time.Second))
			started := time.Now()
			_, _ = c.Write([]byte("v6-must-not-borrow-v4"))
			var b [1]byte
			if _, err := c.Read(b[:]); err == nil {
				t.Fatal("IPv6 connection escaped through IPv4")
			}
			result := frozenTrace(t, path)
			if result["connected"] != false {
				t.Fatalf("failed family connected through fallback: %+v", result)
			}
			encoded, _ := json.Marshal(result)
			if resolver.aaaa.Load() != 0 {
				t.Fatal("resolver's IPv4 policy or transport changed")
			}
			if mode == "timeout" {
				if resolver.sends.Load() != 5 || time.Since(started) > 5*time.Second {
					t.Fatalf("budget violated sends=%d time=%s", resolver.sends.Load(), time.Since(started))
				}
			} else {
				if !strings.Contains(string(encoded), `"other":true`) || resolver.a.Load() == 0 {
					t.Fatalf("missing actual ordinary-family evidence: %s", encoded)
				}
			}
			t.Logf("%s diagnostics sends=%d A=%d AAAA=%d elapsed=%s trace=%s", mode, resolver.sends.Load(), resolver.a.Load(), resolver.aaaa.Load(), time.Since(started), encoded)
		})
	}
}
func TestBrocadeFrozenDNSCanceledLeaderAndStaleRefresh(t *testing.T) {
	frozenBinary(t)
	t.Run("canceled-leader", func(t *testing.T) {
		resolver := frozenResolver(t, false, func(f *frozenDNS, q *mdns.Msg) *mdns.Msg {
			time.Sleep(150 * time.Millisecond)
			return frozenDNSReply(q, 30)
		})
		config, port, _ := frozenDNSConfig(t, resolver, false, frozenEcho(t), "shared.example", false, false)
		frozenStart(t, config)
		leader := frozenDirect(t, port, "127.0.0.1")
		_, _ = leader.Write([]byte("cancel"))
		select {
		case <-resolver.first:
		case <-time.After(time.Second):
			t.Fatal("query did not start")
		}
		_ = leader.Close()
		if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("healthy-waiter")); err != nil {
			t.Fatal(err)
		}
		if resolver.sends.Load() != 1 {
			t.Fatalf("shared work repeated: %d", resolver.sends.Load())
		}
		if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("cache-not-poisoned")); err != nil {
			t.Fatal(err)
		}
		if resolver.sends.Load() != 1 {
			t.Fatal("leader cancellation poisoned cache")
		}
		t.Log("leader closed, follower and cached request pass, one DNS send")
	})
	t.Run("truncated-stale-refresh", func(t *testing.T) {
		resolver := frozenResolver(t, false, func(f *frozenDNS, q *mdns.Msg) *mdns.Msg {
			r := frozenDNSReply(q, 1)
			if f.mode.Load() == 1 {
				r.Answer = nil
				r.Truncated = true
			}
			return r
		})
		config, port, _ := frozenDNSConfig(t, resolver, false, frozenEcho(t), "stale.example", false, true)
		frozenStart(t, config)
		if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("seed")); err != nil {
			t.Fatal(err)
		}
		resolver.mode.Store(1)
		time.Sleep(1200 * time.Millisecond)
		if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("stale")); err != nil {
			t.Fatal(err)
		}
		frozenEventually(t, 2*time.Second, func() bool { return resolver.sends.Load() >= 3 })
		before := resolver.sends.Load()
		if err := frozenRoundtrip(frozenDirect(t, port, "127.0.0.1"), []byte("retained-after-failure")); err != nil {
			t.Fatal(err)
		}
		if resolver.sends.Load() != before {
			t.Fatal("failed refresh did not back off")
		}
		t.Logf("TTL expired; truncated refresh retained usable address; sends=%d", before)
	})
}
