package session

import (
	"sync"
	"sync/atomic"
	"testing"
)

func TestAtomicSpliceCopyStateTransition(t *testing.T) {
	var state AtomicSpliceCopyState
	state.Store(SpliceCopyWaiting)

	var winners atomic.Int32
	var wait sync.WaitGroup
	for range 32 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			if state.CompareAndSwap(SpliceCopyWaiting, SpliceCopyDirect) {
				winners.Add(1)
			}
			_ = state.Load()
		}()
	}
	wait.Wait()

	if got := winners.Load(); got != 1 {
		t.Fatalf("successful state transitions = %d, want 1", got)
	}
	if got := state.Load(); got != SpliceCopyDirect {
		t.Fatalf("state = %d, want %d", got, SpliceCopyDirect)
	}
	if !state.CompareAndSwap(SpliceCopyDirect, SpliceCopySplicing) {
		t.Fatal("direct to splicing transition failed")
	}
	if got := state.Load(); got != SpliceCopySplicing {
		t.Fatalf("state = %d, want %d", got, SpliceCopySplicing)
	}
}

func TestInboundCloneLoadsSpliceState(t *testing.T) {
	inbound := &Inbound{Name: "vless"}
	inbound.CanSpliceCopy.Store(SpliceCopyWaiting)

	clone := inbound.Clone()
	clone.CanSpliceCopy.Store(SpliceCopyDisabled)

	if got := inbound.CanSpliceCopy.Load(); got != SpliceCopyWaiting {
		t.Fatalf("original state = %d, want %d", got, SpliceCopyWaiting)
	}
	if got := clone.CanSpliceCopy.Load(); got != SpliceCopyDisabled {
		t.Fatalf("clone state = %d, want %d", got, SpliceCopyDisabled)
	}
}
