package vless

import (
	"encoding/json"
	"reflect"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/mux"
	"google.golang.org/protobuf/proto"
)

func defaultReverseTuningForTest() *ReverseHealthTuning {
	return &ReverseHealthTuning{
		ProbeJitterPercent: 10, RecoverySuccesses: 2, SpareWorkers: 1,
		MaxHealthyWorkers: 32, MaxSessionsPerWorker: 16,
		ReconnectBackoffBaseMs: 250, ReconnectStableResetMs: 10000,
		CanaryIntervalMs: 1000, CanaryTimeoutMs: 750, CanarySuccesses: 20,
		CanaryStableWindowMs: 10000,
	}
}

func TestReversePolicyFieldsKeepTheirOwnMeaning(t *testing.T) {
	r := &Reverse{
		Tag:       "sentinel-pair",
		CanaryUrl: "https://example.test/health",
		Health: &ReverseHealth{
			DisconnectOnHealthFailure: true,
			ProbeIntervalMs:           1103,
			ProbeTimeoutMs:            211,
			ConfirmTimeoutMs:          307,
			HealthLeaseMs:             1907,
			MinHealthyWorkers:         2,
			MaxIdleReadyWorkers:       5,
			MaxParallelDialsPerPair:   3,
			DialReadyTimeoutMs:        2309,
			ReconnectBackoffCapMs:     2701,
			Tuning: &ReverseHealthTuning{
				ProbeJitterPercent:     17,
				RecoverySuccesses:      4,
				SpareWorkers:           3,
				MaxHealthyWorkers:      11,
				MaxSessionsPerWorker:   13,
				ReconnectBackoffBaseMs: 401,
				ReconnectStableResetMs: 3301,
				CanaryIntervalMs:       1201,
				CanaryTimeoutMs:        503,
				CanarySuccesses:        7,
				CanaryStableWindowMs:   4501,
			},
		},
	}

	got, err := r.HealthConfig("portal")
	if err != nil {
		t.Fatal(err)
	}
	want := mux.ReverseHealthConfig{
		DisconnectOnHealthFailure: true,
		ProbeJitterPercent:        17,
		RecoverySuccesses:         4,
		SpareWorkers:              3,
		MaxHealthyWorkers:         11,
		MaxSessionsPerWorker:      13,
		BackoffBase:               401 * time.Millisecond,
		StableReset:               3301 * time.Millisecond,
		MinHealthyWorkers:         2,
		MaxIdleReadyWorkers:       5,
		MaxParallelDials:          3,
		BackoffCap:                2701 * time.Millisecond,
		Pair:                      "sentinel-pair",
		Role:                      "portal",
		ProbeInterval:             1103 * time.Millisecond,
		ProbeTimeout:              211 * time.Millisecond,
		ConfirmTimeout:            307 * time.Millisecond,
		HealthLease:               1907 * time.Millisecond,
		ReadyTimeout:              2309 * time.Millisecond,
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("reverse policy field mapping changed:\n got: %+v\nwant: %+v", got, want)
	}
	canary, err := r.CanaryConfig()
	if err != nil {
		t.Fatal(err)
	}
	if want := (mux.ReverseCanaryConfig{Interval: 1201 * time.Millisecond, Timeout: 503 * time.Millisecond, Successes: 7, StableWindow: 4501 * time.Millisecond}); canary != want {
		t.Fatalf("canary field mapping changed: got %+v want %+v", canary, want)
	}
}

