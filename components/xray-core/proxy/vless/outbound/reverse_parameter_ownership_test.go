package outbound

import (
	"context"
	stderrors "errors"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type ownershipDialer struct {
	calls        int
	failuresLeft int
}

func (d *ownershipDialer) Dial(context.Context, net.Destination) (stat.Connection, error) {
	d.calls++
	if d.failuresLeft > 0 {
		d.failuresLeft--
		return nil, stderrors.New("dial failed")
	}
	return nil, nil
}

func (*ownershipDialer) DestIpAddress() net.IP { return nil }

func (*ownershipDialer) SetOutboundGateway(context.Context, *session.Outbound) {}

func TestReverseCarrierUsesOnePhysicalDialPerPoolAttempt(t *testing.T) {
	dialer := &ownershipDialer{failuresLeft: 5}
	_, err := dialVLESSServer(context.Background(), dialer, net.TCPDestination(net.LocalHostIP, 443), protocol.RequestCommandRvs)
	if err == nil {
		t.Fatal("failed reverse dial unexpectedly succeeded")
	}
	if dialer.calls != 1 {
		t.Fatalf("one reverse-pool attempt performed %d physical dials", dialer.calls)
	}
}

func TestOrdinaryCarrierKeepsVLESSDialRetry(t *testing.T) {
	dialer := &ownershipDialer{failuresLeft: 1}
	_, err := dialVLESSServer(context.Background(), dialer, net.TCPDestination(net.LocalHostIP, 443), protocol.RequestCommandTCP)
	if err != nil {
		t.Fatalf("ordinary VLESS retry failed: %v", err)
	}
	if dialer.calls != 2 {
		t.Fatalf("ordinary VLESS performed %d dials, want 2", dialer.calls)
	}
}

func TestReverseCarrierDoesNotInheritUserSessionTimeouts(t *testing.T) {
	reverseCtx, reverseCancel := context.WithCancel(context.Background())
	timer := requestActivityTimer(reverseCtx, protocol.RequestCommandRvs, reverseCancel, time.Hour)
	timer.SetTimeout(0)
	select {
	case <-reverseCtx.Done():
		t.Fatal("generic user timeout canceled reverse carrier")
	default:
	}
	reverseCancel()

	ordinaryCtx, ordinaryCancel := context.WithCancel(context.Background())
	timer = requestActivityTimer(ordinaryCtx, protocol.RequestCommandTCP, ordinaryCancel, time.Hour)
	timer.SetTimeout(0)
	select {
	case <-ordinaryCtx.Done():
	default:
		t.Fatal("ordinary carrier stopped honoring user timeout")
	}
}

func TestReverseCommandRequiresItsOwnControlDestination(t *testing.T) {
	target := net.Destination{Address: net.DomainAddress("v1.rvs.cool")}
	command, err := requestCommandForTarget(target)
	if err != nil || command != protocol.RequestCommandRvs {
		t.Fatalf("reverse target mapped to command=%v err=%v", command, err)
	}
	target.Network = net.Network_TCP
	if _, err := requestCommandForTarget(target); err == nil {
		t.Fatal("reverse control target accepted an ordinary network parameter")
	}
}
