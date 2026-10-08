package scenarios

import (
	"context"
	"fmt"
	"io"
	stdnet "net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/metrics"
	"github.com/xtls/xray-core/app/proxyman"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/common/serial"
	core "github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/proxy/anytls"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"github.com/xtls/xray-core/proxy/freedom"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"github.com/xtls/xray-core/transport/internet"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sync/errgroup"
)

const (
	localPerfEnabledEnv     = "XRAY_LOCAL_PERF"
	localPerfConcurrencyEnv = "XRAY_LOCAL_PERF_CONCURRENCY"
	localPerfBodyBytesEnv   = "XRAY_LOCAL_PERF_BODY_BYTES"
	localPerfProfileDirEnv  = "XRAY_LOCAL_PERF_PROFILE_DIR"
	localPerfProfileSecsEnv = "XRAY_LOCAL_PERF_PROFILE_SECONDS"
	localPerfServerCPUsEnv  = "XRAY_LOCAL_PERF_SERVER_CPUS"
	localPerfClientCPUsEnv  = "XRAY_LOCAL_PERF_CLIENT_CPUS"
	localPerfLoadCPUsEnv    = "XRAY_LOCAL_PERF_LOAD_CPUS"
	localPerfVLESSFlowEnv   = "XRAY_LOCAL_PERF_VLESS_FLOW"
	localPerfDirectionEnv   = "XRAY_LOCAL_PERF_DIRECTION"
)

type localPerfOptions struct {
	concurrency    int
	bodyBytes      int
	profileDir     string
	profileSeconds int
	serverCPUs     string
	clientCPUs     string
	loadCPUs       string
	direction      string
}

func localPerfOptionsFromEnv(t *testing.T, defaultConcurrency, defaultBodyBytes int) localPerfOptions {
	t.Helper()
	options := localPerfOptions{
		concurrency:    positiveEnvInt(t, localPerfConcurrencyEnv, defaultConcurrency),
		bodyBytes:      positiveEnvInt(t, localPerfBodyBytesEnv, defaultBodyBytes),
		profileDir:     os.Getenv(localPerfProfileDirEnv),
		profileSeconds: positiveEnvInt(t, localPerfProfileSecsEnv, 5),
		serverCPUs:     os.Getenv(localPerfServerCPUsEnv),
		clientCPUs:     os.Getenv(localPerfClientCPUsEnv),
		loadCPUs:       os.Getenv(localPerfLoadCPUsEnv),
		direction:      os.Getenv(localPerfDirectionEnv),
	}
	if options.direction == "" {
		options.direction = "download"
	}
	if options.direction != "download" && options.direction != "upload" {
		t.Fatalf("%s must be download or upload, got %q", localPerfDirectionEnv, options.direction)
	}
	return options
}

func positiveEnvInt(t *testing.T, name string, fallback int) int {
	t.Helper()
	value := os.Getenv(name)
	if value == "" {
		return fallback
	}
	parsed, err := strconv.Atoi(value)
	if err != nil || parsed <= 0 {
		t.Fatalf("%s must be a positive integer, got %q", name, value)
	}
	return parsed
}

func addLocalCPUProfileEndpoint(config *core.Config, port xnet.Port) {
	config.App = append(config.App, serial.ToTypedMessage(&metrics.Config{
		Listen: fmt.Sprintf("127.0.0.1:%d", port),
	}))
}

func applyLocalPerfCPUAffinity(t *testing.T, options localPerfOptions, servers []*exec.Cmd) {
	t.Helper()
	if options.serverCPUs == "" && options.clientCPUs == "" && options.loadCPUs == "" {
		return
	}
	if runtime.GOOS != "linux" {
		t.Fatalf("local performance CPU affinity requires Linux, got %s", runtime.GOOS)
	}
	if len(servers) != 2 {
		t.Fatalf("local performance CPU affinity requires two Xray processes, got %d", len(servers))
	}

	pinLocalPerfProcess(t, "server Xray", servers[0].Process.Pid, options.serverCPUs)
	pinLocalPerfProcess(t, "client Xray", servers[1].Process.Pid, options.clientCPUs)
	pinLocalPerfProcess(t, "load generator", os.Getpid(), options.loadCPUs)
}

