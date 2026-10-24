import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { BinaryRelease, BinaryReleaseView } from '../src/api';
import { XrayReleaseTab, useXrayRelease } from '../src/panes/xray-release';

const OLD_A = 'a'.repeat(64);
const OLD_B = 'b'.repeat(64);
const NEXT = 'c'.repeat(64);
const RELEASE_ID = 'd'.repeat(64);
const SOURCE_COMMIT = '77ae5216cf2132fcf300fac1a6a4998ea54378ba';

const emptyView = (): BinaryReleaseView => ({
  available: {
    component: 'xray',
    build_id: RELEASE_ID,
    artifacts: [{ arch: 'x86_64', sha256: NEXT }],
    version: SOURCE_COMMIT,
    source: 'embedded',
  },
  current: null,
  legacy_approval: null,
});

const nodes = {
  nodes: [
    {
      node_id: 'edge-a',
      name: '台北入口',
      agent_protocol_version: 30,
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: {
        xray: 'Xray 26.8.1',
        xray_installed_sha256: OLD_A,
        xray_running_sha256: OLD_A,
        xray_handoff: { state: 'ready' as 'ready' | 'blocked' },
      },
    },
    {
      node_id: 'edge-b',
      name: '东京中继',
      agent_protocol_version: 30,
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: {
        xray: 'Xray 26.8.1',
        xray_installed_sha256: OLD_B,
        xray_running_sha256: OLD_B,
        xray_handoff: { state: 'ready' as 'ready' | 'blocked' },
      },
    },
    {
      node_id: 'legacy',
      name: '旧 Agent',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: null,
    },
    {
      node_id: 'current',
      name: '已经替换',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: {
        xray: 'Xray 26.9.1',
        xray_installed_sha256: NEXT,
        xray_running_sha256: NEXT,
      },
    },
    {
      node_id: 'stopped',
      name: '未运行 Xray',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: { xray_installed_sha256: OLD_A, xray_running_sha256: null },
    },
    {
      node_id: 'stale',
      name: '离线机器',
      lifecycle_phase: 'active',
      desired_poll_fresh: false,
      runtime_report_fresh: false,
      runtime_versions: { xray_installed_sha256: OLD_A, xray_running_sha256: OLD_A },
    },
  ],
};

/** 发布页把编辑状态放在页头；这里直接进入编辑，只测 Xray 页签本身。 */
function XrayHarness() {
  const xray = useXrayRelease();
  const [editing, setEditing] = useState(true);
  return <XrayReleaseTab xray={xray} editable editing={editing} onEditingChange={setEditing} />;
}

const clients: QueryClient[] = [];
function mount(view: BinaryReleaseView, nodeView = nodes, handoffBeta = false) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Number.POSITIVE_INFINITY },
      mutations: { retry: false },
    },
  });
  clients.push(client);
  client.setQueryData(['binary-releases', 'xray'], view);
  client.setQueryData(['nodes'], nodeView);
  client.setQueryData(['deployment-settings'], { unattended: true, xray_handoff_beta: handoffBeta });
  return render(
    <QueryClientProvider client={client}>
      <XrayHarness />
    </QueryClientProvider>,
  );
}

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  vi.unstubAllGlobals();
});

