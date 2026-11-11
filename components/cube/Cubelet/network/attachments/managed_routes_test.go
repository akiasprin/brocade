// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
)

func TestPrimaryNATFallbackAndMatchedRouteFailure(t *testing.T) {
	if os.Getenv("BROCADE_TEST_NETNS") != "1" {
		t.Skip("requires an explicitly enabled disposable network namespace")
	}
	script := `set -eu
ip link set lo up
ip link add main0 type dummy
ip link add biz0 type dummy
ip link set main0 up
ip link set biz0 up
ip address add 192.0.2.2/24 dev main0
ip address add 10.0.0.2/24 dev biz0
ip -6 address add fd00::2/64 dev main0 nodad
ip route add default via 192.0.2.1 dev main0
ip -6 route add default via fd00::1 dev main0
for family in -4 -6; do
 ip $family rule add pref 30500 lookup 30001
 ip $family rule add pref 30501 lookup 30002
 ip $family route add unreachable default metric 42760 proto 186 table 30001
 ip $family route add unreachable default metric 42760 proto 186 table 30002
done
` + strings.Join(primaryFallbackCommands(false, false), "\n") + `
ip route get 8.8.8.8 | grep -q 'dev main0'
ip -6 route get 2001:db8::8 | grep -q 'dev main0'
ip route add unreachable 203.0.113.0/24 metric 42760 proto 186 table 30001
ip route add 203.0.113.0/24 via 10.0.0.1 dev biz0 metric 10 proto 186 table 30001
ip route get 203.0.113.10 | grep -q 'dev biz0'
ip route add unreachable 203.0.113.128/25 metric 42760 proto 186 table 30001
if ip route get 203.0.113.130; then exit 21; fi
ip route del 203.0.113.0/24 via 10.0.0.1 dev biz0 metric 10 table 30001
if ip route get 203.0.113.10; then exit 22; fi
ip route get 8.8.8.8 | grep -q 'dev main0'
ip route replace unreachable default metric 42760 proto 186 table 30001
ip route add default via 10.0.0.1 dev biz0 metric 10 proto 186 table 30001
` + strings.Join(primaryFallbackCommands(true, false), "\n") + `
ip route get 9.9.9.9 | grep -q 'dev biz0'
ip route del default via 10.0.0.1 dev biz0 metric 10 table 30001
if ip route get 9.9.9.9; then exit 23; fi
ip -6 route get 2001:db8::9 | grep -q 'dev main0'
`
	command := exec.Command("unshare", "--user", "--map-root-user", "--net", "--mount", "--", "/bin/sh", "-c", script)
	if out, err := command.CombinedOutput(); err != nil {
		t.Fatalf("kernel policy routing did not preserve primary fallback and selected-route isolation: %v\n%s", err, out)
	}
}

func TestFallbackEvidenceRequiresTheManagedThrowRoute(t *testing.T) {
	for _, table := range []int{businessTable, managedGuardTable} {
		good := object{"table": fmt.Sprint(table), "type": "throw", "dst": "default", "metric": 42760}
		if !hasPrimaryFallback([]object{good}, table) {
			t.Fatal("confirmed primary fallback rejected")
		}
		for _, change := range []object{{"table": 254}, {"type": "unreachable"}, {"dst": "203.0.113.0/24"}, {"metric": 10}} {
			bad := object{}
			for key, value := range good {
				bad[key] = value
			}
			for key, value := range change {
				bad[key] = value
			}
			if hasPrimaryFallback([]object{bad}, table) {
				t.Fatalf("unrelated or fail-closed route accepted as fallback: %v", bad)
			}
		}
	}
}

func portRouting() (assignedRouting, *record) {
	ten, twenty := uint16(10), uint16(20)
	r := &record{NICs: map[string]*attachment{
		"home": {ID: "home", Slot: 1},
		"ntt":  {ID: "ntt", Slot: 2},
	}}
	routing := assignedRouting{
		Priorities: []assignedPriority{{InterfaceID: "home", IPv4: &ten}, {InterfaceID: "ntt", IPv4: &twenty}},
		Routes: []assignedRoute{
			{Destination: "0.0.0.0/0", InterfaceID: "home"},
			{Destination: "0.0.0.0/0", InterfaceID: "ntt", Service: &assignedService{"tcp", 1, 1024}},
			{Destination: "0.0.0.0/0", InterfaceID: "ntt", Service: &assignedService{"tcp", 22, 22}},
			{Destination: "192.168.10.0/24", InterfaceID: "home"},
		},
	}
	return routing, r
}

func TestPortRoutesFollowStoreSelectionOrder(t *testing.T) {
	routing, r := portRouting()
	rules, err := serviceRules(routing, r)
	if err != nil {
		t.Fatal(err)
	}
	got := []string{}
	for _, rule := range rules {
		got = append(got, rule.selector())
	}
	want := []string{
		"pref 30200 from 'all' to '192.168.10.0/24' lookup 30201",
		"pref 30201 from 'all' to 'all' ipproto 6 dport 22-22 lookup 30202",
		"pref 30202 from 'all' to 'all' ipproto 6 dport 1-1024 lookup 30202",
	}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Fatalf("port selectors out of selection order:\n%s", strings.Join(got, "\n"))
	}
	// The whole-prefix default after the last port rule stays in the business table.
	routing.Routes = routing.Routes[:1]
	if rules, _ := serviceRules(routing, r); len(rules) != 0 {
		t.Fatalf("plain routes must not create selectors: %v", rules)
	}
}

