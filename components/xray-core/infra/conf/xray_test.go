package conf_test

import (
	"encoding/json"
	"reflect"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/xtls/xray-core/app/dispatcher"
	"github.com/xtls/xray-core/app/log"
	"github.com/xtls/xray-core/app/proxyman"
	"github.com/xtls/xray-core/app/router"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/geodata"
	clog "github.com/xtls/xray-core/common/log"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	core "github.com/xtls/xray-core/core"
	. "github.com/xtls/xray-core/infra/conf"
	"github.com/xtls/xray-core/proxy/vmess"
	"github.com/xtls/xray-core/proxy/vmess/inbound"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/tls"
	"github.com/xtls/xray-core/transport/internet/websocket"
	"google.golang.org/protobuf/proto"
)

func TestXrayConfig(t *testing.T) {
	createParser := func() func(string) (proto.Message, error) {
		return func(s string) (proto.Message, error) {
			config := new(Config)
			if err := json.Unmarshal([]byte(s), config); err != nil {
				return nil, err
			}
			return config.Build()
		}
	}

	runMultiTestCase(t, []TestCase{
		{
			Input: `{
				"log": {
					"access": "/var/log/xray/access.log",
					"loglevel": "error",
					"error": "/var/log/xray/error.log"
				},
				"inbounds": [{
					"streamSettings": {
						"network": "ws",
						"wsSettings": {
							"host": "example.domain",
							"path": ""
						},
						"tlsSettings": {
							"alpn": "h2"
						},
						"security": "tls"
					},
					"protocol": "vmess",
					"port": "443-500",
					"settings": {
						"clients": [
							{
								"security": "aes-128-gcm",
								"id": "0cdf8a45-303d-4fed-9780-29aa7f54175e"
							}
						]
					}
				}],
				"routing": {
					"rules": [
						{
							"ip": [
								"10.0.0.0/8"
							],
							"outboundTag": "blocked"
						}
					]
				}
			}`,
			Parser: createParser(),
			Output: &core.Config{
				App: []*serial.TypedMessage{
					serial.ToTypedMessage(&log.Config{
						ErrorLogType:  log.LogType_File,
						ErrorLogPath:  "/var/log/xray/error.log",
						ErrorLogLevel: clog.Severity_Error,
						AccessLogType: log.LogType_File,
						AccessLogPath: "/var/log/xray/access.log",
					}),
					serial.ToTypedMessage(&dispatcher.Config{}),
					serial.ToTypedMessage(&proxyman.InboundConfig{}),
					serial.ToTypedMessage(&proxyman.OutboundConfig{}),
					serial.ToTypedMessage(&router.Config{
						DomainStrategy: router.Config_AsIs,
						Rule: []*router.RoutingRule{
							{
								Ip: []*geodata.IPRule{
									{
										Value: &geodata.IPRule_Custom{
											Custom: &geodata.CIDRRule{
												Cidr: &geodata.CIDR{Ip: []byte{10, 0, 0, 0}, Prefix: 8},
											},
										},
									},
								},
								TargetTag: &router.RoutingRule_Tag{
									Tag: "blocked",
								},
							},
						},
					}),
				},
				Inbound: []*core.InboundHandlerConfig{
					{
						ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
							PortList: &net.PortList{Range: []*net.PortRange{{
								From: 443,
								To:   500,
							}}},
							StreamSettings: &internet.StreamConfig{
								ProtocolName: "websocket",
								TransportSettings: []*internet.TransportConfig{
									{
										ProtocolName: "websocket",
										Settings: serial.ToTypedMessage(&websocket.Config{
											Host: "example.domain",
										}),
									},
								},
								SecurityType: "xray.transport.internet.tls.Config",
								SecuritySettings: []*serial.TypedMessage{
									serial.ToTypedMessage(&tls.Config{
										NextProtocol: []string{"h2"},
									}),
								},
							},
						}),
						ProxySettings: serial.ToTypedMessage(&inbound.Config{
							User: []*protocol.User{
								{
									Level: 0,
									Account: serial.ToTypedMessage(&vmess.Account{
										Id: "0cdf8a45-303d-4fed-9780-29aa7f54175e",
										SecuritySettings: &protocol.SecurityConfig{
											Type: protocol.SecurityType_AES128_GCM,
										},
									}),
								},
							},
						}),
					},
				},
			},
		},
	})
}

