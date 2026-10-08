import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DeploymentListItem, BinaryRelease } from '../src/api';
import type { Win } from '../src/wm/store';

window.matchMedia = ((media: string) => ({
  matches: false,
  media,
  addEventListener() {},
  removeEventListener() {},
})) as unknown as typeof window.matchMedia;

const { draft } = await import('../src/draft');
const { DeployPane } = await import('../src/panes/deploy');
const { SessionProvider } = await import('../src/session');

const initial = { node_count: 1, chain_group_count: [] };
const who = {
  operator_id: 'root',
  role: 'system-admin' as const,
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};
const win: Win = {
  id: 72,
  key: 'tab:deploy',
  title: '发布',
  x: 0,
  y: 0,
  w: 1280,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'list' } },
};

function deployment(id: number): DeploymentListItem {
  return {
    id,
    revision_id: id,
    status: 'succeeded',
    activation_status: 'activated',
    settlement_status: 'converged',
    activated_at: '2026-09-16T00:00:00Z',
    active: false,
    actor: 'root',
    kind: id === 14 ? 'grants' : 'config',
    note: `发布记录 ${id}`,
    base_revision_id: id - 1,
    rollback_of_deployment_id: null,
    sync_of_deployment_id: null,
    created_at: `2026-09-${String(id + 1).padStart(2, '0')}T00:00:00Z`,
    started_at: '2026-09-16T00:00:00Z',
    finished_at: '2026-09-16T00:01:00Z',
    total_targets: 1,
    changed_targets: 1,
    skipped_targets: 0,
    failed_targets: 0,
    debt_targets: 0,
    disruptive_targets: 0,
    max_wave: 0,
    awaiting_confirmation: false,
  };
}

const activeRelease: BinaryRelease = {
  id: 7,
  component: 'xray',
  build_id: 'xray-release',
  version: '26.4.25',
  artifacts: [{ arch: 'x86_64', sha256: 'c'.repeat(64) }],
  status: 'running',
  active: true,
  note: null,
  created_at: '2026-09-16T00:00:00Z',
  created_by: 'root',
  halted_at: null,
  finished_at: null,
  events: [],
  next_event_before_id: null,
  targets: [
    {
      node_id: 'edge',
      status: 'dispatched',
      attempt: 1,
      before_sha256: 'b'.repeat(64),
      desired_sha256: 'c'.repeat(64),
      arch: 'x86_64',
      error: null,
      reported_performed_update: null,
      reported_service_enabled: null,
      reported_installed_sha256: null,
      reported_running_sha256: null,
      verification: null,
      retryable: false,
      dispatched_at: '2026-09-16T00:00:30Z',
      finished_at: null,
    },
  ],
};

