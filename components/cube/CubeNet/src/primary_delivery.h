// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
/* Copyright (c) 2026 Cube Authors */
#ifndef __PRIMARY_DELIVERY_H
#define __PRIMARY_DELIVERY_H

#include "map.h"

#define PRIMARY_DELIVERY_TTL (120ULL * 1000 * 1000 * 1000)

/* Only a host-originated packet may create a reply permit. The TAP generation
 * prevents a reused ifindex from inheriting another sandbox's permits. */
static __always_inline bool primary_delivery_ports(struct __sk_buff *skb,
		struct primary_delivery_key *key, const struct iphdr *ip, bool reply)
{
	__u16 ports[2];
	struct icmphdr icmp;
	__u32 off = sizeof(struct ethhdr) + (ip->ihl * 4);

	if (ip->ihl < 5 || (ip->frag_off & IP_FRAG_OFF_MASK))
		return false;
	if (ip->protocol == IPPROTO_TCP || ip->protocol == IPPROTO_UDP) {
		if (bpf_skb_load_bytes(skb, off, ports, sizeof(ports)))
			return false;
		key->host_port = ports[reply ? 1 : 0];
		key->guest_port = ports[reply ? 0 : 1];
	} else if (ip->protocol == IPPROTO_ICMP) {
		if (bpf_skb_load_bytes(skb, off, &icmp, sizeof(icmp)))
			return false;
		if (icmp.type != (reply ? 0 : 8))
			return false;
		key->host_port = icmp.un.echo.id;
	}
	return true;
}

static __always_inline bool primary_delivery_permitted(struct primary_delivery_key *key)
{
	__u64 *expires = bpf_map_lookup_elem(&primary_delivery, key);

	return expires && *expires > bpf_ktime_get_ns();
}

static __always_inline void primary_delivery_record(struct __sk_buff *skb,
		__u32 tap, const struct iphdr *ip)
{
	struct primary_delivery_key key = {};
	struct mvm_meta *meta;
	__u64 expires;

	if (ip->saddr != cubegw0_ip)
		return;
	meta = bpf_map_lookup_elem(&ifindex_to_mvmmeta, &tap);
	if (!meta)
		return;
	key.ifindex = tap;
	key.version = meta->version;
	key.protocol = ip->protocol;
	expires = bpf_ktime_get_ns() + PRIMARY_DELIVERY_TTL;
	/* Noninitial fragments have no ports. They cannot create a new host
	 * service connection without an admitted first fragment. */
	key.fragment = 1;
	bpf_map_update_elem(&primary_delivery, &key, &expires, BPF_ANY);
	key.fragment = 0;
	if (primary_delivery_ports(skb, &key, ip, false))
		bpf_map_update_elem(&primary_delivery, &key, &expires, BPF_ANY);
}

/* Undo the Cube-local address normalization inside a related ICMP error as
 * well as in its outer header, so Linux conntrack can find the original flow. */
static __always_inline bool primary_delivery_icmp_error(struct __sk_buff *skb,
		struct primary_delivery_key *key, const struct iphdr *ip)
{
	struct iphdr quote;
	struct icmphdr icmp;
	__u16 ports[2], old_check, new_check;
	__u32 icmp_off = sizeof(struct ethhdr) + ip->ihl * 4;
	__u32 quote_off = icmp_off + sizeof(icmp);
	__u32 l4_off, checksum_off;
	__u32 quote_csum_off = quote_off + __builtin_offsetof(struct iphdr, check);
	__u32 outer_csum_off = icmp_off + __builtin_offsetof(struct icmphdr, checksum);
	__u32 source = cubegw0_ip, destination;
	__u16 quote_check;

