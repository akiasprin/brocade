package freedom

import (
	"bytes"
	"context"
	"io"
	stdnet "net"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/dispatcher"
	appstats "github.com/xtls/xray-core/app/stats"
	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
	"github.com/xtls/xray-core/transport/pipe"
)

func TestLegacyDisableStillAllowsOnlyVisionSplice(t *testing.T) {
	tests := []struct {
		name    string
		inbound *session.Inbound
		want    bool
	}{
		{name: "missing inbound", inbound: nil, want: false},
		{name: "raw dokodemo", inbound: spliceInbound("dokodemo-door", session.SpliceCopyDirect), want: false},
		{name: "raw VLESS", inbound: spliceInbound("vless", session.SpliceCopyDisabled), want: false},
		{name: "Vision waiting", inbound: spliceInbound("vless", session.SpliceCopyWaiting), want: true},
		{name: "Vision direct", inbound: spliceInbound("vless", session.SpliceCopyDirect), want: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := responseSpliceAllowed(false, test.inbound); got != test.want {
				t.Fatalf("responseSpliceAllowed() = %v, want %v", got, test.want)
			}
		})
	}
}

func TestEnabledSpliceKeepsOrdinaryRawConnections(t *testing.T) {
	if !responseSpliceAllowed(true, spliceInbound("dokodemo-door", session.SpliceCopyDirect)) {
		t.Fatal("enabled splice rejected an ordinary raw connection")
	}
}

func spliceInbound(name string, state session.SpliceCopyState) *session.Inbound {
	inbound := &session.Inbound{Name: name}
	inbound.CanSpliceCopy.Store(state)
	return inbound
}

type framedFastPathDNS struct {
	ip      xnet.IP
	domain  string
	option  dns.IPOption
	lookups atomic.Int32
}

func (*framedFastPathDNS) Start() error { return nil }

func (*framedFastPathDNS) Close() error { return nil }

func (*framedFastPathDNS) Type() interface{} { return dns.ClientType() }

func (d *framedFastPathDNS) LookupIP(domain string, option dns.IPOption) ([]xnet.IP, uint32, error) {
	d.domain = domain
	d.option = option
	d.lookups.Add(1)
	return []xnet.IP{d.ip}, 60, nil
}

type framedFastPathDialer struct {
	conn        stat.Connection
	destination xnet.Destination
	calls       atomic.Int32
}

func (d *framedFastPathDialer) Dial(_ context.Context, destination xnet.Destination) (stat.Connection, error) {
	d.destination = destination
	d.calls.Add(1)
	return d.conn, nil
}

func (*framedFastPathDialer) DestIpAddress() xnet.IP { return nil }

func (*framedFastPathDialer) SetOutboundGateway(context.Context, *session.Outbound) {}

type recordingFramedSplicer struct {
	calls   atomic.Int32
	payload []byte
	mu      sync.Mutex
}

func (s *recordingFramedSplicer) SpliceDownlink(_ context.Context, source stdnet.Conn, onBytes func(int64)) (bool, error) {
	s.calls.Add(1)
	payload, err := io.ReadAll(source)
	if len(payload) > 0 && onBytes != nil {
		onBytes(int64(len(payload)))
	}
	s.mu.Lock()
	s.payload = bytes.Clone(payload)
	s.mu.Unlock()
	return true, err
}

func (s *recordingFramedSplicer) bytes() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return bytes.Clone(s.payload)
}

type recordingMultiBufferWriter struct {
	writes atomic.Int32
	mu     sync.Mutex
	data   []byte
}

func (w *recordingMultiBufferWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	w.writes.Add(1)
	data := make([]byte, mb.Len())
	mb.Copy(data)
	buf.ReleaseMulti(mb)
	w.mu.Lock()
	w.data = append(w.data, data...)
	w.mu.Unlock()
	return nil
}

func (*recordingMultiBufferWriter) Close() error { return nil }

func (*recordingMultiBufferWriter) Interrupt() {}

func (w *recordingMultiBufferWriter) bytes() []byte {
	w.mu.Lock()
	defer w.mu.Unlock()
	return bytes.Clone(w.data)
}

type opaqueTestConn struct {
	stdnet.Conn
}

type pacedFramedSplicer struct {
	interval time.Duration
	steps    int
	calls    atomic.Int32
}

func (s *pacedFramedSplicer) SpliceDownlink(ctx context.Context, _ stdnet.Conn, onBytes func(int64)) (bool, error) {
	for range s.steps {
		timer := time.NewTimer(s.interval)
		select {
		case <-ctx.Done():
			timer.Stop()
			return true, ctx.Err()
		case <-timer.C:
		}
		s.calls.Add(1)
		if onBytes != nil {
			onBytes(1)
		}
	}
	return true, nil
}