func pinLocalPerfProcess(t *testing.T, name string, pid int, cpus string) {
	t.Helper()
	if cpus == "" {
		return
	}
	output, err := exec.Command("taskset", "-apc", cpus, strconv.Itoa(pid)).CombinedOutput()
	if err != nil {
		t.Fatalf("pin %s (pid %d) to CPUs %q: %v: %s", name, pid, cpus, err, output)
	}
	t.Logf("pinned %s pid=%d cpus=%s", name, pid, cpus)
}

type localProcessUsage struct {
	cpuTicks            uint64
	readSyscalls        uint64
	writeSyscalls       uint64
	voluntarySwitches   uint64
	involuntarySwitches uint64
	rssBytes            uint64
	peakRSSBytes        uint64
	threads             uint64
}

func readLocalProcessUsage(pid int) (localProcessUsage, error) {
	var usage localProcessUsage
	stat, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return usage, err
	}
	closingParen := strings.LastIndexByte(string(stat), ')')
	if closingParen < 0 {
		return usage, fmt.Errorf("malformed process stat")
	}
	fields := strings.Fields(string(stat[closingParen+1:]))
	if len(fields) <= 12 {
		return usage, fmt.Errorf("process stat has %d fields after comm", len(fields))
	}
	userTicks, err := strconv.ParseUint(fields[11], 10, 64)
	if err != nil {
		return usage, fmt.Errorf("parse user CPU ticks: %w", err)
	}
	systemTicks, err := strconv.ParseUint(fields[12], 10, 64)
	if err != nil {
		return usage, fmt.Errorf("parse system CPU ticks: %w", err)
	}
	usage.cpuTicks = userTicks + systemTicks

	if err := readLocalProcessCounters(fmt.Sprintf("/proc/%d/io", pid), map[string]*uint64{
		"syscr": &usage.readSyscalls,
		"syscw": &usage.writeSyscalls,
	}); err != nil {
		return usage, err
	}
	if err := readLocalProcessCounters(fmt.Sprintf("/proc/%d/status", pid), map[string]*uint64{
		"voluntary_ctxt_switches":    &usage.voluntarySwitches,
		"nonvoluntary_ctxt_switches": &usage.involuntarySwitches,
	}); err != nil {
		return usage, err
	}
	if err := readLocalProcessStatusMemory(fmt.Sprintf("/proc/%d/status", pid), &usage); err != nil {
		return usage, err
	}
	return usage, nil
}

func readLocalProcessStatusMemory(path string, usage *localProcessUsage) error {
	content, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	for _, line := range strings.Split(string(content), "\n") {
		name, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		if name != "VmRSS" && name != "VmHWM" && name != "Threads" {
			continue
		}
		fields := strings.Fields(value)
		if len(fields) == 0 {
			continue
		}
		parsed, err := strconv.ParseUint(fields[0], 10, 64)
		if err != nil {
			return fmt.Errorf("parse %s from %s: %w", name, path, err)
		}
		switch name {
		case "VmRSS":
			usage.rssBytes = parsed * 1024
		case "VmHWM":
			usage.peakRSSBytes = parsed * 1024
		case "Threads":
			usage.threads = parsed
		}
	}
	return nil
}

func readLocalProcessCounters(path string, counters map[string]*uint64) error {
	content, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	for _, line := range strings.Split(string(content), "\n") {
		name, value, ok := strings.Cut(line, ":")
		target := counters[name]
		if !ok || target == nil {
			continue
		}
		parsed, err := strconv.ParseUint(strings.TrimSpace(value), 10, 64)
		if err != nil {
			return fmt.Errorf("parse %s from %s: %w", name, path, err)
		}
		*target = parsed
	}
	return nil
}

