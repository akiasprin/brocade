import { describe, expect, it } from 'vitest';

import type { NodeAgentStateItem, UserListItem } from '../src/api';
import type { FleetTrafficSnapshot } from '../src/fleet-traffic-source';
import { buildFleetTrafficMatrix } from '../src/panes/fleet-traffic';

const NOW = 100_000;

const nodeState = (node_id: string, name = node_id, public_ipv4_country: string | null = null) =>
  ({ node_id, name, lifecycle_phase: 'active', public_ipv4_country }) as NodeAgentStateItem;

const user = (id: string, tenant_id = 'platform') => ({ id, tenant_id, status: 'active' }) as UserListItem;

const rate = (user_id: string, downlink = 2_000, uplink = 1_000, tenant_id = 'platform') => ({
  tenant_id,
  user_id,
  downlink_bytes_per_sec: downlink,
  uplink_bytes_per_sec: uplink,
});

const liveNode = (
  node_id: string,
  received: number,
  users: ReturnType<typeof rate>[],
  report: Record<string, unknown> = {},
) => ({
  node_id,
  connected: true,
  active: true,
  interval_secs: 1,
  samples: [
    {
      node_id,
      received_at_unix_millis: received,
      sample: {
        sampled_at_unix_millis: received,
        rx_bytes_per_sec: 8_000,
        tx_bytes_per_sec: 4_000,
        user_traffic: {
          state: 'ready' as const,
          all_users: true,
          users,
          truncated: false,
          has_gap: false,
          ...report,
        },
      },
    },
  ],
});