func TestPortRuleBandIsDiffedExactly(t *testing.T) {
	routing, r := portRouting()
	rules, _ := serviceRules(routing, r)
	current := []object{
		{"priority": 30201, "src": "all", "ipproto": "tcp", "dport": 22, "dport_mask": "0xffff", "table": "30202"},
		{"priority": 30210, "src": "all", "ipproto": "udp", "dport_start": 53, "dport_end": 53, "table": "30201"},
	}
	adds, deletes := serviceRuleChanges(current, nil, rules)
	if len(adds) != 2 || !strings.Contains(adds[0], "to '192.168.10.0/24' lookup 30201") || !strings.Contains(adds[1], "dport 1-1024") {
		t.Fatalf("unexpected additions: %v", adds)
	}
	if len(deletes) != 1 || deletes[0] != "ip -4 rule del pref 30210 from 'all' to 'all' ipproto 17 dport 53-53 lookup 30201" {
		t.Fatalf("stale band selector not removed exactly: %v", deletes)
	}
	// Without /etc/protocols (minimal Guests) iproute2 prints "ipproto-N"; a host
	// prefix has no dstlen. Both forms must still match, or the band never settles.
	numeric := []object{
		{"priority": 30200, "src": "all", "dst": "192.168.10.0", "dstlen": 24, "table": "30201"},
		{"priority": 30201, "src": "all", "ipproto": "ipproto-6", "dport": 22, "table": "30202"},
		{"priority": 30202, "src": "all", "ipproto": "6", "dport_start": 1, "dport_end": 1024, "table": "30202"},
	}
	if adds, deletes := serviceRuleChanges(numeric, nil, rules); len(adds) != 0 || len(deletes) != 0 {
		t.Fatalf("numeric protocol readback does not settle: %v %v", adds, deletes)
	}
	host := policyRule{"-4", 30200, "all", "203.0.113.11/32", 30201, "tcp", 9090, 9090}
	if !host.matches(object{"priority": 30200, "src": "all", "dst": "203.0.113.11", "ipproto": "ipproto-6", "dport": 9090, "table": "30201"}) {
		t.Fatal("host prefix without dstlen must match its /32 selector")
	}
	// Band selectors are reconciled by diff, never reported as foreign conflicts.
	if _, err := inspectRules(current, nil, "-4"); err != nil {
		t.Fatalf("band rule treated as a conflict: %v", err)
	}
}

func TestPortRoutesSelectNICTablesInARealKernel(t *testing.T) {
	if os.Getenv("BROCADE_TEST_NETNS") != "1" {
		t.Skip("requires an explicitly enabled disposable network namespace")
	}
	routing, r := portRouting()
	rules, err := serviceRules(routing, r)
	if err != nil {
		t.Fatal(err)
	}
	adds, _ := serviceRuleChanges(nil, nil, rules)
	// Hide /etc/protocols as minimal Guest images do: iproute2 then rejects protocol
	// names and prints "ipproto-N", which the generated rules and the diff must handle.
	script := `set -eu
mount --bind /dev/null /etc/protocols
ip link set lo up
for dev in home0 ntt0; do ip link add $dev type dummy; ip link set $dev up; done
ip address add 10.1.0.2/24 dev home0
ip address add 10.2.0.2/24 dev ntt0
ip route add default via 10.1.0.1 dev home0 table 30201
ip route add default via 10.2.0.1 dev ntt0 table 30202
ip route add default via 10.1.0.1 dev home0 table 30001
ip route add 192.168.10.0/24 via 10.1.0.1 dev home0 table 30001
ip rule add pref 30500 lookup 30001
` + strings.Join(adds, "\n") + `
ip route get 203.0.113.9 ipproto 6 dport 22 | grep -q 'dev ntt0'
ip route get 203.0.113.9 ipproto 6 dport 443 | grep -q 'dev ntt0'
ip route get 203.0.113.9 ipproto 6 dport 8443 | grep -q 'dev home0'
ip route get 203.0.113.9 ipproto 17 dport 22 | grep -q 'dev home0'
ip route get 192.168.10.5 ipproto 6 dport 22 | grep -q 'dev home0'
ip -j -4 rule show
`
	command := exec.Command("unshare", "--user", "--map-root-user", "--net", "--mount", "--", "/bin/sh", "-c", script)
	out, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("port-selected egress did not follow the Store's selection: %v\n%s", err, out)
	}
	var actual []object
	if err := json.Unmarshal(out, &actual); err != nil {
		t.Fatalf("rule readback is not JSON: %v\n%s", err, out)
	}
	if adds, deletes := serviceRuleChanges(actual, nil, rules); len(adds) != 0 || len(deletes) != 0 {
		t.Fatalf("applied band does not settle on readback: %v %v\n%s", adds, deletes, out)
	}
}
