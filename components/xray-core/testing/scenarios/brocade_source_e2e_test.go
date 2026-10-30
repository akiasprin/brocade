package scenarios

import (
	"context"
	"fmt"
	stdnet "net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
)

const frozenAlice = "01010101-0101-0101-0101-010101010101"
const frozenBob = "02020202-0202-0202-0202-020202020202"

func frozenWire(t *testing.T, protocol string, server bool) frozenObject {
	if protocol == "vless" {
		return nil
	}
	network := "tcp"
	if protocol == "hysteria" {
		network = "hysteria"
	}
	tls := frozenObject{"serverName": "localhost", "allowInsecure": true}
	if server {
		c, _ := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		certificate, key := c.ToPEM()
		tls = frozenObject{"certificates": []any{frozenObject{"certificate": []string{string(certificate)}, "key": []string{string(key)}}}}
	}
	wire := frozenObject{"network": network, "security": "tls", "tlsSettings": tls}
	if network == "hysteria" {
		wire["hysteriaSettings"] = frozenObject{"version": 2}
	}
	return wire
}
func frozenCredentials(protocol, id, email string) frozenObject {
	switch protocol {
	case "vless":
		return frozenObject{"id": id, "email": email}
	case "anytls":
		return frozenObject{"password": id, "email": email}
	default:
		return frozenObject{"auth": id, "email": email}
	}
}
func frozenIngress(t *testing.T, protocol string, port int) frozenObject {
	users := []any{frozenCredentials(protocol, frozenAlice, "alice@tenant#entry"), frozenCredentials(protocol, frozenBob, "bob@tenant#entry")}
	settings := frozenObject{"users": users}
	if protocol == "vless" {
		settings = frozenObject{"clients": users, "decryption": "none"}
	}
	if protocol == "hysteria" {
		settings["version"] = 2
	}
	c := frozenObject{"tag": "entry", "listen": "127.0.0.1", "port": port, "protocol": protocol, "settings": settings}
	if wire := frozenWire(t, protocol, true); wire != nil {
		c["streamSettings"] = wire
	}
	return c
}
func frozenClient(t *testing.T, protocol string, port, target int, id, source string) int {
	local := frozenPort(t)
	settings := frozenObject{"address": "127.0.0.1", "port": port}
	switch protocol {
	case "vless":
		settings["id"] = id
		settings["encryption"] = "none"
	case "anytls":
		settings["password"] = id
	default:
		settings["auth"] = id
		settings["version"] = 2
	}
	outbound := frozenObject{"protocol": protocol, "settings": settings, "sendThrough": source}
	if wire := frozenWire(t, protocol, false); wire != nil {
		if protocol == "hysteria" {
			wire["hysteriaSettings"].(frozenObject)["auth"] = id
		}
		outbound["streamSettings"] = wire
	}
	in := frozenDoko(local, target)
	in["settings"].(frozenObject)["network"] = "tcp,udp"
	frozenStart(t, frozenObject{"inbounds": []any{in}, "outbounds": []any{outbound}})
	return local
}
func TestBrocadeFrozenSourceBansAndAttribution(t *testing.T) {
	frozenBinary(t)
	for _, protocol := range []string{"vless", "anytls", "hysteria"} {
		t.Run(protocol, func(t *testing.T) {
			echo := frozenEcho(t)
			frozenPacketEcho(t, echo)
			entry := frozenPort(t)
			path := filepath.Join(t.TempDir(), "source.json")
			policy := frozenObject{"revision": 1, "node_ips": []string{}, "users": []any{}}
			frozenWrite(t, path, policy)
			api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{frozenIngress(t, protocol, entry)}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_SOURCE_ACCESS_FILE="+path, "XRAY_BROCADE_SITE_USAGE_ENABLED=1", "XRAY_BROCADE_SITE_ACCOUNTING_FILE="+filepath.Join(t.TempDir(), "sites.bin"))
			stats := frozenStats(t, api)
			alice := frozenClient(t, protocol, entry, echo, frozenAlice, "127.0.0.2")
			bob := frozenClient(t, protocol, entry, echo, frozenBob, "127.0.0.3")
			a := frozenDirect(t, alice, "127.0.0.1")
			b := frozenDirect(t, bob, "127.0.0.1")
			payload := make([]byte, 4096)
			for i := range payload {
				payload[i] = byte(i)
			}
			if err := frozenRoundtrip(a, payload); err != nil {
				t.Fatal(err)
			}
			if err := frozenRoundtrip(b, payload); err != nil {
				t.Fatal(err)
			}
			// Online RPC must report the authenticated physical source, protocol and bytes.
			frozenEventually(t, 2*time.Second, func() bool {
				ctx, cancel := context.WithTimeout(context.Background(), time.Second)
				defer cancel()
				r, err := stats.GetUsersStats(ctx, &statscmd.GetUsersStatsRequest{IncludeTraffic: true})
				if err != nil {
					t.Fatal(err)
				}
				found := 0
				for _, u := range r.Users {
					want := ""
					if u.Email == "alice@tenant#entry" {
						want = "127.0.0.2"
					}
					if u.Email == "bob@tenant#entry" {
						want = "127.0.0.3"
					}
					if want == "" {
						continue
					}
					for _, ip := range u.Ips {
						if ip.Ip == want && ip.UplinkBytes == 4096 && ip.DownlinkBytes == 4096 && len(ip.Protocols) == 1 && ip.Protocols[0] == protocol {
							found++
						}
					}
				}
				return found == 2
			})
			counters := frozenCounters(t, stats, "user>>>")
			for _, user := range []string{"alice", "bob"} {
				for _, d := range []string{"uplink", "downlink"} {
					name := "user>>>" + user + "@tenant#entry>>>traffic>>>" + d
					if counters[name] != 4096 {
						t.Fatalf("%s=%d", name, counters[name])
					}
				}
			}
			t.Logf("two users exact counters=4096/4096, physical IP and %s attribution verified", protocol)
			aliceUDP := frozenUDPDial(t, alice)
			bobUDP := frozenUDPDial(t, bob)
			if err := frozenRoundtrip(aliceUDP, []byte("alice UDP")); err != nil {
				t.Fatal(err)
			}
			if err := frozenRoundtrip(bobUDP, []byte("bob UDP")); err != nil {
				t.Fatal(err)
			}
			policy["revision"] = 2
			policy["users"] = []any{frozenObject{"tenant_id": "tenant", "user_id": "alice", "ips": []string{"127.0.0.2"}}}
			frozenWrite(t, path, policy)
			frozenEventually(t, 2*time.Second, func() bool {
				return frozenCounters(t, stats, "brocade>>>source_access>>>revision")["brocade>>>source_access>>>revision"] == 2
			})
			if err := frozenRoundtrip(a, []byte("denied")); err == nil {
				t.Fatal("user ban left original carrier passing traffic")
			}
			fresh := frozenDirect(t, alice, "127.0.0.1")
			if err := frozenRoundtrip(fresh, []byte("denied")); err == nil {
				t.Fatal("user ban admitted new traffic")
			}
			if err := frozenRoundtrip(b, []byte("bob survives")); err != nil {
				t.Fatalf("other user's original socket interrupted: %v", err)
			}
			if err := frozenRoundtrip(aliceUDP, []byte("denied UDP")); err == nil {
				t.Fatal("user ban left UDP flowing")
			}
			if err := frozenRoundtrip(bobUDP, []byte("bob UDP survives")); err != nil {
				t.Fatal(err)
			}
			policy["revision"] = 3
			policy["node_ips"] = []string{"127.0.0.3"}
			frozenWrite(t, path, policy)
			frozenEventually(t, 2*time.Second, func() bool {
				return frozenCounters(t, stats, "brocade>>>source_access>>>revision")["brocade>>>source_access>>>revision"] == 3
			})
			if err := frozenRoundtrip(b, []byte("denied")); err == nil {
				t.Fatal("node ban left original carrier passing traffic")
			}
			if err := os.WriteFile(path, []byte("corrupt"), 0o600); err != nil {
				t.Fatal(err)
			}
			frozenEventually(t, 2*time.Second, func() bool {
				return frozenCounters(t, stats, "brocade>>>source_access>>>healthy")["brocade>>>source_access>>>healthy"] == 0
			})
			if err := frozenRoundtrip(bobUDP, []byte("corrupt policy cannot unban")); err == nil {
				t.Fatal("corrupt checkpoint removed ban")
			}
			policy["revision"] = 4
			policy["node_ips"] = []string{}
			policy["users"] = []any{}
			frozenWrite(t, path, policy)
			frozenEventually(t, 2*time.Second, func() bool {
				return frozenCounters(t, stats, "brocade>>>source_access>>>revision")["brocade>>>source_access>>>revision"] == 4
			})
			// Fresh client processes avoid application-specific carrier reconnect backoff.
			recovered := frozenClient(t, protocol, entry, echo, frozenAlice, "127.0.0.2")
			if err := frozenRoundtrip(frozenDirect(t, recovered, "127.0.0.1"), []byte("unbanned")); err != nil {
				t.Fatal(err)
			}
			t.Log("user ban, node ban, original-socket revocation, unrelated-user isolation and unban verified")
		})
	}
}

