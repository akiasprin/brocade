import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  analyzeFrontRoutes,
  deleteFront,
  fetchFrontClientConfigState,
  upsertFront,
  type FrontRouteAnalysisView,
  type FrontRouteDecision,
  type FrontRouteStatus,
  type SnapshotApp,
  type UserListItem,
} from '../src/api';
import { draft } from '../src/draft';
import { ChainProxyDetail, ChainProxyListSection } from '../src/panes/chain-proxy';
import { SessionProvider } from '../src/session';

const initial = { node_count: 0, chain_group_count: [] };

beforeEach(() => {
  draft.init('chain-proxy-management-test');
  draft.clear();
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

const chainProxyFixture = (): SnapshotApp => ({
  id: 'app-main',
  label: '全球加速',
  chains: [{ id: 'chain', tenant: 'platform.acme', name: '主链' }],
  steps: [],
  ingresses: [
    {
      id: 'ing-hk',
      chain: 'chain',
      node: 'hk',
      bind: '0.0.0.0',
      port: 443,
      projection: {},
      guard: {
        no_private: false,
        no_bittorrent: false,
        no_mail: false,
        no_udp_amplification: false,
        tcp_and_quic_only: false,
      },
      identity: {},
      wires: {},
    },
    {
      id: 'ing-us',
      chain: 'chain',
      node: 'us',
      bind: '0.0.0.0',
      port: 8443,
      front: 'front-asia',
      projection: {},
      guard: {
        no_private: false,
        no_bittorrent: false,
        no_mail: false,
        no_udp_amplification: false,
        tcp_and_quic_only: false,
      },
      identity: {},
      wires: {},
    },
  ],
  fronts: [
    {
      id: 'front-asia',
      tenant: 'platform.acme',
      name: '亚洲优选',
      strategy: 'select',
      via: ['ing-hk'],
      external_via: [],
    },
  ],
  grants: [
    { tenant: 'platform.acme', user: 'alice', ingress: 'ing-hk' },
    { tenant: 'platform.acme', user: 'alice', ingress: 'ing-us' },
  ],
});

const alice: UserListItem = {
  tenant_id: 'platform.acme',
  id: 'alice',
  status: 'active',
  created_at: '2026-09-12T00:00:00Z',
  created_revision: 1,
};

const routeDecision = (status: FrontRouteStatus, reason: string): FrontRouteDecision => ({
  status,
  chain_id: 'chain',
  node_id: status === 'blocked' ? 'us' : 'hk',
  rule_index: 1,
  selector: 'any',
  action: status === 'blocked' ? 'block' : 'egress',
  reason,
});

const routeAnalysisFixture = (status: FrontRouteStatus = 'reachable'): FrontRouteAnalysisView => {
  const relay = routeDecision('reachable', '成员链路明确放行');
  const landing = routeDecision(status === 'blocked' ? 'blocked' : status, '落地链路判定');
  const combined = routeDecision(status, status === 'blocked' ? '落地链路明确阻断' : '端到端判定');
  return {
    base_client_snapshot_id: 14,
    topology_revision_id: 5,
    permissions_revision_id: 7,
    serving_generation: 23,
    pending_topology: [],
    analysis: {
      front_id: 'front-asia',
      members: [
        {
          id: 'ing-hk',
          kind: 'internal',
          chain_id: 'chain',
          node_id: 'hk',
          pending: false,
        },
      ],
      targets: [
        {
          id: 'ing-us',
          chain_id: 'chain',
          node_id: 'us',
          endpoints: ['us.example.com'],
          landing,
          pending: false,
        },
      ],
      cells: [
        {
          member_id: 'ing-hk',
          target_id: 'ing-us',
          endpoints: [{ endpoint: 'us.example.com', decision: relay }],
          relay,
          landing,
          combined,
        },
      ],
      blocking: status === 'blocked',
    },
  };
};

const renderDetail = () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial,
          who: {
            operator_id: 'admin',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <ChainProxyDetail appId="app-main" frontId="front-asia" onOpen={() => undefined} onBack={() => undefined} />
      </SessionProvider>
    </QueryClientProvider>,
  );
};

it('saves and deletes a complete Front directly instead of creating a browser draft', async () => {
  const writes: { path: string; method: string; body: unknown }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      writes.push({
        path,
        method: init?.method ?? 'GET',
        body: init?.body ? JSON.parse(String(init.body)) : null,
      });
      return Response.json({
        revision_id: 8,
        removed: true,
        front: {
          id: 'front-asia',
          tenant: 'platform',
          name: '亚洲优选',
          strategy: 'fallback',
          via: ['ing-hk', 'ing-sg'],
          external_via: ['vendor'],
        },
        targets: ['ing-us'],
        client_config: {
          snapshot_id: 4,
          status: 'activated',
          serving_generation: 12,
          pending_topology: [],
        },
      });
    }),
  );

  await upsertFront('app-main', {
    expected_revision: 7,
    id: 'front-asia',
    tenant_id: 'platform',
    name: '亚洲优选',
    strategy: 'fallback',
    via: ['ing-hk', 'ing-sg'],
    external_via: ['vendor'],
    targets: ['ing-us'],
  });
  await analyzeFrontRoutes('app-main', {
    expected_revision: 8,
    id: 'front-asia',
    tenant_id: 'platform',
    name: '亚洲优选',
    strategy: 'fallback',
    via: ['ing-hk', 'ing-sg'],
    external_via: ['vendor'],
    targets: ['ing-us'],
  });
  await fetchFrontClientConfigState('app-main', 'front-asia');
  await deleteFront('app-main', 'front-asia', 8);

  expect(draft.ops()).toEqual([]);
  expect(writes).toEqual([
    {
      path: '/apps/app-main/fronts',
      method: 'POST',
      body: {
        expected_revision: 7,
        id: 'front-asia',
        tenant_id: 'platform',
        name: '亚洲优选',
        strategy: 'fallback',
        via: ['ing-hk', 'ing-sg'],
        external_via: ['vendor'],
        targets: ['ing-us'],
      },
    },
    {
      path: '/apps/app-main/front-analysis',
      method: 'POST',
      body: {
        expected_revision: 8,
        id: 'front-asia',
        tenant_id: 'platform',
        name: '亚洲优选',
        strategy: 'fallback',
        via: ['ing-hk', 'ing-sg'],
        external_via: ['vendor'],
        targets: ['ing-us'],
      },
    },
    {
      path: '/apps/app-main/fronts/front-asia',
      method: 'GET',
      body: null,
    },
    {
      path: '/apps/app-main/fronts/front-asia',
      method: 'DELETE',
      body: { expected_revision: 8 },
    },
  ]);
});

