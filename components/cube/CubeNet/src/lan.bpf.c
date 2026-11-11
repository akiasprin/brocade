// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright (c) 2026 Brocade contributors.
#include <vmlinux.h>
#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

#define OK 0
#define DROP 2
#define IPV4 0x0800
#define ARP 0x0806
#define IPV6 0x86dd
#define MAX_ENDPOINTS 512
#define REPLY_TTL (300ULL * 1000000000ULL)

/* This ABI is independent of the existing IPv4 NAT maps. Network namespaces,
 * stable endpoint tokens and policy generations must never be inferred from a
 * recyclable ifindex or an address shared by multiple virtual networks. */
struct lan_endpoint {
    __u64 token;
    __u64 generation;
    __u8 mac[6];
    __u8 flags; /* bit 0: IPv4 off-LAN; bit 1: IPv6 off-LAN */
    __u8 active;
    __u8 ipv4[4];
    __u8 ipv6[16];
    __u8 linklocal[16];
};
struct lan_prefix {
    __u32 prefixlen;
    __u32 ifindex;
    __u64 generation;
    __u32 family;
    __u8 address[16];
};
struct lan_flow {
    __u64 token;
    __u32 family;
    __u8 protocol;
    __u8 pad[3];
    __u8 local[16];
    __u8 remote[16];
    __u16 local_port;
    __u16 remote_port;
};
struct lan_counter { __u64 token; __u64 drops; };
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENDPOINTS);
    __type(key, __u32);
    __type(value, struct lan_endpoint);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} cube_lan_ep_v1 SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 32768);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct lan_prefix);
    __type(value, __u8);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} cube_lan_pfx_v1 SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 8192);
    __type(key, struct lan_flow);
    __type(value, __u64);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} cube_lan_ct_v1 SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENDPOINTS);
    __type(key, __u32);
    __type(value, struct lan_counter);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} cube_lan_stat_v1 SEC(".maps");

static __always_inline int drop(__u32 index, __u64 token)
{
    struct lan_counter *c = bpf_map_lookup_elem(&cube_lan_stat_v1, &index);
    if (c && c->token == token)
        __sync_fetch_and_add(&c->drops, 1);
    return DROP;
}
static __always_inline bool equal16(const __u8 *a, const __u8 *b)
{
    return __builtin_memcmp(a, b, 16) == 0;
}
static __always_inline bool own6(struct lan_endpoint *e, const __u8 *a)
{
    return equal16(a, e->linklocal) || equal16(a, e->ipv6);
}
static __always_inline int ports(struct __sk_buff *skb, __u32 off,
                                struct lan_flow *flow, bool incoming)
{
    __u16 pair[2];
    if (flow->protocol == 6 || flow->protocol == 17) {
        if (bpf_skb_load_bytes(skb, off, pair, sizeof(pair)))
            return -1;
        flow->local_port = pair[incoming ? 1 : 0];
        flow->remote_port = pair[incoming ? 0 : 1];
        return 0;
    }
    if (flow->protocol == 1 || flow->protocol == 58) {
        __u8 h[8];
        if (bpf_skb_load_bytes(skb, off, h, sizeof(h)))
            return -1;
        /* Only echo identifiers create reply permission. ICMP errors use
         * explicit control handling, not an overly broad zero-port flow. */
        if (!((flow->protocol == 1 && (h[0] == 0 || h[0] == 8)) ||
              (flow->protocol == 58 && (h[0] == 128 || h[0] == 129))))
            return -1;
        __builtin_memcpy(&flow->local_port, h + 4, 2);
        flow->remote_port = flow->local_port;
        return 0;
    }
    return -1;
}

