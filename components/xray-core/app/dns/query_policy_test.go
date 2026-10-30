package dns

import (
	"context"
	"encoding/binary"
	"errors"
	"io"
	stdnet "net"
	"net/url"
	"sync/atomic"
	"testing"
	"time"

	mdns "github.com/miekg/dns"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	udp_proto "github.com/xtls/xray-core/common/protocol/udp"
	"github.com/xtls/xray-core/core"
	dnsfeature "github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
	"golang.org/x/net/dns/dnsmessage"
)

func ruleDNSContext() context.Context {
	return context.WithValue(context.Background(), core.XrayKey(1), &core.Instance{})
}

type ruleDNSDispatcher struct {
	routing.Dispatcher
	link *transport.Link
}

func (d *ruleDNSDispatcher) Dispatch(_ context.Context, destination net.Destination) (*transport.Link, error) {
	if destination.Network != net.Network_UDP {
		return nil, errors.New("unexpected DNS transport")
	}
	return d.link, nil
}

type udpRuleFixture struct {
	server *ClassicNameServer
	count  atomic.Int32
	first  chan *mdns.Msg
}

func newUDPRuleFixture(t *testing.T, reply func(int, *mdns.Msg) *mdns.Msg) *udpRuleFixture {
	t.Helper()
	ctx, cancel := context.WithCancel(ruleDNSContext())
	up, writer := pipe.New(pipe.WithSizeLimit(8192))
	reader, down := pipe.New(pipe.WithSizeLimit(8192))
	dispatcher := &ruleDNSDispatcher{link: &transport.Link{Reader: reader, Writer: writer}}
	fixture := &udpRuleFixture{first: make(chan *mdns.Msg, 1)}
	fixture.server = NewClassicNameServer(net.UDPDestination(net.ParseAddress("192.0.2.53"), 53), dispatcher, false, true, 129600, nil)
	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		for {
			packets, err := up.ReadMultiBuffer()
			if err != nil {
				return
			}
			for _, packet := range packets {
				query := new(mdns.Msg)
				err := query.Unpack(packet.Bytes())
				packet.Release()
				if err != nil {
					t.Error(err)
					return
				}
				n := int(fixture.count.Add(1))
				if n == 1 {
					fixture.first <- query.Copy()
				}
				if answer := reply(n, query); answer != nil && ctx.Err() == nil {
					payload, err := answer.Pack()
					if err != nil {
						t.Error(err)
						return
					}
					packet := buf.New()
					_, _ = packet.Write(payload)
					_ = down.WriteMultiBuffer(buf.MultiBuffer{packet})
				}
			}
		}
	}()
	t.Cleanup(func() {
		cancel()
		fixture.server.udpServer.RemoveRay()
		_ = writer.Close()
		_ = down.Close()
		up.Interrupt()
		reader.Interrupt()
		<-stopped
		_ = fixture.server.requestsCleanup.Close()
		_ = fixture.server.cacheController.cacheCleanup.Close()
	})
	return fixture
}

func dnsReply(query *mdns.Msg, rcode int) *mdns.Msg {
	answer := new(mdns.Msg)
	answer.SetReply(query)
	answer.Rcode = rcode
	if rcode == mdns.RcodeSuccess {
		if query.Question[0].Qtype == mdns.TypeA {
			answer.Answer = []mdns.RR{&mdns.A{Hdr: mdns.RR_Header{Name: query.Question[0].Name, Rrtype: mdns.TypeA, Class: mdns.ClassINET, Ttl: 60}, A: stdnet.ParseIP("192.0.2.7")}}
		} else {
			answer.Answer = []mdns.RR{&mdns.AAAA{Hdr: mdns.RR_Header{Name: query.Question[0].Name, Rrtype: mdns.TypeAAAA, Class: mdns.ClassINET, Ttl: 60}, AAAA: stdnet.ParseIP("2001:db8::7")}}
		}
	}
	return answer
}

func ruleClient(server Server, timeout time.Duration) *Client {
	return &Client{server: server, skipFallback: true, tag: "dns:egress:test", timeoutMs: timeout,
		ipOption: &dnsfeature.IPOption{IPv4Enable: true, IPv6Enable: true}}
}

