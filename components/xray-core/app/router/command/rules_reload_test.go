package command

import (
	"context"
	"net"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/router"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/serial"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/test/bufconn"
)

func TestAddRuleReplacesCompleteRoutingThroughExistingRPC(t *testing.T) {
	r := new(router.Router)
	if err := r.Init(context.Background(), &router.Config{}, nil, nil, nil); err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	listener := bufconn.Listen(1 << 20)
	defer listener.Close()
	server := grpc.NewServer()
	RegisterRoutingServiceServer(server, NewRoutingServer(r, nil))
	go server.Serve(listener)
	defer server.Stop()
	conn, err := grpc.NewClient("passthrough:///routing", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) { return listener.DialContext(ctx) }))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client := NewRoutingServiceClient(conn)
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	for _, tag := range []string{"old", "new"} {
		cfg := &router.Config{
			Rule:          []*router.RoutingRule{{RuleTag: tag, Networks: []xnet.Network{xnet.Network_TCP}, TargetTag: &router.RoutingRule_Tag{Tag: "blocked"}}},
			BalancingRule: []*router.BalancingRule{{Tag: "health", OutboundSelector: []string{"out:"}}},
		}
		if _, err := client.AddRule(ctx, &AddRuleRequest{Config: serial.ToTypedMessage(cfg), ShouldAppend: false}); err != nil {
			t.Fatal(err)
		}
		listed, err := client.ListRule(ctx, &ListRuleRequest{})
		if err != nil || len(listed.GetRules()) != 1 || listed.Rules[0].RuleTag != tag {
			t.Fatalf("replacement: %v %v", listed, err)
		}
		if _, err := client.GetBalancerInfo(ctx, &GetBalancerInfoRequest{Tag: "health"}); err != nil {
			t.Fatal(err)
		}
	}
	invalid := &router.Config{Rule: []*router.RoutingRule{{RuleTag: "invalid"}}}
	if _, err := client.AddRule(ctx, &AddRuleRequest{Config: serial.ToTypedMessage(invalid)}); err == nil {
		t.Fatal("invalid table accepted")
	}
	if rules := r.ListRule(); len(rules) != 1 || rules[0].GetRuleTag() != "new" {
		t.Fatal("failed reload changed the live table")
	}
	if _, err := client.AddRule(ctx, &AddRuleRequest{Config: serial.ToTypedMessage(&router.Config{})}); err != nil {
		t.Fatal(err)
	}
	if len(r.ListRule()) != 0 {
		t.Fatal("empty replacement retained old rules")
	}
}