type framedFastPathPolicy struct {
	session policy.Session
}

func (*framedFastPathPolicy) Start() error { return nil }

func (*framedFastPathPolicy) Close() error { return nil }

func (*framedFastPathPolicy) Type() interface{} { return policy.ManagerType() }

func (p *framedFastPathPolicy) ForLevel(uint32) policy.Session { return p.session }

func (*framedFastPathPolicy) ForSystem() policy.System { return policy.System{} }

func TestFramedDownlinkFastPathRunsAfterFreedomDNSAndPreservesCounters(t *testing.T) {
	previousUseSplice := useSplice
	useSplice = true
	t.Cleanup(func() {
		useSplice = previousUseSplice
		internet.InitSystemDialer(nil, nil)
	})

	tests := []struct {
		name           string
		strategy       internet.DomainStrategy
		resolvedIP     xnet.IP
		wantIPv4Lookup bool
		wantIPv6Lookup bool
	}{
		{
			name:           "IPv4",
			strategy:       internet.DomainStrategy_USE_IP4,
			resolvedIP:     xnet.IP{203, 0, 113, 10},
			wantIPv4Lookup: true,
		},
		{
			name:           "IPv6",
			strategy:       internet.DomainStrategy_USE_IP6,
			resolvedIP:     xnet.IP{0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1},
			wantIPv6Lookup: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			originConn, originPeer := freedomTCPConnPair(t)
			defer originConn.Close()
			defer originPeer.Close()

			originReadCounter := new(appstats.Counter)
			dialer := &framedFastPathDialer{conn: &stat.CounterConnection{
				Connection:  originConn,
				ReadCounter: originReadCounter,
			}}
			dnsClient := &framedFastPathDNS{ip: test.resolvedIP}
			internet.InitSystemDialer(dnsClient, nil)

			inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
			if err := inputWriter.Close(); err != nil {
				t.Fatal(err)
			}
			transportWriter := new(recordingMultiBufferWriter)
			userCounter := new(appstats.Counter)
			link := &transport.Link{
				Reader: inputReader,
				Writer: &dispatcher.SizeStatWriter{
					Counter: userCounter,
					Writer:  transportWriter,
				},
			}
			splicer := new(recordingFramedSplicer)
			inbound := &session.Inbound{Name: "anytls", FramedDownlinkSplicer: splicer}
			inbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
			target := xnet.TCPDestination(xnet.DomainAddress("dns-strategy.example"), 443)
			ctx := session.ContextWithInbound(context.Background(), inbound)
			ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{
				OriginalTarget: target,
				Target:         target,
			}})

			payload := bytes.Repeat([]byte("framed-fast-path-response"), 2048)
			writeResult := make(chan error, 1)
			go func() {
				_, err := originPeer.Write(payload)
				if closeErr := originPeer.CloseWrite(); err == nil {
					err = closeErr
				}
				writeResult <- err
			}()

			handler := &Handler{
				policyManager: policy.DefaultManager{},
				config: &Config{
					DomainStrategy: test.strategy,
					IpsBlocked:     &IPRules{},
				},
			}
			if err := handler.Process(ctx, link, dialer); err != nil {
				t.Fatal(err)
			}
			if err := <-writeResult; err != nil {
				t.Fatal(err)
			}

			if got := dnsClient.lookups.Load(); got != 1 {
				t.Fatalf("DNS lookups = %d, want 1", got)
			}
			if dnsClient.domain != "dns-strategy.example" {
				t.Fatalf("resolved domain = %q", dnsClient.domain)
			}
			if dnsClient.option.IPv4Enable != test.wantIPv4Lookup || dnsClient.option.IPv6Enable != test.wantIPv6Lookup {
				t.Fatalf("DNS option = %+v, want IPv4=%v IPv6=%v", dnsClient.option, test.wantIPv4Lookup, test.wantIPv6Lookup)
			}
			if got := dialer.calls.Load(); got != 1 {
				t.Fatalf("dial calls = %d, want 1", got)
			}
			if dialer.destination.Address == nil || !dialer.destination.Address.Family().IsIP() ||
				!bytes.Equal(dialer.destination.Address.IP(), test.resolvedIP) {
				t.Fatalf("dial destination = %v, want %v", dialer.destination, test.resolvedIP)
			}
			if got := splicer.calls.Load(); got != 1 {
				t.Fatalf("framed splicer calls = %d, want 1", got)
			}
			if !bytes.Equal(splicer.bytes(), payload) {
				t.Fatal("framed splicer received different payload")
			}
			if got := originReadCounter.Value(); got != int64(len(payload)) {
				t.Fatalf("outbound read counter = %d, want %d", got, len(payload))
			}
			if got := userCounter.Value(); got != int64(len(payload)) {
				t.Fatalf("user downlink counter = %d, want %d", got, len(payload))
			}
			if got := transportWriter.writes.Load(); got != 0 {
				t.Fatalf("ordinary transport writer received %d writes", got)
			}
		})
	}
}

