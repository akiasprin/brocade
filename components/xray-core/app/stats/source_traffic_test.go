package stats

import (
	"sync"
	"testing"

	"github.com/xtls/xray-core/common/net"
	featurestats "github.com/xtls/xray-core/features/stats"
)

func sourceValue(m *Manager, name string) int64 {
	for _, row := range m.SourceTrafficSnapshot() {
		if row.Name == name {
			return row.Value
		}
	}
	return -1
}

func TestSourceTrafficCanonicalizesDispatcherAddresses(t *testing.T) {
	for _, test := range []struct {
		name, source, canonical string
	}{
		{"IPv4", net.ParseAddress("192.0.2.1").String(), "192.0.2.1"},
		{"dispatcher IPv6", net.ParseAddress("2001:db8::1").String(), "2001:db8::1"},
		{"bare IPv6", "2001:db8::1", "2001:db8::1"},
		{"expanded IPv6", "[2001:0db8:0:0:0:0:0:1]", "2001:db8::1"},
		{"mapped IPv4", "[::ffff:192.0.2.1]", "192.0.2.1"},
	} {
		t.Run(test.name, func(t *testing.T) {
			m := &Manager{}
			m.sourceTraffic.bucketSeconds = 30
			m.sourceTraffic.update(otherDomainPolicy{Enabled: true, MaxDomainsPerUser: 1, NodeCapacity: 1}, 1800000001)
			up, down := featurestats.WithSourceTraffic(m, "alice@t#i", test.source, nil, nil)
			if up == nil || down == nil {
				t.Fatal("dispatcher source address lost its traffic counters")
			}
			up.Add(11)
			down.Add(22)
			// Both source representations must share one entry, even at capacity.
			canonicalUp, canonicalDown := m.SourceTrafficCounters("alice@t#i", test.canonical)
			if canonicalUp == nil || canonicalDown == nil {
				t.Fatal("canonical source created a second entry")
			}
			canonicalUp.Add(3)
			canonicalDown.Add(4)
			prefix := "sourcetraffic>>>1800000000>>>alice@t#i>>>" + test.canonical + ">>>"
			if got := sourceValue(m, prefix+"uplink"); got != 14 {
				t.Fatalf("source uplink = %d, want 14", got)
			}
			if got := sourceValue(m, prefix+"downlink"); got != 26 {
				t.Fatalf("source downlink = %d, want 26", got)
			}
		})
	}
}

func TestSourceTrafficRejectsInvalidSources(t *testing.T) {
	for _, source := range []string{
		"", "example.test", "[2001:db8::1]:443", "[2001:db8::1", "2001:db8::1]", "[[2001:db8::1]]",
		"0.0.0.0", "224.0.0.1", "::", "[::]", "ff02::1", "[ff02::1]", "fe80::1%eth0", "[fe80::1%eth0]",
	} {
		t.Run(source, func(t *testing.T) {
			m := &Manager{}
			m.sourceTraffic.bucketSeconds = 30
			m.sourceTraffic.update(otherDomainPolicy{Enabled: true, MaxDomainsPerUser: 1, NodeCapacity: 1}, 1800000001)
			if up, down := m.SourceTrafficCounters("alice@t#i", source); up != nil || down != nil {
				t.Fatal("invalid source received traffic counters")
			}
		})
	}
}

func TestSourceTrafficRetainsShortConnectionsAndRotatesLongHandles(t *testing.T) {
	m := &Manager{}
	m.sourceTraffic.bucketSeconds = 30
	p := otherDomainPolicy{Enabled: true, TopN: 2, MaxDomainsPerUser: 2, NodeCapacity: 4, ReportIntervalSecs: 15}
	m.sourceTraffic.update(p, 1800000001)
	up, down := featurestats.WithSourceTraffic(m, "alice@t#i", "::ffff:192.0.2.1", nil, nil)
	up.Add(11)
	down.Add(22)
	// No online map/reference exists: closing a short connection cannot delete its bytes.
	if got := sourceValue(m, "sourcetraffic>>>1800000000>>>alice@t#i>>>192.0.2.1>>>uplink"); got != 11 {
		t.Fatalf("short source lost: %d", got)
	}
	m.sourceTraffic.update(p, 1800000031)
	up.Add(3)
	down.Add(4)
	if got := sourceValue(m, "sourcetraffic>>>1800000030>>>alice@t#i>>>192.0.2.1>>>downlink"); got != 4 {
		t.Fatalf("long handle did not rotate: %d", got)
	}
	if got := sourceValue(m, "sourcetraffic>>>1800000000>>>alice@t#i>>>192.0.2.1>>>downlink"); got != 22 {
		t.Fatalf("previous window changed: %d", got)
	}
	m.sourceTraffic.update(p, 1800000061)
	if sourceValue(m, "sourcetraffic>>>1800000000>>>alice@t#i>>>192.0.2.1>>>uplink") != -1 {
		t.Fatal("expired window retained")
	}
}

func TestSourceTrafficBudgetAndConcurrentAdds(t *testing.T) {
	m := &Manager{}
	m.sourceTraffic.bucketSeconds = 30
	m.sourceTraffic.update(otherDomainPolicy{Enabled: true, MaxDomainsPerUser: 1, NodeCapacity: 1}, 1800000001)
	up, _ := m.SourceTrafficCounters("alice@t#i", "192.0.2.1")
	if next, _ := m.SourceTrafficCounters("alice@t#i", "192.0.2.2"); next != nil {
		t.Fatal("source budget exceeded")
	}
	var workers sync.WaitGroup
	for range 8 {
		workers.Go(func() {
			for range 1000 {
				up.Add(1)
			}
		})
	}
	workers.Wait()
	if up.Value() != 8000 || sourceValue(m, "sourcetraffic>>>limited") != 1 {
		t.Fatal("existing source or limited marker lost")
	}
	if invalid, _ := m.SourceTrafficCounters("alice@t#i", "fe80::1%eth0"); invalid != nil {
		t.Fatal("scoped IP accepted")
	}
	if probe, _ := featurestats.WithSourceTraffic(m, "probe#test", "192.0.2.1", nil, nil); probe != nil {
		t.Fatal("probe observed as a user")
	}
}
