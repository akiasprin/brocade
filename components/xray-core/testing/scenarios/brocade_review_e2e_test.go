package scenarios

import (
	"bytes"
	"context"
	stdtls "crypto/tls"
	"encoding/binary"
	"fmt"
	"io"
	stdnet "net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/proxyman"
	handlers "github.com/xtls/xray-core/app/proxyman/command"
	"github.com/xtls/xray-core/app/router"
	routingcmd "github.com/xtls/xray-core/app/router/command"
	statscmd "github.com/xtls/xray-core/app/stats/command"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

func reviewChannel(t *testing.T, api int) *grpc.ClientConn {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", api), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

func reviewClosed(t *testing.T, c stdnet.Conn) {
	t.Helper()
	_ = c.SetReadDeadline(time.Now().Add(time.Second))
	var b [1]byte
	_, err := c.Read(b[:])
	if err == nil {
		t.Fatal("carrier still returned data after removal")
	}
	if timeout, ok := err.(stdnet.Error); ok && timeout.Timeout() {
		t.Fatal("timeout does not prove carrier closure")
	}
}

func TestBrocadeFrozenSourceWindowRetainsClosedTraffic(t *testing.T) {
	frozenBinary(t)
	echo, entry := frozenEcho(t), frozenPort(t)
	policy := frozenPolicy()
	level := policy["levels"].(frozenObject)["0"].(frozenObject)
	level["connIdle"], level["uplinkOnly"], level["downlinkOnly"] = 1, 1, 1
	api, _ := frozenStart(t, frozenObject{"policy": policy, "inbounds": []any{frozenIngress(t, "vless", entry)}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_ACCOUNTING_FILE="+filepath.Join(t.TempDir(), "accounting.bin"))
	client := frozenClient(t, "vless", entry, echo, frozenAlice, "127.0.0.2")
	c := frozenDirect(t, client, "127.0.0.1")
	payload := bytes.Repeat([]byte{0x57}, 3073)
	if err := frozenRoundtrip(c, payload); err != nil {
		t.Fatal(err)
	}
	_ = c.Close()
	stats := frozenStats(t, api)
	frozenEventually(t, 3*time.Second, func() bool {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		users, err := stats.GetUsersStats(ctx, &statscmd.GetUsersStatsRequest{})
		return err == nil && len(users.Users) == 0
	})
	frozenEventually(t, 2*time.Second, func() bool {
		var up, down int64
		for name, value := range frozenCounters(t, stats, "sourcetraffic>>>") {
			if !strings.Contains(name, ">>>alice@tenant#entry>>>127.0.0.2>>>") {
				continue
			}
			if strings.HasSuffix(name, ">>>uplink") {
				up += value
			} else if strings.HasSuffix(name, ">>>downlink") {
				down += value
			}
		}
		return up == int64(len(payload)) && down == up
	})
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if _, err := stats.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: "sourcetraffic>>>", Reset_: true}); status.Code(err) != codes.InvalidArgument {
		t.Fatalf("source window accepted reset: %v", err)
	}
	for name := range frozenCounters(t, stats, "") {
		if strings.HasPrefix(name, "sourcetraffic>>>") {
			t.Fatal("source IP leaked into broad query")
		}
	}
	t.Log("closed source is offline; separate source window retains exactly 3073/3073 bytes; broad query and reset restrictions enforced")
}

func TestBrocadeFrozenDetailCheckpointAdmission(t *testing.T) {
	frozenBinary(t)
	echo, entry := frozenEcho(t), frozenPort(t)
	in := frozenIngress(t, "vless", entry)
	in["sniffing"] = frozenObject{"enabled": true, "destOverride": []string{"http"}, "routeOnly": true}
	control := filepath.Join(t.TempDir(), "other.json")
	frozenWrite(t, control, frozenObject{"enabled": true, "top_n": 1, "max_domains_per_user": 1, "node_capacity": 1})
	api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{in}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_OTHER_DOMAIN_CONTROL_FILE="+control)
	client := frozenClient(t, "vless", entry, echo, frozenAlice, "127.0.0.2")
	stats := frozenStats(t, api)
	exchange := func(domain string) int64 {
		c := frozenDirect(t, client, "127.0.0.1")
		defer c.Close()
		data := []byte("GET / HTTP/1.1\r\nHost: " + domain + "\r\n\r\n")
		if err := frozenRoundtrip(c, data); err != nil {
			t.Fatal(err)
		}
		return int64(len(data))
	}
	first := exchange("checkpoint-a.example")
	var epoch, segment string
	frozenEventually(t, time.Second, func() bool {
		for name, value := range frozenCounters(t, stats, "otherdomain>>>v2") {
			if suffix, ok := strings.CutPrefix(name, "otherdomain>>>epoch:"); ok {
				epoch = suffix
			}
			parts := strings.Split(name, ">>>")
			if len(parts) == 6 && parts[3] == "checkpoint-a.example" && parts[5] == "uplink" && value == first {
				segment = parts[4]
			}
		}
		return epoch != "" && segment != ""
	})
	query := func(pattern string) error {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_, err := stats.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: pattern})
		return err
	}
	if err := query("otherdomain>>>ack>>>" + epoch + ">>>" + segment + ":-1:0"); status.Code(err) != codes.InvalidArgument {
		t.Fatalf("malformed ACK accepted: %v", err)
	}
	_ = exchange("checkpoint-b.example")
	for name := range frozenCounters(t, stats, "otherdomain>>>") {
		if strings.Contains(name, "checkpoint-b.example") {
			t.Fatal("unacknowledged domain evicted at capacity")
		}
	}
	if err := query(fmt.Sprintf("otherdomain>>>ack>>>%s>>>%s:%d:%d", epoch, segment, first, first)); err != nil {
		t.Fatal(err)
	}
	third := exchange("checkpoint-c.example")
	frozenEventually(t, time.Second, func() bool {
		for name, value := range frozenCounters(t, stats, "otherdomain>>>") {
			if strings.Contains(name, "checkpoint-c.example") && strings.HasSuffix(name, ">>>uplink") && value == third {
				return true
			}
		}
		return false
	})
	t.Log("real v2 snapshot/ACK: capacity blocks optional observations before ACK without blocking traffic; acknowledged cold segment permits a new domain")
}

