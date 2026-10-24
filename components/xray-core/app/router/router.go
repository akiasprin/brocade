package router

import (
	"context"
	"maps"
	"sync"
	"sync/atomic"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/outbound"
	"github.com/xtls/xray-core/features/routing"
	routing_dns "github.com/xtls/xray-core/features/routing/dns"
)

// Router is an implementation of routing.Router.
type Router struct {
	domainStrategy Config_DomainStrategy
	snapshot       atomic.Pointer[routingSnapshot]
	dns            dns.Client

	ctx        context.Context
	ohm        outbound.Manager
	dispatcher routing.Dispatcher
	mu         sync.Mutex
	closed     bool
}

// A published snapshot is immutable. Each route lookup retains one generation even
// when DNS resolution causes a second matching pass while a writer publishes another.
type routingSnapshot struct {
	rules     []*Rule
	balancers map[string]*Balancer
}

var emptyRoutingSnapshot routingSnapshot

func (r *Router) loadSnapshot() *routingSnapshot {
	if snapshot := r.snapshot.Load(); snapshot != nil {
		return snapshot
	}
	return &emptyRoutingSnapshot
}

// Route is an implementation of routing.Route.
type Route struct {
	routing.Context
	outboundGroupTags []string
	outboundTag       string
	ruleTag           string
}

// Init initializes the Router.
func (r *Router) Init(ctx context.Context, config *Config, d dns.Client, ohm outbound.Manager, dispatcher routing.Dispatcher) error {
	r.domainStrategy = config.DomainStrategy
	r.dns = d
	r.ctx = ctx
	r.ohm = ohm
	r.dispatcher = dispatcher

	return r.ReloadRules(config, false)
}

// PickRoute implements routing.Router.
func (r *Router) PickRoute(ctx routing.Context) (routing.Route, error) {
	originalCtx := ctx
	rule, ctx, err := r.pickRouteInternal(ctx)
	if err != nil {
		return nil, err
	}
	tag, err := rule.GetTag()
	if err != nil {
		return nil, err
	}
	if rule.Webhook != nil {
		rule.Webhook.Fire(originalCtx, tag)
	}
	return &Route{Context: ctx, outboundTag: tag, ruleTag: rule.RuleTag}, nil
}

// AddRule implements routing.Router.
func (r *Router) AddRule(config *serial.TypedMessage, shouldAppend bool) error {
	c, err := decodeRoutingConfig(config)
	if err != nil {
		return err
	}
	return r.ReloadRules(c, shouldAppend)
}

func decodeRoutingConfig(config *serial.TypedMessage) (*Config, error) {
	if config == nil {
		return nil, errors.New("missing routing configuration")
	}
	inst, err := config.GetInstance()
	if err != nil {
		return nil, err
	}
	if c, ok := inst.(*Config); ok {
		return c, nil
	}
	return nil, errors.New("routing configuration type error")
}

func (r *Router) ReloadRules(config *Config, shouldAppend bool) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return errors.New("router is closed")
	}
	old := r.loadSnapshot()
	next := &routingSnapshot{balancers: make(map[string]*Balancer)}
	if shouldAppend {
		maps.Copy(next.balancers, old.balancers)
	}
	existingTags := make(map[string]bool, len(old.rules)+len(config.Rule))
	if shouldAppend {
		next.rules = append(next.rules, old.rules...)
		for _, rule := range old.rules {
			existingTags[rule.RuleTag] = true
		}
	}
	var addedRules []*Rule
	published := false
	defer func() {
		if !published {
			closeWebhooks(addedRules)
		}
	}()
	for _, rule := range config.BalancingRule {
		if _, found := next.balancers[rule.Tag]; found {
			return errors.New("duplicate balancer tag")
		}
		balancer, err := rule.Build(r.ohm, r.dispatcher)
		if err != nil {
			return err
		}
		next.balancers[rule.Tag] = balancer
	}
	for _, rule := range config.Rule {
		if tag := rule.GetRuleTag(); tag != "" && existingTags[tag] {
			return errors.New("duplicate ruleTag ", rule.GetRuleTag())
		}
		existingTags[rule.GetRuleTag()] = true
		cond, err := rule.BuildCondition()
		if err != nil {
			return err
		}
		rr := &Rule{
			Condition: cond,
			Tag:       rule.GetTag(),
			RuleTag:   rule.GetRuleTag(),
		}
		addedRules = append(addedRules, rr)
		if wh := rule.GetWebhook(); wh != nil {
			notifier, err := NewWebhookNotifier(wh)
			if err != nil {
				return err
			}
			rr.Webhook = notifier
		}
		btag := rule.GetBalancingTag()
		if len(btag) > 0 {
			brule, found := next.balancers[btag]
			if !found {
				return errors.New("balancer ", btag, " not found")
			}
			rr.Balancer = brule
		}
		next.rules = append(next.rules, rr)
	}
	for _, rule := range config.BalancingRule {
		next.balancers[rule.Tag].InjectContext(r.ctx)
	}
	r.snapshot.Store(next)
	published = true
	if !shouldAppend {
		closeWebhooks(old.rules)
	}
	return nil
}

