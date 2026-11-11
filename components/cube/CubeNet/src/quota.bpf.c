// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
/* A first TC filter: admitting a flow never bypasses the following policy. */
#include <vmlinux.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

#define NEXT (-1)
#define DROP 2
#define NS 1000000000ULL
struct q_endpoint { __u64 owner; __u64 token; __u64 instance; };
struct q_config { __u32 maximum; __u32 rate; __u32 burst; __u32 pad; __u64 revision; };
struct q_counter {
    struct bpf_spin_lock lock;
    __u32 count;
    __u64 refill;
    __u64 tokens;
    __u64 denied_capacity;
    __u64 denied_rate;
    __u64 allocation_failures;
};
struct q_key {
    __u64 token;
    __u8 local[16];
    __u8 remote[16];
    __u16 local_port;
    __u16 remote_port;
    __u8 family;
    __u8 protocol;
    __u16 pad;
};
struct q_flow {
    struct bpf_spin_lock lock;
    __u32 deleting;
    __u64 owner;
    __u64 last;
    __u64 expires;
    __u32 replied;
    __u32 draining;
};
#define QMAP(name, key_t, value_t, size) \
struct { __uint(type, BPF_MAP_TYPE_HASH); __uint(max_entries, size); \
    __uint(map_flags, BPF_F_NO_PREALLOC); __type(key, key_t); \
    __type(value, value_t); __uint(pinning, LIBBPF_PIN_BY_NAME); } name SEC(".maps")
QMAP(cube_q_ep_v1, __u32, struct q_endpoint, 8192);
QMAP(cube_q_cfg_v1, __u64, struct q_config, 1024);
QMAP(cube_q_count_v1, __u64, struct q_counter, 1024);
QMAP(cube_q_flow_v1, struct q_key, struct q_flow, 131072);
QMAP(cube_q_retire_v1, __u64, __u8, 8192);

/* At most one spinlock is held, and no helper runs while it is held. */
static __always_inline bool reserve(__u64 owner, __u64 now)
{
    struct q_config *cfg = bpf_map_lookup_elem(&cube_q_cfg_v1, &owner);
    struct q_counter *c = bpf_map_lookup_elem(&cube_q_count_v1, &owner);
    if (!cfg || !c)
        return false;
    __u32 max = cfg->maximum, rate = cfg->rate, burst = cfg->burst;
    bool ok = false;
    bpf_spin_lock(&c->lock);
    if (c->count >= max) {
        c->denied_capacity++;
    } else {
        __u64 delta = now > c->refill ? now - c->refill : 0;
        __u64 ceiling = (__u64)burst * NS;
        __u64 tokens = ceiling;
        if (rate && delta < ceiling / rate) {
            tokens = c->tokens + delta * rate;
            if (tokens > ceiling)
                tokens = ceiling;
        }
        c->tokens = tokens;
        c->refill = now;
        if (tokens >= NS) {
            c->tokens -= NS;
            c->count++;
            ok = true;
        } else {
            c->denied_rate++;
        }
    }
    bpf_spin_unlock(&c->lock);
    return ok;
}
static __always_inline void release(__u64 owner, bool allocation_failed)
{
    struct q_counter *c = bpf_map_lookup_elem(&cube_q_count_v1, &owner);
    if (!c)
        return;
    bpf_spin_lock(&c->lock);
    if (c->count)
        c->count--;
    if (allocation_failed)
        c->allocation_failures++;
    bpf_spin_unlock(&c->lock);
}

