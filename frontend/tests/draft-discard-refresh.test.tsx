/* 丢弃草稿后界面是否回到已提交状态。
 *
 * 这一组用真实的取数路径而非 `setQueryData` 预置：`fetchSnapshot` 会按 `draft.isEmpty()`
 * 在 `/model/snapshot`（已提交）和 `/model/preview`（草稿生效后）之间分叉，预置缓存会把
 * 这个分叉整个跳过，而它正是被怀疑的地方。因此此处 stub 的是 `fetch`。
 *
 * 同时复刻 `forge/shell.tsx` 里那个集中失效器：草稿任何变动都失效 `['snapshot']` 与
 * `['compile']`。它在生产里始终挂载，不复刻就测不到真实行为。 */
import { useEffect, useState } from 'react';
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

/* nodes.tsx 经 ui/topbar → ui/artifact-panel → ui/viewport 在模块求值期读 matchMedia，
   jsdom 没有实现。必须在 import 之前装好，import 是提升的，所以放在这里而不是 beforeEach。 */
window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { fetchSnapshot, setWireGuardLinkDisabled } = await import('../src/api');
type NodeAgentStateItem = import('../src/api').NodeAgentStateItem;
const { draft } = await import('../src/draft');
const { DraftBar } = await import('../src/forge/draft-bar');
const { SessionProvider } = await import('../src/session');
const { DnsCard, WgCard } = await import('../src/panes/nodes');

const SESSION = {
  who: {
    operator_id: 'tester',
    role: 'system-admin' as const,
    tenant_scope: null,
    token_prefix: null,
    masked_assets: false,
  },
};

const HERE = 'hkg-01';
const PEER = 'tyo-01';

function node(id: string, name: string): NodeAgentStateItem {
  return {
    node_id: id,
    tenant_id: 'platform',
    name,
    public_ipv4: null,
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    route_ipv4: null,
    route_ipv6: null,
    token_prefix: null,
    token_created_at: null,
    token_last_used_at: null,
    token_revoked_at: null,
    agent_version: null,
    agent_protocol_version: null,
    runtime_versions: null,
    spool_backlog: null,
    last_local_reconcile: null,
    wireguard_health: null,
    runtime_reported_at: null,
    geodata_observed: null,
    last_poll_at: null,
    last_usage_report_at: null,
    xray_started_at: null,
    mtu: null,
    connection: { conn_idle_secs: null, uplink_only_secs: null, downlink_only_secs: null, buffer_size_kb: null },
    overlay: true,
    egress_allowed: true,
    dns: { t: 'system' },
    domain_strategy: 'use_ip',
    retired_at: null,
    lifecycle_phase: 'active',
    lifecycle_epoch: 1,
    lifecycle_deployment_id: null,
    lifecycle_completed_at: null,
    lifecycle_last_error: null,
    operationally_isolated: false,
    isolated_at: null,
    convergence_debt_count: 0,
    convergence_debt_failed: false,
    service_reentry_ready: false,
    service_reentry_blockers: [],
    wg_transport_kind: 'udp',
    wg_fake_tcp_port: null,
    applied: null,
  };
}

/** 已提交模型：没有任何禁用组合。 */
const committedSnapshot = (): {
  snapshot: {
    revision: number;
    apps: never[];
    nodes: Record<string, unknown>[];
    settings: { overlay: { keepalive_secs: number; mtu: number; disabled_links: { a: string; b: string }[] } };
  };
  node_egress_dns: never[];
  redacted: boolean;
} => ({
  snapshot: {
    revision: 7,
    apps: [],
    nodes: [
      { id: HERE, overlay: true },
      { id: PEER, overlay: true },
    ],
    settings: { overlay: { keepalive_secs: 25, mtu: 1420, disabled_links: [] } },
  },
  node_egress_dns: [],
  redacted: false,
});

/** 草稿生效后的模型。服务端的做法是开事务、正常执行这些 op、读取结果、回滚；
    此处按同样的语义回放本组用到的两种 op，产出等价的快照。 */
type StubOp = {
  op: string;
  a?: string;
  b?: string;
  disabled?: boolean;
  node_id?: string;
  node?: Record<string, unknown>;
};

const previewSnapshot = (ops: StubOp[]) => {
  const base = committedSnapshot();
  for (const op of ops) {
    if (op.op === 'set_wireguard_link_disabled' && op.disabled) {
      base.snapshot.settings.overlay.disabled_links.push({ a: op.a!, b: op.b! });
    }
    if (op.op === 'update_node') {
      const row = base.snapshot.nodes.find(n => n.id === op.node_id);
      /* mtu 的 0 表示清空并回退到全局默认值，与 updateNode 中该字段的约定一致 */
      if (row && op.node) Object.assign(row, op.node, 'mtu' in op.node ? { mtu: op.node.mtu || null } : {});
    }
  }
  return base;
};

let snapshotCalls = 0;
let previewCalls = 0;

function stubFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      const json = (body: unknown) =>
        new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
      if (path === '/model/snapshot') {
        snapshotCalls += 1;
        return json(committedSnapshot());
      }
      if (path === '/links/mtu') {
        return json({ default_mtu: 1420, nodes: [], links: [] });
      }
      if (path === '/model/preview') {
        previewCalls += 1;
        const ops = (
          JSON.parse(String(init?.body)) as { ops: { op: string; a?: string; b?: string; disabled?: boolean }[] }
        ).ops;
        return json({
          snapshot: previewSnapshot(ops),
          compile: { diagnostics: [], summary: {}, system: { nodes: [] }, apps: [] },
          artifacts: { revision: 7, artifacts: [] },
        });
      }
      throw new Error(`未预期的请求：${path}`);
    }),
  );
}

/* 生产里 WgCard 的 disabledLinks 来自 NodeDetail 的 `useQuery(['snapshot'])`，
   而顶栏的集中失效器挂在 shell 上。两者都复刻，缺一个都测不出真实行为。 */
function Harness() {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
      }),
  );
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={SESSION}>
        <ShellDraftInvalidation />
        <DraftBar current={7} />
        <ConfigTab />
      </SessionProvider>
    </QueryClientProvider>
  );
}

function ShellDraftInvalidation() {
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        qc.invalidateQueries({ queryKey: ['snapshot'] });
        qc.invalidateQueries({ queryKey: ['compile'] });
      }),
    [qc],
  );
  return null;
}

function ConfigTab() {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const here = node(HERE, '香港 01');
  return (
    <>
      <WgCard
        node={here}
        listenPort={51820}
        peers={[here, node(PEER, '东京 01')]}
        disabledLinks={snapshot.data?.snapshot.settings?.overlay.disabled_links ?? []}
        enabled
        canEdit
        onSaved={() => {}}
      />
      <DnsCard node={here} canEdit onSaved={() => {}} />
    </>
  );
}

beforeEach(() => {
  snapshotCalls = 0;
  previewCalls = 0;
  draft.init(`discard-refresh-${Math.random()}`);
  draft.clear();
  stubFetch();
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('丢弃草稿后禁 Peer 组合的显示', () => {
  it('全部丢弃之后回到已提交的空列表', async () => {
    render(<Harness />);

    await waitFor(() => expect(screen.getByText('暂无禁用组合')).toBeTruthy());

    /* 加入禁用：写草稿，界面应显示该组合 */
    setWireGuardLinkDisabled(HERE, PEER, true);
    await waitFor(() => expect(screen.getByText('东京 01')).toBeTruthy());
    expect(previewCalls).toBeGreaterThan(0);

    /* 顶栏草稿条的「全部丢弃」 */
    const before = snapshotCalls;
    fireEvent.click(screen.getByText('全部丢弃'));

    await waitFor(() => expect(screen.getByText('暂无禁用组合')).toBeTruthy());
    /* 并且确实重新打了已提交接口，而不是命中旧缓存后碰巧显示为空 */
    expect(snapshotCalls).toBeGreaterThan(before);
  });
});

/* 写草稿的行必须从草稿感知的数据源读回来。`/nodes/agent-state`（`['nodes']`）始终返回
   已提交值，草稿未提交前它不会变——以它为基准的行在保存后会弹回改动前的内容。
   OverlayRow / WgTransportRow / ConnectionCard 的注释都记录过这个坑并已改用
   compile / snapshot；MTU 这一行没有。 */
describe('写草稿的行保存后是否保持新值', () => {
  it('MTU 保存到草稿后输入框应保持新值', async () => {
    render(<Harness />);

    const input = (await screen.findByLabelText('wg0 MTU')) as HTMLInputElement;
    expect(input.value).toBe('');

    fireEvent.change(input, { target: { value: '1380' } });
    expect(input.value).toBe('1380');

    fireEvent.click(screen.getByText('保存到草稿'));

    /* 草稿里确实记下了这次修改 */
    await waitFor(() => expect(draft.ops()).toContainEqual({ op: 'update_node', node_id: HERE, node: { mtu: 1380 } }));

    /* 保存后输入框必须仍是 1380。读 `['nodes']` 时它会回落为空——草稿里记下了这次修改，
       界面却显示什么都没发生。 */
    await waitFor(() => expect(input.value).toBe('1380'));
  });

  it('DNS 保存到草稿后两项都应保持新值', async () => {
    render(<Harness />);

    const servers = (await screen.findByPlaceholderText('system 或 1.1.1.1,8.8.8.8')) as HTMLInputElement;
    await waitFor(() => expect(servers.value).toBe('system'));

    fireEvent.change(servers, { target: { value: '1.1.1.1, 8.8.8.8' } });
    const strategy = screen.getByDisplayValue('UseIP') as HTMLSelectElement;
    fireEvent.change(strategy, { target: { value: 'use_ipv4v6' } });

    fireEvent.click(screen.getByText('保存到草稿'));

    await waitFor(() =>
      expect(draft.ops()).toContainEqual({
        op: 'update_node',
        node_id: HERE,
        node: { dns: { t: 'servers', v: ['1.1.1.1', '8.8.8.8'] }, domain_strategy: 'use_ipv4v6' },
      }),
    );

    await waitFor(() => {
      expect(servers.value).toBe('1.1.1.1, 8.8.8.8');
      expect(strategy.value).toBe('use_ipv4v6');
    });
  });
});
