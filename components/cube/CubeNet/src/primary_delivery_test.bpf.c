// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
/* Copyright (c) 2026 Cube Authors */
#include <vmlinux.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>
#include "cubevs.h"
#include "map.h"
#include "skb.h"
#include "primary_delivery.h"

SEC("tc") int test_primary_record(struct __sk_buff *skb)
{
	struct iphdr ip;
	if (bpf_skb_load_bytes(skb, sizeof(struct ethhdr), &ip, sizeof(ip)))
		return TC_ACT_SHOT;
	primary_delivery_record(skb, 1, &ip);
	return TC_ACT_OK;
}
SEC("tc") int test_primary_reply(struct __sk_buff *skb)
{
	__u32 tap = 1;
	struct iphdr ip;
	struct mvm_meta *meta;
	if (bpf_skb_load_bytes(skb, sizeof(struct ethhdr), &ip, sizeof(ip)))
		return TC_ACT_SHOT;
	meta = bpf_map_lookup_elem(&ifindex_to_mvmmeta, &tap);
	if (!meta)
		return TC_ACT_SHOT;
	return primary_delivery_reply(skb, tap, meta, &ip) ? TC_ACT_OK : TC_ACT_SHOT;
}
char __license[] SEC("license") = "Dual BSD/GPL";