func TestBrocadeFrozenForwardingRemovalAndGuard(t *testing.T) {
	frozenBinary(t)
	echo := frozenEcho(t)
	outbound := frozenFreedom()
	outbound[0].(frozenObject)["tag"] = "out:forward"
	api, _ := frozenStart(t, frozenObject{"outbounds": outbound})
	h := handlers.NewHandlerServiceClient(reviewChannel(t, api))
	add := func(tag string, blocked []string, host string) int {
		port := frozenPort(t)
		proxy := &dokodemo.Config{RewriteAddress: xnet.NewIPOrDomain(xnet.ParseAddress(host)), RewritePort: uint32(echo), AllowedNetworks: []xnet.Network{xnet.Network_TCP}}
		if blocked != nil {
			proxy.ForwardingTargetPolicy = &dokodemo.ForwardingTargetPolicy{BlockedCidrs: blocked}
		}
		receiver := &proxyman.ReceiverConfig{Listen: xnet.NewIPOrDomain(xnet.LocalHostIP), PortList: &xnet.PortList{Range: []*xnet.PortRange{{From: uint32(port), To: uint32(port)}}}}
		message := serial.ToTypedMessage(proxy)
		if strings.HasPrefix(tag, "fwd:") {
			message = serial.ToTypedMessage(&dokodemo.ForwardingConfig{Config: proxy})
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_, err := h.AddInbound(ctx, &handlers.AddInboundRequest{Inbound: &core.InboundHandlerConfig{Tag: tag, ReceiverSettings: serial.ToTypedMessage(receiver), ProxySettings: message}})
		if err != nil {
			t.Fatal(err)
		}
		return port
	}
	remove := func(tag string) {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if _, err := h.RemoveInbound(ctx, &handlers.RemoveInboundRequest{Tag: tag}); err != nil {
			t.Fatal(err)
		}
	}
	one := frozenDirect(t, add("fwd:101", []string{"127.0.0.9/32"}, "127.0.0.1"), "127.0.0.1")
	two := frozenDirect(t, add("fwd:102", []string{"127.0.0.9/32"}, "127.0.0.1"), "127.0.0.1")
	ordinary := frozenDirect(t, add("ordinary", nil, "127.0.0.1"), "127.0.0.1")
	for _, c := range []stdnet.Conn{one, two, ordinary} {
		if err := frozenRoundtrip(c, []byte("established")); err != nil {
			t.Fatal(err)
		}
	}
	remove("fwd:101")
	reviewClosed(t, one)
	for _, c := range []stdnet.Conn{two, ordinary} {
		if err := frozenRoundtrip(c, []byte("survives other forwarding removal")); err != nil {
			t.Fatal(err)
		}
	}
	remove("ordinary")
	if err := frozenRoundtrip(ordinary, []byte("ordinary inbound drains")); err != nil {
		t.Fatal(err)
	}
	remove("fwd:102")
	reviewClosed(t, two)
	for i, host := range []string{"127.0.0.1", "localhost", "::ffff:127.0.0.1"} {
		blocked := frozenDirect(t, add(fmt.Sprintf("fwd:%d", 110+i), []string{"127.0.0.0/8", "::1/128"}, host), "127.0.0.1")
		if err := frozenRoundtrip(blocked, []byte("must not reach loopback")); err == nil {
			t.Fatalf("forwarding guard permitted %s", host)
		}
	}
	newOne := frozenDirect(t, add("fwd:101", []string{"127.0.0.9/32"}, "127.0.0.1"), "127.0.0.1")
	if err := frozenRoundtrip(newOne, []byte("restored as new connection")); err != nil {
		t.Fatal(err)
	}
	t.Log("dynamic RemoveInbound closes only fwd-tag carriers; ordinary carriers drain; literal, resolved localhost and mapped IPv4 loopback guards reject")
}

func reviewMarkedTarget(t *testing.T, marker byte) int {
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
			go func() {
				defer c.Close()
				_ = c.SetDeadline(time.Now().Add(5 * time.Second))
				_, _ = c.Write([]byte{marker})
				_, _ = io.Copy(c, c)
			}()
		}
	}()
	return l.Addr().(*stdnet.TCPAddr).Port
}

