package vless

import (
	"net/url"
	"time"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/mux"
)

// A present group must be complete. Missing values are not field-by-field inheritance: both
// directions must compile the same effective policy. Omitting the entire group selects defaults.
func (r *Reverse) HealthConfig(role string) (mux.ReverseHealthConfig, error) {
	c := mux.DefaultReverseHealthConfig(r.Tag, role)
	h := r.Health
	if h == nil {
		return c, nil
	}
	c.DisconnectOnHealthFailure = h.DisconnectOnHealthFailure
	t := h.Tuning
	if t == nil {
		return c, errors.New("invalid or incomplete reverse health policy: tuning")
	}
	if t.ProbeJitterPercent > 100 {
		return c, errors.New("invalid reverse health tuning: probe_jitter_percent")
	}
	if t.RecoverySuccesses < 1 {
		return c, errors.New("invalid reverse health tuning: recovery_successes")
	}
	if t.MaxHealthyWorkers < 1 {
		return c, errors.New("invalid reverse health tuning: max_healthy_workers")
	}
	if t.MaxSessionsPerWorker > 65535 {
		return c, errors.New("invalid reverse health tuning: max_sessions_per_worker")
	}
	if t.ReconnectBackoffBaseMs < 1 {
		return c, errors.New("invalid reverse health tuning: reconnect_backoff_base_ms")
	}
	c.ProbeJitterPercent = t.ProbeJitterPercent
	c.RecoverySuccesses = t.RecoverySuccesses
	c.SpareWorkers = t.SpareWorkers
	c.MaxHealthyWorkers = t.MaxHealthyWorkers
	c.MaxSessionsPerWorker = t.MaxSessionsPerWorker
	c.BackoffBase = time.Duration(t.ReconnectBackoffBaseMs) * time.Millisecond
	c.StableReset = time.Duration(t.ReconnectStableResetMs) * time.Millisecond
	if c.BackoffBase > time.Duration(h.ReconnectBackoffCapMs)*time.Millisecond || c.MaxHealthyWorkers < h.MaxIdleReadyWorkers || c.SpareWorkers > h.MaxIdleReadyWorkers {
		return c, errors.New("inconsistent reverse health tuning")
	}
	minimumLease := uint64(h.ProbeIntervalMs)*(100+uint64(c.ProbeJitterPercent))/100 + uint64(h.ProbeTimeoutMs)
	if h.ProbeIntervalMs < 1 || h.ProbeTimeoutMs < 1 || h.ProbeTimeoutMs >= h.ProbeIntervalMs || h.ConfirmTimeoutMs < 1 || uint64(h.HealthLeaseMs) < minimumLease || h.DialReadyTimeoutMs < 1 || h.MinHealthyWorkers < 1 || h.MaxIdleReadyWorkers < h.MinHealthyWorkers || h.MaxParallelDialsPerPair < 1 || h.ReconnectBackoffCapMs < 1 {
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

func (r *Reverse) CanaryConfig() (mux.ReverseCanaryConfig, error) {
	c := mux.DefaultReverseCanaryConfig()
	if r.CanaryUrl == "" {
		return c, nil
	}
	u, err := url.Parse(r.CanaryUrl)
	if err != nil || u.Hostname() == "" || (u.Scheme != "http" && u.Scheme != "https") || u.User != nil {
		return c, errors.New("reverse canary_url must be an HTTP(S) URL without credentials")
	}
	if r.Health == nil {
		return c, nil
	}
	if r.Health.Tuning == nil {
		return c, errors.New("invalid or incomplete reverse health policy: tuning")
	}
	t := r.Health.Tuning
	if t.CanaryIntervalMs < 1 {
		return c, errors.New("invalid reverse canary policy")
	}
	c.Interval = time.Duration(t.CanaryIntervalMs) * time.Millisecond
	c.Timeout = time.Duration(t.CanaryTimeoutMs) * time.Millisecond
	c.Successes = t.CanarySuccesses
	c.StableWindow = time.Duration(t.CanaryStableWindowMs) * time.Millisecond
	return c, nil
}
