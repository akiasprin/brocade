package vless

import (
	"encoding/json"
	"testing"
	"time"
)

func TestReverseHealthCompletePolicyValidation(t *testing.T) {
	h := &ReverseHealth{ProbeIntervalMs: 1000, ProbeTimeoutMs: 750, ConfirmTimeoutMs: 750, HealthLeaseMs: 3000, MinHealthyWorkers: 2, MaxIdleReadyWorkers: 2, MaxParallelDialsPerPair: 2, DialReadyTimeoutMs: 2000, ReconnectBackoffCapMs: 2000}
	r := &Reverse{Tag: "pair", Health: h, CanaryUrl: "https://example.test/health"}
	if _, err := r.HealthConfig("portal"); err != nil {
		t.Fatal(err)
	}
	h.ProbeIntervalMs = 750
	if _, err := r.HealthConfig("bridge"); err == nil {
		t.Fatal("timeout >= interval accepted")
	}
	r.Health = &ReverseHealth{ProbeIntervalMs: 1000}
	if _, err := r.HealthConfig("bridge"); err == nil {
		t.Fatal("partial policy accepted")
	}
	r.Health = nil
	r.CanaryUrl = "http://user:password@example.test/health"
	if _, err := r.HealthConfig("portal"); err == nil {
		t.Fatal("canary credentials accepted")
	}
}

func TestReverseTuningJSONCompatibilityAndValidation(t *testing.T) {
	legacy := `{"tag":"test","health":{"probe_interval_ms":1000,"probe_timeout_ms":750,"confirm_timeout_ms":750,"health_lease_ms":3000,"min_healthy_workers":2,"max_idle_ready_workers":2,"max_parallel_dials_per_pair":2,"dial_ready_timeout_ms":2000,"reconnect_backoff_cap_ms":2000}}`
	var r Reverse
	if err := json.Unmarshal([]byte(legacy), &r); err != nil {
		t.Fatal(err)
	}
	c, err := r.HealthConfig("portal")
	if err != nil || c.CanarySuccesses != 20 || c.MaxSessionsPerWorker != 16 {
		t.Fatalf("legacy defaults: %+v %v", c, err)
	}
	tuning := `{"probe_jitter_percent":25,"recovery_successes":3,"spare_workers":2,"max_healthy_workers":7,"max_sessions_per_worker":4,"reconnect_backoff_base_ms":100,"reconnect_stable_reset_ms":2000,"canary_interval_ms":2000,"canary_timeout_ms":1000,"canary_successes":4,"canary_stable_window_ms":5000}`
	r.Health.Tuning = &ReverseHealthTuning{}
	if err := json.Unmarshal([]byte(tuning), r.Health.Tuning); err != nil {
		t.Fatal(err)
	}
	c, err = r.HealthConfig("bridge")
	if err != nil || c.ProbeJitterPercent != 25 || c.RecoverySuccesses != 3 || c.SpareWorkers != 2 || c.MaxHealthyWorkers != 7 || c.MaxSessionsPerWorker != 4 || c.BackoffBase != 100*time.Millisecond || c.StableReset != 2*time.Second || c.CanaryInterval != 2*time.Second || c.CanaryTimeout != time.Second || c.CanarySuccesses != 4 || c.CanaryStableWindow != 5*time.Second {
		t.Fatalf("tuning not applied: %+v %v", c, err)
	}
	t0 := *r.Health.Tuning
	for _, mutate := range []func(*ReverseHealthTuning){
		func(t *ReverseHealthTuning) { t.RecoverySuccesses = 0 },
		func(t *ReverseHealthTuning) { t.CanaryTimeoutMs = t.CanaryIntervalMs },
		func(t *ReverseHealthTuning) { t.MaxHealthyWorkers = 1 },
		func(t *ReverseHealthTuning) { t.SpareWorkers = 3 },
		func(t *ReverseHealthTuning) { t.ReconnectBackoffBaseMs = 3000 },
	} {
		copy := t0
		mutate(&copy)
		r.Health.Tuning = &copy
		if _, err := r.HealthConfig("bridge"); err == nil {
			t.Fatalf("invalid tuning accepted: %+v", copy)
		}
	}
	r.Health.Tuning = &ReverseHealthTuning{RecoverySuccesses: 3}
	if _, err := r.HealthConfig("bridge"); err == nil {
		t.Fatal("partial tuning accepted")
	}
}