	if (bpf_skb_load_bytes(skb, icmp_off, &icmp, sizeof(icmp)) ||
	    (icmp.type != 3 && icmp.type != 11 && icmp.type != 12) ||
	    bpf_skb_load_bytes(skb, quote_off, &quote, sizeof(quote)) ||
	    quote.version != 4 || quote.ihl < 5 || quote.ihl > 15 ||
	    quote.saddr != mvm_gateway_ip || quote.daddr != mvm_inner_ip ||
	    (quote.frag_off & IP_FRAG_OFF_MASK))
		return false;
	l4_off = quote_off + quote.ihl * 4;
	key->protocol = quote.protocol;
	if (quote.protocol == IPPROTO_TCP || quote.protocol == IPPROTO_UDP) {
		if (bpf_skb_load_bytes(skb, l4_off, ports, sizeof(ports)))
			return false;
		key->host_port = ports[0];
		key->guest_port = ports[1];
	} else if (quote.protocol == IPPROTO_ICMP) {
		if (bpf_skb_load_bytes(skb, l4_off, &icmp, sizeof(icmp)) || icmp.type != 8)
			return false;
		key->host_port = icmp.un.echo.id;
	}
	if (!primary_delivery_permitted(key))
		return false;
	/* The assigned sandbox address is supplied by the generation-bound map. */
	{
		struct mvm_meta *meta = bpf_map_lookup_elem(&ifindex_to_mvmmeta, &key->ifindex);
		if (!meta || meta->version != key->version)
			return false;
		destination = meta->ip;
	}
	if (quote.protocol == IPPROTO_UDP || quote.protocol == IPPROTO_TCP) {
		checksum_off = l4_off + (quote.protocol == IPPROTO_UDP ? 6 : 16);
		/* RFC-minimum TCP quotations need not contain the TCP checksum. */
		if (!bpf_skb_load_bytes(skb, checksum_off, &old_check, sizeof(old_check)) &&
		    (old_check || quote.protocol == IPPROTO_TCP)) {
			if (bpf_l4_csum_replace(skb, checksum_off, quote.saddr, source,
				BPF_F_PSEUDO_HDR | sizeof(source)) ||
			    bpf_l4_csum_replace(skb, checksum_off, quote.daddr, destination,
				BPF_F_PSEUDO_HDR | sizeof(destination)) ||
			    bpf_skb_load_bytes(skb, checksum_off, &new_check, sizeof(new_check)) ||
			    bpf_l4_csum_replace(skb, outer_csum_off, old_check, new_check, sizeof(new_check)))
				return false;
		}
	}
	if (bpf_l3_csum_replace(skb, quote_csum_off, quote.saddr, source, sizeof(source)) ||
	    bpf_l3_csum_replace(skb, quote_csum_off, quote.daddr, destination, sizeof(destination)) ||
	    bpf_skb_load_bytes(skb, quote_csum_off, &quote_check, sizeof(quote_check)) ||
	    bpf_l4_csum_replace(skb, outer_csum_off, quote.saddr, source, sizeof(source)) ||
	    bpf_l4_csum_replace(skb, outer_csum_off, quote.daddr, destination, sizeof(destination)) ||
	    bpf_l4_csum_replace(skb, outer_csum_off, quote.check, quote_check, sizeof(quote_check)) ||
	    bpf_skb_store_bytes(skb, quote_off + __builtin_offsetof(struct iphdr, saddr), &source, sizeof(source), 0) ||
	    bpf_skb_store_bytes(skb, quote_off + __builtin_offsetof(struct iphdr, daddr), &destination, sizeof(destination), 0))
		return false;
	return true;
}

static __always_inline bool primary_delivery_reply(struct __sk_buff *skb,
		__u32 ifindex, const struct mvm_meta *meta, const struct iphdr *ip)
{
	struct primary_delivery_key key = {};

	if (ip->daddr != mvm_gateway_ip)
		return false;
	key.ifindex = ifindex;
	key.version = meta->version;
	key.protocol = ip->protocol;
	if (ip->frag_off & IP_FRAG_OFF_MASK) {
		key.fragment = 1;
		return primary_delivery_permitted(&key);
	}
	if (ip->protocol == IPPROTO_ICMP) {
		struct icmphdr icmp;
		if (bpf_skb_load_bytes(skb, sizeof(struct ethhdr) + ip->ihl * 4, &icmp, sizeof(icmp)))
			return false;
		/* The error rewriter may change skb storage. Never reuse an old
		 * packet pointer after it, including on a failed rewrite. */
		if (icmp.type != 0)
			return primary_delivery_icmp_error(skb, &key, ip);
	}
	if (!primary_delivery_ports(skb, &key, ip, true))
		return false;
	return primary_delivery_permitted(&key);
}
#endif
