// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
#ifndef __PRIMARY_CHAIN_H
#define __PRIMARY_CHAIN_H
#include "primary_chain_map.h"

struct chain_fragment {
    __u32 ifindex;
    __u32 version;
    __u64 generation;
    __u32 id;
    __u8 family;
    __u8 protocol;
    __u8 pad[2];
    __u8 destination[16];
};
struct chain_fragment_value { __u64 expires; __u32 mark; __u32 pad; };
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 8192);
    __type(key, struct chain_fragment);
    __type(value, struct chain_fragment_value);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} primary_chain_frag SEC(".maps");

static __always_inline bool primary_chain_prefix(const __u8 *address, const struct chain_route *r)
{
    if (r->bits > (r->family == 4 ? 32 : 128))
        return false;
#pragma unroll
    for (__u32 i = 0; i < 4; i++) {
        if (i * 32 >= r->bits)
            return true;
        __u32 bits = r->bits - i * 32;
        __u32 mask = bits >= 32 ? 0xffffffff : bpf_htonl(0xffffffff << (32 - bits));
        __u32 actual, expected;
        __builtin_memcpy(&actual, address + i * 4, 4);
        __builtin_memcpy(&expected, r->address + i * 4, 4);
        if ((actual & mask) != (expected & mask))
            return false;
    }
    return true;
}

/* A helper-bounded loop avoids multiplying verifier paths for every prefix/port
 * combination. The policy still supports all 256 routes in one atomic map value. */
struct primary_chain_lookup {
    const struct chain_routes *routes;
    const struct chain_fragment *packet;
    __u16 port;
    __u32 mark;
    bool selected;
};
static long primary_chain_select(__u32 i, void *opaque)
{
    struct primary_chain_lookup *lookup = opaque;
    if (i >= CHAIN_MAX_ROUTES || i >= lookup->routes->count)
        return 1;
    /* Keep the range constraint on the register used for map-pointer arithmetic.
     * Clang may otherwise re-zero-extend the original callback index after the check. */
    asm volatile("%0 &= 255" : "+r"(i));
    const struct chain_route *r = &lookup->routes->routes[i];
    if (r->family != lookup->packet->family || !primary_chain_prefix(lookup->packet->destination, r))
        return 0;
    if (r->protocol && (r->protocol != lookup->packet->protocol || lookup->port < r->port_start || lookup->port > r->port_end))
        return 0;
    lookup->mark = r->mark;
    lookup->selected = true;
    return 1;
}

/* -1 means this policy did not select an exit. A selected chain is terminal:
 * it either reaches its marked table, which has a persistent unreachable default,
 * or is dropped. It can never return to the ordinary Cube NAT path. */
static __noinline int primary_chain_packet(struct __sk_buff *skb, __u32 index,
                                          const struct mvm_meta *meta, bool ipv6)
{
    struct chain_routes *routes = bpf_map_lookup_elem(&primary_chain_routes, &index);
    if (!routes || routes->version != meta->version)
        return -1;
    struct ethhdr eth;
    if (bpf_skb_load_bytes(skb, 0, &eth, sizeof(eth)) ||
        __builtin_memcmp(eth.h_source, routes->mac, 6))
        return TC_ACT_SHOT;
    struct chain_fragment fragment = {.ifindex=index,.version=meta->version,.generation=routes->generation,.family=ipv6?6:4};
    __u16 port = 0;
    __u32 offset = sizeof(struct ethhdr), mark = 0;
    bool first = false, later = false;
    if (ipv6) {
        struct ipv6hdr ip;
        if (bpf_skb_load_bytes(skb, offset, &ip, sizeof(ip)) || ip.version != 6 ||
            !primary_chain_ipv6((__u8 *)&ip.saddr) ||
            __builtin_memcmp(((__u8 *)&ip.saddr) + 12, &meta->ip, 4))
            return TC_ACT_SHOT;
        __builtin_memcpy(fragment.destination, &ip.daddr, 16);
        fragment.protocol = ip.nexthdr;
        offset += sizeof(ip);
        if (ip.nexthdr == 44) {
            struct { __u8 next, reserved; __u16 flags; __u32 id; } h;
            if (bpf_skb_load_bytes(skb, offset, &h, sizeof(h)))
                return TC_ACT_SHOT;
            fragment.protocol = h.next;
            fragment.id = h.id;
            later = (bpf_ntohs(h.flags) & 0xfff8) != 0;
            first = !later && (bpf_ntohs(h.flags) & 1);
            offset += sizeof(h);
        }
    } else {
        struct iphdr ip;
        if (bpf_skb_load_bytes(skb, offset, &ip, sizeof(ip)) || ip.version != 4 ||
            ip.ihl < 5 || ip.saddr != mvm_inner_ip)
            return TC_ACT_SHOT;
        __builtin_memcpy(fragment.destination, &ip.daddr, 4);
        fragment.protocol = ip.protocol;
        fragment.id = ip.id;
        later = (ip.frag_off & IP_FRAG_OFF_MASK) != 0;
        first = !later && (ip.frag_off & IP_FLAG_MF);
        offset += ip.ihl * 4;
    }
    if (later) {
        struct chain_fragment_value *saved = bpf_map_lookup_elem(&primary_chain_frag, &fragment);
        if (!saved || saved->expires <= bpf_ktime_get_ns())
            return TC_ACT_SHOT;
        mark = saved->mark;
        /* The cache key includes both TAP incarnation and policy generation.
         * A revoked route cannot continue a fragment train under a new policy. */
        if (!mark) return TC_ACT_SHOT;
    } else {
        if (fragment.protocol == IPPROTO_TCP || fragment.protocol == IPPROTO_UDP) {
            __u16 ports[2];
            if (bpf_skb_load_bytes(skb, offset, ports, sizeof(ports)))
                return TC_ACT_SHOT;
            port = bpf_ntohs(ports[1]);
        }
        struct primary_chain_lookup lookup = {.routes=routes,.packet=&fragment,.port=port};
        bpf_loop(CHAIN_MAX_ROUTES, primary_chain_select, &lookup, 0);
        if (!lookup.selected) return -1;
        mark = lookup.mark;
        /* ICMP must not enter Xray TUN: its local echo responder is not an exit. */
        if (!mark || (fragment.protocol != IPPROTO_TCP && fragment.protocol != IPPROTO_UDP))
            return TC_ACT_SHOT;
        if (first) {
            struct chain_fragment_value value = {.expires=bpf_ktime_get_ns()+30000000000ULL,.mark=mark};
            if (bpf_map_update_elem(&primary_chain_frag, &fragment, &value, BPF_ANY))
                return TC_ACT_SHOT;
        }
    }
    if (!ipv6) {
        struct ethhdr *l2;
        struct iphdr *ip;
        if (pull_headers(skb,&l2,&ip) != TC_ACT_OK || snat(skb,ip,meta->ip))
            return TC_ACT_SHOT;
    }
    skb->mark = mark;
    return bpf_redirect(cubegw0_ifindex, BPF_F_INGRESS);
}
#endif