func TestBrocadeFrozenSiteStatisticsDoNotDuplicateRelay(t *testing.T) {
	frozenBinary(t)
	echo := frozenEcho(t)
	// Plain HTTP metadata is enough to classify without modifying the route.
	entry := frozenPort(t)
	in := frozenIngress(t, "vless", entry)
	in["sniffing"] = frozenObject{"enabled": true, "destOverride": []string{"http"}, "routeOnly": true}
	api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{in}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_SITE_USAGE_ENABLED=1", "XRAY_BROCADE_SITE_ACCOUNTING_FILE="+filepath.Join(t.TempDir(), "sites.bin"))
	client := frozenClient(t, "vless", entry, echo, frozenAlice, "127.0.0.2")
	data := []byte("GET / HTTP/1.1\r\nHost: www.youtube.com\r\nConnection: keep-alive\r\n\r\n")
	c := frozenDirect(t, client, "127.0.0.1")
	if err := frozenRoundtrip(c, data); err != nil {
		t.Fatal(err)
	}
	stats := frozenStats(t, api)
	prefix := "site>>>alice@tenant#entry>>>youtube>>>traffic>>>"
	frozenEventually(t, 2*time.Second, func() bool {
		m := frozenCounters(t, stats, "site>>>")
		return m[prefix+"uplink"] == int64(len(data)) && m[prefix+"downlink"] == int64(len(data))
	})
	// A hop identity is not an authenticated final user and must create no site rows.
	relay := frozenIngress(t, "vless", frozenPort(t))
	relay["settings"].(frozenObject)["clients"] = []any{frozenCredentials("vless", frozenAlice, "hop-relay")}
	relay["sniffing"] = in["sniffing"]
	api2, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{relay}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_SITE_USAGE_ENABLED=1", "XRAY_BROCADE_SITE_ACCOUNTING_FILE="+filepath.Join(t.TempDir(), "sites.bin"))
	cp := frozenClient(t, "vless", relay["port"].(int), echo, frozenAlice, "127.0.0.2")
	if err := frozenRoundtrip(frozenDirect(t, cp, "127.0.0.1"), data); err != nil {
		t.Fatal(err)
	}
	for name := range frozenCounters(t, frozenStats(t, api2), "site>>>") {
		if strings.HasPrefix(name, "site>>>hop-relay") {
			t.Fatalf("relay duplicated site accounting: %s", name)
		}
	}
	t.Logf("site and user attribution exact payload=%d bytes/direction; relay produces no final-user site rows", len(data))
}

