package dns

import (
	"context"
	"github.com/xtls/xray-core/common/net"
	feature_dns "github.com/xtls/xray-core/features/dns"
	"testing"
	"time"
)

type traceMatcher struct{}

func (traceMatcher) Match(string) []uint32 { return []uint32{0} }
func (traceMatcher) MatchAny(string) bool  { return true }

type traceServer struct {
	v4, v6 bool
	fail   bool
}

func (*traceServer) Name() string         { return "diagnostic-fixture" }
func (*traceServer) IsDisableCache() bool { return false }
func (s *traceServer) QueryIP(ctx context.Context, _ string, option feature_dns.IPOption) ([]net.IP, uint32, error) {
	if s.fail {
		return nil, 0, context.DeadlineExceeded
	}
	if option.IPv4Enable && s.v4 {
		return []net.IP{net.ParseIP("192.0.2.1")}, 60, nil
	}
	if option.IPv6Enable && s.v6 {
		return []net.IP{net.ParseIP("2001:db8::1")}, 60, nil
	}
	return nil, 0, feature_dns.ErrEmptyResponse
}
func TestSerialTraceDistinguishesEmptyAnswersErrorsAndActualFallback(t *testing.T) {
	for _, tc := range []struct {
		name           string
		v6, stop, fail bool
		outcome        string
		other          bool
		steps          int
	}{
		{"UseIP A-only", false, false, false, "empty", true, 2},
		{"UseIP dual", true, false, false, "answer", false, 1},
		{"stop", false, true, false, "empty", true, 1},
		{"timeout", false, false, true, "error", false, 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			option := &feature_dns.IPOption{IPv4Enable: true, IPv6Enable: true}
			server := &DNS{ctx: context.Background(), domainMatcher: traceMatcher{}, matcherInfos: []*DomainMatcherInfo{{clientIdx: 0, domainRule: "fixture"}}, clients: []*Client{
				{server: &traceServer{v4: true, v6: tc.v6, fail: tc.fail}, tag: "dns:egress:fixture", skipFallback: true, finalQuery: tc.stop, ipOption: option, timeoutMs: time.Second},
				{server: &traceServer{v4: true, v6: true}, tag: "dns-out", ipOption: option, timeoutMs: time.Second},
			}}
			trace := &feature_dns.LookupTrace{}
			server.serialQuery("service.test", feature_dns.IPOption{IPv6Enable: true, Trace: trace})
			if !trace.Complete || trace.Version != 1 || len(trace.Steps) != tc.steps {
				t.Fatalf("unexpected trace: %+v", trace)
			}
			if trace.Steps[0].Outcome != tc.outcome || trace.Steps[0].Other != tc.other {
				t.Fatalf("unexpected primary evidence: %+v", trace.Steps[0])
			}
			if tc.steps == 2 && (trace.Steps[1].Scoped || trace.Steps[1].Outcome != "answer") {
				t.Fatal("fallback source was not recorded")
			}
		})
	}
}

type nxdomainTraceServer struct{ traceServer }

func (s *nxdomainTraceServer) QueryIP(ctx context.Context, domain string, option feature_dns.IPOption) ([]net.IP, uint32, error) {
	if option.IPv6Enable {
		return nil, 0, feature_dns.RCodeError(3)
	}
	return s.traceServer.QueryIP(ctx, domain, option)
}
func TestSerialTraceNXDOMAINIsAbsenceButTimeoutIsNot(t *testing.T) {
	option := &feature_dns.IPOption{IPv4Enable: true, IPv6Enable: true}
	server := &DNS{ctx: context.Background(), domainMatcher: traceMatcher{}, matcherInfos: []*DomainMatcherInfo{{clientIdx: 0}}, clients: []*Client{
		{server: &nxdomainTraceServer{traceServer{v4: true}}, tag: "dns:egress:fixture", skipFallback: true, ipOption: option, timeoutMs: time.Second},
		{server: &traceServer{v6: true}, tag: "dns-out", ipOption: option, timeoutMs: time.Second},
	}}
	trace := &feature_dns.LookupTrace{}
	server.serialQuery("service.test", feature_dns.IPOption{IPv6Enable: true, Trace: trace})
	if len(trace.Steps) != 2 || trace.Steps[0].Outcome != "empty" || !trace.Steps[0].Other || trace.Steps[1].Outcome != "answer" {
		t.Fatalf("unexpected NXDOMAIN evidence: %+v", trace)
	}
}