// RemoveRule implements routing.Router.
func (r *Router) RemoveRule(tag string) error {
	if tag == "" {
		return errors.New("empty tag name!")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return errors.New("router is closed")
	}
	old := r.loadSnapshot()
	next := &routingSnapshot{rules: make([]*Rule, 0, len(old.rules)), balancers: old.balancers}
	var removed []*Rule
	for _, rule := range old.rules {
		if rule.RuleTag != tag {
			next.rules = append(next.rules, rule)
		} else {
			removed = append(removed, rule)
		}
	}
	r.snapshot.Store(next)
	closeWebhooks(removed)
	return nil
}

// ListRule implements routing.Router
func (r *Router) ListRule() []routing.Route {
	rules := r.loadSnapshot().rules
	ruleList := make([]routing.Route, 0, len(rules))
	for _, rule := range rules {
		ruleList = append(ruleList, &Route{
			outboundTag: rule.Tag,
			ruleTag:     rule.RuleTag,
		})
	}
	return ruleList
}

func (r *Router) pickRouteInternal(ctx routing.Context) (*Rule, routing.Context, error) {
	rules := r.loadSnapshot().rules
	// SkipDNSResolve is set from DNS module.
	// the DOH remote server maybe a domain name,
	// this prevents cycle resolving dead loop
	skipDNSResolve := ctx.GetSkipDNSResolve()

	if r.domainStrategy == Config_IpOnDemand && !skipDNSResolve {
		ctx = routing_dns.ContextWithDNSClient(ctx, r.dns)
	}

	for _, rule := range rules {
		if rule.Apply(ctx) {
			return rule, ctx, nil
		}
	}

	if r.domainStrategy != Config_IpIfNonMatch || len(ctx.GetTargetDomain()) == 0 || skipDNSResolve {
		return nil, ctx, common.ErrNoClue
	}

	ctx = routing_dns.ContextWithDNSClient(ctx, r.dns)

	// Try applying rules again if we have IPs.
	for _, rule := range rules {
		if rule.Apply(ctx) {
			return rule, ctx, nil
		}
	}

	return nil, ctx, common.ErrNoClue
}

// Start implements common.Runnable.
func (r *Router) Start() error {
	return nil
}

func closeWebhooks(rules []*Rule) {
	for _, rule := range rules {
		if rule.Webhook != nil {
			rule.Webhook.Close()
		}
	}
}

// Close implements common.Closable.
func (r *Router) Close() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.closed = true
	old := r.loadSnapshot()
	r.snapshot.Store(&routingSnapshot{})
	closeWebhooks(old.rules)
	return nil
}

// Type implements common.HasType.
func (*Router) Type() interface{} {
	return routing.RouterType()
}

// GetOutboundGroupTags implements routing.Route.
func (r *Route) GetOutboundGroupTags() []string {
	return r.outboundGroupTags
}

// GetOutboundTag implements routing.Route.
func (r *Route) GetOutboundTag() string {
	return r.outboundTag
}

func (r *Route) GetRuleTag() string {
	return r.ruleTag
}

func init() {
	common.Must(common.RegisterConfig((*Config)(nil), func(ctx context.Context, config interface{}) (interface{}, error) {
		r := new(Router)
		if err := core.RequireFeatures(ctx, func(d dns.Client, ohm outbound.Manager, dispatcher routing.Dispatcher) error {
			return r.Init(ctx, config.(*Config), d, ohm, dispatcher)
		}); err != nil {
			return nil, err
		}
		return r, nil
	}))
}
