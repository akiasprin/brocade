package dokodemo_test

import (
	"context"
	"fmt"
	"net"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/proxyman"
	command "github.com/xtls/xray-core/app/proxyman/command"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/core"
	_ "github.com/xtls/xray-core/main/distro/all"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func freePolicyPort(t *testing.T) uint32 {
	t.Helper()
	l, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	p := uint32(l.Addr().(*net.TCPAddr).Port)
	l.Close()
	return p
}

// Exercise the actual gRPC admission, then repeat the audit's localhost-to-API
// attack against the protected listener. All sockets and credentials are local fixtures.
func TestForwardingPolicyStopsTheLoopbackManagementAPIAttack(t *testing.T) {
	apiPort := freePolicyPort(t)
	config := fmt.Sprintf(`{"log":{"loglevel":"none"},"api":{"tag":"api","services":["HandlerService"]},
	"inbounds":[{"tag":"api","listen":"127.0.0.1","port":%d,"protocol":"dokodemo-door","settings":{"address":"127.0.0.1"}}],
	"outbounds":[{"tag":"out:forward","protocol":"freedom"}],
	"routing":{"rules":[{"type":"field","inboundTag":["api"],"outboundTag":"api"}]}}`, apiPort)
	server, err := core.StartInstance("json", []byte(config))
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	conn, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", apiPort), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client := command.NewHandlerServiceClient(conn)
	capability, err := client.GetForwardingTargetPolicy(ctx, &command.ForwardingTargetPolicyRequest{})
	if err != nil || capability.Version != 1 {
		t.Fatalf("serving capability: %v %v", capability, err)
	}
	for _, host := range []string{"127.0.0.1", "localhost", "::ffff:127.0.0.1"} {
		t.Run(host, func(t *testing.T) {
			port := freePolicyPort(t)
			proxy := &dokodemo.Config{RewriteAddress: xnet.NewIPOrDomain(xnet.ParseAddress(host)), RewritePort: apiPort, AllowedNetworks: []xnet.Network{xnet.Network_TCP}}
			receiver := &proxyman.ReceiverConfig{PortList: &xnet.PortList{Range: []*xnet.PortRange{{From: port, To: port}}}, Listen: xnet.NewIPOrDomain(xnet.LocalHostIP)}
			inbound := &core.InboundHandlerConfig{Tag: fmt.Sprintf("fwd:%d", port), ReceiverSettings: serial.ToTypedMessage(receiver), ProxySettings: serial.ToTypedMessage(proxy)}
			if _, err := client.AddInbound(ctx, &command.AddInboundRequest{Inbound: inbound}); err == nil {
				t.Fatal("legacy forwarding was accepted without a policy")
			}
			// Omitting RFC1918 restrictions models explicitly allowed private-network access;
			// it still must not grant loopback access to the management API.
			proxy.ForwardingTargetPolicy = &dokodemo.ForwardingTargetPolicy{BlockedCidrs: []string{"127.0.0.0/8", "::1/128"}}
			inbound.ProxySettings = serial.ToTypedMessage(&dokodemo.ForwardingConfig{Config: proxy})
			if _, err := client.AddInbound(ctx, &command.AddInboundRequest{Inbound: inbound}); err != nil {
				t.Fatal(err)
			}
			attackCtx, attackCancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
			defer attackCancel()
			forwarded, err := grpc.DialContext(attackCtx, fmt.Sprintf("127.0.0.1:%d", port), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
			if err == nil {
				defer forwarded.Close()
				if _, err = command.NewHandlerServiceClient(forwarded).ListInbounds(attackCtx, &command.ListInboundsRequest{}); err == nil {
					t.Fatal("forwarding exposed the management API")
				}
			}
		})
	}
	if _, err := client.ListInbounds(ctx, &command.ListInboundsRequest{}); err != nil {
		t.Fatalf("legitimate local API stopped working: %v", err)
	}
	t.Run("UDP domain target", func(t *testing.T) {
		target, err := net.ListenPacket("udp4", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		defer target.Close()
		port := freePolicyPort(t)
		proxy := &dokodemo.Config{RewriteAddress: xnet.NewIPOrDomain(xnet.DomainAddress("localhost")), RewritePort: uint32(target.LocalAddr().(*net.UDPAddr).Port), AllowedNetworks: []xnet.Network{xnet.Network_UDP}, ForwardingTargetPolicy: &dokodemo.ForwardingTargetPolicy{BlockedCidrs: []string{"127.0.0.0/8", "::1/128"}}}
		receiver := &proxyman.ReceiverConfig{PortList: &xnet.PortList{Range: []*xnet.PortRange{{From: port, To: port}}}, Listen: xnet.NewIPOrDomain(xnet.LocalHostIP)}
		_, err = client.AddInbound(ctx, &command.AddInboundRequest{Inbound: &core.InboundHandlerConfig{Tag: fmt.Sprintf("fwd:%d", port), ReceiverSettings: serial.ToTypedMessage(receiver), ProxySettings: serial.ToTypedMessage(&dokodemo.ForwardingConfig{Config: proxy})}})
		if err != nil {
			t.Fatal(err)
		}
		sender, err := net.Dial("udp4", fmt.Sprintf("127.0.0.1:%d", port))
		if err != nil {
			t.Fatal(err)
		}
		defer sender.Close()
		if _, err = sender.Write([]byte("blocked")); err != nil {
			t.Fatal(err)
		}
		target.SetReadDeadline(time.Now().Add(150 * time.Millisecond))
		if _, _, err = target.ReadFrom(make([]byte, 64)); err == nil {
			t.Fatal("UDP forwarding reached the forbidden loopback target")
		}
	})
}
