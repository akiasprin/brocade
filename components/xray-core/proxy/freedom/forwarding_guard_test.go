package freedom

import (
	"context"
	"errors"
	"net"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/geodata"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/utils"
	"github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type forwardingDNS struct{ fail bool }

func (*forwardingDNS) Start() error      { return nil }
func (*forwardingDNS) Close() error      { return nil }
func (*forwardingDNS) Type() interface{} { return dns.ClientType() }
func (d *forwardingDNS) LookupIP(string, dns.IPOption) ([]xnet.IP, uint32, error) {
	if d.fail {
		return nil, 0, errors.New("injected DNS failure")
	}
	return []xnet.IP{net.IPv4(127, 0, 0, 1)}, 1, nil
}

type forwardingSystemDialer struct{}

func (*forwardingSystemDialer) Dial(ctx context.Context, dest xnet.Destination) (stat.Connection, error) {
	return (&internet.DefaultSystemDialer{}).Dial(ctx, nil, dest, nil)
}
func (*forwardingSystemDialer) DestIpAddress() xnet.IP                                { return nil }
func (*forwardingSystemDialer) SetOutboundGateway(context.Context, *session.Outbound) {}

func TestFreedomOnlyBlocksExplicitIPRules(t *testing.T) {
	for _, blocked := range []*IPRules{nil, {}} {
		handler := new(Handler)
		if err := handler.Init(&Config{IpsBlocked: blocked}, policy.DefaultManager{}); err != nil {
			t.Fatal(err)
		}
		if handler.blockedIPMatcher != nil {
			t.Fatal("missing or empty ipsBlocked unexpectedly enabled an IP blocker")
		}
	}

	rules, err := geodata.ParseIPRules([]string{"127.0.0.0/8"})
	if err != nil {
		t.Fatal(err)
	}
	handler := new(Handler)
	if err := handler.Init(&Config{IpsBlocked: &IPRules{Rules: rules}}, policy.DefaultManager{}); err != nil {
		t.Fatal(err)
	}
	if handler.blockedIPMatcher == nil || !handler.blockedIPMatcher.Match(xnet.LocalHostIP.IP()) {
		t.Fatal("explicit ipsBlocked rule did not enable the IP blocker")
	}
}

func TestForwardingGuardBlocksBeforeTCPConnectIncludingDNSFallback(t *testing.T) {
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	rules, err := geodata.ParseIPRules([]string{"127.0.0.0/8", "::1/128"})
	if err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name, address string
		strategy      internet.DomainStrategy
		failDNS       bool
	}{
		{"literal", "127.0.0.1", internet.DomainStrategy_USE_IP, false},
		{"mapped", "::ffff:127.0.0.1", internet.DomainStrategy_USE_IP, false},
		{"custom_dns", "forwarding.invalid", internet.DomainStrategy_USE_IP, false},
		{"system_dns", "localhost", internet.DomainStrategy_AS_IS, false},
		{"system_fallback", "localhost", internet.DomainStrategy_USE_IP, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			internet.InitSystemDialer(&forwardingDNS{fail: test.failDNS}, nil)
			defer internet.InitSystemDialer(nil, nil)
			handler := new(Handler)
			if err := handler.Init(&Config{DomainStrategy: test.strategy}, policy.DefaultManager{}); err != nil {
				t.Fatal(err)
			}
			matcher, err := geodata.IPReg.BuildIPMatcher(rules)
			if err != nil {
				t.Fatal(err)
			}
			dest := xnet.TCPDestination(xnet.ParseAddress(test.address), xnet.Port(listener.Addr().(*net.TCPAddr).Port))
			ctx := session.ContextWithInbound(context.Background(), &session.Inbound{Name: "dokodemo-door", Tag: "fwd:42", ForwardingTargetIPMatcher: matcher})
			ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: dest}})
			if err := handler.Process(ctx, &transport.Link{}, &forwardingSystemDialer{}); err == nil {
				t.Fatal("private target accepted")
			}
			listener.SetDeadline(time.Now().Add(30 * time.Millisecond))
			if conn, err := listener.Accept(); err == nil {
				conn.Close()
				t.Fatal("private target received TCP SYN/connection")
			}
		})
	}
}

func TestForwardingUDPGuardCoversCachedAndNewPacketDestinations(t *testing.T) {
	receiver, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer receiver.Close()
	sender, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer sender.Close()
	rules, err := geodata.ParseIPRules([]string{"127.0.0.0/8"})
	if err != nil {
		t.Fatal(err)
	}
	handler := new(Handler)
	if err := handler.Init(&Config{DomainStrategy: internet.DomainStrategy_USE_IP, IpsBlocked: &IPRules{Rules: rules}}, policy.DefaultManager{}); err != nil {
		t.Fatal(err)
	}
	internet.InitSystemDialer(&forwardingDNS{}, nil)
	defer internet.InitSystemDialer(nil, nil)
	writer := &PacketWriter{
		PacketConnWrapper: &internet.PacketConnWrapper{PacketConn: sender, Dest: receiver.LocalAddr()},
		Handler:           handler, BlockedIPMatcher: handler.blockedIPMatcher,
		ResolvedUDPAddr: utils.NewTypedSyncMap[string, xnet.Address](),
	}
	writer.ResolvedUDPAddr.Store("cached.invalid", xnet.LocalHostIP)
	for _, address := range []string{"127.0.0.1", "::ffff:127.0.0.1", "cached.invalid", "fresh.invalid"} {
		b := buf.New()
		b.Write([]byte("must not reach origin"))
		dest := xnet.UDPDestination(xnet.ParseAddress(address), xnet.Port(receiver.LocalAddr().(*net.UDPAddr).Port))
		b.UDP = &dest
		if err := writer.WriteMultiBuffer(buf.MultiBuffer{b}); err != nil {
			t.Fatal(err)
		}
	}
	receiver.SetReadDeadline(time.Now().Add(30 * time.Millisecond))
	if _, _, err := receiver.ReadFrom(make([]byte, 100)); err == nil {
		t.Fatal("blocked UDP packet reached target")
	}
	// Explicitly allowed traffic still uses the same packet writer normally.
	writer.BlockedIPMatcher = nil
	b := buf.New()
	b.Write([]byte("allowed"))
	dest := xnet.DestinationFromAddr(receiver.LocalAddr())
	b.UDP = &dest
	if err := writer.WriteMultiBuffer(buf.MultiBuffer{b}); err != nil {
		t.Fatal(err)
	}
	receiver.SetReadDeadline(time.Now().Add(time.Second))
	if _, _, err := receiver.ReadFrom(make([]byte, 100)); err != nil {
		t.Fatal(err)
	}
}