func eventuallyDNS(t *testing.T, check func() bool) {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for !check() {
		if time.Now().After(deadline) {
			t.Fatal("DNS asynchronous cleanup did not complete")
		}
		time.Sleep(time.Millisecond)
	}
}

func TestRuleDNSRecoversDroppedUDPQueries(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(n int, query *mdns.Msg) *mdns.Msg {
		if n < 3 {
			return nil
		}
		return dnsReply(query, mdns.RcodeSuccess)
	})
	result := ruleClient(fixture.server, 400*time.Millisecond).query(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true}, false)
	if result.err != nil || len(result.ips) != 1 || fixture.count.Load() != 3 || result.attempts != 3 {
		t.Fatalf("lost packets were not retried: sends=%d result=%+v", fixture.count.Load(), result)
	}
}

func TestRuleDNSServfailCanRecoverInsideOneBudget(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(n int, query *mdns.Msg) *mdns.Msg {
		if n == 1 {
			return dnsReply(query, mdns.RcodeServerFailure)
		}
		return dnsReply(query, mdns.RcodeSuccess)
	})
	ips, _, err := ruleClient(fixture.server, 400*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if err != nil || len(ips) != 1 || fixture.count.Load() != 2 {
		t.Fatalf("temporary response ended retries: sends=%d err=%v", fixture.count.Load(), err)
	}
}

func TestRuleDNSFiveSendsIncludeBothRecordTypes(t *testing.T) {
	for _, dual := range []bool{false, true} {
		t.Run(map[bool]string{false: "A", true: "A-and-AAAA"}[dual], func(t *testing.T) {
			fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
			start := time.Now()
			_, _, err := ruleClient(fixture.server, 160*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true, IPv6Enable: dual})
			if err == nil || time.Since(start) > 700*time.Millisecond {
				t.Fatalf("query did not obey its total deadline: %v", err)
			}
			eventuallyDNS(t, func() bool {
				fixture.server.RLock()
				defer fixture.server.RUnlock()
				return len(fixture.server.requests) == 0
			})
			if fixture.count.Load() != 5 {
				t.Fatalf("expected 5 total packets, got %d", fixture.count.Load())
			}
		})
	}
}

func TestRuleDNSEmptyAndNXDomainDoNotRetry(t *testing.T) {
	for _, rcode := range []int{mdns.RcodeSuccess, mdns.RcodeNameError} {
		t.Run(mdns.RcodeToString[rcode], func(t *testing.T) {
			fixture := newUDPRuleFixture(t, func(_ int, query *mdns.Msg) *mdns.Msg {
				answer := dnsReply(query, rcode)
				answer.Answer = nil
				return answer
			})
			_, _, err := ruleClient(fixture.server, 400*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
			if !emptyQuery(err) || fixture.count.Load() != 1 {
				t.Fatalf("valid negative response was retried: %d %v", fixture.count.Load(), err)
			}
		})
	}
}

func TestRuleDNSCanceledWaiterDoesNotCancelSharedQuery(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(n int, query *mdns.Msg) *mdns.Msg {
		if n == 1 {
			time.Sleep(120 * time.Millisecond)
			return dnsReply(query, mdns.RcodeSuccess)
		}
		return nil
	})
	client := ruleClient(fixture.server, 600*time.Millisecond)
	completed := make(chan error, 1)
	go func() {
		_, _, err := client.QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
		completed <- err
	}()
	<-fixture.first
	ctx, cancel := context.WithTimeout(ruleDNSContext(), 20*time.Millisecond)
	defer cancel()
	start := time.Now()
	_, _, err := client.QueryIP(ctx, "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if !errors.Is(err, context.DeadlineExceeded) || time.Since(start) > 100*time.Millisecond {
		t.Fatalf("canceled waiter blocked on shared work: %v", err)
	}
	if err := <-completed; err != nil {
		t.Fatalf("waiter canceled the shared owner: %v", err)
	}
}

