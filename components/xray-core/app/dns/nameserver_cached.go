package dns

import (
	"context"
	go_errors "errors"
	"time"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/log"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/features/dns"
)

type CachedNameserver interface {
	getCacheController() *CacheController

	sendQuery(ctx context.Context, noResponseErrCh chan<- error, fqdn string, option dns.IPOption)
}

// queryIP is called from dns.Server->queryIPTimeout
func queryIP(ctx context.Context, s CachedNameserver, domain string, option dns.IPOption) ([]net.IP, uint32, error) {
	fqdn := Fqdn(domain)

	cache := s.getCacheController()
	if !cache.disableCache {
		if rec := cache.findRecords(fqdn); rec != nil {
			ips, ttl, err := merge(option, rec.A, rec.AAAA)
			if !go_errors.Is(err, errRecordNotFound) {
				if ttl > 0 {
					if budget := queryBudgetFrom(ctx); budget != nil && budget.used.Load() == 0 {
						budget.cache.Store(1)
					}
					errors.LogDebugInner(ctx, err, cache.name, " cache HIT ", fqdn, " -> ", ips)
					log.Record(&log.DNSLog{Server: cache.name, Domain: fqdn, Result: ips, Status: log.DNSCacheHit, Elapsed: 0, Error: err})
					return ips, uint32(ttl), err
				}
				positive := (!option.IPv4Enable || positiveRecord(rec.A)) && (!option.IPv6Enable || positiveRecord(rec.AAAA))
				if positive && cache.serveStale && (cache.serveExpiredTTL == 0 || cache.serveExpiredTTL < ttl) {
					if budget := queryBudgetFrom(ctx); budget != nil && budget.used.Load() == 0 {
						budget.cache.Store(2)
					}
					errors.LogDebugInner(ctx, err, cache.name, " cache OPTIMISTE ", fqdn, " -> ", ips)
					log.Record(&log.DNSLog{Server: cache.name, Domain: fqdn, Result: ips, Status: log.DNSCacheOptimiste, Elapsed: 0, Error: err})
					if (!option.IPv4Enable || time.Now().After(rec.A.RetryAfter)) && (!option.IPv6Enable || time.Now().After(rec.AAAA.RetryAfter)) {
						go pull(ctx, s, fqdn, option)
					}
					return ips, 1, err
				}
			}
		}
	} else {
		errors.LogDebug(ctx, "DNS cache is disabled. Querying IP for ", fqdn, " at ", cache.name)
	}

	return fetch(ctx, s, fqdn, option)
}

func pull(ctx context.Context, s CachedNameserver, fqdn string, option dns.IPOption) {
	var nctx context.Context
	var cancel context.CancelFunc
	if budget := queryBudgetFrom(ctx); budget != nil {
		nctx, cancel, _ = newQueryBudget(context.WithoutCancel(ctx), budget.duration)
	} else {
		nctx, cancel = context.WithTimeout(context.WithoutCancel(ctx), 8*time.Second)
		nctx = context.WithValue(nctx, queryDeadlineKey{}, time.Now().Add(8*time.Second))
	}
	defer cancel()

	fetch(nctx, s, fqdn, option)
}

func fetch(ctx context.Context, s CachedNameserver, fqdn string, option dns.IPOption) ([]net.IP, uint32, error) {
	if err := ctx.Err(); err != nil {
		return nil, 0, err
	}
	key := fqdn
	switch {
	case option.IPv4Enable && option.IPv6Enable:
		key = key + "46"
	case option.IPv4Enable:
		key = key + "4"
	case option.IPv6Enable:
		key = key + "6"
	}

	if queryBudgetFrom(ctx) != nil {
		key += ":rule"
	}
	ch := s.getCacheController().requestGroup.DoChan(key, func() (any, error) {
		// The first waiter may be near its own deadline. Shared resolver work
		// must keep its independent bounded lifetime for every other waiter.
		deadline, ok := ctx.Value(queryDeadlineKey{}).(time.Time)
		if budget := queryBudgetFrom(ctx); budget != nil {
			deadline = budget.deadline
		} else if !ok {
			deadline, ok = ctx.Deadline()
			if !ok {
				deadline = time.Now().Add(ruleQueryTimeout)
			}
		}
		shared, cancel := context.WithDeadline(context.WithoutCancel(ctx), deadline)
		defer cancel()
		return doFetch(shared, s, fqdn, option), nil
	})
	select {
	case <-ctx.Done():
		return nil, 0, ctx.Err()
	case completed := <-ch:
		ret := completed.Val.(result)
		if budget := queryBudgetFrom(ctx); budget != nil {
			budget.observeAttempts(ret.attempts)
		}
		return ret.ips, ret.ttl, ret.error
	}
}

type result struct {
	ips []net.IP
	ttl uint32
	error
	attempts int32
}