func startLocalProcessUsage(t *testing.T, enabled bool, servers []*exec.Cmd) func() {
	t.Helper()
	if !enabled || runtime.GOOS != "linux" {
		return func() {}
	}
	if len(servers) != 2 {
		t.Fatalf("local process usage requires two Xray processes, got %d", len(servers))
	}

	names := []string{"server", "client"}
	before := make([]localProcessUsage, len(servers))
	for index, server := range servers {
		usage, err := readLocalProcessUsage(server.Process.Pid)
		if err != nil {
			t.Fatalf("read initial %s Xray usage: %v", names[index], err)
		}
		before[index] = usage
	}
	return func() {
		for index, server := range servers {
			after, err := readLocalProcessUsage(server.Process.Pid)
			if err != nil {
				t.Errorf("read final %s Xray usage: %v", names[index], err)
				continue
			}
			initial := before[index]
			t.Logf(
				"process=%s cpu_ticks=%d read_syscalls=%d write_syscalls=%d voluntary_switches=%d involuntary_switches=%d rss_bytes=%d peak_rss_bytes=%d threads=%d",
				names[index], after.cpuTicks-initial.cpuTicks,
				after.readSyscalls-initial.readSyscalls, after.writeSyscalls-initial.writeSyscalls,
				after.voluntarySwitches-initial.voluntarySwitches,
				after.involuntarySwitches-initial.involuntarySwitches,
				after.rssBytes, after.peakRSSBytes, after.threads,
			)
		}
	}
}

func startLocalCPUProfiles(t *testing.T, ctx context.Context, directory, prefix string, seconds int, ports map[string]xnet.Port) func() {
	t.Helper()
	if directory == "" || len(ports) == 0 {
		return func() {}
	}
	if err := os.MkdirAll(directory, 0o755); err != nil {
		t.Fatal(err)
	}

	type profileResult struct {
		name string
		err  error
	}
	results := make(chan profileResult, len(ports))
	for name, port := range ports {
		name, port := name, port
		go func() {
			request, err := http.NewRequestWithContext(
				ctx,
				http.MethodGet,
				fmt.Sprintf("http://127.0.0.1:%d/debug/pprof/profile?seconds=%d", port, seconds),
				nil,
			)
			if err != nil {
				results <- profileResult{name: name, err: err}
				return
			}
			response, err := http.DefaultClient.Do(request)
			if err != nil {
				results <- profileResult{name: name, err: err}
				return
			}
			defer response.Body.Close()
			if response.StatusCode != http.StatusOK {
				results <- profileResult{name: name, err: fmt.Errorf("profile HTTP status %s", response.Status)}
				return
			}
			path := filepath.Join(directory, prefix+"-"+name+".cpu.pprof")
			file, err := os.Create(path)
			if err == nil {
				_, err = io.Copy(file, response.Body)
				if closeErr := file.Close(); err == nil {
					err = closeErr
				}
			}
			results <- profileResult{name: name, err: err}
		}()
	}

	return func() {
		for range ports {
			result := <-results
			if result.err != nil {
				t.Errorf("capture %s CPU profile: %v", result.name, result.err)
			}
		}
	}
}

func captureLocalHeapProfiles(t *testing.T, ctx context.Context, directory, prefix string, ports map[string]xnet.Port) {
	t.Helper()
	if directory == "" {
		return
	}
	for name, port := range ports {
		request, err := http.NewRequestWithContext(
			ctx, http.MethodGet, fmt.Sprintf("http://127.0.0.1:%d/debug/pprof/heap", port), nil,
		)
		if err != nil {
			t.Errorf("build %s heap profile request: %v", name, err)
			continue
		}
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Errorf("capture %s heap profile: %v", name, err)
			continue
		}
		if response.StatusCode != http.StatusOK {
			_ = response.Body.Close()
			t.Errorf("capture %s heap profile: HTTP status %s", name, response.Status)
			continue
		}
		path := filepath.Join(directory, prefix+"-"+name+".heap.pprof")
		file, createErr := os.Create(path)
		if createErr == nil {
			_, createErr = io.Copy(file, response.Body)
			if closeErr := file.Close(); createErr == nil {
				createErr = closeErr
			}
		}
		if closeErr := response.Body.Close(); createErr == nil {
			createErr = closeErr
		}
		if createErr != nil {
			t.Errorf("write %s heap profile: %v", name, createErr)
		}
	}
}