func TestFramedDownlinkFastPathFallsBackForOpaqueOutbound(t *testing.T) {
	previousUseSplice := useSplice
	useSplice = true
	t.Cleanup(func() { useSplice = previousUseSplice })

	originConn, originPeer := freedomTCPConnPair(t)
	defer originConn.Close()
	defer originPeer.Close()
	dialer := &framedFastPathDialer{conn: &opaqueTestConn{Conn: originConn}}

	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	if err := inputWriter.Close(); err != nil {
		t.Fatal(err)
	}
	output := new(recordingMultiBufferWriter)
	link := &transport.Link{Reader: inputReader, Writer: output}
	splicer := new(recordingFramedSplicer)
	inbound := &session.Inbound{Name: "anytls", FramedDownlinkSplicer: splicer}
	inbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
	target := xnet.TCPDestination(xnet.IPAddress([]byte{203, 0, 113, 20}), 443)
	ctx := session.ContextWithInbound(context.Background(), inbound)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{OriginalTarget: target, Target: target}})

	payload := bytes.Repeat([]byte("opaque-outbound-response"), 1024)
	writeResult := make(chan error, 1)
	go func() {
		_, err := originPeer.Write(payload)
		if closeErr := originPeer.CloseWrite(); err == nil {
			err = closeErr
		}
		writeResult <- err
	}()

	handler := &Handler{
		policyManager: policy.DefaultManager{},
		config:        &Config{IpsBlocked: &IPRules{}},
	}
	if err := handler.Process(ctx, link, dialer); err != nil {
		t.Fatal(err)
	}
	if err := <-writeResult; err != nil {
		t.Fatal(err)
	}
	if got := splicer.calls.Load(); got != 0 {
		t.Fatalf("framed splicer called %d times for opaque outbound", got)
	}
	if !bytes.Equal(output.bytes(), payload) {
		t.Fatal("ordinary transport fallback changed response payload")
	}
}

func TestFramedDownlinkFastPathRefreshesInactivityTimer(t *testing.T) {
	previousUseSplice := useSplice
	useSplice = true
	t.Cleanup(func() { useSplice = previousUseSplice })

	originConn, originPeer := freedomTCPConnPair(t)
	defer originConn.Close()
	defer originPeer.Close()
	dialer := &framedFastPathDialer{conn: originConn}

	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	if err := inputWriter.Close(); err != nil {
		t.Fatal(err)
	}
	link := &transport.Link{Reader: inputReader, Writer: new(recordingMultiBufferWriter)}
	splicer := &pacedFramedSplicer{interval: 50 * time.Millisecond, steps: 5}
	inbound := &session.Inbound{Name: "anytls", FramedDownlinkSplicer: splicer}
	inbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
	target := xnet.TCPDestination(xnet.IPAddress([]byte{203, 0, 113, 30}), 443)
	ctx := session.ContextWithInbound(context.Background(), inbound)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{OriginalTarget: target, Target: target}})

	timeout := 120 * time.Millisecond
	policyManager := &framedFastPathPolicy{session: policy.Session{
		Timeouts: policy.Timeout{
			ConnectionIdle: timeout,
			UplinkOnly:     timeout,
			DownlinkOnly:   timeout,
		},
	}}
	handler := &Handler{
		policyManager: policyManager,
		config:        &Config{IpsBlocked: &IPRules{}},
	}
	if err := handler.Process(ctx, link, dialer); err != nil {
		t.Fatalf("fast path expired while callbacks were reporting activity: %v", err)
	}
	if got := splicer.calls.Load(); got != int32(splicer.steps) {
		t.Fatalf("activity callbacks = %d, want %d", got, splicer.steps)
	}
}

func freedomTCPConnPair(t *testing.T) (*stdnet.TCPConn, *stdnet.TCPConn) {
	t.Helper()
	listener, err := stdnet.ListenTCP("tcp4", &stdnet.TCPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	accepted := make(chan *stdnet.TCPConn, 1)
	acceptErr := make(chan error, 1)
	go func() {
		conn, err := listener.AcceptTCP()
		if err != nil {
			acceptErr <- err
			return
		}
		accepted <- conn
	}()
	client, err := stdnet.DialTCP("tcp4", nil, listener.Addr().(*stdnet.TCPAddr))
	if err != nil {
		t.Fatal(err)
	}
	select {
	case server := <-accepted:
		return server, client
	case err := <-acceptErr:
		_ = client.Close()
		t.Fatal(err)
	case <-time.After(3 * time.Second):
		_ = client.Close()
		t.Fatal("timed out accepting test TCP connection")
	}
	return nil, nil
}