func doFetch(ctx context.Context, s CachedNameserver, fqdn string, option dns.IPOption) result {
	sub4, sub6 := s.getCacheController().registerSubscribers(fqdn, option)
	defer closeSubscribers(sub4, sub6)
	var ch4, ch6 <-chan interface{}
	if sub4 != nil {
		ch4 = sub4.Wait()
	}
	if sub6 != nil {
		ch6 = sub6.Wait()
	}
	var rec4, rec6 *IPRecord
	done4, done6 := !option.IPv4Enable, !option.IPv6Enable
	var errs []error
	noResponseErrCh := make(chan error, 2)
	start := time.Now()
	budget := queryBudgetFrom(ctx)
	sendPending := func() {
		if ctx.Err() != nil || budget != nil && budget.used.Load() >= budget.limit {
			return
		}
		pending := option
		pending.IPv4Enable, pending.IPv6Enable = !done4, !done6
		s.sendQuery(ctx, noResponseErrCh, fqdn, pending)
	}
	sendPending()
	var timer *time.Timer
	var retry <-chan time.Time
	var attempt int
	steps := [...]int{1, 3, 6, 10}
	schedule := func() {
		if budget == nil || attempt >= len(steps) {
			retry = nil
			return
		}
		delay := time.Until(start.Add(budget.duration * time.Duration(steps[attempt]) / 16))
		if delay < 0 {
			delay = 0
		}
		if timer == nil {
			timer = time.NewTimer(delay)
		} else {
			timer.Reset(delay)
		}
		retry = timer.C
	}
	schedule()
	defer func() {
		if timer != nil {
			timer.Stop()
		}
	}()

wait:
	for !done4 || !done6 {
		select {
		case <-ctx.Done():
			errs = append(errs, ctx.Err())
			break wait
		case err := <-noResponseErrCh:
			errs = append(errs, err)
			if budget == nil || budget.used.Load() == 0 {
				break wait
			}
		case message := <-ch4:
			rec4 = message.(*IPRecord)
			if budget == nil || !retryableRecord(rec4) {
				done4, ch4 = true, nil
			}
		case message := <-ch6:
			rec6 = message.(*IPRecord)
			if budget == nil || !retryableRecord(rec6) {
				done6, ch6 = true, nil
			}
		case <-retry:
			sendPending()
			attempt++
			schedule()
		}
	}

	ips, ttl, err := merge(option, rec4, rec6, errs...)
	if len(ips) == 0 && go_errors.Is(err, errRecordNotFound) && len(errs) > 0 {
		err = errors.Combine(errs...)
	}
	for _, item := range []struct {
		requested bool
		v6        bool
		record    *IPRecord
	}{{option.IPv4Enable, false, rec4}, {option.IPv6Enable, true, rec6}} {
		if !item.requested {
			continue
		}
		_, _, failure := item.record.getIPs()
		if failure != nil && !emptyQuery(failure) {
			if go_errors.Is(failure, errRecordNotFound) && len(errs) > 0 {
				failure = errors.Combine(errs...)
			}
			s.getCacheController().storeFailure(fqdn, item.v6, failure)
		}
	}
	var rTTL uint32
	if ttl > 0 {
		rTTL = uint32(ttl)
	} else if ttl == 0 && go_errors.Is(err, errRecordNotFound) {
		rTTL = 0
	} else { // edge case: where a fast rep's ttl expires during the rtt of a slower, parallel query
		rTTL = 1
	}

	log.Record(&log.DNSLog{Server: s.getCacheController().name, Domain: fqdn, Result: ips, Status: log.DNSQueried, Elapsed: time.Since(start), Error: err})
	ret := result{ips: ips, ttl: rTTL, error: err}
	if budget != nil {
		ret.attempts = budget.used.Load()
	}
	return ret
}

func merge(option dns.IPOption, rec4 *IPRecord, rec6 *IPRecord, errs ...error) ([]net.IP, int32, error) {
	var allIPs []net.IP
	var rTTL int32 = dns.DefaultTTL

	mergeReq := option.IPv4Enable && option.IPv6Enable

	if option.IPv4Enable {
		ips, ttl, err := rec4.getIPs() // it's safe
		if !mergeReq {
			return ips, ttl, err
		}
		if ttl < rTTL {
			rTTL = ttl
		}
		if len(ips) > 0 {
			allIPs = append(allIPs, ips...)
		} else {
			errs = append(errs, err)
		}
	}

	if option.IPv6Enable {
		ips, ttl, err := rec6.getIPs() // it's safe
		if !mergeReq {
			return ips, ttl, err
		}
		if ttl < rTTL {
			rTTL = ttl
		}
		if len(ips) > 0 {
			allIPs = append(allIPs, ips...)
		} else {
			errs = append(errs, err)
		}
	}

	if len(allIPs) > 0 {
		return allIPs, rTTL, nil
	}
	if len(errs) == 2 && go_errors.Is(errs[0], errs[1]) {
		return nil, rTTL, errs[0]
	}
	return nil, rTTL, errors.Combine(errs...)
}
