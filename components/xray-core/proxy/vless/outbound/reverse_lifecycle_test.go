package outbound

import (
	"context"
	"github.com/xtls/xray-core/common/mux"
	"testing"
	"time"
)

func TestReverseCloseBeforeDelayedStartCannotResurrect(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	r := &Reverse{ctx: ctx, cancel: cancel, health: mux.DefaultReverseHealthConfig("test", "bridge"), wake: make(chan struct{}, 1)}
	finished := make(chan struct{})
	go func() { r.Start(); close(finished) }()
	r.Close()
	r.Close()
	r.Start()
	select {
	case <-finished:
	case <-time.After(time.Second):
		t.Fatal("start outlived Close")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.workers) != 0 || !r.closed {
		t.Fatal("closed reverse resurrected")
	}
}
func TestReverseDialAdmissionPrioritizesMissingPairsWithoutAnArtificialNodeCap(t *testing.T) {
	a, b := &Reverse{}, &Reverse{}
	a.reportDialDemand(2, 0, true)
	b.reportDialDemand(0, 0, true)
	defer a.forgetDialDemand()
	defer b.forgetDialDemand()
	if a.acquireDial() {
		releaseReverseDial()
		t.Fatal("spare jumped ahead of missing primary")
	}
	if !b.acquireDial() {
		t.Fatal("missing primary rejected")
	}
	releaseReverseDial()
	const attempts = 1024
	for i := 0; i < attempts; i++ {
		if !a.acquireDial() {
			t.Fatalf("dial %d hit an artificial node cap", i+1)
		}
	}
	for i := 0; i < attempts; i++ {
		releaseReverseDial()
	}
}

func TestReverseConfiguredCapacityAndBackoff(t *testing.T) {
	r := &Reverse{health: mux.DefaultReverseHealthConfig("tuning", "bridge")}
	r.health.SpareWorkers = 3
	r.health.MaxHealthyWorkers = 7
	for busy, want := range map[int]int{0: 3, 2: 5, 20: 7} {
		if got := r.desiredWorkers(busy); got != want {
			t.Fatalf("busy=%d desired=%d want=%d", busy, got, want)
		}
	}
	r.health.BackoffBase = 50 * time.Millisecond
	r.health.BackoffCap = 30 * time.Second
	for failures, want := range map[uint]time.Duration{0: 50 * time.Millisecond, 3: 400 * time.Millisecond, 10: 30 * time.Second, 100: 30 * time.Second} {
		r.failures = failures
		if got := r.backoffBase(); got != want {
			t.Fatalf("failures=%d base=%v want=%v", failures, got, want)
		}
	}
	r.health.BackoffBase = time.Duration(^uint32(0)) * time.Millisecond
	r.health.BackoffCap = r.health.BackoffBase
	r.failures = 10
	if got := r.backoffBase(); got != r.health.BackoffCap {
		t.Fatalf("large backoff overflowed: %v", got)
	}
}