function mount({ releases = [] as BinaryRelease[] } = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY }, mutations: { retry: false } },
  });
  const deployments = Array.from({ length: 14 }, (_, index) => deployment(14 - index));
  client.setQueryData(['deployments'], { deployments });
  client.setQueryData(['revisions'], {
    current_revision: 14,
    revisions: [
      {
        id: 14,
        created_at: '2026-09-16T00:00:00Z',
        author: 'root',
        note: '调整代理出站',
        status: 'committed',
        current: true,
        has_snapshot: true,
      },
    ],
  });
  client.setQueryData(['deployment-verify', 14], {
    summary: { changed_targets: 0 },
  });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'edge',
        name: '东京入口',
        agent_version: 'a'.repeat(64),
        lifecycle_phase: 'active',
        desired_poll_fresh: true,
        runtime_report_fresh: true,
        runtime_versions: {
          xray: 'Xray 25.8.3 (Xray, Penetrates Everything.) b4f0898 (go1.26.0 linux/amd64)',
          xray_installed_sha256: 'b'.repeat(64),
          xray_running_sha256: 'b'.repeat(64),
        },
      },
    ],
  });
  client.setQueryData(['binary-releases', 'agent'], {
    available: {
      component: 'agent',
      version: '0.2.0',
      build_id: 'a'.repeat(64),
      artifacts: [{ arch: 'x86_64', sha256: 'a'.repeat(64) }],
      source: 'embedded',
    },
    current: null,
    legacy_approval: null,
  });
  client.setQueryData(['binary-releases', 'xray'], {
    available: {
      component: 'xray',
      build_id: 'xray-release',
      version: '26.4.25',
      artifacts: [{ arch: 'x86_64', sha256: 'c'.repeat(64) }],
      source: 'embedded',
    },
    current: releases[0] ?? null,
    legacy_approval: null,
  });

  return render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial }}>
        <DeployPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('发布页', () => {
  it('照机器详情页分页签，默认是配置页：读数栏 + 发布流水', () => {
    draft.init('deployment-overview');
    draft.clear();
    const view = mount();

    expect(screen.getByRole('heading', { name: '发布' })).toBeTruthy();
    expect(
      view.container.querySelector('.nd-sheet.nd-page.cgc-page > .fg-sheet.nd-paper > .nd-page-head'),
    ).toBeTruthy();
    const tabs = screen.getAllByRole('tab');
    expect(tabs.map(tab => tab.textContent)).toEqual(['配置1', 'Agent', 'Xray1']);
    expect(tabs[0].getAttribute('aria-selected')).toBe('true');

    // 读数栏照用量页：待发布修订数、线上 → 当前、组成与键值行。
    const ledger = screen.getByRole('region', { name: '待发布修订' });
    expect(ledger.classList.contains('usage-ledger')).toBe(true);
    expect(within(ledger).getByText('线上 R13 → 当前 R14')).toBeTruthy();
    expect(screen.getByRole('button', { name: '无待发布变更' })).toBeTruthy();

    // 待发布修订排在流水最前，记录默认只显示 6 条。
    expect(screen.getByText('调整代理出站')).toBeTruthy();
    expect(screen.getByText('发布记录 14')).toBeTruthy();
    expect(screen.queryByText('发布记录 8')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '查看其余 8 条' }));
    expect(screen.getByText('发布记录 2')).toBeTruthy();
    expect(screen.getByRole('button', { name: '收起到最近 6 条' })).toBeTruthy();

    // 软件版本不再是入口加抽屉。
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(view.container.querySelector('.cgf-software-launcher, .cgf-software-drawer')).toBeNull();
  });

  it('Agent 与 Xray 共用已一致状态和冻结的逐台选择范围', () => {
    draft.init('deployment-overview');
    draft.clear();
    mount();

    fireEvent.click(screen.getByRole('tab', { name: 'Agent' }));
    expect(screen.getByRole('tab', { name: 'Agent' }).getAttribute('aria-selected')).toBe('true');
    const agentLedger = screen.getByRole('region', { name: '已一致' });
    expect(within(agentLedger).getByText('可发 v0.2.0 · 升级范围：逐台选择')).toBeTruthy();
    expect(screen.getByRole('table', { name: '逐台 Agent 升级状态' })).toBeTruthy();
    expect(screen.queryByRole('group', { name: '升级范围' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '升级 Agent' }));
    const agentScope = screen.getByRole('group', { name: '升级范围' });
    expect(within(agentScope).getByText('勾选下方可升级的机器')).toBeTruthy();
    expect(within(agentScope).queryByRole('button', { name: '全部机器' })).toBeNull();
    expect((within(agentScope).getByRole('button', { name: '批准' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(agentScope).getByRole('button', { name: '取消' }));
    expect(screen.queryByRole('group', { name: '升级范围' })).toBeNull();

    fireEvent.click(screen.getByRole('tab', { name: 'Xray1' }));
    const xrayLedger = screen.getByRole('region', { name: '已一致' });
    expect(within(xrayLedger).getByText('可发 v26.4.25 · 升级范围：逐台选择')).toBeTruthy();
    expect(screen.getByText('可升级', { selector: '.cgc-st' })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '升级 Xray' }));
    const xrayScope = screen.getByRole('group', { name: '升级范围' });
    expect(within(xrayScope).getByText('勾选下方可升级的机器')).toBeTruthy();
    expect(within(xrayScope).getByText(/^批准时冻结本次机器名单。Xray 替换后验证监听与运行摘要/)).toBeTruthy();
    fireEvent.click(screen.getByLabelText('升级 东京入口'));
    expect(within(xrayScope).getByText('已选 1 台')).toBeTruthy();
    expect((within(xrayScope).getByRole('button', { name: '批准' }) as HTMLButtonElement).disabled).toBe(false);

    // 切页签即放弃未提交的勾选。
    fireEvent.click(screen.getByRole('tab', { name: '配置1' }));
    fireEvent.click(screen.getByRole('tab', { name: 'Xray1' }));
    expect(screen.queryByRole('group', { name: '升级范围' })).toBeNull();
  });

  it('Xray 升级进行中：页签显示进度，页头可取消升级', async () => {
    draft.init('deployment-overview');
    draft.clear();
    let requested = '';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/nodes/agent-state') return json({ nodes: [] });
        requested = String(input);
        return json({
          available: {
            component: 'xray',
            build_id: 'xray-release',
            version: '26.4.25',
            artifacts: [{ arch: 'x86_64', sha256: 'c'.repeat(64) }],
            source: 'embedded',
          },
          current: { ...activeRelease, status: 'canceled', active: false },
          legacy_approval: null,
        });
      }),
    );
    mount({ releases: [activeRelease] });

    const xrayTab = screen.getByRole('tab', { name: 'Xray0/1' });
    expect(xrayTab.querySelector('.nd-tab-badge.cgc-run')?.textContent).toBe('0/1');
    fireEvent.click(xrayTab);
    const ledger = screen.getByRole('region', { name: '本次已升级' });
    expect(within(ledger).getByText(/^升级 #7 · root .+ 发起$/)).toBeTruthy();
    expect(screen.getByText('升级中', { selector: '.cgc-st' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: '升级 Xray' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '停止发布' }));
    const dialog = screen.getByRole('dialog', { name: '停止 Xray 发布？' });
    fireEvent.click(within(dialog).getByRole('button', { name: '停止发布' }));
    await waitFor(() => expect(requested).toBe('/binary-releases/xray/7/cancel'));
  });
});