func TestSniffingConfig_Build(t *testing.T) {
	config := &SniffingConfig{
		Enabled:         true,
		DestOverride:    StringList{"http", "tls"},
		DomainsExcluded: StringList{"full:api.example.com", "domain:blocked.example", "regexp:^test[0-9]+\\.internal$"},
		IPsExcluded:     StringList{"192.168.1.1", "2001:db8::/32"},
		MetadataOnly:    true,
		RouteOnly:       true,
	}

	built, err := config.Build()
	if err != nil {
		t.Fatalf("SniffingConfig.Build() failed: %v", err)
	}

	if !built.Enabled || !built.MetadataOnly || !built.RouteOnly {
		t.Fatalf("SniffingConfig.Build() lost sniffing flags: %+v", built)
	}
	if len(built.DestinationOverride) != 2 {
		t.Fatalf("SniffingConfig.Build() lost destination overrides: %+v", built.DestinationOverride)
	}
	if len(built.DomainsExcluded) != 3 {
		t.Fatalf("SniffingConfig.Build() produced %d domain rules", len(built.DomainsExcluded))
	}
	if len(built.IpsExcluded) != 2 {
		t.Fatalf("SniffingConfig.Build() produced %d ip rules", len(built.IpsExcluded))
	}

	want := []struct {
		ruleType geodata.Domain_Type
		value    string
	}{
		{ruleType: geodata.Domain_Full, value: "api.example.com"},
		{ruleType: geodata.Domain_Domain, value: "blocked.example"},
		{ruleType: geodata.Domain_Regex, value: "^test[0-9]+\\.internal$"},
	}
	for i, tc := range want {
		rule := built.DomainsExcluded[i].GetCustom()
		if rule == nil {
			t.Fatalf("SniffingConfig.Build() produced a non-custom rule at index %d", i)
		}
		if rule.Type != tc.ruleType || rule.Value != tc.value {
			t.Fatalf("SniffingConfig.Build() produced wrong rule at index %d: got (%v, %q), want (%v, %q)", i, rule.Type, rule.Value, tc.ruleType, tc.value)
		}
	}

	wantIPs := []struct {
		ip     []byte
		prefix uint32
	}{
		{ip: []byte(net.ParseAddress("192.168.1.1").IP()), prefix: 32},
		{ip: []byte(net.ParseAddress("2001:db8::").IP()), prefix: 32},
	}
	for i, tc := range wantIPs {
		rule := built.IpsExcluded[i].GetCustom()
		if rule == nil {
			t.Fatalf("SniffingConfig.Build() produced a non-custom ip rule at index %d", i)
		}
		cidr := rule.GetCidr()
		if cidr == nil {
			t.Fatalf("SniffingConfig.Build() produced a custom ip rule without cidr at index %d", i)
		}
		if !reflect.DeepEqual(cidr.Ip, tc.ip) || cidr.Prefix != tc.prefix {
			t.Fatalf("SniffingConfig.Build() produced wrong ip rule at index %d: got (%v, %d), want (%v, %d)", i, cidr.Ip, cidr.Prefix, tc.ip, tc.prefix)
		}
	}
}