func TestBrocadeFrozenSniffFailureSelectsRoute(t *testing.T) {
	frozenBinary(t)
	entry := frozenPort(t)
	in := frozenIngress(t, "vless", entry)
	in["sniffing"] = frozenObject{"enabled": true, "destOverride": []string{"http", "tls"}, "routeOnly": true}
	outbound := func(tag string, target int) frozenObject {
		o := frozenFreedom()[0].(frozenObject)
		o["tag"] = tag
		o["settings"].(frozenObject)["redirect"] = fmt.Sprintf("127.0.0.1:%d", target)
		return o
	}
	api, _ := frozenStart(t, frozenObject{"api": frozenObject{"services": []string{"StatsService", "HandlerService", "RoutingService"}}, "inbounds": []any{in}, "outbounds": []any{outbound("normal", reviewMarkedTarget(t, 'N')), outbound("failed", reviewMarkedTarget(t, 'F'))}, "routing": frozenObject{"rules": []any{frozenObject{"type": "field", "attrs": frozenObject{"xray.sniffing": "^failed$"}, "outboundTag": "failed"}}}})
	client := frozenClient(t, "vless", entry, frozenPort(t), frozenAlice, "127.0.0.2")
	for _, test := range []struct {
		data string
		want byte
	}{
		{"opaque not a recognized protocol", 'F'},
		{"GET / HTTP/1.1\r\nHost: recovered.example\r\n\r\n", 'N'},
		{"GET / HTTP/1.1\r\nHost: recovered.example\r\nxray.sniffing: failed\r\n\r\n", 'N'},
	} {
		c := frozenDirect(t, client, "127.0.0.1")
		_ = c.SetDeadline(time.Now().Add(3 * time.Second))
		if _, err := c.Write([]byte(test.data)); err != nil {
			t.Fatal(err)
		}
		got := make([]byte, len(test.data)+1)
		if _, err := io.ReadFull(c, got); err != nil {
			t.Fatal(err)
		}
		if got[0] != test.want || string(got[1:]) != test.data {
			t.Fatalf("route=%q want=%q or payload changed", got[0], test.want)
		}
		_ = c.Close()
	}
	routes := routingcmd.NewRoutingServiceClient(reviewChannel(t, api))
	update := func(config *router.Config) error {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_, err := routes.AddRule(ctx, &routingcmd.AddRuleRequest{Config: serial.ToTypedMessage(config)})
		return err
	}
	if err := update(&router.Config{Rule: []*router.RoutingRule{{TargetTag: &router.RoutingRule_Tag{Tag: "normal"}}}}); err == nil {
		t.Fatal("invalid replacement routing table was accepted")
	}
	old := frozenDirect(t, client, "127.0.0.1")
	_ = old.SetDeadline(time.Now().Add(3 * time.Second))
	_, _ = old.Write([]byte("old"))
	var before [4]byte
	if _, err := io.ReadFull(old, before[:]); err != nil || string(before[:]) != "Fold" {
		t.Fatalf("failed replacement damaged original table: %q %v", before, err)
	}
	if err := update(&router.Config{Rule: []*router.RoutingRule{{Networks: []xnet.Network{xnet.Network_TCP}, TargetTag: &router.RoutingRule_Tag{Tag: "normal"}}}}); err != nil {
		t.Fatal(err)
	}
	next := frozenDirect(t, client, "127.0.0.1")
	_ = next.SetDeadline(time.Now().Add(3 * time.Second))
	_, _ = next.Write([]byte("new"))
	var after [4]byte
	if _, err := io.ReadFull(next, after[:]); err != nil || string(after[:]) != "Nnew" {
		t.Fatalf("new connection did not use replacement table: %q %v", after, err)
	}
	if err := frozenRoundtrip(old, []byte("existing route survives")); err != nil {
		t.Fatal(err)
	}
	t.Log("opaque IP traffic uses failed route; recovered HTTP domain uses normal route; forged HTTP attribute cannot select failed route")
	t.Log("failed full-table replacement preserves original route; valid replacement switches new connections while existing stream remains intact")
}

