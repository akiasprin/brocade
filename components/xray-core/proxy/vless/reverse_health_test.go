package vless

import "testing"

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