describe('fleet traffic matrix', () => {
  it('keeps roster users and pending cells until grants confirm which rows belong in the matrix', () => {
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [liveNode('n1', NOW, []), liveNode('n2', NOW, [])],
    };
    const nodes = [nodeState('n1'), nodeState('n2')];
    const users = [user('alice'), user('bob')];
    const pending = buildFleetTrafficMatrix(payload, nodes, users, NOW);
    expect(pending.entitlementsKnown).toBe(false);
    expect(pending.users.map(row => row.userId)).toEqual(['alice', 'bob']);
    for (const row of pending.users) {
      expect([...row.cells.values()].map(cell => cell.state)).toEqual(['pending', 'pending']);
      expect(row.unknown).toBe(0);
    }

    payload.expected = [{ node_id: 'n1', users: [{ tenant_id: 'platform', user_id: 'bob' }] }];
    const confirmed = buildFleetTrafficMatrix(payload, nodes, users, NOW);
    expect(confirmed.entitlementsKnown).toBe(true);
    expect(confirmed.users.map(row => row.userId)).toEqual(['bob']);
    expect(confirmed.users[0].cells.get('n1')?.state).toBe('idle');
    expect(confirmed.users[0].cells.get('n2')?.state).toBe('not-entitled');
  });

  it('takes machine IDs and granted users from the authorization snapshot before the roster arrives', () => {
    const matrix = buildFleetTrafficMatrix(
      {
        policy: { enabled: true },
        nodes: [],
        expected: [{ node_id: 'n1', users: [{ tenant_id: 'platform', user_id: 'alice' }] }],
      },
      [],
      [],
      NOW,
    );

    expect(matrix.entitlementsKnown).toBe(true);
    expect(matrix.nodes.map(node => node.id)).toEqual(['n1']);
    expect(matrix.users.map(user => user.userId)).toEqual(['alice']);
    expect(matrix.users[0].cells.get('n1')?.state).toBe('unknown');
  });

  it('distinguishes an entitled zero from an unknown machine and a missing entitlement', () => {
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [liveNode('n1', NOW - 500, [rate('alice')]), { ...liveNode('n2', NOW - 20_000, []), connected: false }],
      expected: [
        {
          node_id: 'n1',
          users: [
            { tenant_id: 'platform', user_id: 'alice' },
            { tenant_id: 'platform', user_id: 'bob' },
          ],
        },
        { node_id: 'n2', users: [{ tenant_id: 'platform', user_id: 'alice' }] },
      ],
    };

    const matrix = buildFleetTrafficMatrix(
      payload,
      [nodeState('n1', '香港'), nodeState('n2', '东京')],
      [user('alice'), user('bob')],
      NOW,
    );
    const alice = matrix.users.find(row => row.userId === 'alice')!;
    const bob = matrix.users.find(row => row.userId === 'bob')!;

    expect(alice.cells.get('n1')).toMatchObject({ state: 'rate', down: 2_000, up: 1_000, partial: false });
    expect(bob.cells.get('n1')).toMatchObject({ state: 'idle' });
    expect(alice.cells.get('n2')).toMatchObject({ state: 'unknown' });
    expect(bob.cells.get('n2')).toMatchObject({ state: 'not-entitled' });
    expect(matrix.nodeTotals.get('n1')).toEqual({ down: 2_000, up: 1_000 });
    expect(matrix.currentNodes).toBe(1);
  });

  it('keeps reported rows visible but marks them partial when the all-user frame is incomplete', () => {
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [liveNode('n1', NOW, [rate('alice'), rate('carol', 500, 250)], { truncated: true })],
      expected: [{ node_id: 'n1', users: [{ tenant_id: 'platform', user_id: 'alice' }] }],
    };

    const matrix = buildFleetTrafficMatrix(payload, [nodeState('n1')], [user('alice')], NOW);

    expect(matrix.users.find(row => row.userId === 'alice')?.cells.get('n1')).toMatchObject({
      state: 'rate',
      partial: true,
    });
    expect(matrix.users.find(row => row.userId === 'carol')?.cells.get('n1')).toMatchObject({
      state: 'rate',
      down: 500,
      up: 250,
      partial: true,
    });
  });

  it('judges freshness independently for each machine', () => {
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [liveNode('n1', NOW - 500, []), liveNode('n2', NOW - 10_001, [])],
      expected: [
        { node_id: 'n1', users: [{ tenant_id: 'platform', user_id: 'alice' }] },
        { node_id: 'n2', users: [{ tenant_id: 'platform', user_id: 'alice' }] },
      ],
    };

    const matrix = buildFleetTrafficMatrix(payload, [nodeState('n1'), nodeState('n2')], [user('alice')], NOW);

    expect(matrix.nodes.map(node => [node.id, node.current])).toEqual([
      ['n1', true],
      ['n2', false],
    ]);
    expect(matrix.users[0].cells.get('n1')?.state).toBe('idle');
    expect(matrix.users[0].cells.get('n2')?.state).toBe('unknown');
  });

  it('names why each machine cannot certify its users', () => {
    const grants = (node_id: string) => ({ node_id, users: [{ tenant_id: 'platform', user_id: 'alice' }] });
    const unsupported = liveNode('agent-old', NOW, []);
    delete (unsupported.samples[0].sample as { user_traffic?: unknown }).user_traffic;
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [
        liveNode('ready', NOW, []),
        liveNode('truncated', NOW, [rate('alice')], { truncated: true }),
        liveNode('warming', NOW, [], { state: 'warming' }),
        liveNode('xray-down', NOW, [], { state: 'unavailable' }),
        liveNode('forwarding', NOW, [], { forwarding_expected: true, forwarding_supported: false }),
        unsupported,
        { ...liveNode('offline', NOW, []), connected: false },
        liveNode('stale', NOW - 10_001, []),
      ],
      expected: ['ready', 'truncated', 'warming', 'xray-down', 'forwarding', 'agent-old', 'offline', 'stale'].map(
        grants,
      ),
    };

    const matrix = buildFleetTrafficMatrix(
      payload,
      [{ ...nodeState('agent-old'), agent_protocol_version: 26 }],
      [user('alice')],
      NOW,
    );
    const phase = Object.fromEntries(matrix.nodes.map(node => [node.id, [node.phase, node.partial]]));

    expect(phase).toEqual({
      ready: ['ready', false],
      truncated: ['ready', true],
      warming: ['pending', false],
      'xray-down': ['unavailable', false],
      forwarding: ['forwarding-unsupported', true],
      'agent-old': ['unsupported', false],
      offline: ['offline', false],
      stale: ['offline', false],
    });
    // Only a complete report turns a missing row into zero; a partial report keeps its rows.
    expect(matrix.users[0].cells.get('ready')?.state).toBe('idle');
    expect(matrix.users[0].cells.get('truncated')).toMatchObject({ state: 'rate', partial: true });
    expect(matrix.users[0].cells.get('forwarding')?.state).toBe('unknown');
    expect(matrix.users[0].unknown).toBe(6);
    expect(matrix.nicNodes).toBe(6);
  });

  it('treats the first NIC-only frame and an unopened stream as loading, without inventing zero rates', () => {
    const nicOnly = (id: string) => {
      const node = liveNode(id, NOW, []);
      delete (node.samples[0].sample as { user_traffic?: unknown }).user_traffic;
      return node;
    };
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [nicOnly('modern'), nicOnly('unknown')],
      expected: ['modern', 'unknown', 'opening'].map(node_id => ({
        node_id,
        users: [{ tenant_id: 'platform', user_id: 'alice' }],
      })),
    };
    const states = [{ ...nodeState('modern'), agent_protocol_version: 36 }, nodeState('unknown'), nodeState('opening')];

    const matrix = buildFleetTrafficMatrix(payload, states, [user('alice')], NOW);

    expect(matrix.nodes.every(node => node.phase === 'pending' && !node.complete)).toBe(true);
    expect(matrix.nodes.find(node => node.id === 'opening')?.current).toBe(false);
    expect(matrix.users[0].unknown).toBe(3);
    expect([...matrix.users[0].cells.values()].every(cell => cell.state === 'unknown')).toBe(true);
    expect(matrix.currentNodes).toBe(0);
    expect(matrix.nicNodes).toBe(2);

    payload.nodes[0] = liveNode('modern', NOW, []);
    const ready = buildFleetTrafficMatrix(payload, states, [user('alice')], NOW);
    expect(ready.nodes.find(node => node.id === 'modern')?.phase).toBe('ready');
    expect(ready.users[0].cells.get('modern')?.state).toBe('idle');
    expect(ready.currentNodes).toBe(1);
  });

  it('groups machines by region, larger regions first, and leaves out users with nothing to show', () => {
    const payload: FleetTrafficSnapshot = {
      policy: { enabled: true },
      nodes: [
        liveNode('hk-b', NOW, [rate('alice', 4_000, 0)]),
        liveNode('hk-a', NOW, []),
        liveNode('jp', NOW, [rate('alice', 1_000, 0), rate('bob', 0, 0)]),
        liveNode('somewhere', NOW, []),
      ],
      expected: [
        {
          node_id: 'hk-b',
          users: [
            { tenant_id: 'platform', user_id: 'alice' },
            { tenant_id: 'platform', user_id: 'bob' },
          ],
        },
        { node_id: 'jp', users: [{ tenant_id: 'platform', user_id: 'alice' }] },
      ],
    };
    const nodes = [
      nodeState('jp', '东京', 'JP'),
      nodeState('hk-b', '香港 B', 'hk'),
      nodeState('somewhere', '未识别'),
      nodeState('hk-a', '香港 A', 'HK'),
    ];

    const matrix = buildFleetTrafficMatrix(payload, nodes, [user('alice'), user('bob'), user('carol')], NOW);

    expect(matrix.nodes.map(node => [node.id, node.country])).toEqual([
      ['hk-a', 'HK'],
      ['hk-b', 'HK'],
      ['jp', 'JP'],
      ['somewhere', null],
    ]);
    // carol holds no grant on these machines and sends nothing.
    expect(matrix.users.map(row => row.userId)).toEqual(['alice', 'bob']);
    expect(matrix.users.map(row => [row.entitled, row.active])).toEqual([
      [2, 2],
      [1, 0],
    ]);
    expect(matrix.nodes.map(node => [node.id, node.entitled, node.active])).toEqual([
      ['hk-a', 0, 0],
      ['hk-b', 2, 1],
      ['jp', 1, 1],
      ['somewhere', 0, 0],
    ]);
    // A reported zero row is a measurement, not traffic.
    expect(matrix.activeUsers).toBe(1);
  });
});
