package router

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/golang/mock/gomock"
	"github.com/xtls/xray-core/app/observatory"
	"github.com/xtls/xray-core/common/geodata"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/extension"
	"github.com/xtls/xray-core/features/outbound"
	"github.com/xtls/xray-core/features/routing"
	routing_session "github.com/xtls/xray-core/features/routing/session"
	"github.com/xtls/xray-core/testing/mocks"
	"google.golang.org/protobuf/proto"
)

func snapshotRule(tag, target string, specific bool) *RoutingRule {
	rule := &RoutingRule{RuleTag: tag, TargetTag: &RoutingRule_Tag{Tag: target}, Networks: []net.Network{net.Network_TCP}}
	if specific {
		rule.InboundTag = []string{"denied"}
	}
	return rule
}
func snapshotPolicy(tag string) *Config {
	return &Config{Rule: []*RoutingRule{snapshotRule(tag, "blocked", true), snapshotRule("fallback", "direct", false)}}
}
func snapshotContext() routing.Context {
	ctx := session.ContextWithInbound(context.Background(), &session.Inbound{Tag: "denied"})
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: net.TCPDestination(net.DomainAddress("example.com"), 443)}})
	return routing_session.AsRoutingContext(ctx)
}
func newSnapshotRouter(t *testing.T) *Router {
	t.Helper()
	r := new(Router)
	if err := r.Init(context.Background(), snapshotPolicy("deny"), nil, nil, nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { r.Close() })
	return r
}
func TestConcurrentRoutingSnapshots(t *testing.T) {
	r := newSnapshotRouter(t)
	stop := make(chan struct{})
	var readers, ready sync.WaitGroup
	var reads atomic.Int64
	failures := make(chan error, 8)
	for range 8 {
		readers.Add(1)
		ready.Add(1)
		go func() {
			defer readers.Done()
			ctx := snapshotContext()
			ready.Done()
			for {
				select {
				case <-stop:
					return
				default:
				}
				route, err := r.PickRoute(ctx)
				if err != nil {
					failures <- err
					return
				}
				if route.GetOutboundTag() != "blocked" {
					failures <- fmt.Errorf("deny policy changed to %q", route.GetOutboundTag())
					return
				}
				reads.Add(1)
			}
		}()
	}
	ready.Wait()
	defer func() {
		close(stop)
		readers.Wait()
		close(failures)
		for err := range failures {
			t.Error(err)
		}
		if reads.Load() == 0 {
			t.Error("no concurrent lookups ran")
		}
	}()
	for round := 0; round < 300; round++ {
		if err := r.AddRule(serial.ToTypedMessage(snapshotPolicy(fmt.Sprint(round))), false); err != nil {
			t.Fatal(err)
		}
		if err := r.ReloadRules(&Config{Rule: []*RoutingRule{snapshotRule("extra", "unused", false)}}, true); err != nil {
			t.Fatal(err)
		}
		if err := r.RemoveRule("extra"); err != nil {
			t.Fatal(err)
		}
		if err := r.ReloadRules(snapshotPolicy("legacy-replace"), false); err != nil {
			t.Fatal(err)
		}
	}
}
func TestRejectedRoutingSnapshotsKeepPreviousPolicy(t *testing.T) {
	for _, mode := range []string{"add-replace", "reload", "append"} {
		t.Run(mode, func(t *testing.T) {
			r := newSnapshotRouter(t)
			previous := r.loadSnapshot()
			invalid := &Config{Rule: []*RoutingRule{snapshotRule("new", "wrong", true), {RuleTag: "invalid", TargetTag: &RoutingRule_Tag{Tag: "wrong"}}}}
			var err error
			switch mode {
			case "add-replace":
				err = r.AddRule(serial.ToTypedMessage(invalid), false)
			case "reload":
				err = r.ReloadRules(invalid, false)
			case "append":
				err = r.ReloadRules(invalid, true)
			}
			if err == nil {
				t.Fatal("invalid table accepted")
			}
			if r.loadSnapshot() != previous {
				t.Fatal("failed update published a partial table")
			}
			route, err := r.PickRoute(snapshotContext())
			if err != nil || route.GetOutboundTag() != "blocked" {
				t.Fatalf("old deny policy changed: %v %v", route, err)
			}
		})
	}
}

type snapshotObservatory struct {
	report *observatory.ObservationResult
}

func (*snapshotObservatory) Type() interface{} { return extension.ObservatoryType() }
func (*snapshotObservatory) Start() error      { return nil }
func (*snapshotObservatory) Close() error      { return nil }
func (o *snapshotObservatory) GetObservation(context.Context) (proto.Message, error) {
	return o.report, nil
}

type snapshotOutboundSelector struct{ outbound.Manager }

func (snapshotOutboundSelector) Select([]string) []string { return []string{"out:a", "out:b"} }

