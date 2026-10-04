package stats_test

import (
	"slices"
	"sync"
	"testing"

	"github.com/xtls/xray-core/app/stats"
)

func TestOnlineMapProtocolReferences(t *testing.T) {
	om := stats.NewOnlineMap()
	const ip = "203.0.113.10"
	check := func(want []string, count int) {
		t.Helper()
		var got []string
		om.ForEachWithProtocols(func(address string, lastSeen int64, protocols []string) bool {
			if address != ip || lastSeen <= 0 {
				t.Fatalf("unexpected source %q at %d", address, lastSeen)
			}
			got = protocols
			return true
		})
		if om.Count() != count || !slices.Equal(got, want) {
			t.Fatalf("count=%d protocols=%v, want %d %v", om.Count(), got, count, want)
		}
	}
	om.AddIPWithProtocol(ip, "vless")
	om.AddIPWithProtocol(ip, "vless")
	om.AddIPWithProtocol(ip, "anytls")
	om.AddIP(ip)
	check([]string{"", "anytls", "vless"}, 1)
	om.RemoveIPWithProtocol(ip, "hysteria") // an unrelated close must not decrement another protocol
	om.RemoveIPWithProtocol(ip, "vless")
	check([]string{"", "anytls", "vless"}, 1)
	om.RemoveIPWithProtocol(ip, "vless")
	check([]string{"", "anytls"}, 1)
	om.RemoveIP(ip)
	check([]string{"anytls"}, 1)
	om.RemoveIPWithProtocol(ip, "anytls")
	om.RemoveIPWithProtocol(ip, "anytls")
	check(nil, 0)
	om.AddIPWithProtocol("127.0.0.1", "vless")
	om.AddIPWithProtocol("[::1]", "anytls")
	check(nil, 0)
}

func TestOnlineMapConcurrentProtocolSnapshots(t *testing.T) {
	om := stats.NewOnlineMap()
	var workers sync.WaitGroup
	for _, protocol := range []string{"vless", "anytls", "hysteria", ""} {
		workers.Go(func() {
			for range 200 {
				om.AddIPWithProtocol("203.0.113.10", protocol)
				om.ForEachWithProtocols(func(_ string, _ int64, protocols []string) bool {
					if !slices.IsSorted(protocols) || len(protocols) == 0 {
						t.Errorf("invalid protocol snapshot: %v", protocols)
					}
					return true
				})
				om.RemoveIPWithProtocol("203.0.113.10", protocol)
			}
		})
	}
	workers.Wait()
	if om.Count() != 0 {
		t.Fatalf("leaked source references: %d", om.Count())
	}
}
