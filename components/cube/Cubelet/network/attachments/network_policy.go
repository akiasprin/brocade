// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"strconv"
	"strings"
)

const contactTable = 30000
const businessTable = 30001
const businessPriority = 30100

type guestNetworkState struct {
	Addresses []object `json:"addresses"`
	Contact4  []object `json:"contact4"`
	Contact6  []object `json:"contact6"`
	Business4 []object `json:"business4"`
	Business6 []object `json:"business6"`
	Rules4    []object `json:"rules4"`
	Rules6    []object `json:"rules6"`
}

func (s *server) guestNetwork(r *record, device string) (guestNetworkState, error) {
	// iproute2 returns an empty array for an as-yet unallocated table on some
	// versions, and an error on others. Inspect all tables and filter in Go.
	script := `set -eu; printf '{"addresses":'; ip -j address show; printf ',"contact4":'; ip -j -4 route show table all; printf ',"contact6":'; ip -j -6 route show table all; printf ',"rules4":'; ip -j -4 rule show; printf ',"rules6":'; ip -j -6 rule show; printf '}'`
	data, e := s.guest(r.Runtime, true, "/bin/sh", "-c", script)
	if e != nil {
		return guestNetworkState{}, e
	}
	var state guestNetworkState
	if json.Unmarshal(data, &state) != nil {
		return state, errors.New("invalid Guest route inventory")
	}
	filter := func(routes []object, table int) []object {
		out := []object{}
		for _, route := range routes {
			t := routeTable(route)
			name := str(route, "table")
			if t == uint64(table) || table == 254 && (name == "main" || route["table"] == nil) {
				out = append(out, route)
			}
		}
		return out
	}
	state.Business4 = filter(state.Contact4, businessTable)
	state.Business6 = filter(state.Contact6, businessTable)
	// Include the original main-table contact default when installing its
	// dedicated reply table. Managed routes remain separately identifiable.
	return state, nil
}
func routeTable(route object) uint64 {
	switch str(route, "table") {
	case "main":
		return 254
	case "local":
		return 255
	case "default":
		return 253
	}
	if route["table"] == nil {
		return 254
	}
	if value, e := strconv.ParseUint(str(route, "table"), 10, 64); e == nil {
		return value
	}
	return number(route, "table")
}

type policyRule struct {
	family   string
	priority int
	src, dst string
	table    int
	// Port-selected egress: an ip rule protocol and destination port range.
	ipproto            string
	portStart, portEnd uint16
}

// selector is the exact ip rule selector, used for both add and delete.
func (rule policyRule) selector() string {
	selector := fmt.Sprintf("pref %d from %s to %s", rule.priority, shellWord(rule.src), shellWord(rule.dst))
	if rule.ipproto != "" {
		selector += fmt.Sprintf(" ipproto %s dport %d-%d", protocolArgument(rule.ipproto), rule.portStart, rule.portEnd)
	}
	return selector + fmt.Sprintf(" lookup %d", rule.table)
}

// protocolArgument is the numeric ip rule protocol. Minimal Guest images ship no
// /etc/protocols, where iproute2 rejects names such as "tcp".
func protocolArgument(name string) string {
	switch normalProtocol(name) {
	case "tcp":
		return "6"
	case "udp":
		return "17"
	}
	return strings.TrimPrefix(name, "ipproto-")
}

// normalProtocol maps what `ip -j rule show` prints (a name when /etc/protocols
// exists, "ipproto-N" without it) to the Store's protocol names.
func normalProtocol(value string) string {
	switch strings.TrimPrefix(value, "ipproto-") {
	case "tcp", "6":
		return "tcp"
	case "udp", "17":
		return "udp"
	}
	return value
}