func TestReverseHealthCompletePolicyValidation(t *testing.T) {
	h := &ReverseHealth{DisconnectOnHealthFailure: true, ProbeIntervalMs: 1000, ProbeTimeoutMs: 750, ConfirmTimeoutMs: 750, HealthLeaseMs: 3000, MinHealthyWorkers: 2, MaxIdleReadyWorkers: 2, MaxParallelDialsPerPair: 2, DialReadyTimeoutMs: 2000, ReconnectBackoffCapMs: 2000, Tuning: defaultReverseTuningForTest()}
	r := &Reverse{Tag: "pair", Health: h, CanaryUrl: "https://example.test/health"}
	if c, err := r.HealthConfig("portal"); err != nil {
		t.Fatal(err)
	} else if !c.DisconnectOnHealthFailure {
		t.Fatal("active disconnect setting was not applied")
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
	if _, err := r.CanaryConfig(); err == nil {
		t.Fatal("canary credentials accepted")
	}
}

func TestReverseTuningJSONRequiresCompletePolicyAndValidates(t *testing.T) {
	partial := `{"tag":"test","health":{"probe_interval_ms":1000,"probe_timeout_ms":750,"confirm_timeout_ms":750,"health_lease_ms":3000,"min_healthy_workers":2,"max_idle_ready_workers":2,"max_parallel_dials_per_pair":2,"dial_ready_timeout_ms":2000,"reconnect_backoff_cap_ms":2000}}`
	var r Reverse
	if err := json.Unmarshal([]byte(partial), &r); err != nil {
		t.Fatal(err)
	}
	if _, err := r.HealthConfig("portal"); err == nil {
		t.Fatal("policy without tuning was accepted")
	}
	tuning := `{"probe_jitter_percent":25,"recovery_successes":3,"spare_workers":2,"max_healthy_workers":7,"max_sessions_per_worker":4,"reconnect_backoff_base_ms":100,"reconnect_stable_reset_ms":2000,"canary_interval_ms":2000,"canary_timeout_ms":1000,"canary_successes":4,"canary_stable_window_ms":5000}`
	r.Health.Tuning = &ReverseHealthTuning{}
	if err := json.Unmarshal([]byte(tuning), r.Health.Tuning); err != nil {
		t.Fatal(err)
	}
	c, err := r.HealthConfig("bridge")
	if err != nil || c.ProbeJitterPercent != 25 || c.RecoverySuccesses != 3 || c.SpareWorkers != 2 || c.MaxHealthyWorkers != 7 || c.MaxSessionsPerWorker != 4 || c.BackoffBase != 100*time.Millisecond || c.StableReset != 2*time.Second {
		t.Fatalf("tuning not applied: %+v %v", c, err)
	}
	r.CanaryUrl = "https://example.test/health"
	canary, err := r.CanaryConfig()
	if err != nil || canary.Interval != 2*time.Second || canary.Timeout != time.Second || canary.Successes != 4 || canary.StableWindow != 5*time.Second {
		t.Fatalf("canary tuning not applied: %+v %v", canary, err)
	}
	t0 := r.Health.Tuning
	for _, mutate := range []func(*ReverseHealthTuning){
		func(t *ReverseHealthTuning) { t.RecoverySuccesses = 0 },
		func(t *ReverseHealthTuning) { t.MaxHealthyWorkers = 1 },
		func(t *ReverseHealthTuning) { t.SpareWorkers = 3 },
		func(t *ReverseHealthTuning) { t.ReconnectBackoffBaseMs = 3000 },
	} {
		copy := proto.Clone(t0).(*ReverseHealthTuning)
		mutate(copy)
		r.Health.Tuning = copy
		if _, err := r.HealthConfig("bridge"); err == nil {
			t.Fatalf("invalid tuning accepted: %+v", copy)
		}
	}
	r.Health.Tuning = &ReverseHealthTuning{RecoverySuccesses: 3}
	if _, err := r.HealthConfig("bridge"); err == nil {
		t.Fatal("partial tuning accepted")
	}
	r.Health.Tuning = &ReverseHealthTuning{
		ProbeJitterPercent: 10, RecoverySuccesses: 2, MaxHealthyWorkers: 2,
		MaxSessionsPerWorker: 1, ReconnectBackoffBaseMs: 1,
	}
	if _, err := r.CanaryConfig(); err == nil {
		t.Fatal("partial canary tuning accepted when canary is enabled")
	}
}
