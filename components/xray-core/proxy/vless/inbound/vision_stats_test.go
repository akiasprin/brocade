package inbound

import (
	"context"
	"testing"

	appstats "github.com/xtls/xray-core/app/stats"
	"github.com/xtls/xray-core/common/session"
)

func TestVisionStatsRecordDirectAndSpliceOnce(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	tracker := newVisionStats(manager).newTracker()
	tracker.MarkDirect()
	tracker.MarkDirect()
	tracker.AddDirectBytes(128)
	tracker.MarkSplice()
	tracker.MarkSplice()
	tracker.AddSpliceBytes(4096)
	assertVisionCounter(t, manager, "completed_connections", 0)
	assertVisionCounter(t, manager, "completed_direct_connections", 0)
	assertVisionCounter(t, manager, "completed_splice_connections", 0)
	tracker.Finish()
	tracker.Finish()

	assertVisionCounter(t, manager, "connections", 1)
	assertVisionCounter(t, manager, "completed_connections", 1)
	assertVisionCounter(t, manager, "direct_connections", 1)
	assertVisionCounter(t, manager, "completed_direct_connections", 1)
	assertVisionCounter(t, manager, "direct_bytes", 128)
	assertVisionCounter(t, manager, "splice_connections", 1)
	assertVisionCounter(t, manager, "completed_splice_connections", 1)
	assertVisionCounter(t, manager, "splice_bytes", 4096)
	assertVisionCounter(t, manager, "not_spliced_connections", 0)
	assertVisionCounter(t, manager, "not_spliced>>>unknown", 0)
}

func TestVisionStatsRecordNotSplicedReason(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	vision := newVisionStats(manager)

	explicit := vision.newTracker()
	explicit.SetNotSplicedReason(session.SpliceNotUsedUnsupportedTransport)
	explicit.Finish()

	afterDirect := vision.newTracker()
	afterDirect.MarkDirect()
	afterDirect.Finish()

	beforeDirect := vision.newTracker()
	beforeDirect.Finish()

	assertVisionCounter(t, manager, "connections", 3)
	assertVisionCounter(t, manager, "completed_connections", 3)
	assertVisionCounter(t, manager, "completed_direct_connections", 1)
	assertVisionCounter(t, manager, "completed_splice_connections", 0)
	assertVisionCounter(t, manager, "not_spliced_connections", 3)
	assertVisionCounter(t, manager, "not_spliced>>>unsupported_transport", 1)
	assertVisionCounter(t, manager, "not_spliced>>>ended_before_splice", 1)
	assertVisionCounter(t, manager, "not_spliced>>>ended_before_direct", 1)
}

func assertVisionCounter(t *testing.T, manager *appstats.Manager, suffix string, want int64) {
	t.Helper()
	counter := manager.GetCounter(visionStatsPrefix + suffix)
	if counter == nil {
		t.Fatalf("counter %q is not registered", visionStatsPrefix+suffix)
	}
	if got := counter.Value(); got != want {
		t.Fatalf("counter %q = %d, want %d", visionStatsPrefix+suffix, got, want)
	}
}