func TestMuxConfig_Build(t *testing.T) {
	tests := []struct {
		name   string
		fields string
		want   *proxyman.MultiplexingConfig
	}{
		{"default", `{"enabled": true, "concurrency": 16}`, &proxyman.MultiplexingConfig{
			Enabled:         true,
			Concurrency:     16,
			XudpConcurrency: 0,
			XudpProxyUDP443: "reject",
		}},
		{"empty def", `{}`, &proxyman.MultiplexingConfig{
			Enabled:         false,
			Concurrency:     0,
			XudpConcurrency: 0,
			XudpProxyUDP443: "reject",
		}},
		{"not enable", `{"enabled": false, "concurrency": 4}`, &proxyman.MultiplexingConfig{
			Enabled:         false,
			Concurrency:     4,
			XudpConcurrency: 0,
			XudpProxyUDP443: "reject",
		}},
		{"forbidden", `{"enabled": false, "concurrency": -1}`, &proxyman.MultiplexingConfig{
			Enabled:         false,
			Concurrency:     -1,
			XudpConcurrency: 0,
			XudpProxyUDP443: "reject",
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			m := &MuxConfig{}
			common.Must(json.Unmarshal([]byte(tt.fields), m))
			if got, _ := m.Build(); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("MuxConfig.Build() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestMuxWorkerPoolConfigBuild(t *testing.T) {
	raw := `{
		"enabled": true,
		"concurrency": 1,
		"workerPool": {
			"prewarmWorkers": 0,
			"reuseThreshold": 2,
			"maxProbingWorkers": 1,
			"probeIntervalMs": 5125,
			"probeTimeoutMs": 2000,
			"idleTtlMs": 24125,
			"maxRequestsPerWorker": 128
		}
	}`
	var config MuxConfig
	common.Must(json.Unmarshal([]byte(raw), &config))
	got, err := config.Build()
	if err != nil {
		t.Fatal(err)
	}
	want := &proxyman.WorkerPoolConfig{
		PrewarmWorkers:       0,
		ReuseThreshold:       2,
		MaxProbingWorkers:    1,
		ProbeIntervalMs:      5125,
		ProbeTimeoutMs:       2000,
		IdleTtlMs:            24125,
		MaxRequestsPerWorker: 128,
	}
	if !proto.Equal(want, got.WorkerPool) {
		t.Fatalf("worker pool mismatch: want %v, got %v", want, got.WorkerPool)
	}
}

func TestMuxWorkerPoolProtobufRoundTrip(t *testing.T) {
	config := &proxyman.WorkerPoolConfig{PrewarmWorkers: 3, ReuseThreshold: 7}
	wire, err := proto.Marshal(config)
	common.Must(err)
	var decoded proxyman.WorkerPoolConfig
	common.Must(proto.Unmarshal(wire, &decoded))
	if !proto.Equal(config, &decoded) {
		t.Fatal("new policy did not round-trip")
	}
}

func TestMuxWorkerPoolConfigRejectsUnknownFields(t *testing.T) {
	raw := `{"prewarmWorkers":0,"reuseThreshold":2,"maxProbingWorkers":1,"probeIntervalMs":5000,"probeTimeoutMs":2000,"idleTtlMs":24000,"maxRequestsPerWorker":128,"unexpected":true}`
	var config WorkerPoolConfig
	if err := json.Unmarshal([]byte(raw), &config); err == nil {
		t.Fatal("unknown worker-pool field was accepted")
	}
}

func TestMuxWorkerPoolConfigRejectsInvalidValues(t *testing.T) {
	valid := WorkerPoolConfig{
		PrewarmWorkers:       0,
		ReuseThreshold:       2,
		MaxProbingWorkers:    1,
		ProbeIntervalMs:      5000,
		ProbeTimeoutMs:       2000,
		IdleTtlMs:            24000,
		MaxRequestsPerWorker: 128,
	}
	tests := map[string]func(*WorkerPoolConfig){
		"max below limit":      func(c *WorkerPoolConfig) { c.ReuseThreshold = 0 },
		"min above max":        func(c *WorkerPoolConfig) { c.PrewarmWorkers = 3 },
		"probing zero":         func(c *WorkerPoolConfig) { c.MaxProbingWorkers = 0 },
		"probing above max":    func(c *WorkerPoolConfig) { c.MaxProbingWorkers = 3 },
		"interval below limit": func(c *WorkerPoolConfig) { c.ProbeIntervalMs = 1 },
		"interval above limit": func(c *WorkerPoolConfig) { c.ProbeIntervalMs = 60001 },
		"timeout below limit":  func(c *WorkerPoolConfig) { c.ProbeTimeoutMs = 199 },
		"timeout above limit":  func(c *WorkerPoolConfig) { c.ProbeTimeoutMs = 10001 },
		"timeout not shorter":  func(c *WorkerPoolConfig) { c.ProbeTimeoutMs = 5000 },
		"ttl below limit":      func(c *WorkerPoolConfig) { c.IdleTtlMs = 0 },
		"ttl cannot fit probe": func(c *WorkerPoolConfig) { c.IdleTtlMs = 6999 },
		"requests below limit": func(c *WorkerPoolConfig) { c.MaxRequestsPerWorker = 0 },
		"requests above limit": func(c *WorkerPoolConfig) { c.MaxRequestsPerWorker = 65536 },
	}
	for name, mutate := range tests {
		t.Run(name, func(t *testing.T) {
			config := valid
			mutate(&config)
			if _, err := config.Build(); err == nil {
				t.Fatal("invalid worker pool was accepted")
			}
		})
	}
}

func TestMuxWorkerPoolConfigAcceptsEveryLegalBoundary(t *testing.T) {
	tests := map[string]WorkerPoolConfig{
		"all lower bounds": {
			PrewarmWorkers:       0,
			ReuseThreshold:       1,
			MaxProbingWorkers:    1,
			ProbeIntervalMs:      2000,
			ProbeTimeoutMs:       200,
			IdleTtlMs:            2200,
			MaxRequestsPerWorker: 1,
		},
		"all upper bounds": {
			PrewarmWorkers:       ^uint32(0),
			ReuseThreshold:       ^uint32(0),
			MaxProbingWorkers:    ^uint32(0),
			ProbeIntervalMs:      60000,
			ProbeTimeoutMs:       10000,
			IdleTtlMs:            ^uint32(0),
			MaxRequestsPerWorker: 65535,
		},
		"equal idle bounds": {
			PrewarmWorkers:       8,
			ReuseThreshold:       8,
			MaxProbingWorkers:    8,
			ProbeIntervalMs:      5125,
			ProbeTimeoutMs:       2000,
			IdleTtlMs:            24125,
			MaxRequestsPerWorker: 128,
		},
		"24 hour idle TTL": {
			ReuseThreshold:       2,
			MaxProbingWorkers:    1,
			ProbeIntervalMs:      5000,
			ProbeTimeoutMs:       2000,
			IdleTtlMs:            86400125,
			MaxRequestsPerWorker: 128,
		},
	}
	for name, pool := range tests {
		t.Run(name, func(t *testing.T) {
			config := MuxConfig{Enabled: true, Concurrency: 1, WorkerPool: &pool}
			if _, err := config.Build(); err != nil {
				t.Fatalf("legal boundary was rejected: %v", err)
			}
		})
	}
}

func TestMuxWorkerPoolRequiresEnabledMux(t *testing.T) {
	pool := &WorkerPoolConfig{
		ReuseThreshold:       1,
		MaxProbingWorkers:    1,
		ProbeIntervalMs:      2000,
		ProbeTimeoutMs:       200,
		IdleTtlMs:            2200,
		MaxRequestsPerWorker: 1,
	}
	for _, config := range []MuxConfig{
		{Enabled: false, Concurrency: 1, WorkerPool: pool},
		{Enabled: true, Concurrency: 0, WorkerPool: pool},
		{Enabled: true, Concurrency: -1, WorkerPool: pool},
	} {
		if _, err := config.Build(); err == nil {
			t.Fatalf("invalid mux config was accepted: %+v", config)
		}
	}
}

func FuzzMuxWorkerPoolConfigBuild(f *testing.F) {
	f.Add(uint32(0), uint32(2), uint32(1), uint32(5125), uint32(2000), uint32(24125), uint32(128))
	f.Add(^uint32(0), ^uint32(0), ^uint32(0), ^uint32(0), ^uint32(0), ^uint32(0), ^uint32(0))
	f.Fuzz(func(t *testing.T, minIdle, maxIdle, maxProbing, interval, timeout, ttl, maxRequests uint32) {
		config := WorkerPoolConfig{
			PrewarmWorkers:       minIdle,
			ReuseThreshold:       maxIdle,
			MaxProbingWorkers:    maxProbing,
			ProbeIntervalMs:      interval,
			ProbeTimeoutMs:       timeout,
			IdleTtlMs:            ttl,
			MaxRequestsPerWorker: maxRequests,
		}
		built, err := config.Build()
		if err != nil {
			return
		}
		if built.PrewarmWorkers > built.ReuseThreshold || built.MaxProbingWorkers > built.ReuseThreshold {
			t.Fatalf("Build() accepted inconsistent values: %+v", built)
		}
		if built.ProbeTimeoutMs >= built.ProbeIntervalMs {
			t.Fatalf("Build() accepted timeout >= interval: %+v", built)
		}
	})
}

func TestConfig_Override(t *testing.T) {
	tests := []struct {
		name string
		orig *Config
		over *Config
		fn   string
		want *Config
	}{
		{
			"combine/empty",
			&Config{},
			&Config{
				LogConfig:    &LogConfig{},
				RouterConfig: &RouterConfig{},
				DNSConfig:    &DNSConfig{},
				Policy:       &PolicyConfig{},
				API:          &APIConfig{},
				Stats:        &StatsConfig{},
				Reverse:      &ReverseConfig{},
			},
			"",
			&Config{
				LogConfig:    &LogConfig{},
				RouterConfig: &RouterConfig{},
				DNSConfig:    &DNSConfig{},
				Policy:       &PolicyConfig{},
				API:          &APIConfig{},
				Stats:        &StatsConfig{},
				Reverse:      &ReverseConfig{},
			},
		},
		{
			"combine/newattr",
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "old"}}},
			&Config{LogConfig: &LogConfig{}}, "",
			&Config{LogConfig: &LogConfig{}, InboundConfigs: []InboundDetourConfig{{Tag: "old"}}},
		},
		{
			"replace/inbounds",
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}}},
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos1", Protocol: "kcp"}}},
			"",
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos0"}, {Tag: "pos1", Protocol: "kcp"}}},
		},
		{
			"replace/inbounds-replaceall",
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}}},
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos1", Protocol: "kcp"}, {Tag: "pos2", Protocol: "kcp"}}},
			"",
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos0"}, {Tag: "pos1", Protocol: "kcp"}, {Tag: "pos2", Protocol: "kcp"}}},
		},
		{
			"replace/notag-append",
			&Config{InboundConfigs: []InboundDetourConfig{{}, {Protocol: "vmess"}}},
			&Config{InboundConfigs: []InboundDetourConfig{{Tag: "pos1", Protocol: "kcp"}}},
			"",
			&Config{InboundConfigs: []InboundDetourConfig{{}, {Protocol: "vmess"}, {Tag: "pos1", Protocol: "kcp"}}},
		},
		{
			"replace/outbounds",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}}},
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos1", Protocol: "kcp"}}},
			"",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos0"}, {Tag: "pos1", Protocol: "kcp"}}},
		},
		{
			"replace/outbounds-prepend",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}, {Tag: "pos3"}}},
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos1", Protocol: "kcp"}, {Tag: "pos2", Protocol: "kcp"}, {Tag: "pos4", Protocol: "kcp"}}},
			"config.json",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos2", Protocol: "kcp"}, {Tag: "pos4", Protocol: "kcp"}, {Tag: "pos0"}, {Tag: "pos1", Protocol: "kcp"}, {Tag: "pos3"}}},
		},
		{
			"replace/outbounds-append",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}}},
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos2", Protocol: "kcp"}}},
			"config_tail.json",
			&Config{OutboundConfigs: []OutboundDetourConfig{{Tag: "pos0"}, {Protocol: "vmess", Tag: "pos1"}, {Tag: "pos2", Protocol: "kcp"}}},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.orig.Override(tt.over, tt.fn)
			if r := cmp.Diff(tt.orig, tt.want); r != "" {
				t.Error(r)
			}
		})
	}
}
