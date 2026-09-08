package mux

import (
	"context"
	"testing"
	"time"

	appdispatcher "github.com/xtls/xray-core/app/dispatcher"
	apppolicy "github.com/xtls/xray-core/app/policy"
	appstats "github.com/xtls/xray-core/app/stats"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	featurepolicy "github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/features/routing"
	featurestats "github.com/xtls/xray-core/features/stats"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

type accountingEchoDispatcher struct {
	policy featurepolicy.Manager
	stats  featurestats.Manager
}

func (d *accountingEchoDispatcher) Dispatch(ctx context.Context, _ net.Destination) (*transport.Link, error) {
	requestReader, requestWriter := pipe.New(pipe.WithoutSizeLimit())
	responseReader, responseWriter := pipe.New(pipe.WithoutSizeLimit())
	link := appdispatcher.WrapLink(ctx, d.policy, d.stats, &transport.Link{
		Reader: responseReader,
		Writer: requestWriter,
	})
	go func() {
		_ = buf.Copy(requestReader, responseWriter)
		_ = responseWriter.Close()
	}()
	return link, nil
}

func (*accountingEchoDispatcher) DispatchLink(context.Context, net.Destination, *transport.Link) error {
	return nil
}
func (*accountingEchoDispatcher) Start() error      { return nil }
func (*accountingEchoDispatcher) Close() error      { return nil }
func (*accountingEchoDispatcher) Type() interface{} { return routing.DispatcherType() }

func TestMuxProbeDoesNotChangeUserTrafficCounters(t *testing.T) {
	policyManager, err := apppolicy.New(context.Background(), &apppolicy.Config{
		Level: map[uint32]*apppolicy.Policy{
			0: {Stats: &apppolicy.Policy_Stats{UserUplink: true, UserDownlink: true}},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	statsManager, err := appstats.NewManager(context.Background(), &appstats.Config{})
	if err != nil {
		t.Fatal(err)
	}
	dispatcher := &accountingEchoDispatcher{policy: policyManager, stats: statsManager}

	cfg := testPoolConfig()
	clock := newFakePoolClock()
	uplinkReader, uplinkWriter := pipe.New(pipe.WithoutSizeLimit())
	downlinkReader, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
	serverCtx := session.ContextWithInbound(context.Background(), &session.Inbound{User: &protocol.MemoryUser{
		Email: "mux-billing@example.com",
		Level: 0,
	}})
	server, err := NewServerWorker(serverCtx, dispatcher, &transport.Link{Reader: uplinkReader, Writer: downlinkWriter})
	if err != nil {
		t.Fatal(err)
	}
	client, err := NewClientWorker(transport.Link{Reader: downlinkReader, Writer: uplinkWriter}, ClientStrategy{
		MaxConcurrency: 1,
		MaxConnection:  128,
		WorkerPool:     cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	addWorkerForTest(t, picker, client, workerIdleReady, clock.Now())
	manager := &ClientManager{Enabled: true, Picker: picker}
	t.Cleanup(func() {
		_ = manager.Close()
		_ = server.Close()
	})

	business := openFaultEchoSession(t, manager)
	business.echo(t, "billable payload")
	_ = business.input.Close()
	waitForTest(t, "business session idle", func() bool { return workerStateForTest(client) == workerIdleReady })
	if picker.WorkerPoolStats().ProbeSentTotal != 0 {
		t.Fatal("recent echoed business traffic triggered a redundant probe")
	}
	clock.Advance(2 * time.Second)
	next := openFaultEchoSession(t, manager)
	next.echo(t, "second billable payload")
	_ = next.input.Close()
	waitForTest(t, "second business session idle", func() bool { return workerStateForTest(client) == workerIdleReady })
	if picker.WorkerPoolStats().ProbeSentTotal != 0 || factory.count() != 0 {
		t.Fatal("short consecutive business flows probed or replaced their worker")
	}

	uplink := statsManager.GetCounter("user>>>mux-billing@example.com>>>traffic>>>uplink")
	downlink := statsManager.GetCounter("user>>>mux-billing@example.com>>>traffic>>>downlink")
	if uplink == nil || downlink == nil || uplink.Value()+downlink.Value() == 0 {
		t.Fatal("business traffic did not reach the real Xray user counters")
	}
	beforeUplink, beforeDownlink := uplink.Value(), downlink.Value()

	clock.Advance(cfg.ProbeInterval)
	waitForTest(t, "periodic probe round trip", func() bool {
		stats := picker.WorkerPoolStats()
		// The peer's final End frame can arrive after local session closure.
		// That is legitimate I/O and moves the deadline. Advance to that deadline
		// while idle, but never advance an in-flight probe ahead of its real Pong.
		if stats.ProbeSentTotal == 0 {
			client.poolAccess.Lock()
			due := client.nextProbeAt
			ready := client.poolState == workerIdleReady
			client.poolAccess.Unlock()
			if ready && due.After(clock.Now()) {
				clock.Advance(due.Sub(clock.Now()))
			}
		}
		return stats.ProbeAckTotal == 1 && workerStateForTest(client) == workerIdleReady
	})
	if uplink.Value() != beforeUplink || downlink.Value() != beforeDownlink {
		t.Fatalf("probe changed user traffic counters: before=(%d,%d) after=(%d,%d)", beforeUplink, beforeDownlink, uplink.Value(), downlink.Value())
	}
}
