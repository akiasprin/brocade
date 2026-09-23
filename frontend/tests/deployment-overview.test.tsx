import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { DeploymentListItem } from '../src/api';
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

function mount() {
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
          xray: 'Xray 26.4.25 (Xray, Penetrates Everything.) b4f0898 (go1.26.0 linux/amd64)',
          xray_installed_sha256: 'b'.repeat(64),
          xray_running_sha256: 'b'.repeat(64),
        },
      },
    ],
  });
  client.setQueryData(['agent-release'], {
    agent_version: '0.2.0',
    console_version: '0.2.0',
    build_commit: 'test',
    available_release_id: 'a'.repeat(64),
    available_agents: [{ arch: 'x86_64', sha256: 'a'.repeat(64) }],
    released: {
      release_id: 'a'.repeat(64),
      scope: 'all',
      nodes: [],
      note: null,
      version: '0.2.0',
      commit: 'test',
      released_at: '2026-09-16T00:00:00Z',
      released_by: 'root',
    },
  });
  client.setQueryData(['xray-releases'], {
    available_release_id: 'xray-release',
    available_xrays: [{ arch: 'x86_64', sha256: 'c'.repeat(64) }],
    xray_version: '26.4.25',
    console_version: '0.2.0',
    build_commit: 'test',
    history: [],
    next_history_before_id: null,
    releases: [],
  });

  return render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial }}>
        <DeployPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  draft.clear();
});

describe('发布概览', () => {
  it('一条流水里给出修订读数、软件读数与发布记录', () => {
    draft.init('deployment-overview');
    draft.clear();
    const view = mount();

    // 页标题由面板抬头承担，读数写在标题右侧。
    expect(screen.getByRole('heading', { name: '发布' })).toBeTruthy();
    expect(view.container.querySelector('.cgf > header .rd')?.textContent).toContain('线上 R13 · 当前 R14');

    // 未发布的修订排在流水顶部；本例里产物没有差异，分组文字要说明这一点，
    // 否则「1 个修订」紧挨着禁用的「无待发布变更」读起来自相矛盾。
    expect(screen.getByText('调整代理出站')).toBeTruthy();
    expect(view.container.querySelector('.cgf-gmeta')?.textContent).toContain('产物与线上一致，无需下发');
    expect(screen.getByRole('button', { name: '无待发布变更' })).toBeTruthy();

    // 软件读数行：两样软件各一格，展开的机器表由行尾按钮打开。
    expect(screen.getByText('Agent')).toBeTruthy();
    expect(screen.getByText('全部机器 已替换')).toBeTruthy();
    expect(screen.getByText('1/1 台在跑')).toBeTruthy();
    expect(screen.queryByText(/Penetrates Everything/)).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Agent 版本' })).toBeNull();

    // 软件发布与单据排在同一条流里。
    expect(screen.getByText('批准 Agent v0.2.0')).toBeTruthy();
    expect(screen.getByText('发布记录 14')).toBeTruthy();
    expect(screen.queryByText('发布记录 2')).toBeNull();
    expect(view.container.querySelector('.cgf-row .st-skipped')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '查看其余 2 条' }));
    expect(screen.getByText('发布记录 2')).toBeTruthy();
    expect(screen.getByRole('button', { name: '收起到最近 12 条' })).toBeTruthy();
  });

  it('软件读数行的按钮就地展开机器表，收起后回到读数', () => {
    draft.init('deployment-overview');
    draft.clear();
    mount();

    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    expect(screen.getByRole('heading', { name: 'Agent 版本' })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '收起' }));
    expect(screen.queryByRole('heading', { name: 'Agent 版本' })).toBeNull();
    expect(screen.getByText('全部机器 已替换')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '选择机器' }));
    expect(screen.getByRole('heading', { name: 'Xray 版本' })).toBeTruthy();
  });
});
