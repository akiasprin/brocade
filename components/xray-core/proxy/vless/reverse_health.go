package vless

import (
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/mux"
	"net/url"
	"time"
)

// A present group must be complete. Omitted tuning uses versioned defaults. Missing values are not field-by-field
// inheritance: both directions must compile the same effective policy.
func (r *Reverse) HealthConfig(role string) (mux.ReverseHealthConfig, error) {
	c := mux.DefaultReverseHealthConfig(r.Tag, role)
	if r.CanaryUrl != "" {
		u, err := url.Parse(r.CanaryUrl)
		if err != nil || u.Hostname() == "" || (u.Scheme != "http" && u.Scheme != "https") || u.User != nil {
			return c, errors.New("reverse canary_url must be an HTTP(S) URL without credentials")
		}
	}
	h := r.Health
	if h == nil {
		return c, nil
	}
	if t := h.Tuning; t != nil {
		if t.ProbeJitterPercent > 50 {
			return c, errors.New("invalid reverse health tuning: probe_jitter_percent")
		}
		if t.RecoverySuccesses < 1 || t.RecoverySuccesses > 8 {
			return c, errors.New("invalid reverse health tuning: recovery_successes")
		}
		if t.SpareWorkers < 1 || t.SpareWorkers > 8 {
			return c, errors.New("invalid reverse health tuning: spare_workers")
		}
		if t.MaxHealthyWorkers < 1 || t.MaxHealthyWorkers > 32 {
			return c, errors.New("invalid reverse health tuning: max_healthy_workers")
		}
		if t.MaxSessionsPerWorker < 1 || t.MaxSessionsPerWorker > 256 {
			return c, errors.New("invalid reverse health tuning: max_sessions_per_worker")
		}
		if t.ReconnectBackoffBaseMs < 50 || t.ReconnectBackoffBaseMs > 30000 {
			return c, errors.New("invalid reverse health tuning: reconnect_backoff_base_ms")
		}
		if t.ReconnectStableResetMs < 1000 || t.ReconnectStableResetMs > 300000 {
			return c, errors.New("invalid reverse health tuning: reconnect_stable_reset_ms")
		}
		if t.CanaryIntervalMs < 100 || t.CanaryIntervalMs > 60000 {
			return c, errors.New("invalid reverse health tuning: canary_interval_ms")
		}
		if t.CanaryTimeoutMs < 50 || t.CanaryTimeoutMs > 30000 {
			return c, errors.New("invalid reverse health tuning: canary_timeout_ms")
		}
		if t.CanarySuccesses < 1 || t.CanarySuccesses > 1000 {
			return c, errors.New("invalid reverse health tuning: canary_successes")
		}
		if t.CanaryStableWindowMs > 300000 {
			return c, errors.New("invalid reverse health tuning: canary_stable_window_ms")
		}
		c.ProbeJitterPercent = t.ProbeJitterPercent
		c.RecoverySuccesses = t.RecoverySuccesses
		c.SpareWorkers = t.SpareWorkers
		c.MaxHealthyWorkers = t.MaxHealthyWorkers
		c.MaxSessionsPerWorker = t.MaxSessionsPerWorker
		c.BackoffBase = time.Duration(t.ReconnectBackoffBaseMs) * time.Millisecond
		c.StableReset = time.Duration(t.ReconnectStableResetMs) * time.Millisecond
		c.CanaryInterval = time.Duration(t.CanaryIntervalMs) * time.Millisecond
		c.CanaryTimeout = time.Duration(t.CanaryTimeoutMs) * time.Millisecond
		c.CanarySuccesses = t.CanarySuccesses
		c.CanaryStableWindow = time.Duration(t.CanaryStableWindowMs) * time.Millisecond
	}
	if c.BackoffBase > time.Duration(h.ReconnectBackoffCapMs)*time.Millisecond || c.MaxHealthyWorkers < h.MaxIdleReadyWorkers || c.SpareWorkers > h.MaxIdleReadyWorkers || c.CanaryTimeout >= c.CanaryInterval {
		return c, errors.New("inconsistent reverse health tuning")
	}
	if h.ProbeIntervalMs < 100 || h.ProbeIntervalMs > 60000 || h.ProbeTimeoutMs < 50 || h.ProbeTimeoutMs > 10000 || h.ProbeTimeoutMs >= h.ProbeIntervalMs || h.ConfirmTimeoutMs < 50 || h.ConfirmTimeoutMs > 10000 || h.HealthLeaseMs < h.ProbeIntervalMs*(100+c.ProbeJitterPercent)/100+h.ProbeTimeoutMs || h.HealthLeaseMs > 120000 || h.DialReadyTimeoutMs < 200 || h.DialReadyTimeoutMs > 30000 || h.MinHealthyWorkers < 1 || h.MinHealthyWorkers > 8 || h.MaxIdleReadyWorkers < h.MinHealthyWorkers || h.MaxIdleReadyWorkers > 16 || h.MaxParallelDialsPerPair < 1 || h.MaxParallelDialsPerPair > 8 || h.ReconnectBackoffCapMs < 250 || h.ReconnectBackoffCapMs > 30000 {
		return c, errors.New("invalid or incomplete reverse health policy")
	}
	c.ProbeInterval = time.Duration(h.ProbeIntervalMs) * time.Millisecond
	c.ProbeTimeout = time.Duration(h.ProbeTimeoutMs) * time.Millisecond
	c.ConfirmTimeout = time.Duration(h.ConfirmTimeoutMs) * time.Millisecond
	c.HealthLease = time.Duration(h.HealthLeaseMs) * time.Millisecond
	c.ReadyTimeout = time.Duration(h.DialReadyTimeoutMs) * time.Millisecond
	c.MinHealthyWorkers = int(h.MinHealthyWorkers)
	c.MaxIdleReadyWorkers = int(h.MaxIdleReadyWorkers)
	c.MaxParallelDials = int(h.MaxParallelDialsPerPair)
	c.BackoffCap = time.Duration(h.ReconnectBackoffCapMs) * time.Millisecond
	return c, nil
}