it('shows one shared Front card with its per-user projection impact', () => {
  const app: SnapshotApp = {
    id: 'app-main',
    label: '全球加速',
    chains: [{ id: 'chain', tenant: 'platform', name: '主链' }],
    steps: [],
    ingresses: [
      {
        id: 'ing-hk',
        chain: 'chain',
        node: 'hk',
        bind: '0.0.0.0',
        port: 443,
        projection: {},
        guard: {
          no_private: false,
          no_bittorrent: false,
          no_mail: false,
          no_udp_amplification: false,
          tcp_and_quic_only: false,
        },
        identity: {},
        wires: {},
      },
      {
        id: 'ing-us',
        chain: 'chain',
        node: 'us',
        bind: '0.0.0.0',
        port: 8443,
        front: 'front-asia',
        projection: {},
        guard: {
          no_private: false,
          no_bittorrent: false,
          no_mail: false,
          no_udp_amplification: false,
          tcp_and_quic_only: false,
        },
        identity: {},
        wires: {},
      },
    ],
    fronts: [
      {
        id: 'front-asia',
        tenant: 'platform.acme',
        name: '亚洲优选',
        strategy: 'select',
        via: ['ing-hk'],
        external_via: [],
      },
    ],
    grants: [
      { tenant: 'platform.acme.child', user: 'alice', ingress: 'ing-hk' },
      { tenant: 'platform.acme.child', user: 'alice', ingress: 'ing-us' },
      { tenant: 'platform.beta', user: 'alice', ingress: 'ing-hk' },
      { tenant: 'platform.beta', user: 'alice', ingress: 'ing-us' },
    ],
  };
  const users: UserListItem[] = [
    {
      tenant_id: 'platform.acme.child',
      id: 'alice',
      status: 'active',
      created_at: '2026-09-12T00:00:00Z',
      created_revision: 1,
    },
    {
      tenant_id: 'platform.beta',
      id: 'alice',
      status: 'active',
      created_at: '2026-09-12T00:00:00Z',
      created_revision: 1,
    },
  ];
  const view = render(
    <ChainProxyListSection apps={[app]} users={users} editable onOpen={() => undefined} onCreate={() => undefined} />,
  );

  expect(view.getByText('链式代理')).toBeTruthy();
  const card = view.getByRole('button', { name: /亚洲优选/ }).textContent ?? '';
  expect(card).toContain('1 成员');
  expect(card).toContain('1 目标');
  expect(card).toContain('1 用户');
  expect(view.getByText('客户端订阅 · 无需发布机器')).toBeTruthy();
});

