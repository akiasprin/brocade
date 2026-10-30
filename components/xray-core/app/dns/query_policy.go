package dns

import (
	"context"
	go_errors "errors"
	"sync/atomic"
	"time"

	dnsfeature "github.com/xtls/xray-core/features/dns"
	"golang.org/x/net/dns/dnsmessage"
)

const ruleQueryAttempts = 5
const ruleQueryTimeout = 4 * time.Second
const resolutionFailureTTL = time.Second

type queryBudgetKey struct{}
type queryDeadlineKey struct{}

// Dedicated TCP DNS connections inherit the query lifetime while retaining the
// detached DNS context's routing identity. UDP keeps its shared socket lifetime.
type queryNetworkContext struct {
	context.Context
	request context.Context
}

func (c queryNetworkContext) Deadline() (time.Time, bool) { return c.request.Deadline() }
func (c queryNetworkContext) Done() <-chan struct{}       { return c.request.Done() }
func (c queryNetworkContext) Err() error                  { return c.request.Err() }

// A budget belongs to one resolver member, including both requested record types.
// Its work deadline is independent of any waiter's shorter lifetime.
type queryBudget struct {
	limit    int32
	used     atomic.Int32
	cache    atomic.Uint32
	duration time.Duration
	deadline time.Time
}

func newQueryBudget(ctx context.Context, timeout time.Duration) (context.Context, context.CancelFunc, *queryBudget) {
	if timeout <= 0 || timeout > ruleQueryTimeout {
		timeout = ruleQueryTimeout
	}
	deadline := time.Now().Add(timeout)
	ctx, cancel := context.WithDeadline(ctx, deadline)
	budget := &queryBudget{limit: ruleQueryAttempts, duration: timeout, deadline: deadline}
	return context.WithValue(ctx, queryBudgetKey{}, budget), cancel, budget
}

func queryBudgetFrom(ctx context.Context) *queryBudget {
	budget, _ := ctx.Value(queryBudgetKey{}).(*queryBudget)
	return budget
}

func reportQueryError(ctx context.Context, ch chan<- error, err error) {
	if ch == nil {
		return
	}
	select {
	case ch <- err:
	case <-ctx.Done():
	}
}

func reserveQuery(ctx context.Context) bool {
	if ctx.Err() != nil {
		return false
	}
	budget := queryBudgetFrom(ctx)
	if budget == nil {
		return true
	}
	for {
		used := budget.used.Load()
		if used >= budget.limit {
			return false
		}
		if budget.used.CompareAndSwap(used, used+1) {
			return true
		}
	}
}

func (b *queryBudget) observeAttempts(attempts int32) {
	for {
		used := b.used.Load()
		if used >= attempts || b.used.CompareAndSwap(used, attempts) {
			return
		}
	}
}

func retryableRecord(record *IPRecord) bool {
	return record != nil && record.RCode == dnsmessage.RCodeServerFailure
}

func positiveRecord(record *IPRecord) bool {
	return record != nil && record.Error == nil && record.RCode == dnsmessage.RCodeSuccess && len(record.IP) > 0
}

// query records only the member's effective result. A late or unused member may
// finish to warm its own cache, but cannot change the selected logical path.
func (c *Client) query(ctx context.Context, domain string, option dnsfeature.IPOption, trace bool) queryResult {
	start := time.Now()
	var budget *queryBudget
	var cancel context.CancelFunc
	if c.skipFallback {
		ctx, cancel, budget = newQueryBudget(ctx, c.timeoutMs)
	} else {
		ctx, cancel = context.WithTimeout(ctx, c.timeoutMs)
	}
	defer cancel()
	ips, ttl, err := c.QueryIP(ctx, domain, option)
	result := queryResult{ips: ips, ttl: ttl, err: err}
	if trace && c.skipFallback && len(ips) == 0 && emptyQuery(err) && ctx.Err() == nil {
		// Diagnostics favor the same policy/cache/transport as ordinary resolution.
		// A cold cache must not change the conclusion, so perform the real lookup
		// instead of treating a cache miss as evidence of address absence. Both
		// families still share this member's four-second/five-send budget. A
		// failed supplementary lookup leaves Other false (unconfirmed).
		other := dnsfeature.IPOption{IPv4Enable: option.IPv6Enable, IPv6Enable: option.IPv4Enable}
		otherIPs, _, _ := c.QueryIP(ctx, domain, other)
		result.other = len(otherIPs) > 0
	}
	result.elapsed = uint32(time.Since(start).Milliseconds())
	result.finished = time.Now()
	if budget != nil {
		result.attempts = uint32(budget.used.Load())
		result.cache = budget.cache.Load()
	}
	return result
}

func emptyQuery(err error) bool {
	return err == nil || dnsfeature.RCodeFromError(err) == 3 || go_errors.Is(err, dnsfeature.ErrEmptyResponse)
}

func (r *queryResult) traceMember(index int) dnsfeature.LookupTraceMember {
	outcome := "error"
	if len(r.ips) > 0 {
		outcome = "answer"
	} else if emptyQuery(r.err) {
		outcome = "empty"
	}
	cache := ""
	switch r.cache {
	case 1:
		cache = "hit"
	case 2:
		cache = "stale"
	default:
		if r.attempts > 0 {
			cache = "miss"
		}
	}
	return dnsfeature.LookupTraceMember{Index: uint32(index), Outcome: outcome, Other: r.other,
		Attempts: r.attempts, ElapsedMs: r.elapsed, RCode: dnsfeature.RCodeFromError(r.err), Cache: cache}
}

func appendGroupTrace(trace *dnsfeature.LookupTrace, clients []*Client, results []*queryResult, group group, winner int) {
	if trace == nil {
		return
	}
	if len(trace.Steps) >= 8 || len(clients[group.start].tag) > 64 {
		trace.Complete = false
		return
	}
	step := dnsfeature.LookupTraceStep{Tag: clients[group.start].tag, Scoped: clients[group.start].skipFallback, Outcome: "empty"}
	for index := group.start; index <= group.end; index++ {
		result := results[index]
		if result == nil {
			continue
		}
		if len(step.Members) >= 16 {
			trace.Complete = false
			break
		}
		member := result.traceMember(index - group.start + 1)
		step.Members = append(step.Members, member)
		if member.Outcome == "error" {
			step.Outcome = "error"
		}
		step.Other = step.Other || member.Other
	}
	if winner >= group.start && winner <= group.end {
		step.Outcome, step.Other = "answer", false
		if winner-group.start < 16 {
			step.Winner = uint32(winner - group.start + 1)
		} else {
			trace.Complete = false
		}
	} else if step.Outcome == "error" {
		step.Other = false
	}
	trace.Steps = append(trace.Steps, step)
}
