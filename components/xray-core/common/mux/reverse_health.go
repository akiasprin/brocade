package mux

import (
	"context"
	"encoding/json"
	"sort"
	"sync"
	"time"

	"github.com/xtls/xray-core/common/bitmask"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/signal/done"
)

// Reverse health is independent of the ordinary idle Mux pool. Both ends must
// run this protocol; an endpoint without VALIDATED support never becomes READY.
type ReverseHealthConfig struct {
	ProbeJitterPercent   uint32
	RecoverySuccesses    uint32
	SpareWorkers         uint32
	MaxHealthyWorkers    uint32
	MaxSessionsPerWorker uint32
	BackoffBase          time.Duration
	StableReset          time.Duration
	CanaryInterval       time.Duration
	CanaryTimeout        time.Duration
	CanarySuccesses      uint32
	CanaryStableWindow   time.Duration

	ActiveSessions                                           func() uint32
	DrainIdle                                                func() bool
	MinHealthyWorkers, MaxIdleReadyWorkers, MaxParallelDials int
	BackoffCap                                               time.Duration
	Pair                                                     string
	Role                                                     string
	ProbeInterval                                            time.Duration
	ProbeTimeout                                             time.Duration
	ConfirmTimeout                                           time.Duration
	HealthLease                                              time.Duration
	ReadyTimeout                                             time.Duration
	Wake                                                     chan<- struct{}
}

func DefaultReverseHealthConfig(pair, role string) ReverseHealthConfig {
	return ReverseHealthConfig{ProbeJitterPercent: 10, RecoverySuccesses: 2, SpareWorkers: 1, MaxHealthyWorkers: 32, MaxSessionsPerWorker: 16, BackoffBase: 250 * time.Millisecond, StableReset: 10000 * time.Millisecond, CanaryInterval: 1000 * time.Millisecond, CanaryTimeout: 750 * time.Millisecond, CanarySuccesses: 20, CanaryStableWindow: 10000 * time.Millisecond, MinHealthyWorkers: 2, MaxIdleReadyWorkers: 2, MaxParallelDials: 2, BackoffCap: 2 * time.Second, Pair: pair, Role: role, ProbeInterval: time.Second, ProbeTimeout: 750 * time.Millisecond, ConfirmTimeout: 750 * time.Millisecond, HealthLease: 3 * time.Second, ReadyTimeout: 2 * time.Second}
}

type ReverseHealthSnapshot struct {
	ActiveSessions     uint32 `json:"active_sessions"`
	AffectedSessions   uint32 `json:"affected_sessions"`
	ControlQueueDepth  int    `json:"control_queue_depth"`
	QueueDelayMS       int64  `json:"queue_delay_ms"`
	SchedulerLagMS     int64  `json:"scheduler_lag_ms"`
	WorkerID           uint64 `json:"worker_id,string"`
	Pair               string `json:"pair"`
	Role               string `json:"role"`
	State              string `json:"state"`
	Reason             string `json:"reason"`
	AckAgeMS           int64  `json:"ack_age_ms"`
	RTTMS              int64  `json:"rtt_ms"`
	Probes             uint64 `json:"probes"`
	Acks               uint64 `json:"acks"`
	Timeouts           uint64 `json:"timeouts"`
	RejectedDispatches uint64 `json:"rejected_dispatches"`
}
type ReverseHealthEvent struct {
	Sequence uint64 `json:"sequence"`
	AtUnixMS int64  `json:"at_unix_ms"`
	From     string `json:"from"`
	ReverseHealthSnapshot
}
type ReverseHealthReport struct {
	Canaries        []ReverseCanary         `json:"canaries"`
	BootID          uint64                  `json:"boot_id,string"`
	Sequence        uint64                  `json:"sequence"`
	SampledAtUnixMS int64                   `json:"sampled_at_unix_ms"`
	Workers         []ReverseHealthSnapshot `json:"workers"`
	Events          []ReverseHealthEvent    `json:"events"`
}

