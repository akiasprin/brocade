package mtproto

import (
	"context"
	"io"
	"net"
	"slices"
	"testing"
	"time"

	appstats "github.com/xtls/xray-core/app/stats"
	"github.com/xtls/xray-core/features/policy"
	featurestats "github.com/xtls/xray-core/features/stats"
)

func TestAccountConnectionPublishesUserTrafficAndOnlineProtocol(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	server, client := net.Pipe()
	defer client.Close()

	const (
		email    = "alice@example.test#ing-main"
		sourceIP = "203.0.113.10"
	)
	accounted, stopAccounting := accountConnection(
		manager,
		newOnlineObservationTracker(100*time.Millisecond, 16),
		policy.Session{Stats: policy.Stats{UserUplink: true, UserDownlink: true, UserOnline: true}},
		email,
		sourceIP,
		server,
	)
	defer accounted.Close()

	online := manager.GetOnlineMap("user>>>" + email + ">>>online")
	if online == nil || online.Count() != 1 {
		t.Fatalf("online map count = %v, want 1", online)
	}
	protocols, ok := online.(featurestats.ProtocolOnlineMap)
	if !ok {
		t.Fatal("online map does not retain authenticated protocol metadata")
	}
	var gotIP string
	var gotProtocols []string
	protocols.ForEachWithProtocols(func(ip string, _ int64, names []string) bool {
		gotIP = ip
		gotProtocols = append([]string(nil), names...)
		return false
	})
	if gotIP != sourceIP || !slices.Equal(gotProtocols, []string{protocolName}) {
		t.Fatalf("online source = %q %v", gotIP, gotProtocols)
	}

	uplink := []byte("client to proxy")
	go func() { _, _ = client.Write(uplink) }()
	gotUplink := make([]byte, len(uplink))
	if _, err := io.ReadFull(accounted, gotUplink); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(gotUplink, uplink) {
		t.Fatalf("uplink = %q", gotUplink)
	}

	downlink := []byte("proxy to client")
	written := make(chan error, 1)
	go func() {
		_, err := accounted.Write(downlink)
		written <- err
	}()
	gotDownlink := make([]byte, len(downlink))
	if _, err := io.ReadFull(client, gotDownlink); err != nil {
		t.Fatal(err)
	}
	if err := <-written; err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(gotDownlink, downlink) {
		t.Fatalf("downlink = %q", gotDownlink)
	}

	if got := manager.GetCounter("user>>>" + email + ">>>traffic>>>uplink").Value(); got != int64(len(uplink)) {
		t.Fatalf("uplink counter = %d", got)
	}
	if got := manager.GetCounter("user>>>" + email + ">>>traffic>>>downlink").Value(); got != int64(len(downlink)) {
		t.Fatalf("downlink counter = %d", got)
	}

	stopAccounting()
	if online.Count() != 1 {
		t.Fatalf("online map count during observation grace = %d, want 1", online.Count())
	}
	waitForOnlineCount(t, online, 0)
}

func TestAccountConnectionRespectsDisabledUserStats(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	server, client := net.Pipe()
	defer server.Close()
	defer client.Close()

	accounted, stopAccounting := accountConnection(
		manager,
		newOnlineObservationTracker(time.Minute, 16),
		policy.Session{},
		"alice@example.test#ing-main",
		"203.0.113.10",
		server,
	)
	defer stopAccounting()

	if accounted != server {
		t.Fatal("connection was wrapped while user statistics were disabled")
	}
	if manager.GetCounter("user>>>alice@example.test#ing-main>>>traffic>>>uplink") != nil {
		t.Fatal("uplink counter was registered while disabled")
	}
	if manager.GetOnlineMap("user>>>alice@example.test#ing-main>>>online") != nil {
		t.Fatal("online map was registered while disabled")
	}
}

func TestOnlineObservationReconnectRenewsOneBoundedReference(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	tracker := newOnlineObservationTracker(100*time.Millisecond, 16)
	const email = "alice@example.test#ing-main"

	stopFirst := tracker.observe(manager, email, "203.0.113.10")
	online := manager.GetOnlineMap("user>>>" + email + ">>>online")
	stopFirst()
	stopSecond := tracker.observe(manager, email, "203.0.113.10")
	time.Sleep(150 * time.Millisecond)
	if online.Count() != 1 {
		t.Fatalf("renewed online map count = %d, want 1", online.Count())
	}
	stopSecond()
	waitForOnlineCount(t, online, 0)
}

func TestOnlineObservationCapFallsBackToConnectionLifetime(t *testing.T) {
	manager, err := appstats.NewManager(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	tracker := newOnlineObservationTracker(time.Minute, 1)
	stopFirst := tracker.observe(manager, "alice@example.test#ing-main", "203.0.113.10")
	stopSecond := tracker.observe(manager, "bob@example.test#ing-main", "203.0.113.11")
	second := manager.GetOnlineMap("user>>>bob@example.test#ing-main>>>online")
	stopSecond()
	if second.Count() != 0 {
		t.Fatalf("over-cap online map count = %d, want immediate removal", second.Count())
	}
	stopFirst()
}

func waitForOnlineCount(t *testing.T, online featurestats.OnlineMap, want int) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for online.Count() != want {
		if time.Now().After(deadline) {
			t.Fatalf("online map count = %d, want %d", online.Count(), want)
		}
		time.Sleep(time.Millisecond)
	}
}
