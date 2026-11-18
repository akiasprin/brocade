// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
#ifndef __PRIMARY_CHAIN_REPLY_H
#define __PRIMARY_CHAIN_REPLY_H
#include "primary_chain_map.h"

static __always_inline int primary_chain_reply6(struct __sk_buff *skb)
{
    struct ipv6hdr ip;
    __u32 destination;
    if (bpf_skb_load_bytes(skb,sizeof(struct ethhdr),&ip,sizeof(ip)) ||
        ip.version != 6 || !primary_chain_ipv6((__u8 *)&ip.daddr))
        return TC_ACT_SHOT;
    __builtin_memcpy(&destination,((__u8 *)&ip.daddr)+12,4);
    __u32 *index = bpf_map_lookup_elem(&mvmip_to_ifindex,&destination);
    if (!index) return TC_ACT_SHOT;
    struct mvm_meta *meta = bpf_map_lookup_elem(&ifindex_to_mvmmeta,index);
    struct chain_routes *routes = bpf_map_lookup_elem(&primary_chain_routes,index);
    if (!meta || !routes || meta->version != routes->version || !routes->count)
        return TC_ACT_SHOT;
    __u32 tap = *index;
    if (bpf_skb_store_bytes(skb,__builtin_offsetof(struct ethhdr,h_dest),routes->mac,6,0))
        return TC_ACT_SHOT;
    /* The original primary gateway MAC is shared with cube-dev. */
    __u8 mac[6];
    __u32 lo = cubegw0_macaddr_p1;
    __u16 hi = cubegw0_macaddr_p2;
    __builtin_memcpy(mac,&lo,4);
    __builtin_memcpy(mac+4,&hi,2);
    if (bpf_skb_store_bytes(skb,__builtin_offsetof(struct ethhdr,h_source),mac,6,0))
        return TC_ACT_SHOT;
    return bpf_redirect(tap,0);
}
#endif
