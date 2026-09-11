package mux

import "time"

type ReverseCanaryConfig struct {
	Interval     time.Duration
	Timeout      time.Duration
	Successes    uint32
	StableWindow time.Duration
}

func DefaultReverseCanaryConfig() ReverseCanaryConfig {
	return ReverseCanaryConfig{
		Interval:     time.Second,
		Timeout:      750 * time.Millisecond,
		Successes:    20,
		StableWindow: 10 * time.Second,
	}
}

type ReverseCanary struct {
	FreshnessBudgetMS    int64 `json:"freshness_budget_ms"`
	requiredSuccesses    uint64
	stableWindow         time.Duration
	Pair                 string `json:"pair"`
	State                string `json:"state"`
	Reason               string `json:"reason"`
	SampledAtUnixMS      int64  `json:"sampled_at_unix_ms"`
	LatencyMS            int64  `json:"latency_ms"`
	ConsecutiveSuccesses uint64 `json:"consecutive_successes"`
	FirstOKUnixMS        int64  `json:"first_ok_unix_ms"`
	StableSinceUnixMS    int64  `json:"stable_since_unix_ms"`
	LastFailureUnixMS    int64  `json:"last_failure_unix_ms"`
	Attempts             uint64 `json:"attempts"`
	Failures             uint64 `json:"failures"`
}

var reverseCanaries = make(map[string]*ReverseCanary) // guarded by reverseHealthRegistry

func NewReverseCanary(pair string, config ReverseCanaryConfig) *ReverseCanary {
	c := &ReverseCanary{FreshnessBudgetMS: max(15*time.Second, 2*config.Interval+config.Timeout).Milliseconds(), requiredSuccesses: uint64(config.Successes), stableWindow: config.StableWindow, Pair: pair, State: "UNKNOWN", Reason: "awaiting_probe"}
	reverseHealthRegistry.Lock()
	reverseCanaries[pair] = c
	reverseHealthRegistry.seq++
	reverseHealthRegistry.Unlock()
	return c
}

func (c *ReverseCanary) Record(started time.Time, reason string) {
	now := time.Now()
	reverseHealthRegistry.Lock()
	defer reverseHealthRegistry.Unlock()
	if reverseCanaries[c.Pair] != c {
		return
	}
	c.SampledAtUnixMS = now.UnixMilli()
	c.LatencyMS = now.Sub(started).Milliseconds()
	c.Attempts++
	before := c.State
	c.Reason = reason
	if reason == "success" {
		c.State = "AVAILABLE"
		c.ConsecutiveSuccesses++
		if c.ConsecutiveSuccesses == 1 {
			c.FirstOKUnixMS = now.UnixMilli()
		}
		if c.ConsecutiveSuccesses >= c.requiredSuccesses && now.UnixMilli()-c.FirstOKUnixMS >= c.stableWindow.Milliseconds() {
			c.StableSinceUnixMS = c.FirstOKUnixMS
		}
	} else {
		c.State = "FAILED"
		c.Failures++
		c.ConsecutiveSuccesses = 0
		c.StableSinceUnixMS = 0
		c.FirstOKUnixMS = 0
		c.LastFailureUnixMS = now.UnixMilli()
	}
	if before != c.State {
		reverseHealthRegistry.seq++
	}
}

func (c *ReverseCanary) Close() {
	reverseHealthRegistry.Lock()
	defer reverseHealthRegistry.Unlock()
	if reverseCanaries[c.Pair] == c {
		delete(reverseCanaries, c.Pair)
		reverseHealthRegistry.seq++
	}
}