func TestRuleDNSLateUDPPacketCannotReplaceFailure(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	_, _, _ = ruleClient(fixture.server, 100*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	query := <-fixture.first
	eventuallyDNS(t, func() bool {
		fixture.server.RLock()
		defer fixture.server.RUnlock()
		return len(fixture.server.requests) == 0
	})
	payload, _ := dnsReply(query, mdns.RcodeSuccess).Pack()
	packet := buf.New()
	_, _ = packet.Write(payload)
	fixture.server.HandleResponse(ruleDNSContext(), &udp_proto.Packet{Payload: packet})
	if record := fixture.server.cacheController.findRecords("test.example."); record == nil || record.A == nil || record.A.Error == nil {
		t.Fatal("late packet replaced the completed failure")
	}
}

func TestRuleDNSFailureIsNotServedStale(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(_ int, query *mdns.Msg) *mdns.Msg { return dnsReply(query, mdns.RcodeSuccess) })
	fixture.server.cacheController.ips["test.example."] = &record{A: &IPRecord{Expire: time.Now().Add(-time.Second), Error: context.DeadlineExceeded}}
	ips, _, err := ruleClient(fixture.server, 400*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if err != nil || len(ips) != 1 || fixture.count.Load() != 1 {
		t.Fatalf("expired failure hid recovery: %v", err)
	}
}

func TestRuleDNSRefreshFailurePreservesPositiveStale(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(_ int, query *mdns.Msg) *mdns.Msg { return dnsReply(query, mdns.RcodeServerFailure) })
	cache := fixture.server.cacheController
	cache.ips["test.example."] = &record{A: &IPRecord{Expire: time.Now().Add(-time.Minute), IP: []net.IP{stdnet.ParseIP("192.0.2.9")}, RCode: dnsmessage.RCodeSuccess}}
	client := ruleClient(fixture.server, 160*time.Millisecond)
	ips, _, err := client.QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if err != nil || len(ips) != 1 {
		t.Fatalf("stale positive was not available: %v", err)
	}
	eventuallyDNS(t, func() bool {
		record := cache.findRecords("test.example.")
		return record != nil && record.A != nil && record.A.RetryAfter.After(time.Now())
	})
	if record := cache.findRecords("test.example."); !positiveRecord(record.A) {
		t.Fatal("SERVFAIL overwrote an available stale address")
	}
}