var reverseHealthRegistry = struct {
	sync.Mutex
	boot    uint64
	seq     uint64
	workers map[uint64]*ReverseHealth
	events  []ReverseHealthEvent
	logs    chan ReverseHealthEvent
}{boot: newProbeSeed(), workers: make(map[uint64]*ReverseHealth), logs: make(chan ReverseHealthEvent, 256)}

func init() {
	go func() {
		for e := range reverseHealthRegistry.logs {
			b, _ := json.Marshal(e)
			errors.LogInfo(context.Background(), "reverse_health ", string(b))
		}
	}()
}

// A snapshot plus a bounded event tail allows polling clients to detect sequence
// gaps and rebuild their state without ever blocking a data-plane callback.
func GetReverseHealthSnapshot() ReverseHealthReport {
	r := &reverseHealthRegistry
	r.Lock()
	report := ReverseHealthReport{BootID: r.boot, Sequence: r.seq, SampledAtUnixMS: time.Now().UnixMilli(), Workers: []ReverseHealthSnapshot{}, Events: append([]ReverseHealthEvent{}, r.events...)}
	report.Canaries = []ReverseCanary{}
	for _, c := range reverseCanaries {
		report.Canaries = append(report.Canaries, *c)
	}
	workers := make([]*ReverseHealth, 0, len(r.workers))
	for _, w := range r.workers {
		workers = append(workers, w)
	}
	r.Unlock()
	for _, w := range workers {
		report.Workers = append(report.Workers, w.Snapshot())
	}
	sort.Slice(report.Workers, func(i, j int) bool { return report.Workers[i].WorkerID < report.Workers[j].WorkerID })
	return report
}

type ReverseHealth struct {
	counters                                                   *reverseHealthCounters
	mu                                                         sync.Mutex
	config                                                     ReverseHealthConfig
	writer                                                     *healthWriter
	done                                                       *done.Instance
	snapshot                                                   ReverseHealthSnapshot
	started, lastAck, nextProbe, pendingDeadline, hardDeadline time.Time
	pendingID, nextID, peerID                                  uint64
	sentAt                                                     time.Time
	ownValidated, peerValidated                                bool
	recoveryAcks                                               int
}