func TestBrocadeFrozenTLSVerificationPolicy(t *testing.T) {
	frozenBinary(t)
	entry, echo := frozenPort(t), frozenEcho(t)
	api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{frozenIngress(t, "anytls", entry)}, "outbounds": frozenFreedom()})
	for _, mode := range []string{"default", "strict", "explicit-insecure"} {
		t.Run(mode, func(t *testing.T) {
			local := frozenPort(t)
			wire := frozenWire(t, "anytls", false)
			tls := wire["tlsSettings"].(frozenObject)
			delete(tls, "allowInsecure")
			if mode != "default" {
				tls["allowInsecure"] = mode == "explicit-insecure"
			}
			out := frozenObject{"protocol": "anytls", "settings": frozenObject{"address": "127.0.0.1", "port": entry, "password": frozenAlice}, "streamSettings": wire}
			frozenStart(t, frozenObject{"inbounds": []any{frozenDoko(local, echo)}, "outbounds": []any{out}})
			err := frozenRoundtrip(frozenDirect(t, local, "127.0.0.1"), []byte("verification policy"))
			if mode == "explicit-insecure" {
				if err != nil {
					t.Fatal(err)
				}
			} else {
				if err == nil {
					t.Fatal("self-signed certificate accepted without explicit allowInsecure")
				}
				if n := frozenCounters(t, frozenStats(t, api), "user>>>")["user>>>alice@tenant#entry>>>traffic>>>uplink"]; n != 0 {
					t.Fatalf("verification failure forwarded user payload: %d", n)
				}
			}
		})
	}
}

