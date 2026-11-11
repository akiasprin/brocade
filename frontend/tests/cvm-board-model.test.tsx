import { describe, expect, it } from 'vitest';
import type { CvmEntry, CvmEntryBoard, CvmInstance, CvmInterface, CvmNicCatalog } from '../src/api';
import {
  entryMachines,
  exitTiles,
  nextFreePort,
  parseService,
  repointRoute,
  routeLabel,
  ruleLinks,
  withRoute,
  withoutRoute,
} from '../src/panes/cvm/board/model';

const entry = (id: string, publication: string, address: string, mode: CvmEntry['mode']): CvmEntry => ({
  id,
  revision: 1,
  publication_id: publication,
  provider_node: 'hk',
  instance_id: 'web',
  address,
  visible_address: address,
  mode,
  remote: true,
  state: 'active',
  error: null,
});
const board: CvmEntryBoard = {
  can_manage: false,
  entries: [
    entry('ssh', 'pub-hk', '203.0.113.20', { kind: 'l4', protocol: 'tcp', listen_port: 30022, target_port: 22 }),
    entry('revoked', 'pub-gone', '203.0.113.99', { kind: 'l3' }),
  ],
  publications: [
    {
      id: 'pub-hk',
      revision: 1,
      admitting: true,
      retired: false,
      provider_name: '香港',
      active_entries: 1,
      spec: {
        name: '香港',
        provider_node: 'hk',
        addresses: [
          { address: '203.0.113.20', kind: 'shared', port_start: 30000, port_end: 30003 },
          { address: '10.0.0.30', public_address: '198.51.100.30', kind: 'dedicated' },
        ],
        transports: [],
        grants: [],
        max_entries: 8,
      },
      usage: [
        {
          address: '203.0.113.20',
          whole_taken: false,
          whole_entry_id: null,
          ports: [
            { protocol: 'tcp', port_start: 30000, port_end: 30001, entry_id: null },
            { protocol: 'udp', port_start: 30002, port_end: 30002, entry_id: null },
          ],
        },
        { address: '198.51.100.30', whole_taken: false, whole_entry_id: null, ports: [] },
      ],
    },
  ],
};

describe('composition board model', () => {
  it('groups addresses by machine and keeps entries whose publication is no longer listed', () => {
    const machines = entryMachines(board, []);
    expect(machines).toHaveLength(1);
    expect(machines[0].addresses.map(row => row.entries.map(item => item.id))).toEqual([['ssh'], []]);
    expect(machines[0].orphans.map(item => item.id)).toEqual(['revoked']);
  });

  it('picks the first port free for every selected protocol', () => {
    const offer = board.publications[0].spec.addresses[0];
    if (offer.kind !== 'shared') throw new Error('fixture');
    const usage = board.publications[0].usage[0];
    expect(nextFreePort(offer, usage, 'tcp')).toBe(30002);
    expect(nextFreePort(offer, usage, 'tcp_udp')).toBe(30003);
    expect(nextFreePort(offer, usage, 'udp', 30001)).toBe(30001);
    expect(nextFreePort({ ...offer, port_end: 30001 }, usage, 'tcp')).toBeNull();
  });

  it('parses and labels port-selected routes', () => {
    expect(parseService('tcp/443')).toEqual({ protocol: 'tcp', port_start: 443, port_end: 443 });
    expect(parseService('UDP/1000-2000')).toEqual({ protocol: 'udp', port_start: 1000, port_end: 2000 });
    for (const bad of ['tcp/0', 'tcp/2-1', 'icmp/1', 'tcp/70000', '443']) expect(parseService(bad)).toBeNull();
    expect(
      routeLabel({
        destination: '0.0.0.0/0',
        interface_id: 'n',
        service: { protocol: 'tcp', port_start: 22, port_end: 22 },
      }),
    ).toBe('tcp/22');
    expect(
      routeLabel({
        destination: '10.0.0.0/8',
        interface_id: 'n',
        service: { protocol: 'udp', port_start: 1, port_end: 9 },
      }),
    ).toBe('udp/1-9 10.0.0.0/8');
  });

  it('adds, moves and removes routes while giving a new NIC a lower priority', () => {
    const home = { id: 'home', allocated: { ipv4: '10.1.0.2', ipv6: null } } as CvmInterface;
    const ntt = { id: 'ntt', allocated: { ipv4: '10.2.0.2', ipv6: 'fd00::2' } } as CvmInterface;
    let routing = withRoute(
      { revision: 3, priorities: [], routes: [], dns: [] },
      { destination: '0.0.0.0/0', interface_id: 'home' },
      home,
    );
    expect(routing.priorities).toEqual([{ interface_id: 'home', ipv4: 10, ipv6: null }]);
    routing = withRoute(
      routing,
      { destination: '0.0.0.0/0', interface_id: 'ntt', service: parseService('tcp/22') },
      ntt,
    );
    expect(routing.priorities[1]).toEqual({ interface_id: 'ntt', ipv4: 20, ipv6: 20 });
    const moved = repointRoute(routing, 1, home);
    expect(moved.routes.map(route => route.interface_id)).toEqual(['home', 'home']);
    expect(moved.routes[1].service).toEqual({ protocol: 'tcp', port_start: 22, port_end: 22 });
    expect(withoutRoute(moved, 0).routes).toHaveLength(1);
    expect(routing.revision).toBe(3);
  });

  it('maps routes to their exit networks and classifies exits', () => {
    const instance = {
      desired: {
        id: 'web',
        spec: {
          interfaces: [{ id: 'nic', allocated: { publication_id: 'net-hk' } }],
          routing: {
            revision: 1,
            priorities: [],
            dns: [],
            routes: [{ destination: '0.0.0.0/0', interface_id: 'nic' }],
          },
        },
      },
    } as unknown as CvmInstance;
    expect(ruleLinks([instance])).toMatchObject([{ instance: 'web', exit: 'net-hk', layer: 'L3' }]);
    const catalog = {
      providers: [],
      attachments: [],
      can_manage: false,
      publications: [
        {
          id: 'net-hk',
          retired: false,
          spec: {
            name: 'a',
            provider_node: 'hk',
            destinations: ['0.0.0.0/0'],
            address_mode: 'shared_nat',
            shared_ipv4: '203.0.113.20',
          },
        },
        {
          id: 'net-lan',
          retired: false,
          spec: {
            name: 'b',
            provider_node: 'office',
            destinations: ['192.168.10.0/24'],
            address_mode: 'dedicated_routed',
          },
        },
        {
          id: 'net-old',
          retired: true,
          spec: { name: 'c', provider_node: 'old', destinations: ['0.0.0.0/0'], address_mode: 'shared_nat' },
        },
      ],
    } as unknown as CvmNicCatalog;
    expect(exitTiles(catalog, []).map(tile => [tile.id, tile.kind, tile.address])).toEqual([
      ['net-hk', 'shared', '203.0.113.20'],
      ['net-lan', 'private', null],
    ]);
  });
});
