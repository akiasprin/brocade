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
func TestReverseDialAdmissionPrioritizesMissingPairsAndIsBounded(t *testing.T) {
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
	acquired := 0
	for a.acquireDial() {
		acquired++
	}
	for i := 0; i < acquired; i++ {
		releaseReverseDial()
	}
	if acquired != 32 {
		t.Fatalf("node dial budget=%d", acquired)
	}
}
