// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
#ifndef __PRIMARY_CHAIN_MAP_H
#define __PRIMARY_CHAIN_MAP_H
#ifndef ETH_P_IPV6
#define ETH_P_IPV6 0x86dd
#endif
#define CHAIN_MAX_ROUTES 256
struct chain_route {
    __u8 address[16];
    __u8 bits;
    __u8 family;
    __u8 protocol;
    __u8 pad;
    __u16 port_start;
    __u16 port_end;
    __u32 mark; /* zero is a NIC-selected route: reject on the primary device */
};
struct chain_routes {
    __u32 version; /* primary TAP incarnation, not the configuration generation */
    __u32 count;
    __u64 generation;
    __u8 mac[6];
    __u8 pad[2];
    struct chain_route routes[CHAIN_MAX_ROUTES];
};
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 512);
    __type(key, __u32);
    __type(value, struct chain_routes);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} primary_chain_routes SEC(".maps");

/* The primary IPv6 address is node-local, deterministically derived from Cube's
 * existing delivery address. It allocates neither a business NIC nor a public IP. */
static __always_inline bool primary_chain_ipv6(const __u8 *address)
{
    const __u8 prefix[12] = {0xfd,0x68,0xbc,0xad,0,0,0,1,0,0,0,0};
    return !__builtin_memcmp(address, prefix, 12);
}
#endif
