package internet_test

import (
	"context"
	"errors"
	"net"
	"testing"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/finalmask"
)

type failedPacketMask struct{ cause error }

func (m failedPacketMask) WrapPacketConnClient(net.PacketConn, *xnet.Destination, *finalmask.Dialer) (net.PacketConn, error) {
	return nil, m.cause
}

func (m failedPacketMask) WrapPacketConnServer(net.PacketConn, net.Addr, *finalmask.ListenConfig) (net.PacketConn, error) {
	return nil, m.cause
}

type packetTestGate struct{ closed bool }

func (*packetTestGate) BeforeAccept(ctx context.Context) (context.Context, bool) { return ctx, true }
func (*packetTestGate) AfterAccept(bool)                                         {}
func (*packetTestGate) StartConnection() bool                                    { return true }
func (*packetTestGate) FinishConnection()                                        {}
func (g *packetTestGate) Close()                                                 { g.closed = true }

func TestMaskedHandoffReleasesSocketAndGateOnMaskFailure(t *testing.T) {
	cause := errors.New("mask initialization failed")
	var raw net.PacketConn
	gate := &packetTestGate{}
	release, err := internet.RegisterPacketHandoffController(func(address string, _ *internet.SocketConfig, bind func(string) (net.PacketConn, error)) (net.PacketConn, internet.PacketHandoffGate, error) {
		var err error
		raw, err = bind(address)
		if _, ok := raw.(*net.UDPConn); !ok {
			t.Fatalf("controller received a wrapped socket: %T", raw)
		}
		return raw, gate, err
	})
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	mask := finalmask.NewFinalMask(nil, []finalmask.UDPMask{failedPacketMask{cause}}, nil, nil, nil, nil)
	conn, returnedGate, err := internet.ListenPacketWithHandoff(context.Background(), &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)}, &internet.MemoryStreamConfig{FinalMask: mask})
	if !errors.Is(err, cause) || conn != nil || returnedGate != nil {
		t.Fatalf("mask failure was lost: conn=%v gate=%v err=%v", conn, returnedGate, err)
	}
	if !gate.closed {
		t.Fatal("failed mask retained its generation admission gate")
	}
	if _, err := raw.WriteTo([]byte("closed"), raw.LocalAddr()); !errors.Is(err, net.ErrClosed) {
		t.Fatalf("failed mask retained the raw socket: %v", err)
	}
}