static __always_inline int account(struct __sk_buff *skb, bool incoming)
{
    __u32 index = incoming ? skb->ifindex : skb->ingress_ifindex;
    struct q_endpoint *ep = bpf_map_lookup_elem(&cube_q_ep_v1, &index);
    if (!ep)
        return NEXT;
    __u64 owner = ep->owner;
    struct q_key key = {.token = ep->token};
    struct ethhdr eth;
    __u32 off = sizeof(eth);
    if (bpf_skb_load_bytes(skb, 0, &eth, sizeof(eth)))
        return DROP;
    if (eth.h_proto == bpf_htons(0x0800)) {
        struct iphdr ip;
        if (bpf_skb_load_bytes(skb, off, &ip, sizeof(ip)) || ip.ihl < 5)
            return DROP;
        /* Later fragments cannot establish a flow. Existing policy handles
         * their permission; only an admitted first fragment can reassemble. */
        if (ip.frag_off & bpf_htons(0x1fff))
            return NEXT;
        key.family = 4;
        key.protocol = ip.protocol;
        __builtin_memcpy(key.local, incoming ? &ip.daddr : &ip.saddr, 4);
        __builtin_memcpy(key.remote, incoming ? &ip.saddr : &ip.daddr, 4);
        off += (__u32)ip.ihl * 4;
    } else if (eth.h_proto == bpf_htons(0x86dd)) {
        struct ipv6hdr ip;
        if (bpf_skb_load_bytes(skb, off, &ip, sizeof(ip)))
            return DROP;
        key.family = 6;
        key.protocol = ip.nexthdr;
        __builtin_memcpy(key.local, incoming ? &ip.daddr : &ip.saddr, 16);
        __builtin_memcpy(key.remote, incoming ? &ip.saddr : &ip.daddr, 16);
        off += sizeof(ip);
        for (int i = 0; i < 4; i++) {
            if (key.protocol != 0 && key.protocol != 43 && key.protocol != 60 && key.protocol != 44)
                break;
            __u8 h[8];
            if (bpf_skb_load_bytes(skb, off, h, sizeof(h)))
                return DROP;
            if (key.protocol == 44) {
                __u16 fragment;
                __builtin_memcpy(&fragment, h + 2, 2);
                if (fragment & bpf_htons(0xfff8))
                    return NEXT;
                off += 8;
            } else {
                off += ((__u32)h[1] + 1) * 8;
            }
            key.protocol = h[0];
        }
    } else {
        return NEXT; /* ARP and existing L2 policy remain independent. */
    }
    __u8 tcp_flags = 0;
    if (key.protocol == 6 || key.protocol == 17) {
        __u16 ports[2];
        if (bpf_skb_load_bytes(skb, off, ports, sizeof(ports)))
            return DROP;
        key.local_port = ports[incoming ? 1 : 0];
        key.remote_port = ports[incoming ? 0 : 1];
        if (key.protocol == 6 && bpf_skb_load_bytes(skb, off + 13, &tcp_flags, 1))
            return DROP;
    } else if (key.protocol == 1 || key.protocol == 58) {
        __u8 h[8];
        if (bpf_skb_load_bytes(skb, off, h, sizeof(h)))
            return DROP;
        if (!((key.protocol == 1 && (h[0] == 0 || h[0] == 8)) ||
              (key.protocol == 58 && (h[0] == 128 || h[0] == 129))))
            return NEXT; /* ND/PMTU and network errors do not create sessions. */
        __builtin_memcpy(&key.local_port, h + 4, 2);
        key.remote_port = key.local_port;
    } else {
        if (key.protocol == 0 || key.protocol == 43 || key.protocol == 60 || key.protocol == 44)
            return DROP; /* Never misclassify an unparsed extension as one TCP flow. */
        /* Other IP protocols use a bounded peer/protocol state, like generic
         * conntrack. Their authorization remains the following policy's job. */
    }
    __u64 now = bpf_ktime_get_ns();
    struct q_flow *seen = bpf_map_lookup_elem(&cube_q_flow_v1, &key);
    if (seen) {
        bool deleting;
        bpf_spin_lock(&seen->lock);
        deleting = seen->deleting;
        if (!deleting) {
            seen->last = now;
            if ((seen->replied & 1) != incoming)
                seen->replied |= 2;
            if (key.protocol == 6) {
                if ((tcp_flags & 0x10) && !(tcp_flags & 2))
                    seen->replied |= 4;
                if (tcp_flags & 5) {
                    seen->draining = 1;
                    seen->expires = now + ((tcp_flags & 4) ? 10 : 120) * NS;
                } else if (!seen->draining) {
                    seen->expires = now + ((seen->replied & 4) ? 10800 : 60) * NS;
                }
            } else {
                seen->expires = now + (key.protocol == 17 && (seen->replied & 2) ? 180 : 30) * NS;
            }
        }
        bpf_spin_unlock(&seen->lock);
        return deleting ? DROP : NEXT;
    }
    __u64 host = 0;
    if (!reserve(host, now))
        return DROP;
    if (!reserve(owner, now)) {
        release(host, false);
        return DROP;
    }
    struct q_flow fresh = {.owner = owner, .last = now,
        .expires = now + (key.protocol == 6 ? ((tcp_flags & 2) ? 60 : 10800) : 30) * NS,
        .replied = incoming};
    long err = bpf_map_update_elem(&cube_q_flow_v1, &key, &fresh, BPF_NOEXIST);
    if (err) {
        release(host, err != -17);
        release(owner, err != -17);
        /* A racing packet may have installed precisely this flow. */
        return bpf_map_lookup_elem(&cube_q_flow_v1, &key) ? NEXT : DROP;
    }
    return NEXT;
}
SEC("tc") int cube_q_in(struct __sk_buff *skb) { return account(skb, false); }
SEC("tc") int cube_q_out(struct __sk_buff *skb) { return account(skb, true); }

/* Userspace supplies a bounded key batch using test-run. The kernel rechecks
 * expiry under the flow lock, so a stale scan cannot delete refreshed traffic. */
SEC("tc") int cube_q_gc(struct __sk_buff *skb)
{
    struct q_key key;
    if (bpf_skb_load_bytes(skb, 0, &key, sizeof(key)))
        return 0;
    __u64 now = bpf_ktime_get_ns();
    struct q_flow *f = bpf_map_lookup_elem(&cube_q_flow_v1, &key);
    if (!f)
        return 0;
    bool expired = false;
    __u64 owner = f->owner;
    bpf_spin_lock(&f->lock);
    if (!f->deleting && f->expires <= now) {
        f->deleting = 1;
        expired = true;
    }
    bpf_spin_unlock(&f->lock);
    if (expired && bpf_map_delete_elem(&cube_q_flow_v1, &key) == 0) {
        __u64 host = 0;
        release(host, false);
        release(owner, false);
    }
    return 0;
}
char __license[] SEC("license") = "Dual BSD/GPL";

static long sweep_one(void *map, const struct q_key *key, struct q_flow *f, void *ctx)
{
    __u64 now = bpf_ktime_get_ns();
    bool retired = bpf_map_lookup_elem(&cube_q_retire_v1, &key->token) != 0;
    if (!retired && f->expires > now)
        return 0;
    __u64 owner = f->owner;
    bool expired = false;
    bpf_spin_lock(&f->lock);
    if (!f->deleting && (retired || f->expires <= now)) {
        f->deleting = 1;
        expired = true;
    }
    bpf_spin_unlock(&f->lock);
    if (expired && bpf_map_delete_elem(&cube_q_flow_v1, key) == 0) {
        __u64 host = 0;
        release(host, false);
        release(owner, false);
    }
    return 0;
}
SEC("tc") int cube_q_sweep(struct __sk_buff *skb)
{
    /* The map hard capacity bounds traversal. One syscall replaces per-entry
     * userspace GC syscalls on a one-core host. */
    bpf_for_each_map_elem(&cube_q_flow_v1, sweep_one, 0, 0);
    return 0;
}
