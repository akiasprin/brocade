package dispatcher

import (
	"context"
	"slices"
	"sync"
	"testing"
	"time"

	appstats "github.com/xtls/xray-core/app/stats"
	"github.com/xtls/xray-core/features/stats"
)

type simultaneousRegistration struct {
	stats.Manager
	ready sync.WaitGroup
}

func (m *simultaneousRegistration) RegisterOnlineMap(name string) (stats.OnlineMap, error) {
	m.ready.Done()
	m.ready.Wait()
	return m.Manager.RegisterOnlineMap(name)
}

func TestTrackOnlineProtocolsSurviveConcurrentRegistrationAndIndependentClose(t *testing.T) {
	m, err := appstats.NewManager(context.Background(), &appstats.Config{})
	if err != nil {
		t.Fatal(err)
	}
	race := &simultaneousRegistration{Manager: m}
	race.ready.Add(2)
	vless, closeVless := context.WithCancel(context.Background())
	anytls, closeAnyTLS := context.WithCancel(context.Background())
	t.Cleanup(closeVless)
	t.Cleanup(closeAnyTLS)
	var workers sync.WaitGroup
	workers.Go(func() { trackOnlineIP(vless, race, "alice@example.test#i-main", "203.0.113.10", "vless") })
	workers.Go(func() { trackOnlineIP(anytls, race, "alice@example.test#i-main", "203.0.113.10", "anytls") })
	workers.Wait()
	om := m.GetOnlineMap("user>>>alice@example.test#i-main>>>online").(stats.ProtocolOnlineMap)
	check := func(want []string, count int) {
		t.Helper()
		deadline := time.Now().Add(2 * time.Second)
		for {
			var got []string
			om.ForEachWithProtocols(func(_ string, _ int64, protocols []string) bool { got = protocols; return true })
			if om.Count() == count && slices.Equal(got, want) {
				return
			}
			if time.Now().After(deadline) {
				t.Fatalf("got %v (%d sources), want %v (%d)", got, om.Count(), want, count)
			}
			time.Sleep(time.Millisecond)
		}
	}
	check([]string{"anytls", "vless"}, 1)
	closeAnyTLS()
	check([]string{"vless"}, 1)
	closeVless()
	check(nil, 0)
}
