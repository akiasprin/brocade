package outbound_test

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/policy"
	"github.com/xtls/xray-core/app/proxyman"
	. "github.com/xtls/xray-core/app/proxyman/outbound"
	"github.com/xtls/xray-core/app/stats"
	commonmux "github.com/xtls/xray-core/common/mux"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/session"
	core "github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/outbound"
	"github.com/xtls/xray-core/proxy/freedom"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet/stat"
	_ "github.com/xtls/xray-core/transport/internet/tcp"
)

func TestInterfaces(t *testing.T) {
	_ = (outbound.Handler)(new(Handler))
	_ = (outbound.Manager)(new(Manager))
}

const xrayKey core.XrayKey = 1

func TestOutboundWithoutStatCounter(t *testing.T) {
	config := &core.Config{
		App: []*serial.TypedMessage{
			serial.ToTypedMessage(&stats.Config{}),
			serial.ToTypedMessage(&policy.Config{
				System: &policy.SystemPolicy{
					Stats: &policy.SystemPolicy_Stats{
						InboundUplink: true,
					},
				},
			}),
		},
	}

	v, _ := core.New(config)
	v.AddFeature((outbound.Manager)(new(Manager)))
	ctx := context.WithValue(context.Background(), xrayKey, v)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{}})
	h, _ := NewHandler(ctx, &core.OutboundHandlerConfig{
		Tag:           "tag",
		ProxySettings: serial.ToTypedMessage(&freedom.Config{}),
	})
	conn, _ := h.(*Handler).Dial(ctx, net.TCPDestination(net.DomainAddress("localhost"), 13146))
	_, ok := conn.(*stat.CounterConnection)
	if ok {
		t.Errorf("Expected conn to not be CounterConnection")
	}
}

func TestOutboundWithStatCounter(t *testing.T) {
	config := &core.Config{
		App: []*serial.TypedMessage{
			serial.ToTypedMessage(&stats.Config{}),
			serial.ToTypedMessage(&policy.Config{
				System: &policy.SystemPolicy{
					Stats: &policy.SystemPolicy_Stats{
						OutboundUplink:   true,
						OutboundDownlink: true,
					},
				},
			}),
		},
	}

	v, _ := core.New(config)
	v.AddFeature((outbound.Manager)(new(Manager)))
	ctx := context.WithValue(context.Background(), xrayKey, v)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{}})
	h, _ := NewHandler(ctx, &core.OutboundHandlerConfig{
		Tag:           "tag",
		ProxySettings: serial.ToTypedMessage(&freedom.Config{}),
	})
	conn, _ := h.(*Handler).Dial(ctx, net.TCPDestination(net.DomainAddress("localhost"), 13146))
	_, ok := conn.(*stat.CounterConnection)
	if !ok {
		t.Errorf("Expected conn to be CounterConnection")
	}
}

func TestHandlerRejectsInvalidMuxWorkerPoolProto(t *testing.T) {
	validPool := &proxyman.WorkerPoolConfig{
		ReuseThreshold:       2,
		MaxProbingWorkers:    1,
		ProbeIntervalMs:      5000,
		ProbeTimeoutMs:       2000,
		IdleTtlMs:            24000,
		MaxRequestsPerWorker: 128,
	}
	tests := map[string]*proxyman.MultiplexingConfig{
		"invalid values": {
			Enabled:     true,
			Concurrency: 1,
			WorkerPool: &proxyman.WorkerPoolConfig{
				ReuseThreshold:       2,
				MaxProbingWorkers:    1,
				ProbeIntervalMs:      5000,
				ProbeTimeoutMs:       5000,
				IdleTtlMs:            24000,
				MaxRequestsPerWorker: 128,
			},
		},
		"disabled mux": {
			Enabled:     false,
			Concurrency: 1,
			WorkerPool:  validPool,
		},
	}
	for name, muxConfig := range tests {
		t.Run(name, func(t *testing.T) {
			v, _ := core.New(&core.Config{})
			v.AddFeature((outbound.Manager)(new(Manager)))
			ctx := context.WithValue(context.Background(), xrayKey, v)
			_, err := NewHandler(ctx, &core.OutboundHandlerConfig{
				Tag: "invalid-pool",
				SenderSettings: serial.ToTypedMessage(&proxyman.SenderConfig{
					MultiplexSettings: muxConfig,
				}),
				ProxySettings: serial.ToTypedMessage(&freedom.Config{}),
			})
			if err == nil {
				t.Fatal("invalid direct protobuf worker pool was accepted")
			}
		})
	}
}

