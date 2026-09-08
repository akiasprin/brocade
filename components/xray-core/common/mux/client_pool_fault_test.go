package mux

import (
	"context"
	"fmt"
	"io"
	stdnet "net"
	"os"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

type faultMode uint32

const (
	faultPass faultMode = iota
	faultBlackhole
	faultDelay
)

type faultTCPConnection struct {
	front        stdnet.Conn
	back         stdnet.Conn
	owner        *faultTCPProxy
	uplinkMode   atomic.Uint32
	downlinkMode atomic.Uint32
	delay        atomic.Int64
	once         sync.Once
}

func (c *faultTCPConnection) setMode(mode faultMode, delay time.Duration) {
	c.delay.Store(int64(delay))
	c.uplinkMode.Store(uint32(mode))
	c.downlinkMode.Store(uint32(mode))
}

func (c *faultTCPConnection) setUplinkMode(mode faultMode)   { c.uplinkMode.Store(uint32(mode)) }
func (c *faultTCPConnection) setDownlinkMode(mode faultMode) { c.downlinkMode.Store(uint32(mode)) }

func (c *faultTCPConnection) close(reset bool) {
	c.once.Do(func() {
		if reset {
			if tcp, ok := c.front.(*stdnet.TCPConn); ok {
				_ = tcp.SetLinger(0)
			}
		}
		_ = c.front.Close()
		_ = c.back.Close()
		if c.owner != nil {
			c.owner.active.Add(-1)
		}
	})
}

func (c *faultTCPConnection) forward(dst, src stdnet.Conn, mode *atomic.Uint32) {
	defer c.close(false)
	payload := make([]byte, 32*1024)
	for {
		n, err := src.Read(payload)
		if n > 0 {
			switch faultMode(mode.Load()) {
			case faultBlackhole:
				// Consume without forwarding: the TCP connection remains open but
				// its peer never observes the bytes or a close signal.
				continue
			case faultDelay:
				time.Sleep(time.Duration(c.delay.Load()))
			}
			if _, writeErr := dst.Write(payload[:n]); writeErr != nil {
				return
			}
		}
		if err != nil {
			return
		}
	}
}

type faultTCPProxy struct {
	listener stdnet.Listener
	mu       sync.Mutex
	conns    []*faultTCPConnection
	servers  []*ServerWorker
	active   atomic.Int32
	done     chan struct{}
}

func newFaultTCPProxy(t *testing.T, dispatcher routing.Dispatcher) *faultTCPProxy {
	t.Helper()
	listener, err := stdnet.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	proxy := &faultTCPProxy{listener: listener, done: make(chan struct{})}
	go proxy.accept(dispatcher)
	t.Cleanup(proxy.close)
	return proxy
}

func (p *faultTCPProxy) accept(dispatcher routing.Dispatcher) {
	defer close(p.done)
	for {
		front, err := p.listener.Accept()
		if err != nil {
			return
		}
		proxyBack, serverBack := stdnet.Pipe()
		conn := &faultTCPConnection{front: front, back: proxyBack}
		serverContext := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{}})
		server, err := NewServerWorker(serverContext, dispatcher, &transport.Link{
			Reader: buf.NewReader(serverBack),
			Writer: buf.NewWriter(serverBack),
		})
		if err != nil {
			conn.close(false)
			_ = serverBack.Close()
			continue
		}
		conn.owner = p
		p.active.Add(1)
		p.mu.Lock()
		p.conns = append(p.conns, conn)
		p.servers = append(p.servers, server)
		p.mu.Unlock()
		go conn.forward(proxyBack, front, &conn.uplinkMode)
		go conn.forward(front, proxyBack, &conn.downlinkMode)
	}
}

func (p *faultTCPProxy) address() string { return p.listener.Addr().String() }

func (p *faultTCPProxy) connection(index int) *faultTCPConnection {
	p.mu.Lock()
	defer p.mu.Unlock()
	if index < 0 || index >= len(p.conns) {
		return nil
	}
	return p.conns[index]
}

func (p *faultTCPProxy) connectionCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(p.conns)
}

