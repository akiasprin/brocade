package command_test

import (
	"context"
	"slices"
	"testing"

	"github.com/xtls/xray-core/app/stats"
	. "github.com/xtls/xray-core/app/stats/command"
	featurestats "github.com/xtls/xray-core/features/stats"
	"google.golang.org/protobuf/proto"
)

func TestGetUsersStatsReportsProtocolsWithoutChangingLabelsOrTraffic(t *testing.T) {
	m, err := stats.NewManager(context.Background(), &stats.Config{})
	if err != nil {
		t.Fatal(err)
	}
	const label = "alice@example.test#i-main"
	om, err := m.RegisterOnlineMap("user>>>" + label + ">>>online")
	if err != nil {
		t.Fatal(err)
	}
	om.(featurestats.ProtocolOnlineMap).AddIPWithProtocol("203.0.113.10", "vless")
	om.(featurestats.ProtocolOnlineMap).AddIPWithProtocol("203.0.113.10", "hysteria")
	counter, err := m.RegisterCounter("user>>>" + label + ">>>traffic>>>uplink")
	if err != nil {
		t.Fatal(err)
	}
	counter.Set(123)
	response, err := NewStatsServer(m).GetUsersStats(context.Background(), &GetUsersStatsRequest{IncludeTraffic: true})
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := proto.Marshal(response)
	if err != nil {
		t.Fatal(err)
	}
	decoded := new(GetUsersStatsResponse)
	if err := proto.Unmarshal(encoded, decoded); err != nil {
		t.Fatal(err)
	}
	if len(decoded.Users) != 1 || decoded.Users[0].Email != label || len(decoded.Users[0].Ips) != 1 {
		t.Fatalf("unexpected users: %v", decoded)
	}
	user := decoded.Users[0]
	if !slices.Equal(user.Ips[0].Protocols, []string{"hysteria", "vless"}) || user.Traffic.Uplink != 123 || counter.Value() != 123 {
		t.Fatalf("protocol/traffic mismatch: %v", user)
	}
}