func frozenUDPDial(t *testing.T, port int) stdnet.Conn {
	t.Helper()
	c, err := stdnet.DialTimeout("udp", fmt.Sprintf("127.0.0.1:%d", port), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}
func frozenPacketEcho(t *testing.T, port int) {
	t.Helper()
	c, err := stdnet.ListenPacket("udp", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	go func() {
		b := make([]byte, 65536)
		for {
			n, peer, err := c.ReadFrom(b)
			if err != nil {
				return
			}
			_, _ = c.WriteTo(b[:n], peer)
		}
	}()
}

func TestBrocadeFrozenOtherDomainAndFailedTargetAttribution(t *testing.T) {
	frozenBinary(t)
	echo := frozenEcho(t)
	port := frozenPort(t)
	in := frozenIngress(t, "vless", port)
	in["sniffing"] = frozenObject{"enabled": true, "destOverride": []string{"http", "tls"}, "routeOnly": true}
	dir := t.TempDir()
	other := filepath.Join(dir, "other.json")
	failed := filepath.Join(dir, "failed.json")
	frozenWrite(t, other, frozenObject{"enabled": true})
	frozenWrite(t, failed, frozenObject{"enabled": true})
	api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{in}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_OTHER_DOMAIN_CONTROL_FILE="+other, "XRAY_BROCADE_SNIFFING_FAILED_CONTROL_FILE="+failed)
	client := frozenClient(t, "vless", port, echo, frozenAlice, "127.0.0.2")
	stats := frozenStats(t, api)
	for _, item := range []struct {
		pattern, key string
		data         []byte
	}{
		{"otherdomain>>>", "other.e2e.example", []byte("GET / HTTP/1.1\r\nHost: other.e2e.example\r\n\r\n")},
		{"sniffingfailed>>>", fmt.Sprintf("tcp://127.0.0.1:%d", echo), []byte("opaque unrecognized protocol")},
	} {
		if err := frozenRoundtrip(frozenDirect(t, client, "127.0.0.1"), item.data); err != nil {
			t.Fatal(err)
		}
		frozenEventually(t, 2*time.Second, func() bool {
			m := frozenCounters(t, stats, item.pattern)
			matched := 0
			for name, value := range m {
				if strings.Contains(name, ">>>alice@tenant#entry>>>"+item.key+">>>") && value == int64(len(item.data)) {
					matched++
				}
			}
			return matched == 2
		})
		t.Logf("%s label/target exact %d bytes per direction", item.pattern, len(item.data))
	}
}
