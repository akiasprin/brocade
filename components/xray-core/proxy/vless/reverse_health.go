package vless

import (
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/mux"
	"net/url"
	"time"
)

// A present group must be complete. Missing values are not field-by-field
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
	if h.ProbeIntervalMs < 100 || h.ProbeIntervalMs > 60000 || h.ProbeTimeoutMs < 50 || h.ProbeTimeoutMs > 10000 || h.ProbeTimeoutMs >= h.ProbeIntervalMs || h.ConfirmTimeoutMs < 50 || h.ConfirmTimeoutMs > 10000 || h.HealthLeaseMs < h.ProbeIntervalMs*11/10+h.ProbeTimeoutMs || h.HealthLeaseMs > 120000 || h.DialReadyTimeoutMs < 200 || h.DialReadyTimeoutMs > 30000 || h.MinHealthyWorkers < 1 || h.MinHealthyWorkers > 8 || h.MaxIdleReadyWorkers < h.MinHealthyWorkers || h.MaxIdleReadyWorkers > 16 || h.MaxParallelDialsPerPair < 1 || h.MaxParallelDialsPerPair > 8 || h.ReconnectBackoffCapMs < 250 || h.ReconnectBackoffCapMs > 30000 {
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
