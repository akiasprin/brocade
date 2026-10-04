package dispatcher

import (
	"bytes"
	"context"
	gotls "crypto/tls"
	"encoding/binary"
	"io"
	stdnet "net"
	"testing"
	"time"

	appRouter "github.com/xtls/xray-core/app/router"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/geodata"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/outbound"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/transport"
)

type framedDownlinkRouteCapture struct {
	tag         string
	target      xnet.Destination
	routeTarget xnet.Destination
	protocol    string
	payload     []byte
	err         error
}

type framedDownlinkRouteHandler struct {
	tag     string
	results chan<- framedDownlinkRouteCapture
}

func (*framedDownlinkRouteHandler) Start() error { return nil }

func (*framedDownlinkRouteHandler) Close() error { return nil }

func (h *framedDownlinkRouteHandler) Tag() string { return h.tag }

func (*framedDownlinkRouteHandler) SenderSettings() *serial.TypedMessage { return nil }

func (*framedDownlinkRouteHandler) ProxySettings() *serial.TypedMessage { return nil }

func (h *framedDownlinkRouteHandler) Dispatch(ctx context.Context, link *transport.Link) {
	result := framedDownlinkRouteCapture{tag: h.tag}
	outbounds := session.OutboundsFromContext(ctx)
	if len(outbounds) > 0 {
		result.target = outbounds[len(outbounds)-1].Target
		result.routeTarget = outbounds[len(outbounds)-1].RouteTarget
	}
	if content := session.ContentFromContext(ctx); content != nil {
		result.protocol = content.Protocol
	}
	mb, err := link.Reader.ReadMultiBuffer()
	if !mb.IsEmpty() {
		result.payload = make([]byte, mb.Len())
		mb.Copy(result.payload)
		buf.ReleaseMulti(mb)
	}
	result.err = err
	common.Interrupt(link.Reader)
	_ = common.Close(link.Writer)
	h.results <- result
}

type framedDownlinkRouteManager struct {
	handlers       map[string]outbound.Handler
	defaultHandler outbound.Handler
}

func (*framedDownlinkRouteManager) Start() error { return nil }

func (*framedDownlinkRouteManager) Close() error { return nil }

func (*framedDownlinkRouteManager) Type() interface{} { return outbound.ManagerType() }

func (m *framedDownlinkRouteManager) GetHandler(tag string) outbound.Handler { return m.handlers[tag] }

func (m *framedDownlinkRouteManager) GetDefaultHandler() outbound.Handler { return m.defaultHandler }

func (m *framedDownlinkRouteManager) AddHandler(_ context.Context, handler outbound.Handler) error {
	m.handlers[handler.Tag()] = handler
	return nil
}

func (m *framedDownlinkRouteManager) RemoveHandler(_ context.Context, tag string) error {
	delete(m.handlers, tag)
	return nil
}

func (m *framedDownlinkRouteManager) ListHandlers(context.Context) []outbound.Handler {
	handlers := make([]outbound.Handler, 0, len(m.handlers))
	for _, handler := range m.handlers {
		handlers = append(handlers, handler)
	}
	return handlers
}

type inertFramedDownlinkSplicer struct{}

func (inertFramedDownlinkSplicer) SpliceDownlink(context.Context, stdnet.Conn, func(int64)) (bool, error) {
	return false, nil
}

func TestFramedDownlinkMetadataPreservesTLSSniffingAndRouteOnly(t *testing.T) {
	results := make(chan framedDownlinkRouteCapture, 2)
	matched := &framedDownlinkRouteHandler{tag: "matched", results: results}
	fallback := &framedDownlinkRouteHandler{tag: "fallback", results: results}
	manager := &framedDownlinkRouteManager{
		handlers: map[string]outbound.Handler{
			matched.tag:  matched,
			fallback.tag: fallback,
		},
		defaultHandler: fallback,
	}

	router := new(appRouter.Router)
	if err := router.Init(context.Background(), &appRouter.Config{Rule: []*appRouter.RoutingRule{{
		TargetTag:  &appRouter.RoutingRule_Tag{Tag: matched.tag},
		InboundTag: []string{"anytls-in"},
		UserEmail:  []string{"route-regression@example.com"},
		Protocol:   []string{"tls"},
		Domain: []*geodata.DomainRule{{
			Value: &geodata.DomainRule_Custom{Custom: &geodata.Domain{Type: geodata.Domain_Full, Value: "sniff.example"}},
		}},
	}}}, nil, manager, nil); err != nil {
		t.Fatal(err)
	}

	dispatcher := new(DefaultDispatcher)
	if err := dispatcher.Init(&Config{}, manager, router, policy.DefaultManager{}, nil); err != nil {
		t.Fatal(err)
	}

	content := &session.Content{SniffingRequest: session.SniffingRequest{
		Enabled:                        true,
		RouteOnly:                      true,
		OverrideDestinationForProtocol: []string{"tls"},
	}}
	inbound := &session.Inbound{
		Tag:                   "anytls-in",
		Name:                  "anytls",
		User:                  &protocol.MemoryUser{Email: "route-regression@example.com"},
		FramedDownlinkSplicer: inertFramedDownlinkSplicer{},
	}
	ctx := context.WithValue(context.Background(), core.XrayKey(1), &core.Instance{})
	ctx = session.ContextWithInbound(ctx, inbound)
	ctx = session.ContextWithContent(ctx, content)
	original := xnet.TCPDestination(xnet.ParseAddress("192.0.2.10"), 443)
	link, err := dispatcher.Dispatch(ctx, original)
	if err != nil {
		t.Fatal(err)
	}

	clientHello := captureTLSClientHello(t, "sniff.example")
	if err := link.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(clientHello)}); err != nil {
		t.Fatal(err)
	}
	_ = common.Close(link.Writer)

	select {
	case result := <-results:
		if result.err != nil {
			t.Fatal(result.err)
		}
		if result.tag != matched.tag {
			t.Fatalf("selected outbound = %q, want %q", result.tag, matched.tag)
		}
		if result.protocol != "tls" {
			t.Fatalf("sniffed protocol = %q, want tls", result.protocol)
		}
		if result.target != original {
			t.Fatalf("routeOnly changed dial target to %v, want %v", result.target, original)
		}
		if result.routeTarget.Address == nil || result.routeTarget.Address.String() != "sniff.example" {
			t.Fatalf("route target = %v, want sniff.example", result.routeTarget)
		}
		if !bytes.Equal(result.payload, clientHello) {
			t.Fatal("TLS ClientHello changed while sniffing")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("timed out waiting for routed TLS payload")
	}
}

func captureTLSClientHello(t *testing.T, serverName string) []byte {
	t.Helper()
	client, server := stdnet.Pipe()
	t.Cleanup(func() {
		_ = client.Close()
		_ = server.Close()
	})

	handshakeDone := make(chan error, 1)
	go func() {
		tlsClient := gotls.Client(client, &gotls.Config{
			InsecureSkipVerify: true,
			ServerName:         serverName,
		})
		handshakeDone <- tlsClient.Handshake()
	}()

	header := make([]byte, 5)
	if _, err := io.ReadFull(server, header); err != nil {
		t.Fatal(err)
	}
	length := int(binary.BigEndian.Uint16(header[3:5]))
	body := make([]byte, length)
	if _, err := io.ReadFull(server, body); err != nil {
		t.Fatal(err)
	}
	_ = server.Close()
	select {
	case <-handshakeDone:
	case <-time.After(time.Second):
		t.Fatal("TLS ClientHello generator did not stop")
	}
	return append(header, body...)
}