func TestRuleDNSUDPTruncationUsesSameProtocolAndQuota(t *testing.T) {
	fixture := newUDPRuleFixture(t, func(_ int, query *mdns.Msg) *mdns.Msg {
		answer := dnsReply(query, mdns.RcodeSuccess)
		if query.IsEdns0() == nil {
			answer.Truncated = true
			answer.Answer = nil
		}
		return answer
	})
	ips, _, err := ruleClient(fixture.server, 400*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
	if err != nil || len(ips) != 1 || fixture.count.Load() != 2 {
		t.Fatalf("EDNS UDP retry failed: packets=%d err=%v", fixture.count.Load(), err)
	}
}

func TestRuleDNSTCPRetriesAndCancelsBlockedReads(t *testing.T) {
	for _, recover := range []bool{true, false} {
		t.Run(map[bool]string{true: "recover", false: "cancel"}[recover], func(t *testing.T) {
			listener, err := stdnet.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			defer listener.Close()
			var count, active atomic.Int32
			stopped := make(chan struct{})
			go func() {
				defer close(stopped)
				for {
					conn, err := listener.Accept()
					if err != nil {
						return
					}
					active.Add(1)
					go func() {
						defer active.Add(-1)
						defer conn.Close()
						var size uint16
						if binary.Read(conn, binary.BigEndian, &size) != nil {
							return
						}
						payload := make([]byte, size)
						if _, err := io.ReadFull(conn, payload); err != nil {
							return
						}
						query := new(mdns.Msg)
						if query.Unpack(payload) != nil {
							return
						}
						n := count.Add(1)
						if recover {
							if n < 3 {
								return
							}
							answer, _ := dnsReply(query, mdns.RcodeSuccess).Pack()
							_ = binary.Write(conn, binary.BigEndian, uint16(len(answer)))
							_, _ = conn.Write(answer)
						} else {
							_, _ = io.Copy(io.Discard, conn)
						}
					}()
				}
			}()
			address, _ := url.Parse("tcp+local://" + listener.Addr().String())
			server, err := NewTCPLocalNameServer(address, false, false, 0, nil)
			if err != nil {
				t.Fatal(err)
			}
			defer server.cacheController.cacheCleanup.Close()
			ips, _, err := ruleClient(server, 400*time.Millisecond).QueryIP(ruleDNSContext(), "test.example", dnsfeature.IPOption{IPv4Enable: true})
			if recover && (err != nil || len(ips) != 1 || count.Load() != 3) {
				t.Fatalf("TCP did not recover: sends=%d err=%v", count.Load(), err)
			}
			if !recover && (err == nil || count.Load() != 5) {
				t.Fatalf("TCP did not obey its quota: sends=%d err=%v", count.Load(), err)
			}
			eventuallyDNS(t, func() bool { return active.Load() == 0 })
			listener.Close()
			<-stopped
		})
	}
}

type groupRuleMatcher struct{ count int }

func (m groupRuleMatcher) Match(string) []uint32 {
	indices := make([]uint32, m.count)
	for index := range indices {
		indices[index] = uint32(index)
	}
	return indices
}
func (m groupRuleMatcher) MatchAny(string) bool { return m.count > 0 }

func groupedRuleDNS(clients ...*Client) *DNS {
	infos := make([]*DomainMatcherInfo, len(clients))
	for index := range infos {
		infos[index] = &DomainMatcherInfo{clientIdx: uint16(index), domainRule: "fixture"}
	}
	return &DNS{ctx: ruleDNSContext(), clients: clients, domainMatcher: groupRuleMatcher{len(clients)}, matcherInfos: infos, enableParallelQuery: true}
}

func drainRuleQuery(t *testing.T, fixture *udpRuleFixture, family string) {
	t.Helper()
	eventuallyDNS(t, func() bool { return fixture.count.Load() > 0 })
	_, _, _ = fixture.server.cacheController.requestGroup.Do("test.example."+family+":rule", func() (any, error) { return nil, nil })
}

func TestParallelRuleGroupUsesFastMemberAndPreservesPriority(t *testing.T) {
	slow := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	preferred := newUDPRuleFixture(t, func(n int, q *mdns.Msg) *mdns.Msg {
		if n < 3 {
			return nil
		}
		return dnsReply(q, mdns.RcodeSuccess)
	})
	later := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg {
		answer := dnsReply(q, mdns.RcodeSuccess)
		answer.Answer[0].(*mdns.A).A = stdnet.ParseIP("192.0.2.8")
		return answer
	})
	defer drainRuleQuery(t, slow, "4")
	a, b, c := ruleClient(slow.server, 400*time.Millisecond), ruleClient(preferred.server, 400*time.Millisecond), ruleClient(later.server, 400*time.Millisecond)
	a.policyID, b.policyID, c.policyID = 1, 1, 2
	c.tag = "dns:egress:later"
	trace := &dnsfeature.LookupTrace{}
	ips, _, err := groupedRuleDNS(a, b, c).parallelQuery("test.example", dnsfeature.IPOption{IPv4Enable: true, Trace: trace})
	if err != nil || len(ips) != 1 || !ips[0].Equal(stdnet.ParseIP("192.0.2.7")) {
		t.Fatalf("lower-priority group won: %v %v", ips, err)
	}
	if !trace.Complete || len(trace.Steps) != 1 || trace.Steps[0].Winner != 2 || len(trace.Steps[0].Members) != 1 || trace.Steps[0].Members[0].Attempts != 3 {
		t.Fatalf("wrong race evidence: %+v", trace)
	}
}

