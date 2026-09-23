package dispatcher

import (
	"context"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	commonctx "github.com/xtls/xray-core/common/ctx"
	"github.com/xtls/xray-core/common/geodata"
	"github.com/xtls/xray-core/common/log"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/core"
)

type diagnosticResult struct{ protocol, domain string }

func (r diagnosticResult) Protocol() string { return r.protocol }
func (r diagnosticResult) Domain() string   { return r.domain }

type diagnosticDomainMatcher struct{ geodata.DomainMatcher }

func (diagnosticDomainMatcher) MatchAny(s string) bool { return s == "example.com" }

type diagnosticIPMatcher struct{ geodata.IPMatcher }

func (diagnosticIPMatcher) Match(net.IP) bool { return true }

func TestSniffDecisionDistinguishesParsingAndApplication(t *testing.T) {
	ip := net.TCPDestination(net.ParseAddress("192.0.2.1"), 443)
	domain := net.TCPDestination(net.ParseAddress("example.com"), 443)
	request := session.SniffingRequest{Enabled: true, OverrideDestinationForProtocol: []string{"tls"}}
	dispatcher := &DefaultDispatcher{}
	for _, test := range []struct {
		name                            string
		original                        net.Destination
		result                          SniffResult
		request                         session.SniffingRequest
		applied, wantReason, wantResult string
		changed                         bool
	}{
		{"IP recovered", ip, diagnosticResult{"tls", "example.com"}, request, "destination", "accepted", "success", true},
		{"domain already supplied", domain, diagnosticResult{"tls", "example.com"}, request, "destination", "accepted", "success", false},
		{"route only", ip, diagnosticResult{"tls", "example.com"}, session.SniffingRequest{Enabled: true, RouteOnly: true, OverrideDestinationForProtocol: []string{"tls"}}, "route", "accepted", "success", true},
		{"excluded domain", ip, diagnosticResult{"tls", "example.com"}, session.SniffingRequest{Enabled: true, ExcludeForDomain: diagnosticDomainMatcher{}}, "none", "excluded_domain", "success", false},
		{"excluded IP", ip, diagnosticResult{"tls", "example.com"}, session.SniffingRequest{Enabled: true, ExcludeForIP: diagnosticIPMatcher{}}, "none", "excluded_ip", "success", false},
		{"unselected protocol", ip, diagnosticResult{"http", "example.com"}, request, "none", "protocol_not_selected", "success", false},
		{"protocol without domain", ip, diagnosticResult{"bittorrent", ""}, request, "none", "no_domain", "protocol_only", false},
		{"metadata domain source", ip, CompositeResult(diagnosticResult{"fakedns", "example.com"}, diagnosticResult{"tls", "other.example"}), session.SniffingRequest{Enabled: true, OverrideDestinationForProtocol: []string{"fakedns"}}, "destination", "accepted", "success", true},
	} {
		t.Run(test.name, func(t *testing.T) {
			reason := dispatcher.sniffOverrideReason(context.Background(), test.result, test.request, test.original)
			d := makeSniffDecision(test.request, test.original, test.result, nil, reason, test.applied, time.Millisecond)
			if d.reason != test.wantReason || d.result != test.wantResult || d.changed != test.changed {
				t.Fatalf("unexpected diagnostic: %s", d)
			}
			if test.name == "metadata domain source" && d.source != "fakedns" {
				t.Fatalf("wrong domain source: %s", d)
			}
		})
	}
}

type diagnosticReader struct {
	data  []byte
	err   error
	calls int
}

func (r *diagnosticReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	return r.ReadMultiBufferTimeout(time.Second)
}
func (r *diagnosticReader) ReadMultiBufferTimeout(time.Duration) (buf.MultiBuffer, error) {
	r.calls++
	if len(r.data) > 0 {
		data := r.data
		r.data = nil
		return buf.MergeBytes(nil, data), nil
	}
	return nil, r.err
}

