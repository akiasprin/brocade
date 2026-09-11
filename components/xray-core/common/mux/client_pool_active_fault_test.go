package mux

import (
	"testing"
	"time"
)

func TestActiveTCPBlackholeQuarantinesNewRequestsAndPreservesOldFlow(t *testing.T) {
	cfg := &WorkerPoolConfig{ReuseThreshold: 1, MaxProbingWorkers: 1, ProbeInterval: 2 * time.Second, ProbeTimeout: 200 * time.Millisecond, IdleTTL: 30 * time.Second}
	proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
	factory := &tcpWorkerFactory{address: proxy.address(), strategy: ClientStrategy{MaxConcurrency: 8, MaxConnection: 128, WorkerPool: cfg}}
	clock := newFakePoolClock()
	p := newPoolPickerForTest(clock, cfg, factory)
	m := &ClientManager{Enabled: true, Picker: p}
	t.Cleanup(func() { m.Close() })
	old := openFaultEchoSession(t, m)
	old.echo(t, "active-before-probe")
	p.access.Lock()
	w := p.workers[0]
	p.access.Unlock()
	clock.Advance(cfg.ProbeInterval)
	waitForTest(t, "real active Ping/Pong", func() bool { return p.WorkerPoolStats().ProbeAckTotal == 1 })
	old.echo(t, "active-after-probe")
	proxy.connection(0).setMode(faultBlackhole, 0)
	clock.Advance(cfg.ProbeInterval)
	clock.Advance(cfg.ProbeTimeout)
	if !w.IsFull() || w.Closed() || w.ActiveConnections() != 1 {
		t.Fatal("blackhole did not quarantine only new admission")
	}
	fresh := openFaultEchoSession(t, m)
	fresh.echo(t, "new-request-uses-new-carrier")
	if factory.created.Load() != 2 {
		t.Fatalf("created %d carriers; expected one replacement", factory.created.Load())
	}
	clock.Advance(cfg.confirmTimeout())
	if workerStateForTest(w) != workerDraining || w.Closed() {
		t.Fatal("failed confirmation must drain, not kill old TCP flow")
	}
	proxy.connection(0).setMode(faultPass, 0)
	old.echo(t, "old-flow-survives-and-resumes-after-route-recovers")
	old.input.Close()
	waitForTest(t, "drain after old session finishes", w.Closed)
	fresh.input.Close()
}
