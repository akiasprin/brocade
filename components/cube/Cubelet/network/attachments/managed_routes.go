// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"sort"
	"strings"
)

const managedBusinessPriority = 30500
const managedGuardTable = 30002

// Port-selected egress rules live in their own priority band, after the contact and
// per-NIC source selectors and before the business table.
const serviceBandStart = 30200
const serviceBandEnd = 30500

type assignedPriority struct {
	InterfaceID string  `json:"interface_id"`
	IPv4        *uint16 `json:"ipv4"`
	IPv6        *uint16 `json:"ipv6"`
}
type assignedService struct {
	Protocol  string `json:"protocol"`
	PortStart uint16 `json:"port_start"`
	PortEnd   uint16 `json:"port_end"`
}
type assignedRoute struct {
	Destination string           `json:"destination"`
	InterfaceID string           `json:"interface_id"`
	Service     *assignedService `json:"service,omitempty"`
}
type assignedRouting struct {
	Revision   uint64             `json:"revision"`
	Priorities []assignedPriority `json:"priorities"`
	Routes     []assignedRoute    `json:"routes"`
	DNS        []string           `json:"dns"`
}
type chosenRoute struct {
	prefix   netip.Prefix
	id       string
	priority uint16
}

func selectedRoutes(raw object, r *record) (assignedRouting, []chosenRoute, error) {
	data, err := json.Marshal(raw)
	if err != nil {
		return assignedRouting{}, nil, err
	}
	var routing assignedRouting
	if json.Unmarshal(data, &routing) != nil || len(routing.Routes) > 256 || len(routing.Priorities) > 16 || len(routing.DNS) > 4 {
		return routing, nil, errors.New("invalid managed container route configuration")
	}
	priorities := map[string]assignedPriority{}
	for _, priority := range routing.Priorities {
		if r.NICs[priority.InterfaceID] == nil || priorities[priority.InterfaceID].InterfaceID != "" {
			return routing, nil, errors.New("priority references an unknown or repeated NIC")
		}
		priorities[priority.InterfaceID] = priority
	}
	winners := map[netip.Prefix]chosenRoute{}
	for _, route := range routing.Routes {
		prefix, err := netip.ParsePrefix(route.Destination)
		nic := r.NICs[route.InterfaceID]
		priority := priorities[route.InterfaceID]
		if err != nil || prefix != prefix.Masked() || nic == nil || nic.Allocated == nil {
			return routing, nil, errors.New("route references an invalid prefix or unallocated NIC")
		}
		selected := priority.IPv6
		if prefix.Addr().Is4() {
			selected = priority.IPv4
			if nic.IPv4 == "" {
				return routing, nil, errors.New("route requires unallocated IPv4")
			}
		} else if nic.IPv6 == "" {
			return routing, nil, errors.New("route requires unallocated IPv6")
		}
		if selected == nil {
			return routing, nil, errors.New("route is missing its address-family priority")
		}
		if service := route.Service; service != nil {
			if service.Protocol != "tcp" && service.Protocol != "udp" || service.PortStart == 0 || service.PortEnd < service.PortStart {
				return routing, nil, errors.New("invalid port-selected route")
			}
			// Port routes are policy selectors (serviceRules), not business table routes.
			continue
		}
		if previous, ok := winners[prefix]; ok {
			if previous.priority == *selected && previous.id != route.InterfaceID {
				return routing, nil, errors.New("same prefix and priority select different NICs")
			}
			if previous.priority <= *selected {
				continue
			}
		}
		winners[prefix] = chosenRoute{prefix, route.InterfaceID, *selected}
	}
	routes := make([]chosenRoute, 0, len(winners))
	for _, route := range winners {
		routes = append(routes, route)
	}
	sort.Slice(routes, func(i, j int) bool {
		if routes[i].prefix.Addr().Is4() != routes[j].prefix.Addr().Is4() {
			return routes[i].prefix.Addr().Is4()
		}
		if routes[i].prefix.Bits() != routes[j].prefix.Bits() {
			return routes[i].prefix.Bits() > routes[j].prefix.Bits()
		}
		return routes[i].prefix.String() < routes[j].prefix.String()
	})
	return routing, routes, nil
}