func newReverseHealth(config ReverseHealthConfig, writer *healthWriter, closed *done.Instance) *ReverseHealth {
	now := time.Now()
	id := newProbeSeed()
	h := &ReverseHealth{counters: &reverseCounters[reverseRole(config.Role)], config: config, writer: writer, done: closed, started: now, nextProbe: now, nextID: id, snapshot: ReverseHealthSnapshot{WorkerID: id, Pair: config.Pair, Role: config.Role, State: "VALIDATING", Reason: "created"}}
	h.counters.created.Add(1)
	reverseHealthRegistry.Lock()
	reverseHealthRegistry.workers[id] = h
	reverseHealthRegistry.Unlock()
	h.mu.Lock()
	h.eventLocked("DIALING")
	h.mu.Unlock()
	go h.run()
	return h
}
func (h *ReverseHealth) eventLocked(from string) {
	r := &reverseHealthRegistry
	r.Lock()
	r.seq++
	snapshot := h.snapshotLocked(time.Now())
	e := ReverseHealthEvent{Sequence: r.seq, AtUnixMS: time.Now().UnixMilli(), From: from, ReverseHealthSnapshot: snapshot}
	if len(r.events) == 256 {
		copy(r.events, r.events[1:])
		r.events = r.events[:255]
	}
	r.events = append(r.events, e)
	select {
	case r.logs <- e:
	default:
		if h.counters != nil {
			h.counters.telemetryDropped.Add(1)
		}
	}
	r.Unlock()
	if h.config.Wake != nil {
		select {
		case h.config.Wake <- struct{}{}:
		default:
		}
	}
}
func (h *ReverseHealth) transitionLocked(state, reason string) {
	if h.snapshot.State == state {
		return
	}
	from := h.snapshot.State
	if state == "DEAD" && h.config.ActiveSessions != nil {
		h.snapshot.AffectedSessions = h.config.ActiveSessions()
	}
	h.snapshot.State = state
	h.snapshot.Reason = reason
	h.eventLocked(from)
}
func (h *ReverseHealth) stop(reason string) {
	h.mu.Lock()
	if h.snapshot.State != "CLOSED" {
		if reason == "transport_closed" && h.snapshot.State != "DEAD" {
			h.transitionLocked("DEAD", reason)
		}
		if h.counters != nil {
			h.counters.closed.Add(1)
		}
		h.transitionLocked("CLOSED", reason)
	}
	h.mu.Unlock()
	reverseHealthRegistry.Lock()
	delete(reverseHealthRegistry.workers, h.snapshot.WorkerID)
	reverseHealthRegistry.Unlock()
}
func (h *ReverseHealth) failLocked(reason string) {
	h.transitionLocked("DEAD", reason)
	h.done.Close() // done.Close only closes a channel; transport cleanup runs outside this lock.
}
func (h *ReverseHealth) sendLocked(id uint64, option bitmask.Byte) bool {
	if err := h.writer.enqueue(FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe | option, ProbeID: id}); err != nil {
		if h.counters != nil {
			h.counters.queueFailures.Add(1)
		}
		h.failLocked("control_queue_full_or_closed")
		return false
	}
	return true
}
func (h *ReverseHealth) probeLocked(now time.Time, timeout time.Duration) {
	h.nextID++
	if h.nextID == 0 {
		h.nextID++
	}
	h.pendingID = h.nextID
	h.sentAt = now
	h.pendingDeadline = now.Add(timeout)
	h.snapshot.Probes++
	if h.counters != nil {
		h.counters.probes.Add(1)
	}
	h.sendLocked(h.pendingID, 0)
}
func (h *ReverseHealth) checkLocked(now time.Time) {
	state := h.snapshot.State
	if state == "CLOSED" || state == "DEAD" {
		return
	}
	if state == "DRAINING" && h.config.DrainIdle != nil && h.config.DrainIdle() {
		h.failLocked("drain_complete")
		return
	}
	if h.done.Done() {
		h.transitionLocked("DEAD", "transport_closed")
		return
	}
	if state == "VALIDATING" && !now.Before(h.started.Add(h.config.ReadyTimeout)) {
		h.failLocked("validation_timeout")
		return
	}
	if state == "SUSPECT" && !now.Before(h.hardDeadline) {
		h.failLocked("confirmation_timeout")
		return
	}
	expired := h.pendingID != 0 && !now.Before(h.pendingDeadline)
	leaseExpired := !h.lastAck.IsZero() && !now.Before(h.lastAck.Add(h.config.HealthLease))
	if expired || leaseExpired {
		h.snapshot.Timeouts++
		if h.counters != nil {
			h.counters.timeouts.Add(1)
		}
		if state == "SUSPECT" || state == "DRAINING" {
			h.failLocked("probe_timeout")
			return
		}
		if state == "VALIDATING" {
			h.failLocked("initial_probe_timeout")
			return
		}
		h.transitionLocked("SUSPECT", "probe_timeout")
		h.recoveryAcks = 0
		h.hardDeadline = h.lastAck.Add(h.config.HealthLease + h.config.ConfirmTimeout)
		// Derive hard deadline from the original deadline, never from delayed callback execution.
		if expired {
			h.hardDeadline = h.pendingDeadline.Add(h.config.ConfirmTimeout)
		}
		if !now.Before(h.hardDeadline) {
			h.failLocked("confirmation_timeout")
			return
		}
		h.probeLocked(now, h.hardDeadline.Sub(now))
	}
}
func (h *ReverseHealth) usableLocked(now time.Time) bool {
	h.checkLocked(now)
	ok := h.snapshot.State == "READY" && !h.done.Done()

	return ok
}
func (h *ReverseHealth) Usable() bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.usableLocked(time.Now())
}
func (h *ReverseHealth) Snapshot() ReverseHealthSnapshot {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.checkLocked(time.Now())
	return h.snapshotLocked(time.Now())
}
func (h *ReverseHealth) snapshotLocked(now time.Time) ReverseHealthSnapshot {
	s := h.snapshot
	s.AckAgeMS = -1
	if !h.lastAck.IsZero() {
		s.AckAgeMS = now.Sub(h.lastAck).Milliseconds()
	}
	if h.config.ActiveSessions != nil {
		s.ActiveSessions = h.config.ActiveSessions()
	}
	if h.writer != nil {
		s.ControlQueueDepth = len(h.writer.control)
		s.QueueDelayMS = h.writer.queueDelayMS.Load()
	}
	return s
}