func TestHandlerPublishesManagedMuxPoolForObservation(t *testing.T) {
	v, _ := core.New(&core.Config{})
	v.AddFeature((outbound.Manager)(new(Manager)))
	ctx := context.WithValue(context.Background(), xrayKey, v)
	handler, err := NewHandler(ctx, &core.OutboundHandlerConfig{
		Tag: "out:app-a/chain-a>peer-a",
		SenderSettings: serial.ToTypedMessage(&proxyman.SenderConfig{
			MultiplexSettings: &proxyman.MultiplexingConfig{
				Enabled:     true,
				Concurrency: 8,
				WorkerPool: &proxyman.WorkerPoolConfig{
					PrewarmWorkers:       1,
					ReuseThreshold:       2,
					MaxProbingWorkers:    1,
					ProbeIntervalMs:      5000,
					ProbeTimeoutMs:       2000,
					IdleTtlMs:            24000,
					MaxRequestsPerWorker: 128,
				},
			},
		}),
		ProxySettings: serial.ToTypedMessage(&freedom.Config{}),
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = handler.Close() })

	for _, pool := range commonmux.GetMuxSnapshot().Pools {
		if pool.Pair == "out:app-a/chain-a>peer-a" && pool.Kind == "tcp" {
			if pool.Config.Concurrency != 8 || pool.Config.PrewarmWorkers != 1 {
				t.Fatalf("effective pool config = %+v", pool.Config)
			}
			return
		}
	}
	t.Fatal("managed Mux pool was not published")
}

type drainingTestHandler struct {
	drained atomic.Bool
}

func (*drainingTestHandler) Start() error                              { return nil }
func (*drainingTestHandler) Close() error                              { return nil }
func (*drainingTestHandler) Tag() string                               { return "draining-test" }
func (*drainingTestHandler) Dispatch(context.Context, *transport.Link) {}
func (*drainingTestHandler) SenderSettings() *serial.TypedMessage      { return nil }
func (*drainingTestHandler) ProxySettings() *serial.TypedMessage       { return nil }
func (h *drainingTestHandler) Drain()                                  { h.drained.Store(true) }

func TestRemoveHandlerDrainsRemovedHandler(t *testing.T) {
	manager, err := New(context.Background(), &proxyman.OutboundConfig{})
	if err != nil {
		t.Fatal(err)
	}
	handler := &drainingTestHandler{}
	if err := manager.AddHandler(context.Background(), handler); err != nil {
		t.Fatal(err)
	}
	if err := manager.RemoveHandler(context.Background(), handler.Tag()); err != nil {
		t.Fatal(err)
	}
	if !handler.drained.Load() {
		t.Fatal("removed handler was not drained")
	}
}

func TestTagsCache(t *testing.T) {

	test_duration := 10 * time.Second
	threads_num := 50
	delay := 10 * time.Millisecond
	tags_prefix := "node"

	tags := sync.Map{}
	counter := atomic.Uint64{}

	ohm, err := New(context.Background(), &proxyman.OutboundConfig{})
	if err != nil {
		t.Error("failed to create outbound handler manager")
	}
	config := &core.Config{
		App: []*serial.TypedMessage{},
	}
	v, _ := core.New(config)
	v.AddFeature(ohm)
	ctx := context.WithValue(context.Background(), xrayKey, v)

	stop_add_rm := false
	wg_add_rm := sync.WaitGroup{}
	addHandlers := func() {
		defer wg_add_rm.Done()
		for !stop_add_rm {
			time.Sleep(delay)
			idx := counter.Add(1)
			tag := fmt.Sprintf("%s%d", tags_prefix, idx)
			cfg := &core.OutboundHandlerConfig{
				Tag:           tag,
				ProxySettings: serial.ToTypedMessage(&freedom.Config{}),
			}
			if h, err := NewHandler(ctx, cfg); err == nil {
				if err := ohm.AddHandler(ctx, h); err == nil {
					// t.Log("add handler:", tag)
					tags.Store(tag, nil)
				} else {
					t.Error("failed to add handler:", tag)
				}
			} else {
				t.Error("failed to create handler:", tag)
			}
		}
	}

	rmHandlers := func() {
		defer wg_add_rm.Done()
		for !stop_add_rm {
			time.Sleep(delay)
			tags.Range(func(key interface{}, value interface{}) bool {
				if _, ok := tags.LoadAndDelete(key); ok {
					// t.Log("remove handler:", key)
					ohm.RemoveHandler(ctx, key.(string))
					return false
				}
				return true
			})
		}
	}

	selectors := []string{tags_prefix}
	wg_get := sync.WaitGroup{}
	stop_get := false
	getTags := func() {
		defer wg_get.Done()
		for !stop_get {
			time.Sleep(delay)
			_ = ohm.Select(selectors)
			// t.Logf("get tags: %v", tag)
		}
	}

	for i := 0; i < threads_num; i++ {
		wg_add_rm.Add(2)
		go rmHandlers()
		go addHandlers()
		wg_get.Add(1)
		go getTags()
	}

	time.Sleep(test_duration)
	stop_add_rm = true
	wg_add_rm.Wait()
	stop_get = true
	wg_get.Wait()
}