// serviceRules mirrors the Store's route selection for port-selected egress: a more
// specific prefix first, port rules before whole-prefix rules at the same prefix, then
// NIC priority, then the narrower range. Every route ranked up to the last port rule
// becomes a selector in that order; the rest is the business table's longest match.
func serviceRules(routing assignedRouting, r *record) ([]policyRule, error) {
	type ranked struct {
		prefix   netip.Prefix
		route    assignedRoute
		priority uint16
		width    uint32
	}
	priorities := map[string]assignedPriority{}
	for _, priority := range routing.Priorities {
		priorities[priority.InterfaceID] = priority
	}
	out := []policyRule{}
	for _, v4 := range []bool{true, false} {
		list := []ranked{}
		for _, route := range routing.Routes {
			prefix, err := netip.ParsePrefix(route.Destination)
			if err != nil || prefix.Addr().Is4() != v4 {
				continue
			}
			selected := priorities[route.InterfaceID].IPv6
			if v4 {
				selected = priorities[route.InterfaceID].IPv4
			}
			if selected == nil {
				continue
			}
			width := uint32(1 << 31)
			if route.Service != nil {
				width = uint32(route.Service.PortEnd) - uint32(route.Service.PortStart)
			}
			list = append(list, ranked{prefix, route, *selected, width})
		}
		sort.SliceStable(list, func(i, j int) bool {
			a, b := list[i], list[j]
			if a.prefix.Bits() != b.prefix.Bits() {
				return a.prefix.Bits() > b.prefix.Bits()
			}
			if (a.route.Service == nil) != (b.route.Service == nil) {
				return a.route.Service != nil
			}
			if a.priority != b.priority {
				return a.priority < b.priority
			}
			if a.width != b.width {
				return a.width < b.width
			}
			if a.prefix != b.prefix {
				return a.prefix.String() < b.prefix.String()
			}
			return a.route.InterfaceID < b.route.InterfaceID
		})
		last := -1
		for index, item := range list {
			if item.route.Service != nil {
				last = index
			}
		}
		family := "-6"
		if v4 {
			family = "-4"
		}
		for _, item := range list[:last+1] {
			a := r.NICs[item.route.InterfaceID]
			if a == nil {
				return nil, errors.New("port-selected route references an unknown NIC")
			}
			dest := item.prefix.String()
			if item.prefix.Bits() == 0 {
				dest = "all"
			}
			rule := policyRule{family, serviceBandStart + len(out), "all", dest, 30200 + a.Slot, "", 0, 0}
			if service := item.route.Service; service != nil {
				rule.ipproto, rule.portStart, rule.portEnd = service.Protocol, service.PortStart, service.PortEnd
			}
			if rule.priority >= serviceBandEnd {
				return nil, errors.New("too many port-selected routes")
			}
			out = append(out, rule)
		}
	}
	return out, nil
}

// serviceRuleChanges diffs the band exactly: missing selectors are added first, stale
// ones removed after, so a changing selection never falls back to the business table.
func serviceRuleChanges(actual4, actual6 []object, desired []policyRule) (adds, deletes []string) {
	for _, group := range []struct {
		family string
		actual []object
	}{{"-4", actual4}, {"-6", actual6}} {
		for _, rule := range desired {
			if rule.family != group.family {
				continue
			}
			found := false
			for _, existing := range group.actual {
				if rule.matches(existing) {
					found = true
					break
				}
			}
			if !found {
				adds = append(adds, fmt.Sprintf("ip %s rule add %s", group.family, rule.selector()))
			}
		}
		for _, existing := range group.actual {
			priority := number(existing, "priority")
			if priority < serviceBandStart || priority >= serviceBandEnd {
				continue
			}
			wanted := false
			for _, rule := range desired {
				if rule.family == group.family && rule.matches(existing) {
					wanted = true
					break
				}
			}
			if !wanted {
				deletes = append(deletes, fmt.Sprintf("ip %s rule del %s", group.family, observedRule(existing, group.family).selector()))
			}
		}
	}
	return adds, deletes
}