static __always_inline int packet(struct __sk_buff *skb, bool incoming)
{
    __u32 index = incoming ? skb->ifindex : skb->ingress_ifindex;
    struct lan_endpoint *ep = bpf_map_lookup_elem(&cube_lan_ep_v1, &index);
    if (!ep || !ep->active)
        return DROP;
    __u64 token = ep->token;
    struct ethhdr eth;
    if (bpf_skb_load_bytes(skb, 0, &eth, sizeof(eth)))
        return drop(index, token);
    if (!incoming && __builtin_memcmp(eth.h_source, ep->mac, 6))
        return drop(index, token);
    if (eth.h_proto == bpf_htons(ARP)) {
        __u8 arp[28];
        if (bpf_skb_load_bytes(skb, 14, arp, sizeof(arp)))
            return drop(index, token);
        if (arp[0] != 0 || arp[1] != 1 || arp[2] != 8 || arp[3] != 0 ||
            arp[4] != 6 || arp[5] != 4 || arp[6] != 0 || (arp[7] != 1 && arp[7] != 2))
            return drop(index, token);
        if (!incoming && (__builtin_memcmp(arp + 8, ep->mac, 6) ||
                          __builtin_memcmp(arp + 14, ep->ipv4, 4)))
            return drop(index, token);
        return OK;
    }
    struct lan_flow flow = {.token = token};
    struct lan_prefix prefix = {.ifindex = index, .generation = ep->generation};
    __u32 off;
    bool offlan;
    if (eth.h_proto == bpf_htons(IPV4)) {
        struct iphdr ip;
        /* IPv4 options include source routing. v1 deliberately rejects options
         * rather than classifying only the first hop of a source-routed path. */
        if (bpf_skb_load_bytes(skb, 14, &ip, sizeof(ip)) || ip.version != 4 || ip.ihl != 5)
            return drop(index, token);
        if (__builtin_memcmp(incoming ? &ip.daddr : &ip.saddr, ep->ipv4, 4))
            return drop(index, token);
        /* A port policy must not be bypassed with an unclassified fragment. */
        if (ip.frag_off & bpf_htons(0x3fff))
            return drop(index, token);
        flow.family = 4;
        flow.protocol = ip.protocol;
        __builtin_memcpy(flow.local, incoming ? &ip.daddr : &ip.saddr, 4);
        __builtin_memcpy(flow.remote, incoming ? &ip.saddr : &ip.daddr, 4);
        prefix.family = 4;
        prefix.prefixlen = 128 + 32;
        __builtin_memcpy(prefix.address, &ip.daddr, 4);
        off = 14 + (__u32)ip.ihl * 4;
        offlan = ep->flags & 1;
    } else if (eth.h_proto == bpf_htons(IPV6)) {
        struct ipv6hdr ip;
        if (bpf_skb_load_bytes(skb, 14, &ip, sizeof(ip)) || ip.version != 6)
            return drop(index, token);
        bool unspecified = equal16((__u8 *)&ip.saddr, (__u8[16]){});
        if (!incoming && !own6(ep, (__u8 *)&ip.saddr) && !unspecified)
            return drop(index, token);
        if (incoming && !own6(ep, (__u8 *)&ip.daddr) && ip.daddr.in6_u.u6_addr8[0] != 0xff)
            return drop(index, token);
        flow.family = 6;
        flow.protocol = ip.nexthdr;
        off = 54;
        /* Bounded parsing of hop-by-hop, routing and destination options.
         * Fragment/AH/ESP and overlong chains are explicit unsupported traffic. */
#pragma unroll
        for (int i = 0; i < 4; i++) {
            if (flow.protocol == 0 || flow.protocol == 43 || flow.protocol == 60) {
                __u8 ext[4];
                if (bpf_skb_load_bytes(skb, off, ext, sizeof(ext)))
                    return drop(index, token);
                if (flow.protocol == 43 && ext[3] != 0)
                    return drop(index, token);
                flow.protocol = ext[0];
                off += ((__u32)ext[1] + 1) * 8;
            }
        }
        if (flow.protocol == 0 || flow.protocol == 43 || flow.protocol == 60 ||
            flow.protocol == 44 || flow.protocol == 50 || flow.protocol == 51)
            return drop(index, token);
        if (flow.protocol == 58) {
            __u8 icmp[8];
            if (bpf_skb_load_bytes(skb, off, icmp, sizeof(icmp)))
                return drop(index, token);
            if (icmp[0] >= 133 && icmp[0] <= 137) {
                if (ip.hop_limit != 255 || icmp[1] != 0)
                    return drop(index, token);
                if (!incoming && (icmp[0] == 134 || icmp[0] == 137))
                    return drop(index, token);
                if (!incoming && (icmp[0] == 135 || icmp[0] == 136)) {
                    __u8 target[16];
                    if (bpf_skb_load_bytes(skb, off + 8, target, 16))
                        return drop(index, token);
                    if ((icmp[0] == 136 || unspecified) && !own6(ep, target))
                        return drop(index, token);
                }
                if (unspecified && icmp[0] != 135)
                    return drop(index, token);
                return OK;
            }
            /* Deliver network errors, notably Packet Too Big, to the Guest.
             * They do not populate the return-flow permission map. */
            if (incoming && icmp[0] >= 1 && icmp[0] <= 4)
                return OK;
        }
        if (unspecified)
            return drop(index, token);
        __builtin_memcpy(flow.local, incoming ? &ip.daddr : &ip.saddr, 16);
        __builtin_memcpy(flow.remote, incoming ? &ip.saddr : &ip.daddr, 16);
        prefix.family = 6;
        prefix.prefixlen = 128 + 128;
        __builtin_memcpy(prefix.address, &ip.daddr, 16);
        offlan = ep->flags & 2;
    } else {
        return drop(index, token);
    }
    int parsed = ports(skb, off, &flow, incoming);
    __u64 now = bpf_ktime_get_ns();
    if (incoming) {
        if (!parsed)
            bpf_map_update_elem(&cube_lan_ct_v1, &flow, &now, BPF_ANY);
        return OK;
    }
    if (offlan || bpf_map_lookup_elem(&cube_lan_pfx_v1, &prefix))
        return OK;
    if (!parsed) {
        __u64 *seen = bpf_map_lookup_elem(&cube_lan_ct_v1, &flow);
        if (seen && now - *seen < REPLY_TTL) {
            *seen = now;
            return OK;
        }
    }
    return drop(index, token);
}
SEC("tc") int cubevs_lan_in(struct __sk_buff *skb) { return packet(skb, false); }
SEC("tc") int cubevs_lan_out(struct __sk_buff *skb) { return packet(skb, true); }
char __license[] SEC("license") = "Dual BSD/GPL";