func TestAddRuleReplacementReusesObservatoryHealth(t *testing.T) {
	obs := &snapshotObservatory{report: &observatory.ObservationResult{Status: []*observatory.OutboundStatus{
		{OutboundTag: "out:a", Alive: false, Delay: 1},
		{OutboundTag: "out:b", Alive: true, Delay: 20},
	}}}
	instance := new(core.Instance)
	if err := instance.AddFeature(obs); err != nil {
		t.Fatal(err)
	}
	ctx := context.WithValue(context.Background(), core.XrayKey(1), instance)
	cfg := &Config{
		Rule:          []*RoutingRule{{RuleTag: "balanced", Networks: []net.Network{net.Network_TCP}, TargetTag: &RoutingRule_BalancingTag{BalancingTag: "health"}}},
		BalancingRule: []*BalancingRule{{Tag: "health", Strategy: "leastping", OutboundSelector: []string{"out:"}}},
	}
	r := new(Router)
	if err := r.Init(ctx, cfg, nil, snapshotOutboundSelector{}, nil); err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	previous := r.loadSnapshot().balancers["health"]
	for round := range 20 {
		route, err := r.PickRoute(snapshotContext())
		if err != nil || route.GetOutboundTag() != "out:b" {
			t.Fatalf("lost healthy selection: %v %v", route, err)
		}
		cfg.Rule[0].RuleTag = fmt.Sprint(round)
		if err := r.AddRule(serial.ToTypedMessage(cfg), false); err != nil {
			t.Fatal(err)
		}
		current := r.loadSnapshot().balancers["health"]
		if current == previous {
			t.Fatal("replacement did not rebuild the balancer as upstream does")
		}
		if current.strategy.(*LeastPingStrategy).observatory != obs {
			t.Fatal("replacement lost the existing observatory")
		}
		previous = current
	}
	if instance.GetFeature(extension.ObservatoryType()) != obs || len(obs.report.Status) != 2 {
		t.Fatal("health observations were replaced")
	}
}

func TestAddRuleReplacementValidationAndEmptyTable(t *testing.T) {
	r := newSnapshotRouter(t)
	previous := r.loadSnapshot()
	duplicate := snapshotPolicy("fallback")
	if err := r.AddRule(serial.ToTypedMessage(duplicate), false); err == nil {
		t.Fatal("duplicate tags accepted")
	}
	if r.loadSnapshot() != previous {
		t.Fatal("duplicate tags changed routing")
	}
	if err := r.AddRule(serial.ToTypedMessage(&Config{}), false); err != nil {
		t.Fatal(err)
	}
	if len(r.ListRule()) != 0 {
		t.Fatal("empty replacement retained old rules")
	}
	if err := r.AddRule(nil, false); err == nil {
		t.Fatal("missing config accepted")
	}
}

type snapshotDomainBarrier struct {
	routing.Context
	entered, release chan struct{}
	once             sync.Once
}

func (c *snapshotDomainBarrier) GetTargetDomain() string {
	c.once.Do(func() { close(c.entered); <-c.release })
	return c.Context.GetTargetDomain()
}
func TestRoutingSnapshotSurvivesDNSSecondPass(t *testing.T) {
	controller := gomock.NewController(t)
	mockDNS := mocks.NewDNSClient(controller)
	mockDNS.EXPECT().LookupIP("example.com", gomock.Any()).Return([]net.IP{{192, 168, 0, 1}}, uint32(600), nil).AnyTimes()
	cfg := &Config{DomainStrategy: Config_IpIfNonMatch, Rule: []*RoutingRule{{RuleTag: "old", TargetTag: &RoutingRule_Tag{Tag: "old"}, Ip: []*geodata.IPRule{{Value: &geodata.IPRule_Custom{Custom: &geodata.CIDRRule{Cidr: &geodata.CIDR{Ip: []byte{192, 168, 0, 0}, Prefix: 16}}}}}}}}
	r := new(Router)
	if err := r.Init(context.Background(), cfg, mockDNS, nil, nil); err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	ctx := &snapshotDomainBarrier{Context: snapshotContext(), entered: make(chan struct{}), release: make(chan struct{})}
	result := make(chan string, 1)
	go func() {
		route, err := r.PickRoute(ctx)
		if err != nil {
			result <- err.Error()
		} else {
			result <- route.GetOutboundTag()
		}
	}()
	select {
	case <-ctx.entered:
	case <-time.After(time.Second):
		close(ctx.release)
		t.Fatal("lookup did not reach DNS second-pass boundary")
	}
	err := r.AddRule(serial.ToTypedMessage(&Config{Rule: []*RoutingRule{snapshotRule("new", "new", false)}}), false)
	close(ctx.release)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case tag := <-result:
		if tag != "old" {
			t.Fatalf("lookup mixed generations: %q", tag)
		}
	case <-time.After(time.Second):
		t.Fatal("lookup stalled")
	}
}