func familyOf(ip netip.Addr) string {
	if ip.Is4() {
		return "-4"
	}
	return "-6"
}
func sourcePriority(a *attachment) int { return 30100 + a.Slot*2 }
func routeDestination(route object, v4 bool) (netip.Prefix, error) {
	dst := str(route, "dst")
	if dst == "default" || dst == "" {
		if v4 {
			dst = "0.0.0.0/0"
		} else {
			dst = "::/0"
		}
	}
	if !strings.Contains(dst, "/") {
		ip, err := netip.ParseAddr(dst)
		if err != nil {
			return netip.Prefix{}, err
		}
		return netip.PrefixFrom(ip, ip.BitLen()), nil
	}
	return netip.ParsePrefix(dst)
}
func hasTerminal(routes []object, prefix netip.Prefix) bool {
	for _, route := range routes {
		actual, err := routeDestination(route, prefix.Addr().Is4())
		if err == nil && actual == prefix && str(route, "type") == "unreachable" && number(route, "metric") == 42760 {
			return true
		}
	}
	return false
}

// A completed business table falls through to the original primary route only
// for unmatched traffic. Selected prefixes retain their independent terminal
// routes, including a selected business default route.
func primaryFallbackCommands(default4, default6 bool) []string {
	commands := []string{}
	for _, family := range []struct {
		name       string
		hasDefault bool
	}{{"-4", default4}, {"-6", default6}} {
		if !family.hasDefault {
			commands = append(commands, fmt.Sprintf("ip %s route replace throw default metric 42760 proto 186 table %d", family.name, businessTable))
		}
		commands = append(commands, fmt.Sprintf("ip %s route replace throw default metric 42760 proto 186 table %d", family.name, managedGuardTable))
	}
	return commands
}