func TestBrocadeFrozenGeoIPFamilyRoutes(t *testing.T) {
	frozenBinary(t)
	assets := os.Getenv("BROCADE_E2E_ASSETS")
	if assets == "" {
		assets = "../../resources" // ordinary go test package working directory
	}
	assets, err := filepath.Abs(assets)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(assets, "geoip.dat")); err != nil {
		t.Fatal(err)
	}
	outbound := func(tag string, marker byte) frozenObject {
		o := frozenFreedom()[0].(frozenObject)
		o["tag"] = tag
		o["settings"].(frozenObject)["redirect"] = fmt.Sprintf("127.0.0.1:%d", reviewMarkedTarget(t, marker))
		return o
	}
	entry := frozenPort(t)
	frozenStart(t, frozenObject{"inbounds": []any{frozenIngress(t, "vless", entry)}, "outbounds": []any{outbound("normal", 'N'), outbound("cn4", '4'), outbound("cn6", '6')}, "routing": frozenObject{"rules": []any{frozenObject{"type": "field", "ip": []string{"geoip:cn@ipv4"}, "outboundTag": "cn4"}, frozenObject{"type": "field", "ip": []string{"geoip:cn@ipv6"}, "outboundTag": "cn6"}}}}, "xray.location.asset="+assets)
	for _, test := range []struct {
		ip   string
		want byte
	}{{"114.114.114.114", '4'}, {"240e::1", '6'}, {"8.8.8.8", 'N'}, {"2001:4860:4860::8888", 'N'}} {
		c := frozenDirect(t, entry, "127.0.0.1")
		_ = c.SetDeadline(time.Now().Add(2 * time.Second))
		request := append([]byte{0}, bytes.Repeat([]byte{1}, 16)...)
		request = append(request, 0, 1)
		request = binary.BigEndian.AppendUint16(request, 443)
		ip := stdnet.ParseIP(test.ip)
		if v4 := ip.To4(); v4 != nil {
			request = append(request, 1)
			request = append(request, v4...)
		} else {
			request = append(request, 3)
			request = append(request, ip.To16()...)
		}
		request = append(request, 'x')
		if _, err := c.Write(request); err != nil {
			t.Fatal(err)
		}
		var response [4]byte
		if _, err := io.ReadFull(c, response[:]); err != nil {
			t.Fatal(err)
		}
		if response != [4]byte{0, 0, test.want, 'x'} {
			t.Fatalf("%s routed with response %v", test.ip, response)
		}
		_ = c.Close()
	}
	t.Log("real geoip.dat: cn IPv4 and IPv6 select distinct routes; non-cn addresses use fallback, with local redirect targets")
}