// observedRule reads a rule of the port-selected band back from `ip -j rule show`.
func observedRule(actual object, family string) policyRule {
	dest := str(actual, "dst")
	if dest == "" {
		dest = "all"
	} else if length := number(actual, "dstlen"); length > 0 {
		dest = fmt.Sprintf("%s/%d", dest, length)
	}
	start, end := number(actual, "dport"), number(actual, "dport")
	if _, ok := actual["dport_start"]; ok {
		start, end = number(actual, "dport_start"), number(actual, "dport_end")
	}
	return policyRule{family, int(number(actual, "priority")), "all", dest, int(routeTable(actual)), normalProtocol(str(actual, "ipproto")), uint16(start), uint16(end)}
}

func (rule policyRule) matches(actual object) bool {
	source := str(actual, "src")
	if source == "" {
		source = "all"
	}
	dest := str(actual, "dst")
	if dest == "" {
		dest = "all"
	}
	if length := number(actual, "srclen"); length > 0 {
		source = fmt.Sprintf("%s/%d", source, length)
	}
	if length := number(actual, "dstlen"); length > 0 {
		dest = fmt.Sprintf("%s/%d", dest, length)
	}
	normal := func(value string) string {
		if p, e := netip.ParsePrefix(value); e == nil && p.Bits() == p.Addr().BitLen() {
			return p.Addr().String()
		}
		return value
	}
	if number(actual, "priority") != uint64(rule.priority) || routeTable(actual) != uint64(rule.table) || normal(source) != normal(rule.src) || normal(dest) != normal(rule.dst) {
		return false
	}
	start, end := number(actual, "dport"), number(actual, "dport")
	if _, ok := actual["dport_start"]; ok {
		start, end = number(actual, "dport_start"), number(actual, "dport_end")
	}
	if normalProtocol(str(actual, "ipproto")) != rule.ipproto || start != uint64(rule.portStart) || end != uint64(rule.portEnd) {
		return false
	}
	for key := range actual {
		switch key {
		case "priority", "src", "dst", "srclen", "dstlen", "table", "protocol":
		case "ipproto", "dport", "dport_start", "dport_end", "dport_mask":
			if rule.ipproto == "" {
				return false
			}
		default:
			return false
		}
	}
	return true
}
func inspectRules(actual []object, desired []policyRule, family string) ([]string, error) {
	commands := []string{}
	for _, existing := range actual {
		priority := number(existing, "priority")
		if priority == 0 || priority >= 32766 {
			continue
		}
		// The port-selected band is reconciled by exact diff (serviceRuleChanges).
		if priority >= serviceBandStart && priority < serviceBandEnd {
			continue
		}
		owned := false
		for _, rule := range desired {
			if rule.family == family && rule.matches(existing) {
				owned = true
				break
			}
		}
		// The generic rule is removed explicitly when the last business attachment
		// is removed. Anything else at a managed priority is a conflict, not ours.
		if !owned && (policyRule{family, businessPriority, "all", "all", businessTable, "", 0, 0}).matches(existing) {
			owned = true
		}
		if !owned {
			return nil, errors.New("Guest policy routing conflicts with managed contact/business isolation")
		}
	}
	for _, rule := range desired {
		if rule.family != family {
			continue
		}
		found := false
		for _, existing := range actual {
			if rule.matches(existing) {
				found = true
				break
			}
		}
		if !found {
			commands = append(commands, fmt.Sprintf("ip %s rule add %s", family, rule.selector()))
		}
	}
	return commands, nil
}
func (s *server) configureGuestNetwork(r *record, d object, observed []any) (object, error) {
	devices := map[string]string{}
	var contact object
	for _, raw := range observed {
		nic := raw.(map[string]any)
		devices[str(nic, "id")] = str(nic, "device")
		if str(nic, "id") == r.ContactID {
			contact = nic
		}
	}
	if contact == nil {
		return nil, errors.New("contact observation missing")
	}
	state, e := s.guestNetwork(r, devices[r.ContactID])
	if e != nil {
		return nil, e
	}
	return s.configureAllocatedGuest(r, d, observed, state)
}