func hasPrimaryFallback(routes []object, table int) bool {
	for _, route := range routes {
		if routeTable(route) == uint64(table) && str(route, "type") == "throw" &&
			(str(route, "dst") == "default" || str(route, "dst") == "") && number(route, "metric") == 42760 {
			return true
		}
	}
	return false
}
func (s *server) configureAllocatedGuest(r *record, d object, observed []any, state guestNetworkState) (object, error) {
	routing, routes, err := selectedRoutes(obj(obj(d, "spec"), "routing"), r)
	if err != nil {
		return nil, err
	}
	devices := map[string]string{}
	var contact object
	for _, raw := range observed {
		nic := raw.(map[string]any)
		devices[str(nic, "id")] = str(nic, "device")
		if str(nic, "id") == r.ContactID {
			contact = nic
		}
	}
	if contact == nil || !interfaceName(devices[r.ContactID]) {
		return nil, errors.New("original contact NIC is missing")
	}
	commands := []string{"set -eu"}
	rules := []policyRule{}
	wanted := map[string]bool{r.ContactID: true}
	for _, raw := range array(obj(d, "spec"), "interfaces") {
		wanted[str(raw.(map[string]any), "id")] = true
	}
	// An independent terminal rule remains valid while the business table is rebuilt.
	for _, family := range []string{"-4", "-6"} {
		commands = append(commands, fmt.Sprintf("ip %s route replace unreachable default metric 42760 proto 186 table %d", family, managedGuardTable))
		rules = append(rules, policyRule{family, managedBusinessPriority + 1, "all", "all", managedGuardTable, "", 0, 0}, policyRule{family, managedBusinessPriority, "all", "all", businessTable, "", 0, 0})
	}
	for index, raw := range array(contact, "addresses") {
		prefix, err := netip.ParsePrefix(raw.(string))
		if err != nil {
			return nil, err
		}
		family := familyOf(prefix.Addr())
		rules = append(rules, policyRule{family, 30010 + index, prefix.Addr().String(), "all", contactTable, "", 0, 0})
		commands = append(commands, fmt.Sprintf("ip %s route replace %s dev %s src %s table %d", family, shellWord(prefix.Masked().String()), shellWord(devices[r.ContactID]), shellWord(prefix.Addr().String()), contactTable))
	}
	contactDefault := false
	for _, group := range []struct {
		family string
		routes []object
	}{{"-4", state.Contact4}, {"-6", state.Contact6}} {
		for _, route := range group.routes {
			if routeTable(route) != 254 || str(route, "dst") != "default" || str(route, "dev") != devices[r.ContactID] {
				continue
			}
			via := ""
			if gateway := str(route, "gateway"); gateway != "" {
				if _, err := netip.ParseAddr(gateway); err != nil {
					return nil, err
				}
				via = " via " + shellWord(gateway)
			}
			commands = append(commands, fmt.Sprintf("ip %s route replace default%s dev %s table %d", group.family, via, shellWord(devices[r.ContactID]), contactTable))
			if group.family == "-4" {
				contactDefault = true
			}
		}
	}
	if !contactDefault {
		return nil, errors.New("original contact default route is unavailable")
	}
	for _, id := range sortedAttachments(r) {
		a := r.NICs[id]
		if !wanted[id] {
			continue
		}
		device := devices[id]
		if a.Allocated == nil || !interfaceName(device) {
			return nil, errors.New("allocated Guest NIC is not observed")
		}
		for _, dev := range state.Addresses {
			if str(dev, "ifname") != device {
				continue
			}
			for _, raw := range array(dev, "addr_info") {
				address := raw.(map[string]any)
				ip, e := netip.ParseAddr(str(address, "local"))
				if e != nil {
					return nil, e
				}
				if ip.Is6() && ip.IsLinkLocalUnicast() {
					continue
				}
				prefix := netip.PrefixFrom(ip, int(number(address, "prefixlen"))).String()
				if prefix != a.IPv4 && prefix != a.IPv6 {
					commands = append(commands, fmt.Sprintf("ip %s address del %s dev %s", familyOf(ip), shellWord(prefix), shellWord(device)))
				}
			}
		}
		commands = append(commands, fmt.Sprintf("ip link set dev %s mtu %d up", shellWord(device), a.Allocated.MTU))
		for _, address := range []string{a.IPv4, a.IPv6} {
			if address == "" {
				continue
			}
			prefix, err := netip.ParsePrefix(address)
			if err != nil {
				return nil, err
			}
			family := familyOf(prefix.Addr())
			source := s.sourceFor(a)
			gateway := source.IPv4Gateway
			extra := ""
			if family == "-6" {
				gateway = source.IPv6Gateway
				extra = " nodad"
				commands = append(commands, "printf 0 > /proc/sys/net/ipv6/conf/"+device+"/accept_ra")
			}
			rules = append(rules, policyRule{family, sourcePriority(a), prefix.Addr().String(), "all", 30200 + a.Slot, "", 0, 0}, policyRule{family, sourcePriority(a) + 1, prefix.Addr().String(), "all", managedGuardTable, "", 0, 0})
			commands = append(commands, fmt.Sprintf("ip %s address replace %s dev %s%s", family, shellWord(address), shellWord(device), extra), fmt.Sprintf("ip %s route replace unreachable default metric 42760 proto 186 table %d", family, 30200+a.Slot), fmt.Sprintf("ip %s route replace default via %s dev %s onlink src %s metric 10 proto 186 table %d", family, shellWord(gateway), shellWord(device), shellWord(prefix.Addr().String()), 30200+a.Slot))
		}
	}
	// Remove only selectors derived from durable old assignments. Unknown policy
	// selectors remain a conflict, never permission to flush Guest configuration.
	for _, a := range r.NICs {
		stale := []string{a.PreviousIPv4, a.PreviousIPv6}
		if !wanted[a.ID] {
			stale = append(stale, a.IPv4, a.IPv6)
		}
		for _, address := range stale {
			if address == "" {
				continue
			}
			prefix, err := netip.ParsePrefix(address)
			if err != nil {
				return nil, err
			}
			family := familyOf(prefix.Addr())
			actual := &state.Rules4
			if family == "-6" {
				actual = &state.Rules6
			}
			selectors := []policyRule{{family, sourcePriority(a), prefix.Addr().String(), "all", 30200 + a.Slot, "", 0, 0}, {family, sourcePriority(a) + 1, prefix.Addr().String(), "all", managedGuardTable, "", 0, 0}}
			keep := []object{}
			for _, existing := range *actual {
				removed := false
				for _, rule := range selectors {
					if rule.matches(existing) {
						commands = append(commands, fmt.Sprintf("ip %s rule del pref %d from %s lookup %d", family, rule.priority, shellWord(rule.src), rule.table))
						removed = true
						break
					}
				}
				if !removed {
					keep = append(keep, existing)
				}
			}
			*actual = keep
			if device := devices[a.ID]; interfaceName(device) {
				commands = append(commands, fmt.Sprintf("if ip %s -o address show dev %s | awk '{print $4}' | /bin/grep -Fxq %s; then ip %s address del %s dev %s; fi", family, shellWord(device), shellWord(address), family, shellWord(address), shellWord(device)))
			}
		}
	}
	// Install terminal selectors before route replacements can remove a live path.
	adds := []string{}
	for _, group := range []struct {
		family string
		actual []object
	}{{"-4", state.Rules4}, {"-6", state.Rules6}} {
		newRules, err := inspectRules(group.actual, rules, group.family)
		if err != nil {
			return nil, err
		}
		adds = append(adds, newRules...)
	}
	sort.SliceStable(adds, func(i, j int) bool {
		return strings.Contains(adds[i], fmt.Sprintf("lookup %d", managedGuardTable)) && !strings.Contains(adds[j], fmt.Sprintf("lookup %d", managedGuardTable))
	})
	services, err := serviceRules(routing, r)
	if err != nil {
		return nil, err
	}
	serviceAdds, serviceDeletes := serviceRuleChanges(state.Rules4, state.Rules6, services)
	adds = append(adds, serviceAdds...)
	rules = append(rules, services...)
	commands = append(commands[:1], append(append([]string{fmt.Sprintf("ip -4 route replace unreachable default metric 42760 proto 186 table %d", managedGuardTable), fmt.Sprintf("ip -6 route replace unreachable default metric 42760 proto 186 table %d", managedGuardTable)}, adds...), commands[1:]...)...)
	for _, family := range []string{"-4", "-6"} {
		// iproute2 rejects flushing a table that has never had a route. Seed
		// it first; the independent guard table protects traffic during the flush.
		terminal := fmt.Sprintf("ip %s route replace unreachable default metric 42760 proto 186 table %d", family, businessTable)
		commands = append(commands, terminal, fmt.Sprintf("ip %s route flush table %d", family, businessTable), terminal)
	}
	// Every selected prefix has its own device-independent terminal route. A
	// disappearing more-specific NIC must not expose a less-specific live default.
	for _, route := range routes {
		commands = append(commands, fmt.Sprintf("ip %s route replace unreachable %s metric 42760 proto 186 table %d", familyOf(route.prefix.Addr()), shellWord(route.prefix.String()), businessTable))
	}
	default4, default6 := "", ""
	for _, route := range routes {
		a := r.NICs[route.id]
		source := s.sourceFor(a)
		gateway, address := source.IPv4Gateway, a.Allocated.IPv4
		if !route.prefix.Addr().Is4() {
			gateway, address = source.IPv6Gateway, a.Allocated.IPv6
		}
		commands = append(commands, fmt.Sprintf("ip %s route replace %s via %s dev %s onlink src %s metric 10 proto 186 table %d", familyOf(route.prefix.Addr()), shellWord(route.prefix.String()), shellWord(gateway), shellWord(devices[route.id]), shellWord(address), businessTable))
		if route.prefix.Bits() == 0 {
			if route.prefix.Addr().Is4() {
				default4 = route.id
			} else {
				default6 = route.id
			}
		}
	}
	if len(routing.DNS) > 0 {
		dns := []string{}
		for _, server := range routing.DNS {
			ip, err := netip.ParseAddr(server)
			if err != nil || ip.IsUnspecified() || ip.IsMulticast() || ip.IsLoopback() {
				return nil, errors.New("invalid container DNS server")
			}
			dns = append(dns, "nameserver "+ip.String())
		}
		commands = append(commands, "umask 077; mkdir -p /etc/brocade; if [ ! -e /etc/brocade/network-dns-original ]; then cp -pL /etc/resolv.conf /etc/brocade/network-dns-original; fi", "printf %s "+shellWord(strings.Join(dns, "\n")+"\n")+" > /etc/resolv.conf")
	} else {
		commands = append(commands, "if [ -f /etc/brocade/network-dns-original ]; then cat /etc/brocade/network-dns-original > /etc/resolv.conf; rm /etc/brocade/network-dns-original; fi")
	}
	commands = append(commands, primaryFallbackCommands(default4 != "", default6 != "")...)
	commands = append(commands, serviceDeletes...)
	if _, err = s.guest(r.Runtime, false, "/bin/sh", "-c", strings.Join(commands, "; ")); err != nil {
		return nil, err
	}
	after, err := s.guestNetwork(r, devices[r.ContactID])
	if err != nil {
		return nil, err
	}
	for _, family := range []struct {
		routes     []object
		hasDefault bool
	}{{after.Contact4, default4 != ""}, {after.Contact6, default6 != ""}} {
		if !hasPrimaryFallback(family.routes, managedGuardTable) || !family.hasDefault && !hasPrimaryFallback(family.routes, businessTable) {
			return nil, errors.New("unmatched traffic has no confirmed primary NAT fallback")
		}
	}
	for _, rule := range rules {
		actual := after.Rules4
		if rule.family == "-6" {
			actual = after.Rules6
		}
		found := false
		for _, existing := range actual {
			if rule.matches(existing) {
				found = true
				break
			}
		}
		if !found {
			return nil, errors.New("managed Guest routing selector not observed")
		}
	}
	for _, route := range routes {
		actual := after.Business4
		if !route.prefix.Addr().Is4() {
			actual = after.Business6
		}
		if !hasTerminal(actual, route.prefix) {
			return nil, errors.New("selected prefix lacks persistent fail-closed protection")
		}
		found := false
		for _, existing := range actual {
			prefix, e := routeDestination(existing, route.prefix.Addr().Is4())
			if e == nil && prefix == route.prefix && str(existing, "type") != "unreachable" && str(existing, "dev") == devices[route.id] && number(existing, "metric") == 10 {
				found = true
				break
			}
		}
		if !found {
			return nil, errors.New("selected Guest business route not observed")
		}
	}
	for _, id := range sortedAttachments(r) {
		a := r.NICs[id]
		if !wanted[id] {
			continue
		}
		for _, expected := range []string{a.IPv4, a.IPv6} {
			if expected == "" {
				continue
			}
			prefix, _ := netip.ParsePrefix(expected)
			found := false
			for _, dev := range after.Addresses {
				if !strings.EqualFold(str(dev, "address"), a.MAC) {
					continue
				}
				for _, raw := range array(dev, "addr_info") {
					address := raw.(map[string]any)
					if str(address, "local") == prefix.Addr().String() && number(address, "prefixlen") == uint64(prefix.Bits()) {
						found = true
					}
				}
			}
			if !found {
				return nil, errors.New("Store-assigned Guest address not observed")
			}
		}
		a.PreviousIPv4 = ""
		a.PreviousIPv6 = ""
	}
	result := object{"routing_revision": routing.Revision, "contact_route_preserved": true, "business_fallback_blocked": true, "default_ipv4_interface": nil, "default_ipv6_interface": nil}
	if default4 != "" {
		result["default_ipv4_interface"] = default4
	}
	if default6 != "" {
		result["default_ipv6_interface"] = default6
	}
	return result, nil
}