func TestBrocadeFrozenCertificateRotationKeepsConnections(t *testing.T) {
	frozenBinary(t)
	dir := t.TempDir()
	certificatePath, keyPath := filepath.Join(dir, "cert.pem"), filepath.Join(dir, "key.pem")
	writePair := func() []byte {
		pair, _ := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		certificate, key := pair.ToPEM()
		for path, data := range map[string][]byte{certificatePath: certificate, keyPath: key} {
			if err := os.WriteFile(path+".tmp", data, 0o600); err != nil {
				t.Fatal(err)
			}
			if err := os.Rename(path+".tmp", path); err != nil {
				t.Fatal(err)
			}
		}
		parsed, err := stdtls.X509KeyPair(certificate, key)
		if err != nil {
			t.Fatal(err)
		}
		return parsed.Certificate[0]
	}
	first := writePair()
	port := frozenPort(t)
	in := frozenDoko(port, frozenEcho(t))
	in["streamSettings"] = frozenObject{"network": "tcp", "security": "tls", "tlsSettings": frozenObject{"certificates": []any{frozenObject{"certificateFile": certificatePath, "keyFile": keyPath, "reloadInterval": 1}}}}
	frozenStart(t, frozenObject{"inbounds": []any{in}, "outbounds": frozenFreedom()})
	dial := func() (*stdtls.Conn, error) {
		return stdtls.DialWithDialer(&stdnet.Dialer{Timeout: time.Second}, "tcp", fmt.Sprintf("127.0.0.1:%d", port), &stdtls.Config{ServerName: "localhost", InsecureSkipVerify: true})
	}
	original, err := dial()
	if err != nil {
		t.Fatal(err)
	}
	defer original.Close()
	if !bytes.Equal(original.ConnectionState().PeerCertificates[0].Raw, first) {
		t.Fatal("initial leaf mismatch")
	}
	if err := frozenRoundtrip(original, []byte("before rotation")); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(certificatePath, []byte("invalid certificate"), 0o600); err != nil {
		t.Fatal(err)
	}
	time.Sleep(1200 * time.Millisecond)
	retained, err := dial()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(retained.ConnectionState().PeerCertificates[0].Raw, first) {
		t.Fatal("invalid replacement discarded last valid certificate")
	}
	_ = retained.Close()
	second := writePair()
	frozenEventually(t, 4*time.Second, func() bool {
		c, err := dial()
		if err != nil {
			return false
		}
		defer c.Close()
		return bytes.Equal(c.ConnectionState().PeerCertificates[0].Raw, second)
	})
	if err := frozenRoundtrip(original, []byte("old connection survives new leaf")); err != nil {
		t.Fatal(err)
	}
	// A fresh standard TLS client without an explicit verification exception
	// still rejects the self-signed leaf used by this isolated fixture.
	strict, err := stdtls.DialWithDialer(&stdnet.Dialer{Timeout: time.Second}, "tcp", fmt.Sprintf("127.0.0.1:%d", port), &stdtls.Config{ServerName: "localhost"})
	if err == nil {
		_ = strict.Close()
		t.Fatal("untrusted self-signed certificate unexpectedly verified")
	}
	t.Log("real TLS/dokodemo: invalid file retains leaf; new valid pair rotates without restarting or interrupting established plaintext traffic")
}