describe('Xray 升级', () => {
  it('Beta 关闭只允许直接重启，不显示交接或排空选项', () => {
    mount(emptyView());
    expect(screen.getAllByRole('radio')).toHaveLength(1);
    expect((screen.getByRole('radio', { name: '直接重启' }) as HTMLInputElement).checked).toBe(true);
    expect(screen.queryByRole('radio', { name: '双跑交接 Beta' })).toBeNull();
    expect(screen.queryByRole('spinbutton', { name: '旧进程排空等待秒数' })).toBeNull();
  });

  it('关闭 Beta 后清除尚未批准的交接选择，再开启仍默认重启', async () => {
    mount(emptyView(), nodes, true);
    fireEvent.click(screen.getByRole('radio', { name: '双跑交接 Beta' }));
    clients.at(-1)!.setQueryData(['deployment-settings'], { unattended: true, xray_handoff_beta: false });
    await waitFor(() => expect(screen.queryByRole('radio', { name: '双跑交接 Beta' })).toBeNull());
    clients.at(-1)!.setQueryData(['deployment-settings'], { unattended: true, xray_handoff_beta: true });
    await waitFor(() => expect(screen.getByRole('radio', { name: '双跑交接 Beta' })).toBeTruthy());
    expect((screen.getByRole('radio', { name: '直接重启' }) as HTMLInputElement).checked).toBe(true);
  });

  it('v40 允许交接条件不足时降级，旧 Agent 仍需 ready', () => {
    const candidates = structuredClone(nodes);
    for (const node of candidates.nodes.slice(0, 2)) {
      node.runtime_versions!.xray_handoff!.state = 'blocked';
    }
    candidates.nodes[0].agent_protocol_version = 40;
    candidates.nodes[1].agent_protocol_version = 39;
    mount(emptyView(), candidates, true);
    fireEvent.click(screen.getByRole('radio', { name: '双跑交接 Beta' }));
    expect((screen.getByLabelText('升级 台北入口') as HTMLInputElement).disabled).toBe(false);
    expect((screen.getByLabelText('升级 东京中继') as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByLabelText('升级 台北入口'));
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    expect(
      within(screen.getByRole('dialog', { name: '确认双跑升级？' })).getByText(/交接条件不足时会降级直接重启/),
    ).toBeTruthy();
  });

  it('显示源码 commit，按实际运行摘要识别当前构建，不显示上游版本号', () => {
    mount(emptyView());
    expect(screen.getByText('可发 commit')).toBeTruthy();
    const current = screen.getByRole('rowheader', { name: '已经替换' }).closest<HTMLElement>('[role="row"]')!;
    expect(within(current).getByText('77ae5216cf21')).toBeTruthy();
    const old = screen.getByRole('rowheader', { name: '台北入口' }).closest<HTMLElement>('[role="row"]')!;
    expect(within(old).getByText('commit 未知')).toBeTruthy();
    expect(screen.queryByText(/v?26\.(8|9)\.1/)).toBeNull();
  });

  it('默认直接重启，显式选择双跑交接 Beta 后默认排空 120 秒', async () => {
    const view = emptyView();
    let sent: unknown;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input) === '/nodes/agent-state') return json(nodes);
        expect(String(input)).toBe('/binary-releases/xray');
        expect(init?.method).toBe('POST');
        sent = JSON.parse(String(init?.body));
        return json(view);
      }),
    );
    mount(view, nodes, true);

    expect(screen.getAllByRole('radio')).toHaveLength(2);
    expect((screen.getByRole('radio', { name: '直接重启' }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole('radio', { name: '双跑交接 Beta' }) as HTMLInputElement).checked).toBe(false);
    expect(screen.queryByRole('radio', { name: '平滑交接' })).toBeNull();
    expect(screen.queryByRole('spinbutton', { name: '旧进程排空等待秒数' })).toBeNull();
    fireEvent.click(screen.getByRole('radio', { name: '双跑交接 Beta' }));
    expect((screen.getByRole('spinbutton', { name: '旧进程排空等待秒数' }) as HTMLInputElement).value).toBe('120');
    expect((screen.getByLabelText('升级 旧 Agent') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('升级 离线机器') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('升级 已经替换') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('升级 未运行 Xray') as HTMLInputElement).disabled).toBe(true);
    // 不能升级的机器在名下写出原因，而不是只给一个灰掉的勾选框。
    expect(screen.getByText('最近 90 秒没有领取期望状态')).toBeTruthy();
    expect(screen.getByText('Xray 当前未运行')).toBeTruthy();
    fireEvent.click(screen.getByLabelText('升级 台北入口'));
    fireEvent.click(screen.getByLabelText('升级 东京中继'));
    expect(screen.getByText('已选 2 台')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    const confirmation = screen.getByRole('dialog', { name: '确认双跑升级？' });
    expect(screen.getByText(/旧连接最多保留 120 秒/)).toBeTruthy();
    expect(within(confirmation).queryByText(/超时|破坏性/)).toBeNull();
    expect(within(confirmation).getByRole('button', { name: '确认升级' }).classList.contains('danger')).toBe(false);
    expect(sent).toBeUndefined();
    fireEvent.click(within(confirmation).getByRole('button', { name: '确认升级' }));

    await waitFor(() =>
      expect(sent).toEqual({
        idempotency_key: expect.any(String),
        build_id: RELEASE_ID,
        nodes: ['edge-a', 'edge-b'],
        note: null,
        xray_strategy: 'handoff-force',
        xray_drain_timeout_secs: 120,
      }),
    );
    expect(screen.queryByText(/分批|波次/)).toBeNull();
    expect(screen.getByRole('button', { name: '发布历史' })).toBeTruthy();
  });

  it('取消默认重启确认时不创建发布，保留已选机器', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    mount(emptyView());
    fireEvent.click(screen.getByLabelText('升级 台北入口'));
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    const dialog = screen.getByRole('dialog', { name: '确认直接重启 Xray？' });
    fireEvent.click(within(dialog).getByRole('button', { name: '取消' }));

    expect(fetch).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect((screen.getByLabelText('升级 台北入口') as HTMLInputElement).checked).toBe(true);
  });

  it('调整双跑排空时间后仍提交强制退役策略', async () => {
    const view = emptyView();
    let sent: unknown;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input) === '/nodes/agent-state') return json(nodes);
        expect(String(input)).toBe('/binary-releases/xray');
        sent = JSON.parse(String(init?.body));
        return json(view);
      }),
    );
    mount(view, nodes, true);
    fireEvent.click(screen.getByRole('radio', { name: '双跑交接 Beta' }));
    fireEvent.change(screen.getByRole('spinbutton', { name: '旧进程排空等待秒数' }), {
      target: { value: '240' },
    });
    fireEvent.click(screen.getByLabelText('升级 台北入口'));
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    expect(screen.getByText(/旧连接最多保留 240 秒/)).toBeTruthy();
    expect(sent).toBeUndefined();
    fireEvent.click(screen.getByRole('button', { name: '确认升级' }));

    await waitFor(() =>
      expect(sent).toEqual({
        idempotency_key: expect.any(String),
        build_id: RELEASE_ID,
        nodes: ['edge-a'],
        note: null,
        xray_strategy: 'handoff-force',
        xray_drain_timeout_secs: 240,
      }),
    );
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('进行中保持逐台状态，历史通过独立入口按需读取', async () => {
    const release: BinaryRelease = {
      id: 7,
      component: 'xray',
      build_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'halted',
      active: true,
      note: null,
      created_at: '2026-09-12T00:00:00Z',
      created_by: 'admin',
      halted_at: '2026-09-12T00:02:00Z',
      finished_at: null,
      events: [],
      next_event_before_id: null,
      targets: [
        {
          node_id: 'edge-a',
          status: 'succeeded',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: true,
          reported_service_enabled: true,
          reported_installed_sha256: NEXT,
          reported_running_sha256: NEXT,
          verification: 'receipt',
          retryable: false,
          dispatched_at: '2026-09-12T00:01:00Z',
          finished_at: '2026-09-12T00:02:00Z',
        },
        {
          node_id: 'edge-b',
          status: 'failed-recovered',
          attempt: 1,
          before_sha256: OLD_B,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: '启动检查失败',
          reported_performed_update: true,
          reported_service_enabled: true,
          reported_installed_sha256: OLD_B,
          reported_running_sha256: OLD_B,
          verification: 'receipt',
          retryable: true,
          dispatched_at: '2026-09-12T00:01:00Z',
          finished_at: '2026-09-12T00:02:00Z',
        },
      ],
    };
    const view = { ...emptyView(), current: release };
    let requested = '';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/nodes/agent-state') return json(nodes);
        requested = String(input);
        return json(view);
      }),
    );
    mount(view);

    expect(screen.getByText('已升级', { selector: '.cgc-st' })).toBeTruthy();
    expect(screen.getByText('失败 · 原版本运行', { selector: '.cgc-st' })).toBeTruthy();
    // 失败原因写在机器名下；进行中不能再勾选新的机器。
    expect(screen.getByText('启动检查失败')).toBeTruthy();
    expect(screen.queryByRole('group', { name: '升级范围' })).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByRole('button', { name: /波/ })).toBeNull();
    expect(screen.queryByRole('button', { name: '查看审计' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    await waitFor(() => expect(requested).toBe('/binary-releases/xray/7/targets/edge-b/retry'));
  });

  it('仅在当前发布中展示经过身份校验的双进程状态', () => {
    const release: BinaryRelease = {
      id: 17,
      component: 'xray',
      build_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'running',
      active: true,
      note: null,
      xray_strategy: 'handoff',
      xray_drain_timeout_secs: 300,
      created_at: '2026-10-15T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: null,
      events: [],
      next_event_before_id: null,
      targets: [
        {
          node_id: 'edge-a',
          status: 'dispatched',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: null,
          reported_service_enabled: null,
          reported_installed_sha256: null,
          reported_running_sha256: null,
          verification: null,
          retryable: false,
          dispatched_at: '2026-10-15T00:00:10Z',
          finished_at: null,
        },
      ],
    };
    const nodeView = {
      nodes: nodes.nodes.map(node =>
        node.node_id === 'edge-a'
          ? {
              ...node,
              xray_upgrade: {
                release_id: 17,
                attempt: 1,
                strategy: 'handoff' as const,
                phase: 'draining' as const,
                old: {
                  pid: 101,
                  pid_start_ticks: 1001,
                  boot_id: '01234567-89ab-cdef-0123-456789abcdef',
                  sha256: OLD_A,
                  alive: true,
                  accepting: false,
                  active_connections: 4,
                },
                candidate: {
                  pid: 202,
                  pid_start_ticks: 1002,
                  boot_id: '01234567-89ab-cdef-0123-456789abcdef',
                  sha256: NEXT,
                  alive: true,
                  accepting: true,
                  active_connections: 1,
                },
                drain_deadline_unix_secs: 1_800_000_000,
              },
            }
          : node,
      ),
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/nodes/agent-state') return json(nodeView);
        return json({ ...emptyView(), current: release });
      }),
    );

    mount({ ...emptyView(), current: release }, nodeView);

    expect(screen.getByText(/阶段 draining · 旧 PID 101 · 新 PID 202 · 旧连接 4/)).toBeTruthy();
  });

  it('一小时后把仍在执行的 handoff 标为整体超时但不伪造终态', () => {
    const release: BinaryRelease = {
      id: 18,
      component: 'xray',
      build_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'running',
      active: true,
      note: null,
      xray_strategy: 'handoff',
      xray_drain_timeout_secs: 300,
      created_at: '2026-10-15T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: null,
      events: [],
      next_event_before_id: null,
      targets: [
        {
          node_id: 'edge-a',
          status: 'dispatched',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: null,
          reported_service_enabled: null,
          reported_installed_sha256: null,
          reported_running_sha256: null,
          verification: null,
          retryable: true,
          dispatched_at: '2026-10-15T00:00:00Z',
          execution_deadline_at: '2026-10-15T01:00:00Z',
          execution_timed_out: true,
          finished_at: null,
        },
      ],
    };

    mount({ ...emptyView(), current: release });

    expect(screen.getAllByText('整体执行超时')).toHaveLength(2);
    expect(screen.getByText(/超过 1 小时仍未收到终态回执.*不会自动终止 Xray/)).toBeTruthy();
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  });

  it('同一次 handoff 仍上报双进程时不允许从页面重试', () => {
    const release: BinaryRelease = {
      id: 19,
      component: 'xray',
      build_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'running',
      active: true,
      note: null,
      xray_strategy: 'handoff',
      xray_drain_timeout_secs: 300,
      created_at: '2026-10-15T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: null,
      events: [],
      next_event_before_id: null,
      targets: [
        {
          node_id: 'edge-a',
          status: 'dispatched',
          attempt: 2,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: null,
          reported_service_enabled: null,
          reported_installed_sha256: null,
          reported_running_sha256: null,
          verification: null,
          retryable: true,
          dispatched_at: '2026-10-15T00:00:00Z',
          execution_deadline_at: '2026-10-15T01:00:00Z',
          execution_timed_out: true,
          finished_at: null,
        },
      ],
    };
    const nodeView = {
      nodes: nodes.nodes.map(node =>
        node.node_id === 'edge-a'
          ? {
              ...node,
              xray_upgrade: {
                release_id: 19,
                attempt: 2,
                strategy: 'handoff' as const,
                phase: 'draining' as const,
                old: {
                  pid: 101,
                  pid_start_ticks: 1001,
                  boot_id: '01234567-89ab-cdef-0123-456789abcdef',
                  sha256: OLD_A,
                  alive: true,
                  accepting: false,
                  active_connections: 4,
                },
                candidate: {
                  pid: 202,
                  pid_start_ticks: 1002,
                  boot_id: '01234567-89ab-cdef-0123-456789abcdef',
                  sha256: NEXT,
                  alive: true,
                  accepting: true,
                  active_connections: 1,
                },
              },
            }
          : node,
      ),
    };

    mount({ ...emptyView(), current: release }, nodeView);

    expect(screen.getByText(/阶段 draining · 旧 PID 101 · 新 PID 202 · 旧连接 4/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: '重试' })).toBeNull();
  });

  it.each([
    [false, false],
    [true, false],
    [true, true],
  ])('强制退役=%s、恢复=%s 的成功统一显示绿色已升级，真实异常恢复仍保留说明', (forced, recovered) => {
    const release: BinaryRelease = {
      id: 20,
      component: 'xray',
      build_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'running',
      active: true,
      note: null,
      xray_strategy: 'handoff-force',
      xray_drain_timeout_secs: 300,
      created_at: '2026-10-15T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: null,
      events: [],
      next_event_before_id: null,
      targets: [
        {
          node_id: 'edge-a',
          status: 'succeeded',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: true,
          reported_service_enabled: true,
          reported_installed_sha256: NEXT,
          reported_running_sha256: NEXT,
          reported_forced_retirement: forced,
          recovered_after_dirty: recovered,
          verification: 'receipt',
          retryable: false,
          dispatched_at: '2026-10-15T00:00:10Z',
          finished_at: '2026-10-15T00:05:00Z',
        },
        {
          node_id: 'edge-b',
          status: 'succeeded',
          attempt: 1,
          before_sha256: OLD_B,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: true,
          reported_service_enabled: true,
          reported_installed_sha256: NEXT,
          reported_running_sha256: NEXT,
          reported_forced_retirement: false,
          recovered_after_dirty: true,
          verification: 'receipt',
          retryable: false,
          dispatched_at: '2026-10-15T00:00:10Z',
          finished_at: '2026-10-15T00:06:00Z',
        },
        {
          node_id: 'legacy',
          status: 'pending',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: null,
          arch: null,
          error: null,
          reported_performed_update: null,
          reported_service_enabled: null,
          reported_installed_sha256: null,
          reported_running_sha256: null,
          reported_forced_retirement: null,
          recovered_after_dirty: false,
          verification: null,
          retryable: false,
          dispatched_at: null,
          finished_at: null,
        },
      ],
    };

    const nodeView = {
      nodes: nodes.nodes.map(node =>
        node.node_id === 'edge-a'
          ? {
              ...node,
              xray_upgrade: {
                release_id: 20,
                attempt: 1,
                strategy: 'handoff-force' as const,
                phase: 'blocked' as const,
                old: {
                  pid: 101,
                  pid_start_ticks: 1001,
                  boot_id: '01234567-89ab-cdef-0123-456789abcdef',
                  sha256: OLD_A,
                  alive: true,
                  accepting: false,
                  active_connections: 4,
                },
                blocked_reason: '旧代仍有 4 条连接，继续保留并等待自然排空',
              },
            }
          : node,
      ),
    };
    mount({ ...emptyView(), current: release }, nodeView);

    const upgraded = screen.getByText('已升级', { selector: '.cgc-st' });
    expect(upgraded.closest('.cgc-tr')?.classList.contains('ok')).toBe(true);
    const ledger = screen.getByRole('region', { name: '已升级' });
    expect(within(ledger).getByText('已升级', { selector: 'dt' }).closest('div')?.classList.contains('cgc-io-ok')).toBe(
      true,
    );
    expect(screen.queryByText('强制退役完成')).toBeNull();
    expect(screen.getByText('自动恢复完成', { selector: '.cgc-st' })).toBeTruthy();
    expect(screen.queryByText(/旧进程未在等待期内自然排空/)).toBeNull();
    expect(screen.queryByText(/旧代仍有 4 条连接/)).toBeNull();
    expect(screen.getByText(/Agent 随后完成本地恢复/)).toBeTruthy();
  });

  it('默认直接重启必须二次确认，请求不携带排空时间', async () => {
    const view = emptyView();
    let sent: unknown;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input) === '/nodes/agent-state') return json(nodes);
        sent = JSON.parse(String(init?.body));
        return json(view);
      }),
    );
    mount(view);
    expect((screen.getByRole('radio', { name: '直接重启' }) as HTMLInputElement).checked).toBe(true);
    expect(screen.queryByRole('spinbutton', { name: '旧进程排空等待秒数' })).toBeNull();
    fireEvent.click(screen.getByLabelText('升级 台北入口'));
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    expect(screen.getByText('确认直接重启 Xray？')).toBeTruthy();
    expect(sent).toBeUndefined();
    fireEvent.click(screen.getByRole('button', { name: '确认破坏性升级' }));

    await waitFor(() =>
      expect(sent).toEqual({
        idempotency_key: expect.any(String),
        build_id: RELEASE_ID,
        nodes: ['edge-a'],
        note: null,
        xray_strategy: 'restart',
      }),
    );
  });
});