func TestSniffDiagnosticsPreserveHTTPPayload(t *testing.T) {
	wire := "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n"
	reader := &cachedReader{reader: &diagnosticReader{data: []byte(wire), err: io.EOF}}
	ctx := context.WithValue(context.Background(), core.XrayKey(1), &core.Instance{})
	result, err := sniffer(ctx, reader, false, net.Network_TCP)
	if err != nil || result.Domain() != "example.com" {
		t.Fatalf("sniff = %v, %v", result, err)
	}
	request := session.SniffingRequest{Enabled: true, OverrideDestinationForProtocol: []string{"http"}}
	original := net.TCPDestination(net.ParseAddress("example.com"), 80)
	reason := (&DefaultDispatcher{}).sniffOverrideReason(context.Background(), result, request, original)
	d := makeSniffDecision(request, original, result, nil, reason, "destination", time.Millisecond)
	if d.result != "success" || d.changed || d.source != "http1" {
		t.Fatalf("unexpected diagnostic: %s", d)
	}
	mb, err := reader.ReadMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(mb)
	got := make([]byte, mb.Len())
	mb.Copy(got)
	if string(got) != wire {
		t.Fatalf("sniffing changed the forwarded payload: %q", got)
	}
}

type diagnosticLogCapture struct{ messages []log.Message }

func (c *diagnosticLogCapture) Handle(m log.Message) { c.messages = append(c.messages, m) }

func TestSniffDiagnosticUsesDebugAndConnectionID(t *testing.T) {
	capture := &diagnosticLogCapture{}
	log.RegisterHandler(capture)
	defer log.RegisterHandler(&diagnosticLogCapture{})
	ctx := commonctx.ContextWithID(context.Background(), commonctx.ID(12345))
	logSniffDecision(ctx, session.SniffingRequest{}, net.TCPDestination(net.ParseAddress("example.com"), 443), nil, nil, "disabled", "none", 0)
	if len(capture.messages) != 1 {
		t.Fatalf("got %d messages", len(capture.messages))
	}
	message, ok := capture.messages[0].(*log.GeneralMessage)
	if !ok || message.Severity != log.Severity_Debug {
		t.Fatalf("not Debug: %v", capture.messages[0])
	}
	if text := message.String(); !strings.Contains(text, "12345") || !strings.Contains(text, "result=disabled") {
		t.Fatalf("missing context: %s", text)
	}
}

func TestSniffDiagnosticWarnsWhenIPHasNoRoutableDomain(t *testing.T) {
	capture := &diagnosticLogCapture{}
	log.RegisterHandler(capture)
	defer log.RegisterHandler(&diagnosticLogCapture{})
	ctx := commonctx.ContextWithID(context.Background(), commonctx.ID(12345))
	request := session.SniffingRequest{Enabled: true, OverrideDestinationForProtocol: []string{"tls"}}
	original := net.TCPDestination(net.ParseAddress("192.0.2.1"), 443)
	logSniffDecision(ctx, request, original, nil, buf.ErrReadTimeout, "", "none", 200*time.Millisecond)
	if len(capture.messages) != 1 {
		t.Fatalf("got %d messages", len(capture.messages))
	}
	message, ok := capture.messages[0].(*log.GeneralMessage)
	if !ok || message.Severity != log.Severity_Warning {
		t.Fatalf("not Warning: %v", capture.messages[0])
	}
	if text := message.String(); !strings.Contains(text, "12345") || !strings.Contains(text, "result=failed") || !strings.Contains(text, "reason=timeout") {
		t.Fatalf("missing failure context: %s", text)
	}
}

func TestSniffDiagnosticsFailureReasons(t *testing.T) {
	for _, test := range []struct {
		name string
		err  error
		want string
	}{
		{"read timeout", buf.ErrReadTimeout, "timeout"},
		{"EOF", io.EOF, "eof"},
		{"empty attempts", nil, "attempt_limit"},
	} {
		t.Run(test.name, func(t *testing.T) {
			r := &diagnosticReader{err: test.err}
			ctx := context.WithValue(context.Background(), core.XrayKey(1), &core.Instance{})
			_, err := sniffer(ctx, &cachedReader{reader: r}, false, net.Network_TCP)
			if got := sniffFailureReason(err); got != test.want {
				t.Fatalf("reason=%s error=%v", got, err)
			}
			if test.err == nil && r.calls != 2 {
				t.Fatalf("attempt budget changed: %d", r.calls)
			}
		})
	}
	request := session.SniffingRequest{}
	original := net.TCPDestination(net.ParseAddress("example.com"), 443)
	d := makeSniffDecision(request, original, nil, nil, "disabled", "none", 0)
	if d.result != "disabled" || d.changed {
		t.Fatalf("disabled: %s", d)
	}
	request.Enabled = true
	d = makeSniffDecision(request, original, diagnosticResult{"tls", "example.com\nforged-entry"}, nil, "excluded_domain", "none", 0)
	if strings.Contains(d.String(), "\n") {
		t.Fatal("domain injected a log line")
	}
}