it('reloads the durable subscription tuple for an existing Front', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      if (path === '/model/snapshot') {
        return Response.json({ snapshot: { revision: 8, apps: [chainProxyFixture()], external_outbounds: [] } });
      }
      if (path === '/tenants') {
        return Response.json({
          tenants: [{ id: 'platform.acme', name: 'Acme', node_count: 2, user_count: 1, operator_count: 1 }],
        });
      }
      if (path === '/users?include_disabled=true') return Response.json({ users: [alice] });
      if (path === '/grant-probes/capability') {
        return Response.json({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/apps/app-main/front-analysis') return Response.json(routeAnalysisFixture());
      if (path === '/apps/app-main/fronts/front-asia') {
        return Response.json({
          front_id: 'front-asia',
          head_snapshot_id: 14,
          serving_snapshot_id: 14,
          topology_revision_id: 5,
          permissions_revision_id: 7,
          serving_generation: 23,
          active: true,
          pending_topology: ['front:front-asia:via:ing-sg'],
        });
      }
      throw new Error(`unexpected request ${path}`);
    }),
  );

  const view = renderDetail();
  const state = await view.findByLabelText('当前订阅生效状态');
  expect(state.textContent).toContain('客户端头 #14');
  expect(state.textContent).toContain('正在下发 G23');
  expect(state.textContent).toContain('拓扑 R5');
  expect(state.textContent).toContain('权限 R7');
  expect(state.textContent).toContain('1 项等待拓扑');
});

it('refreshes the expected revision after a save conflict without discarding the form', async () => {
  let snapshotRevision = 8;
  const submittedRevisions: number[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/model/snapshot') {
        return Response.json({
          snapshot: { revision: snapshotRevision, apps: [chainProxyFixture()], external_outbounds: [] },
        });
      }
      if (path === '/tenants') {
        return Response.json({
          tenants: [{ id: 'platform.acme', name: 'Acme', node_count: 2, user_count: 1, operator_count: 1 }],
        });
      }
      if (path === '/users?include_disabled=true') return Response.json({ users: [alice] });
      if (path === '/grant-probes/capability') {
        return Response.json({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/apps/app-main/front-analysis' && init?.method === 'POST') {
        return Response.json(routeAnalysisFixture());
      }
      if (path === '/apps/app-main/fronts/front-asia' && (init?.method ?? 'GET') === 'GET') {
        return Response.json({
          front_id: 'front-asia',
          head_snapshot_id: 14,
          serving_snapshot_id: 14,
          topology_revision_id: 5,
          permissions_revision_id: 7,
          serving_generation: 23,
          active: true,
          pending_topology: [],
        });
      }
      if (path === '/apps/app-main/fronts' && init?.method === 'POST') {
        const body = JSON.parse(String(init.body)) as { expected_revision: number };
        submittedRevisions.push(body.expected_revision);
        if (submittedRevisions.length === 1) {
          snapshotRevision = 9;
          return Response.json({ error: 'stale revision' }, { status: 409 });
        }
        return Response.json({
          revision_id: 10,
          front: chainProxyFixture().fronts[0],
          targets: ['ing-us'],
          client_config: {
            snapshot_id: 15,
            status: 'activated',
            serving_generation: 24,
            pending_topology: [],
          },
        });
      }
      throw new Error(`unexpected request ${path}`);
    }),
  );

  const view = renderDetail();
  const name = await view.findByDisplayValue('亚洲优选');
  fireEvent.change(name, { target: { value: '亚洲优选（编辑中）' } });
  fireEvent.click(await view.findByRole('button', { name: '保存前置组' }));
  await view.findByText(/现已刷新到修订 #9/);
  expect(view.getByDisplayValue('亚洲优选（编辑中）')).toBeTruthy();

  fireEvent.click(await view.findByRole('button', { name: '保存前置组' }));
  await waitFor(() => expect(submittedRevisions).toEqual([8, 9]));
});

it('shows the authoritative path decision and prevents saving a known blocked route', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/model/snapshot') {
        return Response.json({ snapshot: { revision: 8, apps: [chainProxyFixture()], external_outbounds: [] } });
      }
      if (path === '/tenants') {
        return Response.json({
          tenants: [{ id: 'platform.acme', name: 'Acme', node_count: 2, user_count: 1, operator_count: 1 }],
        });
      }
      if (path === '/users?include_disabled=true') return Response.json({ users: [alice] });
      if (path === '/grant-probes/capability') {
        return Response.json({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/apps/app-main/front-analysis' && init?.method === 'POST') {
        return Response.json(routeAnalysisFixture('blocked'));
      }
      if (path === '/apps/app-main/fronts/front-asia' && (init?.method ?? 'GET') === 'GET') {
        return Response.json({
          front_id: 'front-asia',
          head_snapshot_id: 14,
          serving_snapshot_id: 14,
          topology_revision_id: 5,
          permissions_revision_id: 7,
          serving_generation: 23,
          active: true,
          pending_topology: [],
        });
      }
      throw new Error(`unexpected request ${path}`);
    }),
  );

  const view = renderDetail();
  expect((await view.findByRole('alert')).textContent).toContain('当前组合存在明确阻断');
  expect(view.getByRole('button', { name: '主链 · ing-hk 到 主链 · ing-us：阻断' })).toBeTruthy();
  expect(view.getByText('落地链路明确阻断')).toBeTruthy();
  expect((view.getByRole('button', { name: '存在阻断，不能保存' }) as HTMLButtonElement).disabled).toBe(true);
});