func TestBrocadeFrozenHTTPFrontendsIgnoreForgedSource(t *testing.T) {
	frozenBinary(t)
	for _, network := range []string{"ws", "httpupgrade", "xhttp"} {
		t.Run(network, func(t *testing.T) {
			entry, local, echo := frozenPort(t), frozenPort(t), frozenEcho(t)
			policy := filepath.Join(t.TempDir(), "source.json")
			frozenWrite(t, policy, frozenObject{"revision": 1, "node_ips": []string{}, "users": []any{}})
			key := map[string]string{"ws": "wsSettings", "httpupgrade": "httpupgradeSettings", "xhttp": "xhttpSettings"}[network]
			serverWire := frozenObject{"network": network, key: frozenObject{"path": "/review"}}
			clientWire := frozenObject{"network": network, key: frozenObject{"path": "/review", "headers": frozenObject{"X-Forwarded-For": "203.0.113.77"}}}
			if network == "xhttp" {
				serverWire[key].(frozenObject)["mode"] = "packet-up"
				clientWire[key].(frozenObject)["mode"] = "packet-up"
			}
			in := frozenIngress(t, "vless", entry)
			in["streamSettings"] = serverWire
			api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{in}, "outbounds": frozenFreedom()}, "XRAY_BROCADE_SOURCE_ACCESS_FILE="+policy)
			out := frozenObject{"protocol": "vless", "settings": frozenObject{"address": "127.0.0.1", "port": entry, "id": frozenAlice, "encryption": "none"}, "streamSettings": clientWire, "sendThrough": "127.0.0.2"}
			frozenStart(t, frozenObject{"inbounds": []any{frozenDoko(local, echo)}, "outbounds": []any{out}})
			c := frozenDirect(t, local, "127.0.0.1")
			if err := frozenRoundtrip(c, []byte("authenticated frontend traffic")); err != nil {
				t.Fatal(err)
			}
			stats := frozenStats(t, api)
			frozenEventually(t, 2*time.Second, func() bool {
				ctx, cancel := context.WithTimeout(context.Background(), time.Second)
				defer cancel()
				users, err := stats.GetUsersStats(ctx, &statscmd.GetUsersStatsRequest{})
				if err != nil {
					t.Fatal(err)
				}
				for _, user := range users.Users {
					for _, ip := range user.Ips {
						if ip.Ip == "203.0.113.77" {
							t.Fatal("untrusted XFF became authenticated source")
						}
						if user.Email == "alice@tenant#entry" && ip.Ip == "127.0.0.2" {
							return true
						}
					}
				}
				return false
			})
			frozenWrite(t, policy, frozenObject{"revision": 2, "node_ips": []string{"127.0.0.2"}, "users": []any{}})
			frozenEventually(t, 2*time.Second, func() bool {
				return frozenCounters(t, stats, "brocade>>>source_access>>>revision")["brocade>>>source_access>>>revision"] == 2
			})
			if err := frozenRoundtrip(c, []byte("physical source now banned")); err == nil {
				t.Fatal("physical source ban left carrier passing traffic")
			}
			t.Log("forged XFF ignored in online identity; ban of actual source stops existing carrier")
		})
	}
}

func TestBrocadeFrozenUserRevocationIsolation(t *testing.T) {
	frozenBinary(t)
	for _, protocol := range []string{"vless", "anytls", "hysteria"} {
		t.Run(protocol, func(t *testing.T) {
			entry, echo := frozenPort(t), frozenEcho(t)
			frozenPacketEcho(t, echo)
			api, _ := frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{frozenIngress(t, protocol, entry)}, "outbounds": frozenFreedom()})
			alicePort := frozenClient(t, protocol, entry, echo, frozenAlice, "127.0.0.2")
			bobPort := frozenClient(t, protocol, entry, echo, frozenBob, "127.0.0.3")
			alice := frozenDirect(t, alicePort, "127.0.0.1")
			bob := frozenDirect(t, bobPort, "127.0.0.1")
			aliceUDP, bobUDP := frozenUDPDial(t, alicePort), frozenUDPDial(t, bobPort)
			for _, c := range []stdnet.Conn{alice, bob, aliceUDP, bobUDP} {
				if err := frozenRoundtrip(c, []byte("before revocation")); err != nil {
					t.Fatal(err)
				}
			}
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			h := handlers.NewHandlerServiceClient(reviewChannel(t, api))
			if _, err := h.AlterInbound(ctx, &handlers.AlterInboundRequest{Tag: "entry", Operation: serial.ToTypedMessage(&handlers.RemoveUserOperation{Email: "alice@tenant#entry"})}); err != nil {
				t.Fatal(err)
			}
			for _, c := range []stdnet.Conn{alice, aliceUDP} {
				if err := frozenRoundtrip(c, []byte("revoked original session")); err == nil {
					t.Fatal("revoked user's established traffic survived")
				}
			}
			fresh := frozenDirect(t, alicePort, "127.0.0.1")
			if err := frozenRoundtrip(fresh, []byte("new session with revoked credential")); err == nil {
				t.Fatal("revoked credential admitted new traffic")
			}
			for _, c := range []stdnet.Conn{bob, bobUDP} {
				if err := frozenRoundtrip(c, []byte("Bob original session survives")); err != nil {
					t.Fatalf("revoking Alice interrupted Bob: %v", err)
				}
			}
			t.Log("RemoveUser stops original Alice TCP/UDP and rejects new traffic; Bob original TCP/UDP remain usable")
		})
	}
}