// TestLocalAnyTLSHighConcurrency is an explicit local capacity/profile gate.
// It stays out of regular CI because its result depends on host CPU topology.
func TestLocalAnyTLSHighConcurrency(t *testing.T) {
	if os.Getenv(localPerfEnabledEnv) != "1" {
		t.Skip("set XRAY_LOCAL_PERF=1 to run the local capacity profile")
	}
	options := localPerfOptionsFromEnv(t, 64, 128<<20)
	origin, expectedBodyHash := startFragmentedHTTPSOrigin(t, options.bodyBytes)
	defer origin.Close()
	originPort := uint32(origin.Listener.Addr().(*stdnet.TCPAddr).Port)

	certificate, certificateHash := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
	password := "local-anytls-performance-password"
	serverPort := tcp.PickPort()
	serverConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
				PortList: &xnet.PortList{Range: []*xnet.PortRange{xnet.SinglePortRange(serverPort)}},
				Listen:   xnet.NewIPOrDomain(xnet.LocalHostIP),
				StreamSettings: &internet.StreamConfig{
					ProtocolName: "tcp",
					SecurityType: serial.GetMessageType(&xraytls.Config{}),
					SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&xraytls.Config{
						Certificate: []*xraytls.Certificate{xraytls.ParseCertificate(certificate)},
					})},
				},
			}),
			ProxySettings: serial.ToTypedMessage(&anytls.ServerConfig{Users: []*protocol.User{{
				Email: "local-anytls-performance@example.com",
				Account: serial.ToTypedMessage(&anytls.Account{
					Password: password,
				}),
			}}}),
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			ProxySettings: serial.ToTypedMessage(&freedom.Config{IpsBlocked: &freedom.IPRules{}}),
		}},
	}

	clientPort := tcp.PickPort()
	clientConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
				PortList: &xnet.PortList{Range: []*xnet.PortRange{xnet.SinglePortRange(clientPort)}},
				Listen:   xnet.NewIPOrDomain(xnet.LocalHostIP),
			}),
			ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
				Address: xnet.NewIPOrDomain(xnet.LocalHostIP), Port: originPort, Networks: []xnet.Network{xnet.Network_TCP},
			}),
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			SenderSettings: serial.ToTypedMessage(&proxyman.SenderConfig{StreamSettings: &internet.StreamConfig{
				ProtocolName: "tcp",
				SecurityType: serial.GetMessageType(&xraytls.Config{}),
				SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&xraytls.Config{
					ServerName: "localhost", PinnedPeerCertSha256: [][]byte{certificateHash[:]},
				})},
			}}),
			ProxySettings: serial.ToTypedMessage(&anytls.ClientConfig{Server: &protocol.ServerEndpoint{
				Address: xnet.NewIPOrDomain(xnet.LocalHostIP), Port: uint32(serverPort),
				User: &protocol.User{
					Email:   "local-anytls-performance@example.com",
					Account: serial.ToTypedMessage(&anytls.Account{Password: password}),
				},
			}}),
		}},
	}

	profilePorts := make(map[string]xnet.Port)
	if options.profileDir != "" {
		profilePorts["server"] = tcp.PickPort()
		profilePorts["client"] = tcp.PickPort()
		addLocalCPUProfileEndpoint(serverConfig, profilePorts["server"])
		addLocalCPUProfileEndpoint(clientConfig, profilePorts["client"])
	}

	servers, err := InitializeServerConfigs(serverConfig, clientConfig)
	if err != nil {
		t.Fatal(err)
	}
	defer CloseAllServers(servers)
	applyLocalPerfCPUAffinity(t, options, servers)

	timeout := time.Duration(max(30, options.profileSeconds+15)) * time.Second
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	waitProfiles := startLocalCPUProfiles(t, ctx, options.profileDir, "anytls", options.profileSeconds, profilePorts)
	logProcessUsage := startLocalProcessUsage(t, true, servers)

	started := time.Now()
	var group errgroup.Group
	for index := range options.concurrency {
		index := index
		group.Go(func() error {
			if options.direction == "upload" {
				return uploadFragmentedHTTPS(ctx, clientPort, index+1, options.bodyBytes)
			}
			return fetchFragmentedHTTPS(ctx, clientPort, index+1, expectedBodyHash, options.bodyBytes)
		})
	}
	if err := group.Wait(); err != nil {
		t.Fatal(err)
	}
	elapsed := time.Since(started)
	logProcessUsage()
	waitProfiles()
	captureLocalHeapProfiles(t, ctx, options.profileDir, "anytls", profilePorts)
	transferred := int64(options.concurrency) * int64(options.bodyBytes)
	t.Logf(
		"protocol=anytls direction=%s connections=%d transferred=%d elapsed=%s throughput=%.2f GiB/s",
		options.direction, options.concurrency, transferred, elapsed, float64(transferred)/elapsed.Seconds()/(1<<30),
	)
}
