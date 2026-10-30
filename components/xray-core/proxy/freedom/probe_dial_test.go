package freedom

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"syscall"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/probetrace"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type probeDialDNS struct{}

func (*probeDialDNS) Start() error      { return nil }
func (*probeDialDNS) Close() error      { return nil }
func (*probeDialDNS) Type() interface{} { return dns.ClientType() }
func (*probeDialDNS) LookupIP(_ string, option dns.IPOption) ([]xnet.IP, uint32, error) {
	if option.Trace != nil {
		option.Trace.Version = 1
		option.Trace.Complete = true
		option.Trace.Steps = []dns.LookupTraceStep{{Tag: "fixture", Outcome: "answer"}}
	}
	return []xnet.IP{xnet.ParseAddress("2001:db8::1").IP()}, 60, nil
}

type probeDialFailure struct{ calls int }

func (d *probeDialFailure) Dial(context.Context, xnet.Destination) (stat.Connection, error) {
	d.calls++
	return nil, &internet.ProbeDialError{Code: "ipv6_no_address", Cause: &os.SyscallError{Syscall: "connect", Err: syscall.ENETUNREACH}}
}
func (*probeDialFailure) DestIpAddress() xnet.IP                                { return nil }
func (*probeDialFailure) SetOutboundGateway(context.Context, *session.Outbound) {}

func TestProbeDialFailureRetainsFamilyCategoryAndOneActualAttempt(t *testing.T) {
	node := filepath.Join(t.TempDir(), "node-id")
	if err := os.WriteFile(node, []byte("actual-exit"), 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(probetrace.NodeFileEnv, node)
	t.Setenv(probetrace.ProfileEnv, `{"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","strategies":{"out:egress":"UseIP"}}`)
	internet.InitSystemDialer(&probeDialDNS{}, nil)
	t.Cleanup(func() { internet.InitSystemDialer(nil, nil) })
	dest := xnet.TCPDestination(xnet.DomainAddress("www.youtube.com"), 443)
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{Target: dest, Tag: "out:egress:probe-v6"}})
	state := &probetrace.State{ID: "00000000000000000000000000000001", Domain: "www.youtube.com"}
	ctx = probetrace.WithState(ctx, state)
	h := &Handler{}
	if err := h.Init(&Config{DomainStrategy: internet.DomainStrategy_FORCE_IP6}, nil); err != nil {
		t.Fatal(err)
	}
	dialer := &probeDialFailure{}
	link := &transport.Link{Reader: buf.NewReader(bytes.NewReader(nil)), Writer: buf.NewWriter(&bytes.Buffer{})}
	if err := h.Process(ctx, link, dialer); err == nil {
		t.Fatal("failed dial succeeded")
	}
	result := state.Result()
	if dialer.calls != 1 || result == nil || result.Error != "ipv6_no_address" || result.Family != "v6" || result.Connected || !result.Complete || result.Exit != "actual-exit" {
		t.Fatalf("attempts=%d evidence=%+v", dialer.calls, result)
	}
}
