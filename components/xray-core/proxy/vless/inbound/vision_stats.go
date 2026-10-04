package inbound

import (
	"sync/atomic"

	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/stats"
)

const visionStatsPrefix = "vless>>>vision>>>"

type visionStats struct {
	connections stats.Counter
	// Completion counters share the Finish boundary. Their deltas let an
	// observer calculate splice success without mixing newly opened and still
	// active connections:
	// completed_splice_connections / completed_direct_connections.
	completedConnections       stats.Counter
	directConnections          stats.Counter
	completedDirectConnections stats.Counter
	directBytes                stats.Counter
	spliceConnections          stats.Counter
	completedSpliceConnections stats.Counter
	spliceBytes                stats.Counter
	notSplicedTotal            stats.Counter
	notSpliced                 [session.SpliceNotUsedEndedBeforeSplice + 1]stats.Counter
}

func newVisionStats(manager stats.Manager) *visionStats {
	result := &visionStats{
		connections:                getOrRegisterVisionCounter(manager, visionStatsPrefix+"connections"),
		completedConnections:       getOrRegisterVisionCounter(manager, visionStatsPrefix+"completed_connections"),
		directConnections:          getOrRegisterVisionCounter(manager, visionStatsPrefix+"direct_connections"),
		completedDirectConnections: getOrRegisterVisionCounter(manager, visionStatsPrefix+"completed_direct_connections"),
		directBytes:                getOrRegisterVisionCounter(manager, visionStatsPrefix+"direct_bytes"),
		spliceConnections:          getOrRegisterVisionCounter(manager, visionStatsPrefix+"splice_connections"),
		completedSpliceConnections: getOrRegisterVisionCounter(manager, visionStatsPrefix+"completed_splice_connections"),
		spliceBytes:                getOrRegisterVisionCounter(manager, visionStatsPrefix+"splice_bytes"),
		notSplicedTotal:            getOrRegisterVisionCounter(manager, visionStatsPrefix+"not_spliced_connections"),
	}
	for reason, suffix := range map[session.SpliceNotUsedReason]string{
		session.SpliceNotUsedUnknown:                  "unknown",
		session.SpliceNotUsedUnsupportedCommand:       "unsupported_command",
		session.SpliceNotUsedUnsupportedTransport:     "unsupported_transport",
		session.SpliceNotUsedOuterTLSNot13:            "outer_tls_not_13",
		session.SpliceNotUsedGloballyDisabled:         "globally_disabled",
		session.SpliceNotUsedOutboundNotRaw:           "outbound_not_raw",
		session.SpliceNotUsedInboundIneligible:        "inbound_ineligible",
		session.SpliceNotUsedMissingOutbound:          "missing_outbound",
		session.SpliceNotUsedOutboundIneligible:       "outbound_ineligible",
		session.SpliceNotUsedRawConnectionUnavailable: "raw_connection_unavailable",
		session.SpliceNotUsedEndedBeforeDirect:        "ended_before_direct",
		session.SpliceNotUsedEndedBeforeSplice:        "ended_before_splice",
	} {
		result.notSpliced[reason] = getOrRegisterVisionCounter(manager, visionStatsPrefix+"not_spliced>>>"+suffix)
	}
	return result
}

func getOrRegisterVisionCounter(manager stats.Manager, name string) stats.Counter {
	if manager == nil {
		return nil
	}
	if counter := manager.GetCounter(name); counter != nil {
		return counter
	}
	counter, err := manager.RegisterCounter(name)
	if err == nil {
		return counter
	}
	// Dynamic inbound handlers may register the shared process counters at the
	// same time. Treat the winner's counter as authoritative.
	return manager.GetCounter(name)
}

func (s *visionStats) newTracker() session.SpliceMetrics {
	if s == nil || s.connections == nil {
		return nil
	}
	s.connections.Add(1)
	return &visionTracker{stats: s}
}

type visionTracker struct {
	stats    *visionStats
	direct   atomic.Bool
	spliced  atomic.Bool
	finished atomic.Bool
	reason   atomic.Uint32
}

func (t *visionTracker) MarkDirect() {
	if t != nil && t.direct.CompareAndSwap(false, true) && t.stats.directConnections != nil {
		t.stats.directConnections.Add(1)
	}
}

func (t *visionTracker) AddDirectBytes(bytes int64) {
	if t != nil && bytes > 0 && t.stats.directBytes != nil {
		t.stats.directBytes.Add(bytes)
	}
}

func (t *visionTracker) MarkSplice() {
	if t != nil && t.spliced.CompareAndSwap(false, true) && t.stats.spliceConnections != nil {
		t.stats.spliceConnections.Add(1)
	}
}

func (t *visionTracker) AddSpliceBytes(bytes int64) {
	if t != nil && bytes > 0 && t.stats.spliceBytes != nil {
		t.stats.spliceBytes.Add(bytes)
	}
}

func (t *visionTracker) SetNotSplicedReason(reason session.SpliceNotUsedReason) {
	if t == nil || reason == session.SpliceNotUsedUnknown {
		return
	}
	t.reason.CompareAndSwap(uint32(session.SpliceNotUsedUnknown), uint32(reason))
}

func (t *visionTracker) Finish() {
	if t == nil || !t.finished.CompareAndSwap(false, true) {
		return
	}
	if t.stats.completedConnections != nil {
		t.stats.completedConnections.Add(1)
	}
	if t.direct.Load() && t.stats.completedDirectConnections != nil {
		t.stats.completedDirectConnections.Add(1)
	}
	if t.spliced.Load() {
		if t.stats.completedSpliceConnections != nil {
			t.stats.completedSpliceConnections.Add(1)
		}
		return
	}
	reason := session.SpliceNotUsedReason(t.reason.Load())
	if reason == session.SpliceNotUsedUnknown {
		if t.direct.Load() {
			reason = session.SpliceNotUsedEndedBeforeSplice
		} else {
			reason = session.SpliceNotUsedEndedBeforeDirect
		}
	}
	if t.stats.notSplicedTotal != nil {
		t.stats.notSplicedTotal.Add(1)
	}
	if int(reason) < len(t.stats.notSpliced) {
		if counter := t.stats.notSpliced[reason]; counter != nil {
			counter.Add(1)
		}
	}
}