func TestParallelRuleGroupHasIndependentMemberQuotasAndStopBoundary(t *testing.T) {
	for _, stop := range []bool{false, true} {
		t.Run(map[bool]string{false: "fallback", true: "stop"}[stop], func(t *testing.T) {
			first := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
			second := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
			later := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg { return dnsReply(q, mdns.RcodeSuccess) })
			a, b, c := ruleClient(first.server, 160*time.Millisecond), ruleClient(second.server, 160*time.Millisecond), ruleClient(later.server, 400*time.Millisecond)
			a.policyID, b.policyID, c.policyID = 1, 1, 2
			b.finalQuery = stop
			c.tag = "dns:egress:later"
			trace := &dnsfeature.LookupTrace{}
			ips, _, err := groupedRuleDNS(a, b, c).parallelQuery("test.example", dnsfeature.IPOption{IPv4Enable: true, Trace: trace})
			if first.count.Load() != 5 || second.count.Load() != 5 {
				t.Fatalf("members shared a send quota: %d %d", first.count.Load(), second.count.Load())
			}
			if !trace.Complete || trace.Steps[0].Outcome != "error" || len(trace.Steps[0].Members) != 2 || trace.Steps[0].Members[0].Attempts != 5 || trace.Steps[0].Members[1].Attempts != 5 {
				t.Fatalf("missing member failures: %+v", trace)
			}
			if stop {
				if err == nil || len(ips) != 0 || len(trace.Steps) != 1 || later.count.Load() != 0 {
					t.Fatal("stop boundary queried a later group")
				}
			} else if err != nil || len(ips) == 0 || len(trace.Steps) != 2 || trace.Steps[1].Winner != 1 {
				t.Fatalf("fallback group was not adopted: %+v %v", trace, err)
			}
		})
	}
}

func TestParallelRuleGroupSeparatesAbsenceFromOneFailingMember(t *testing.T) {
	empty := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg {
		answer := dnsReply(q, mdns.RcodeSuccess)
		if q.Question[0].Qtype == mdns.TypeAAAA {
			answer.Answer = nil
		}
		return answer
	})
	broken := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
	later := newUDPRuleFixture(t, func(_ int, q *mdns.Msg) *mdns.Msg { return dnsReply(q, mdns.RcodeSuccess) })
	a, b, c := ruleClient(empty.server, 160*time.Millisecond), ruleClient(broken.server, 160*time.Millisecond), ruleClient(later.server, 400*time.Millisecond)
	a.policyID, b.policyID, c.policyID = 1, 1, 2
	c.tag = "dns:egress:later"
	trace := &dnsfeature.LookupTrace{}
	_, _, err := groupedRuleDNS(a, b, c).parallelQuery("test.example", dnsfeature.IPOption{IPv6Enable: true, Trace: trace})
	if err != nil || len(trace.Steps) != 2 || trace.Steps[0].Outcome != "error" || trace.Steps[0].Other {
		t.Fatalf("uncertain member claimed an absent family: %+v %v", trace, err)
	}
	if !trace.Steps[0].Members[0].Other {
		t.Fatal("available opposite-family evidence was lost")
	}
}

func TestRuleDNSCallerCanCancelWithoutLosingInstanceContext(t *testing.T) {
	for _, parallel := range []bool{false, true} {
		t.Run(map[bool]string{false: "serial", true: "parallel"}[parallel], func(t *testing.T) {
			fixture := newUDPRuleFixture(t, func(int, *mdns.Msg) *mdns.Msg { return nil })
			client := ruleClient(fixture.server, 400*time.Millisecond)
			resolver := groupedRuleDNS(client)
			resolver.enableParallelQuery = parallel
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			completed := make(chan error, 1)
			go func() {
				var err error
				if parallel {
					_, _, err = resolver.parallelQuery("test.example", dnsfeature.IPOption{IPv4Enable: true, Context: ctx})
				} else {
					_, _, err = resolver.serialQuery("test.example", dnsfeature.IPOption{IPv4Enable: true, Context: ctx})
				}
				completed <- err
			}()
			eventuallyDNS(t, func() bool { return fixture.count.Load() > 0 })
			cancel()
			select {
			case err := <-completed:
				if err == nil {
					t.Fatal("canceled caller unexpectedly succeeded")
				}
			case <-time.After(100 * time.Millisecond):
				t.Fatal("caller cancellation did not interrupt DNS waiting")
			}
			drainRuleQuery(t, fixture, "4")
		})
	}
}