func (p *faultTCPProxy) activeConnectionCount() int32 { return p.active.Load() }

func (p *faultTCPProxy) close() {
	_ = p.listener.Close()
	<-p.done
	p.mu.Lock()
	connections := append([]*faultTCPConnection(nil), p.conns...)
	servers := append([]*ServerWorker(nil), p.servers...)
	p.mu.Unlock()
	for _, connection := range connections {
		connection.close(false)
	}
	for _, server := range servers {
		_ = server.Close()
	}
}

type faultEchoDispatcher struct{}

func (*faultEchoDispatcher) Dispatch(context.Context, net.Destination) (*transport.Link, error) {
	requestReader, requestWriter := pipe.New(pipe.WithoutSizeLimit())
	responseReader, responseWriter := pipe.New(pipe.WithoutSizeLimit())
	go func() {
		_ = buf.Copy(requestReader, responseWriter)
		_ = responseWriter.Close()
	}()
	return &transport.Link{Reader: responseReader, Writer: requestWriter}, nil
}

func (*faultEchoDispatcher) DispatchLink(context.Context, net.Destination, *transport.Link) error {
	return nil
}
func (*faultEchoDispatcher) Start() error      { return nil }
func (*faultEchoDispatcher) Close() error      { return nil }
func (*faultEchoDispatcher) Type() interface{} { return routing.DispatcherType() }

type tcpWorkerFactory struct {
	address  string
	strategy ClientStrategy
	created  atomic.Int32
}

func (f *tcpWorkerFactory) Create() (*ClientWorker, error) {
	connection, err := stdnet.Dial("tcp", f.address)
	if err != nil {
		return nil, err
	}
	f.created.Add(1)
	return NewClientWorker(transport.Link{
		Reader: buf.NewReader(connection),
		Writer: buf.NewWriter(connection),
	}, f.strategy)
}

type faultEchoSession struct {
	input  *pipe.Writer
	output *pipe.Reader
}

func openFaultEchoSessionE(manager *ClientManager) (*faultEchoSession, error) {
	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	outputReader, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("echo.test"), 443),
	}})
	if err := manager.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}); err != nil {
		return nil, err
	}
	return &faultEchoSession{input: inputWriter, output: outputReader}, nil
}

func openFaultEchoSession(t *testing.T, manager *ClientManager) *faultEchoSession {
	t.Helper()
	session, err := openFaultEchoSessionE(manager)
	if err != nil {
		t.Fatal(err)
	}
	return session
}

func (s *faultEchoSession) echoE(payload string) error {
	data := buf.FromBytes([]byte(payload))
	if err := s.input.WriteMultiBuffer(buf.MultiBuffer{data}); err != nil {
		return err
	}
	result, err := s.output.ReadMultiBuffer()
	if err != nil && err != io.EOF {
		return err
	}
	if got := result.String(); got != payload {
		return fmt.Errorf("echo response = %q, want %q", got, payload)
	}
	return nil
}

func (s *faultEchoSession) echo(t *testing.T, payload string) {
	t.Helper()
	if err := s.echoE(payload); err != nil {
		t.Fatal(err)
	}
}

func dispatchEcho(t *testing.T, manager *ClientManager, payload string) *pipe.Writer {
	t.Helper()
	session := openFaultEchoSession(t, manager)
	session.echo(t, payload)
	return session.input
}