func (h *ReverseHealth) Drain() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.snapshot.State == "READY" {
		h.transitionLocked("DRAINING", "planned_rotation")
		h.sendLocked(h.nextID, OptionDrain)
	}
}
func (h *ReverseHealth) receive(meta *FrameMetadata) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	now := time.Now()
	h.checkLocked(now)
	if h.done.Done() {
		return errors.New("reverse worker closed")
	}
	switch {
	case meta.Option.Has(OptionDrain):
		if h.snapshot.State == "READY" {
			h.transitionLocked("DRAINING", "peer_rotation")
		}
	case meta.Option.Has(OptionValidated):
		if meta.ProbeID == h.peerID && h.peerID != 0 {
			h.peerValidated = true
		}
	case meta.Option.Has(OptionAck):
		if meta.ProbeID != h.pendingID || h.pendingID == 0 || !now.Before(h.pendingDeadline) {
			return nil
		}
		h.snapshot.Acks++
		h.snapshot.RTTMS = now.Sub(h.sentAt).Milliseconds()
		if h.counters != nil {
			h.counters.acks.Add(1)
			for i, b := range reverseRTTBuckets {
				if h.snapshot.RTTMS <= b {
					h.counters.rtt[i].Add(1)
				}
			}
		}
		h.lastAck = now
		h.pendingID = 0
		if !h.ownValidated {
			h.ownValidated = true
			if !h.sendLocked(meta.ProbeID, OptionValidated) {
				return errors.New("reverse validation write failed")
			}
		}
		if h.snapshot.State == "SUSPECT" {
			h.recoveryAcks++
			if uint32(h.recoveryAcks) < h.config.RecoverySuccesses {
				h.probeLocked(now, time.Until(h.hardDeadline))
				return nil
			}
			h.transitionLocked("READY", "confirmed_recovery")
		}
		// Per-worker jitter prevents synchronized fleet probes.
		h.nextProbe = now.Add(h.probeDelay())
	default:
		h.peerID = meta.ProbeID
		if !h.sendLocked(meta.ProbeID, OptionAck) {
			return errors.New("reverse pong write failed")
		}
	}
	if h.snapshot.State == "VALIDATING" && h.ownValidated && h.peerValidated {
		h.transitionLocked("READY", "bidirectional_validated")
	}
	return nil
}
func (h *ReverseHealth) run() {
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	defer h.stop("transport_closed")
	for {
		h.mu.Lock()
		now := time.Now()
		h.checkLocked(now)
		if h.snapshot.State != "CLOSED" && !h.done.Done() && h.pendingID == 0 && !now.Before(h.nextProbe) {
			timeout := h.config.ProbeTimeout
			if h.snapshot.State == "VALIDATING" {
				timeout = time.Until(h.started.Add(h.config.ReadyTimeout))
			}
			h.probeLocked(now, timeout)
		}
		h.mu.Unlock()
		select {
		case <-h.done.Wait():
			return
		case fired := <-ticker.C:
			h.mu.Lock()
			h.snapshot.SchedulerLagMS = time.Since(fired).Milliseconds()
			h.mu.Unlock()
		}
	}
}
func (m *ClientWorker) ReverseHealth() *ReverseHealth { return m.health }
func (w *ServerWorker) ReverseHealth() *ReverseHealth { return w.health }

func (h *ReverseHealth) probeDelay() time.Duration {
	jitter := uint64(h.config.ProbeJitterPercent) * 10
	factor := 1000 - jitter + h.nextID%(2*jitter+1)
	return h.config.ProbeInterval * time.Duration(factor) / 1000
}
