package mux

import "time"

type ReverseCanary struct {
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
func NewReverseCanary(pair string) *ReverseCanary {
	c := &ReverseCanary{Pair: pair, State: "UNKNOWN", Reason: "awaiting_probe"}
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
		if c.ConsecutiveSuccesses >= 20 && now.UnixMilli()-c.FirstOKUnixMS >= 10000 {
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