func TestFaultProxyBlackholedProbeDoesNotDelayNextRequest(t *testing.T) {
	cfg := &WorkerPoolConfig{
		MinIdleWorkers:    0,
		MaxIdleWorkers:    2,
		MaxProbingWorkers: 1,
		ProbeInterval:     2 * time.Second,
		ProbeTimeout:      200 * time.Millisecond,
		IdleTTL:           5 * time.Second,
	}
	proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
	factory := &tcpWorkerFactory{
		address: proxy.address(),
		strategy: ClientStrategy{
			MaxConcurrency: 1,
			MaxConnection:  128,
			WorkerPool:     cfg,
		},
	}
	clock := newFakePoolClock()
	picker := newPoolPickerForTest(clock, cfg, factory)
	manager := &ClientManager{Enabled: true, Picker: picker}
	t.Cleanup(func() { manager.Close() })

	firstInput := dispatchEcho(t, manager, "first")
	waitForTest(t, "first TCP connection", func() bool { return proxy.connectionCount() == 1 })
	proxy.connection(0).setMode(faultBlackhole, 0)
	_ = firstInput.Close()

	picker.access.Lock()
	firstWorker := picker.workers[0]
	picker.access.Unlock()
	waitForTest(t, "first worker freshly idle", func() bool { return workerStateForTest(firstWorker) == workerIdleReady })
	clock.Advance(cfg.ProbeInterval)
	waitForTest(t, "first worker probing", func() bool { return workerStateForTest(firstWorker) == workerProbing })

	started := time.Now()
	secondInput := dispatchEcho(t, manager, "second")
	if elapsed := time.Since(started); elapsed >= cfg.ProbeTimeout {
		t.Fatalf("new request waited for probe timeout: %s", elapsed)
	}
	if got := factory.created.Load(); got != 2 {
		t.Fatalf("TCP connections created = %d, want 2", got)
	}
	_ = secondInput.Close()
	waitForTest(t, "blackholed Ping written", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	clock.Advance(cfg.ProbeTimeout)
	waitForTest(t, "blackholed worker timeout", firstWorker.Closed)

	picker.access.Lock()
	var healthy *ClientWorker
	for _, worker := range picker.workers {
		if worker != firstWorker {
			healthy = worker
			break
		}
	}
	picker.access.Unlock()
	if healthy == nil {
		t.Fatal("healthy replacement worker not found")
	}
	waitForTest(t, "replacement probe", func() bool { return workerStateForTest(healthy) == workerIdleReady })
	thirdInput := dispatchEcho(t, manager, "third")
	_ = thirdInput.Close()
	if got := factory.created.Load(); got != 2 {
		t.Fatalf("healthy worker was not reused; TCP connections = %d", got)
	}
}

func TestFaultProxyFINAndRSTRecovery(t *testing.T) {
	for _, reset := range []bool{false, true} {
		name := map[bool]string{false: "FIN", true: "RST"}[reset]
		t.Run(name, func(t *testing.T) {
			cfg := &WorkerPoolConfig{
				MaxIdleWorkers:    2,
				MaxProbingWorkers: 1,
				ProbeInterval:     2 * time.Second,
				ProbeTimeout:      200 * time.Millisecond,
				IdleTTL:           5 * time.Second,
			}
			proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
			factory := &tcpWorkerFactory{
				address: proxy.address(),
				strategy: ClientStrategy{
					MaxConcurrency: 1,
					MaxConnection:  128,
					WorkerPool:     cfg,
				},
			}
			picker := &IncrementalWorkerPicker{Factory: factory, Pool: cfg}
			manager := &ClientManager{Enabled: true, Picker: picker}
			t.Cleanup(func() { manager.Close() })

			input := dispatchEcho(t, manager, "before-close")
			_ = input.Close()
			waitForTest(t, "first TCP connection", func() bool { return proxy.connectionCount() == 1 })
			picker.access.Lock()
			firstWorker := picker.workers[0]
			picker.access.Unlock()
			waitForTest(t, "idle-ready worker", func() bool { return workerStateForTest(firstWorker) == workerIdleReady })
			proxy.connection(0).close(reset)
			waitForTest(t, "closed worker", firstWorker.Closed)

			replacement := dispatchEcho(t, manager, "after-close")
			_ = replacement.Close()
			if got := factory.created.Load(); got != 2 {
				t.Fatalf("TCP connections created = %d, want 2", got)
			}
		})
	}
}

func TestFaultProxyOneWayProbeLossTimesOut(t *testing.T) {
	for _, direction := range []string{"ping", "pong"} {
		t.Run(direction, func(t *testing.T) {
			cfg := &WorkerPoolConfig{
				MaxIdleWorkers:    2,
				MaxProbingWorkers: 1,
				ProbeInterval:     2 * time.Second,
				ProbeTimeout:      200 * time.Millisecond,
				IdleTTL:           5 * time.Second,
			}
			proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
			factory := &tcpWorkerFactory{
				address: proxy.address(),
				strategy: ClientStrategy{
					MaxConcurrency: 1,
					MaxConnection:  128,
					WorkerPool:     cfg,
				},
			}
			clock := newFakePoolClock()
			picker := newPoolPickerForTest(clock, cfg, factory)
			manager := &ClientManager{Enabled: true, Picker: picker}
			t.Cleanup(func() { manager.Close() })

			input := dispatchEcho(t, manager, "before-loss")
			waitForTest(t, "TCP connection", func() bool { return proxy.connectionCount() == 1 })
			if direction == "ping" {
				proxy.connection(0).setUplinkMode(faultBlackhole)
			} else {
				proxy.connection(0).setDownlinkMode(faultBlackhole)
			}
			_ = input.Close()
			picker.access.Lock()
			worker := picker.workers[0]
			picker.access.Unlock()
			waitForTest(t, "freshly idle worker", func() bool { return workerStateForTest(worker) == workerIdleReady })
			clock.Advance(cfg.ProbeInterval)
			waitForTest(t, "Ping written", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
			clock.Advance(cfg.ProbeTimeout)
			waitForTest(t, "probe timeout", worker.Closed)
			stats := picker.WorkerPoolStats()
			if stats.ProbeTimeoutTotal != 1 || stats.WorkerClosedProbeTotal != 1 {
				t.Fatalf("probe loss stats = %+v", stats)
			}
		})
	}
}

func TestProbeFailureDoesNotInterruptAnotherActiveWorker(t *testing.T) {
	cfg := &WorkerPoolConfig{
		MaxIdleWorkers:    2,
		MaxProbingWorkers: 1,
		ProbeInterval:     2 * time.Second,
		ProbeTimeout:      200 * time.Millisecond,
		IdleTTL:           5 * time.Second,
	}
	proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
	factory := &tcpWorkerFactory{
		address: proxy.address(),
		strategy: ClientStrategy{
			MaxConcurrency: 1,
			MaxConnection:  128,
			WorkerPool:     cfg,
		},
	}
	clock := newFakePoolClock()
	picker := newPoolPickerForTest(clock, cfg, factory)
	manager := &ClientManager{Enabled: true, Picker: picker}
	t.Cleanup(func() { manager.Close() })

	active := openFaultEchoSession(t, manager)
	active.echo(t, "active-before")
	failing := openFaultEchoSession(t, manager)
	failing.echo(t, "will-fail")
	waitForTest(t, "two TCP connections", func() bool { return proxy.connectionCount() == 2 })
	proxy.connection(1).setMode(faultBlackhole, 0)
	_ = failing.input.Close()

	picker.access.Lock()
	workers := append([]*ClientWorker(nil), picker.workers...)
	picker.access.Unlock()
	if len(workers) != 2 {
		t.Fatalf("worker count = %d, want 2", len(workers))
	}
	waitForTest(t, "second worker freshly idle", func() bool { return workerStateForTest(workers[1]) == workerIdleReady })
	clock.Advance(cfg.ProbeInterval)
	waitForTest(t, "second worker probing", func() bool { return workerStateForTest(workers[1]) == workerProbing })
	active.echo(t, "active-during-probe")
	waitForTest(t, "second Ping written", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	clock.Advance(cfg.ProbeTimeout)
	waitForTest(t, "second worker timeout", workers[1].Closed)
	active.echo(t, "active-after-timeout")
	_ = active.input.Close()
}

func TestWorkerPoolResourcePressureAndConvergence(t *testing.T) {
	if os.Getenv("XRAY_MUX_STRESS") != "1" {
		t.Skip("set XRAY_MUX_STRESS=1 to run the production resource gate")
	}
	baselineGoroutines := runtime.NumGoroutine()
	baselineFDs := -1
	if runtime.GOOS == "linux" {
		entries, err := os.ReadDir("/proc/self/fd")
		if err != nil {
			t.Fatal(err)
		}
		baselineFDs = len(entries)
	}

	cfg := &WorkerPoolConfig{
		MaxIdleWorkers:    8,
		MaxProbingWorkers: 8,
		ProbeInterval:     2 * time.Second,
		ProbeTimeout:      200 * time.Millisecond,
		IdleTTL:           5 * time.Second,
	}
	proxy := newFaultTCPProxy(t, &faultEchoDispatcher{})
	factory := &tcpWorkerFactory{
		address: proxy.address(),
		strategy: ClientStrategy{
			MaxConcurrency: 1,
			MaxConnection:  4096,
			WorkerPool:     cfg,
		},
	}
	picker := &IncrementalWorkerPicker{Factory: factory, Pool: cfg, Tag: "resource-pressure"}
	manager := &ClientManager{Enabled: true, Picker: picker}

	for i := range 1000 {
		s, err := openFaultEchoSessionE(manager)
		if err != nil {
			t.Fatalf("sequential stream %d: %v", i, err)
		}
		if err := s.echoE("sequential"); err != nil {
			t.Fatalf("sequential stream %d: %v", i, err)
		}
		_ = s.input.Close()
	}
	waitForConvergence := func(phase string) {
		t.Helper()
		convergenceDeadline := time.Now().Add(cfg.IdleTTL + 5*time.Second)
		for {
			stats := picker.WorkerPoolStats()
			reservedIdle := stats.WorkersIdleReady + stats.WorkersProbeQueued + stats.WorkersProbing + stats.WorkersWarmDialing
			if stats.WorkersActive == 0 && stats.WorkersDraining == 0 && reservedIdle <= cfg.MaxIdleWorkers {
				return
			}
			if time.Now().After(convergenceDeadline) {
				var activeEmpty, activeSessions, activeReservations int
				picker.access.Lock()
				for _, worker := range picker.workers {
					state, _ := worker.poolStateSnapshot()
					if state != workerActive {
						continue
					}
					size := worker.sessionManager.Size()
					worker.poolAccess.Lock()
					reservations := worker.poolReservations
					worker.poolAccess.Unlock()
					switch {
					case size > 0:
						activeSessions++
					case reservations > 0:
						activeReservations++
					default:
						activeEmpty++
					}
				}
				picker.access.Unlock()
				t.Fatalf("%s pool did not converge: %+v; active empty=%d sessions=%d reservations=%d", phase, stats, activeEmpty, activeSessions, activeReservations)
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	waitForConvergence("sequential pressure")

	start := make(chan struct{})
	errs := make(chan error, 128)
	var group sync.WaitGroup
	group.Add(128)
	for i := range 128 {
		go func(index int) {
			defer group.Done()
			<-start
			s, err := openFaultEchoSessionE(manager)
			if err == nil {
				err = s.echoE(fmt.Sprintf("concurrent-%03d", index))
				_ = s.input.Close()
			}
			errs <- err
		}(i)
	}
	close(start)
	group.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}

	waitForConvergence("concurrent pressure")
	if stats := picker.WorkerPoolStats(); stats.WorkersIdleReady+stats.WorkersProbeQueued+stats.WorkersProbing+stats.WorkersWarmDialing > cfg.MaxIdleWorkers {
		t.Fatalf("pool did not honor max idle after pressure: %+v", stats)
	}

	if err := manager.Close(); err != nil {
		t.Fatal(err)
	}
	proxy.close()
	waitForTest(t, "all pressure sockets to close", func() bool {
		return proxy.activeConnectionCount() == 0
	})
	runtime.GC()
	waitForTest(t, "goroutine convergence", func() bool {
		return runtime.NumGoroutine() <= baselineGoroutines+8
	})
	if baselineFDs >= 0 {
		waitForTest(t, "file descriptor convergence", func() bool {
			entries, err := os.ReadDir("/proc/self/fd")
			return err == nil && len(entries) <= baselineFDs+4
		})
	}
}
