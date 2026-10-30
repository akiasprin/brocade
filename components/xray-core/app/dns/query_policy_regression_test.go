package dns

import (
	"context"
	"errors"
	stdnet "net"
	"testing"
	"time"

	mdns "github.com/miekg/dns"
	"github.com/xtls/xray-core/common/net"
	dnsfeature "github.com/xtls/xray-core/features/dns"
	"golang.org/x/net/dns/dnsmessage"
)

func TestRuleDNSSharedWorkOutlivesLeaderDeadline(t *testing.T) {
	for _, scoped := range []bool{true, false} {
		t.Run(map[bool]string{true: "rule", false: "machine"}[scoped], func(t *testing.T) {
			fixture := newUDPRuleFixture(t, func(n int, q *mdns.Msg) *mdns.Msg {
				if n == 1 {
					time.Sleep(100 * time.Millisecond)
					return dnsReply(q, mdns.RcodeSuccess)
				}
				return nil
			})
			client := ruleClient(fixture.server, 400*time.Millisecond)
			client.skipFallback = scoped
			early, cancel := context.WithTimeout(ruleDNSContext(), 30*time.Millisecond)
			defer cancel()
			first := make(chan error, 1)
			go func() {
				_, _, err := client.QueryIP(early, "test.example", dnsfeature.IPOption{IPv4Enable: true})
				first <- err
			}()
			<-fixture.first
			ips, _, err := client.QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
			if firstErr := <-first; !errors.Is(firstErr, context.DeadlineExceeded) {
				t.Fatalf("short waiter did not exit on its own deadline: %v", firstErr)
			}
			if err != nil || len(ips) == 0 {
				t.Fatalf("healthy waiter inherited the leader's deadline: %v", err)
			}
			record := fixture.server.cacheController.findRecords("test.example.")
			if record == nil || !positiveRecord(record.A) {
				t.Fatal("individual timeout poisoned the shared DNS cache")
			}
		})
	}
}

func TestRuleDNSCanceledCallerStartsNoSharedWork(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	ctx, cancel := context.WithCancel(ruleDNSContext())
	cancel()
	_, _, err := ruleClient(fixture.server, 100*time.Millisecond).QueryIP(ctx, "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("expected canceled caller, got %v", err)
	}
	if fixture.count.Load() != 0 || fixture.server.cacheController.findRecords("test.example.") != nil {
		t.Fatal("canceled caller sent packets or cached a personal cancellation")
	}
}

func TestRuleDNSTruncatedRefreshPreservesPositiveStale(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg {
		answer := dnsReply(q, mdns.RcodeSuccess)
		answer.Truncated = true
		return answer
	})
	cache := fixture.server.cacheController
	cache.ips["test.example."] = &record{A: &IPRecord{Expire: time.Now().Add(-time.Minute), IP: []net.IP{stdnet.ParseIP("192.0.2.9")}, RCode: dnsmessage.RCodeSuccess}}
	ips, _, err := ruleClient(fixture.server, 200*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if err != nil || len(ips) != 1 {
		t.Fatalf("stale positive was unavailable: %v", err)
	}
	eventuallyDNS(t, func() bool {
		rec := cache.findRecords("test.example.")
		return rec != nil && rec.A != nil && rec.A.RetryAfter.After(time.Now())
	})
	rec := cache.findRecords("test.example.")
	if !positiveRecord(rec.A) || !rec.A.IP[0].Equal(stdnet.ParseIP("192.0.2.9")) {
		t.Fatalf("truncated refresh erased the usable address: %+v", rec.A)
	}
	if fixture.count.Load() != 2 {
		t.Fatalf("expected UDP plus EDNS attempt, got %d", fixture.count.Load())
	}
}

// Diagnostics may wait for the real resolver: cold and warm cache state must not
// change a confirmed ordinary-family conclusion. Ordinary lookups do not add it.
func TestRuleDNSDiagnosticUsesActualResolverConsistently(t *testing.T) {
	for _, warm := range []bool{false, true} {
		t.Run(map[bool]string{false: "cold", true: "warm"}[warm], func(t *testing.T) {
			fixture := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg {
				time.Sleep(60 * time.Millisecond)
				return dnsReply(q, mdns.RcodeSuccess)
			})
			client := ruleClient(fixture.server, 400*time.Millisecond)
			client.ipOption = &dnsfeature.IPOption{IPv4Enable: true}
			if warm {
				if _, _, err := client.QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true}); err != nil {
					t.Fatal(err)
				}
			}
			result := client.query(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv6Enable: true}, true)
			if !emptyQuery(result.err) || !result.other || fixture.count.Load() < 1 || fixture.count.Load() > 5 {
				t.Fatalf("diagnosis changed with cache state: warm=%v sends=%d result=%+v", warm, fixture.count.Load(), result)
			}
			// A reply arriving after the first retry is still valid evidence;
			// consistency concerns the answer, not a fixed packet count.
			if !warm && (result.attempts == 0 || result.elapsed < 50) {
				t.Fatalf("cold diagnosis skipped actual resolver work: %+v", result)
			}
		})
	}
}

func TestRuleDNSDiagnosticKeepsOneMemberBudget(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	client := ruleClient(fixture.server, 160*time.Millisecond)
	client.ipOption = &dnsfeature.IPOption{IPv4Enable: true}
	result := client.query(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv6Enable: true}, true)
	if !emptyQuery(result.err) || result.other || fixture.count.Load() != 5 || result.attempts != 5 {
		t.Fatalf("failed diagnosis invented evidence or exceeded member quota: sends=%d result=%+v", fixture.count.Load(), result)
	}
}

func TestRuleDNSOrdinaryLookupDoesNotSendSupplementaryQueries(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	client := ruleClient(fixture.server, 160*time.Millisecond)
	client.ipOption = &dnsfeature.IPOption{IPv4Enable: true}
	result := client.query(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv6Enable: true}, false)
	if !emptyQuery(result.err) || fixture.count.Load() != 0 {
		t.Fatalf("ordinary lookup started supplementary work: %+v", result)
	}
}