it('runs the pinned matrix cell with the selected users serving tuple', async () => {
  let requestBody: Record<string, unknown> | undefined;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/model/snapshot') {
        return Response.json({ snapshot: { revision: 8, apps: [chainProxyFixture()], external_outbounds: [] } });
      }
      if (path === '/tenants') {
        return Response.json({
          tenants: [{ id: 'platform.acme', name: 'Acme', node_count: 2, user_count: 1, operator_count: 1 }],
        });
      }
      if (path === '/users?include_disabled=true') return Response.json({ users: [alice] });
      if (path === '/grant-probes/capability') {
        return Response.json({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/apps/app-main/front-analysis' && init?.method === 'POST') {
        return Response.json(routeAnalysisFixture());
      }
      if (path === '/apps/app-main/fronts/front-asia' && (init?.method ?? 'GET') === 'GET') {
        return Response.json({
          front_id: 'front-asia',
          head_snapshot_id: 14,
          serving_snapshot_id: 14,
          topology_revision_id: 5,
          permissions_revision_id: 7,
          serving_generation: 23,
          active: true,
          pending_topology: [],
        });
      }
      if (path === '/users/platform.acme/alice/front-probes' && init?.method === 'POST') {
        requestBody = JSON.parse(String(init.body)) as Record<string, unknown>;
        return Response.json({
          reused: false,
          job: {
            id: 'p-live',
            kind: 'front-combination',
            tenant_id: 'platform.acme',
            user_id: 'alice',
            serving_revision: 5,
            serving_generation: 23,
            client_snapshot_id: 14,
            timeout_secs: 10,
            status: 'completed',
            message: '网络拨测全部通过',
            created_at_unix_secs: 1,
            finished_at_unix_secs: 2,
            items: [
              {
                id: 'member=>target',
                name: '香港 → 美国',
                app_id: 'app-main',
                app_name: '全球加速',
                chain_id: 'chain',
                ingress_id: 'ing-us',
                family: 'ipv4',
                protocol: 'vless',
                member_id: 'ing-hk',
                member_name: '香港',
                member_family: 'ipv4',
                member_protocol: 'vless',
                status: 'passed',
                ttfb_ms: 42,
                detail: null,
              },
            ],
          },
        });
      }
      throw new Error(`unexpected request ${path}`);
    }),
  );

  const view = renderDetail();
  const probe = await view.findByRole('button', { name: '实测这条路径' });
  await waitFor(() => expect((probe as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(probe);
  await view.findByText('网络拨测全部通过');
  expect(requestBody).toEqual({
    app_id: 'app-main',
    front_id: 'front-asia',
    member_id: 'ing-hk',
    target_id: 'ing-us',
    expected_serving_generation: 23,
    expected_client_snapshot_id: 14,
  });
  expect(view.getByText('通过 · 42ms')).toBeTruthy();
  expect(view.getByText(/Serving G23 · 客户端 #14/)).toBeTruthy();
  expect(view.getByText(/完成于 .* · 单项超时 10s/)).toBeTruthy();
});

it('fails closed when the authoritative connectivity result is unavailable', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/model/snapshot') {
        return Response.json({ snapshot: { revision: 8, apps: [chainProxyFixture()], external_outbounds: [] } });
      }
      if (path === '/tenants') {
        return Response.json({
          tenants: [{ id: 'platform.acme', name: 'Acme', node_count: 2, user_count: 1, operator_count: 1 }],
        });
      }
      if (path === '/users?include_disabled=true') return Response.json({ users: [alice] });
      if (path === '/grant-probes/capability') {
        return Response.json({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/apps/app-main/front-analysis' && init?.method === 'POST') {
        return Response.json({ error: 'analysis unavailable' }, { status: 503 });
      }
      if (path === '/apps/app-main/fronts/front-asia' && (init?.method ?? 'GET') === 'GET') {
        return Response.json({
          front_id: 'front-asia',
          head_snapshot_id: 14,
          serving_snapshot_id: 14,
          topology_revision_id: 5,
          permissions_revision_id: 7,
          serving_generation: 23,
          active: true,
          pending_topology: [],
        });
      }
      throw new Error(`unexpected request ${path}`);
    }),
  );

  const view = renderDetail();
  expect((await view.findByText(/连通性结果不可用/)).textContent).toContain('不会开放保存');
  expect((view.getByRole('button', { name: '连通性不可用' }) as HTMLButtonElement).disabled).toBe(true);
});
