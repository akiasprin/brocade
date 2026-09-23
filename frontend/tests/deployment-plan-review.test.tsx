import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DeploymentPlan } from '../src/api';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const initial = { node_count: 0, chain_group_count: [] };

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

const { DeployPane } = await import('../src/panes/deploy');

const plan: DeploymentPlan = {
  revision: 12,
  base_revision_id: 11,
  warnings: [],
  summary: {
    total_targets: 5,
    changed_targets: 4,
    skipped_targets: 1,
    deferred_targets: 0,
    disruptive_targets: 3,
    max_wave: 3,
  },
  targets: [
    {
      node_id: 'edge-safe',
      status: 'pending',
      wave: 0,
      disruptive: false,
      actions: ['sync-grants'],
    },
    {
      node_id: 'edge-risk',
      status: 'pending',
      wave: 1,
      disruptive: true,
      actions: ['apply-xray'],
    },
    {
      node_id: 'edge-same',
      status: 'skipped',
      wave: 0,
      disruptive: false,
      actions: [],
    },
    {
      node_id: 'edge-rollout-1',
      status: 'pending',
      wave: 2,
      disruptive: true,
      prerequisites: ['edge-risk'],
      actions: ['apply-xray'],
    },
    {
      node_id: 'edge-rollout-2',
      status: 'pending',
      wave: 3,
      disruptive: true,
      prerequisites: ['edge-rollout-1'],
      actions: ['apply-xray'],
    },
  ],
};

const windowState: Win = {
  id: 91,
  key: 'tab:deploy',
  title: '发布',
  x: 0,
  y: 0,
  w: 980,
  h: 720,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'plan', revision: 12, key: 'review-key' } },
};

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('发布计划审阅', () => {
  it('按发布阶段审阅计划，把依赖批次收进全量发布，并保持创建参数不变', async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY }, mutations: { retry: false } },
    });
    client.setQueryData(['revisions'], {
      current_revision: 12,
      revisions: [
        {
          id: 12,
          created_at: '2026-09-14T02:00:00Z',
          author: 'release-reviewer',
          note: '调整入口和出口',
          status: 'committed',
          current: true,
          has_snapshot: true,
        },
      ],
    });
    client.setQueryData(['deployments'], {
      deployments: [{ id: 4, revision_id: 11, status: 'succeeded' }],
    });
    client.setQueryData(['plan', 12], plan);
    client.setQueryData(['artifact-index', 12], { revision: 12, artifacts: [] });
    client.setQueryData(['artifact-index', 11], { revision: 11, artifacts: [] });
    client.setQueryData(['nodes'], {
      nodes: [
        { node_id: 'edge-safe', name: '台北入口' },
        { node_id: 'edge-risk', name: '新加坡中继' },
        { node_id: 'edge-same', name: '香港出口' },
        { node_id: 'edge-rollout-1', name: '香港入口 B' },
        { node_id: 'edge-rollout-2', name: '东京 IIJ-01' },
      ],
    });

    let createBody: unknown;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === '/deployments' && init?.method === 'POST') {
          createBody = JSON.parse(String(init.body));
          return json({ deployment_id: 5, status: 'planned', reused: false, plan });
        }
        if (path === '/deployments?limit=50') return json({ deployments: [] });
        throw new Error(`未预期的请求：${path}`);
      }),
    );

    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            initial,
            who: {
              operator_id: 'release-reviewer',
              role: 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
          }}
        >
          <DeployPane win={windowState} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    // 页头、底部状态结论与主操作分别保留一次，和评审稿的连续单据结构一致。
    expect(view.getAllByText('创建变更单')).toHaveLength(3);
    expect(view.getByText('运行基线')).toBeTruthy();
    expect(view.getByText('变更机器')).toBeTruthy();
    expect(view.getByText('执行计划')).toBeTruthy();
    expect(view.getByText('更新机器配置')).toBeTruthy();
    expect(view.getByText('发布验证')).toBeTruthy();
    expect(view.getAllByText('全量发布')).toHaveLength(1);
    expect(view.getByText('步骤 1/2')).toBeTruthy();
    expect(view.getByText('步骤 2/2')).toBeTruthy();
    expect(view.getByText('产物差异')).toBeTruthy();
    expect(view.getAllByText(/更新 Xray/)).toHaveLength(3);
    expect(view.queryByText('apply-xray')).toBeNull();
    expect(view.queryByText(/^波次 /)).toBeNull();
    expect(view.getByText('台北入口')).toBeTruthy();
    expect(view.getByText('新加坡中继')).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: '创建变更单' }));
    await waitFor(() =>
      expect(createBody).toEqual({
        revision_id: 12,
        idempotency_key: 'review-key',
      }),
    );
  });

  it('详情页沿用同一阶段结构，并用当前机器名称表达确认动作', () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY }, mutations: { retry: false } },
    });
    client.setQueryData(['deployment', 5, true], {
      id: 5,
      revision_id: 12,
      status: 'running',
      activation_status: 'waiting',
      settlement_status: 'converged',
      activated_at: null,
      debt_targets: 0,
      active: true,
      actor: 'release-reviewer',
      note: '调整入口和出口',
      base_revision_id: 11,
      warnings: [],
      created_at: '2026-09-14T02:00:00Z',
      started_at: '2026-09-14T02:00:05Z',
      halted_at: null,
      finished_at: null,
      rollback_of_deployment_id: null,
      sync_of_deployment_id: null,
      divergence_cleared_at: null,
      targets: plan.targets
        .filter(target => target.status !== 'skipped')
        .map(target => ({
          node_id: target.node_id,
          wave: target.wave,
          disruptive: target.disruptive,
          status: target.wave < 2 ? 'succeeded' : 'pending',
          error: null,
          desired_structure: { actions: target.actions },
          observed_before: null,
          observed_after: null,
          verdict: null,
          dispatched_at: target.wave < 2 ? '2026-09-14T02:00:10Z' : null,
        })),
    });
    client.setQueryData(['nodes'], {
      nodes: [
        { node_id: 'edge-safe', name: '台北入口' },
        { node_id: 'edge-risk', name: '新加坡中继' },
        { node_id: 'edge-rollout-1', name: '香港入口 B' },
        { node_id: 'edge-rollout-2', name: '东京 IIJ-01' },
      ],
    });

    const detailWindow: Win = {
      ...windowState,
      data: { drill: { p: 'detail', id: 5 } },
    };
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            initial,
            who: {
              operator_id: 'release-reviewer',
              role: 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
          }}
        >
          <DeployPane win={detailWindow} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    expect(view.getByText('变更单 #5')).toBeTruthy();
    expect(view.getByText('发布验证')).toBeTruthy();
    expect(view.getByText('全量发布')).toBeTruthy();
    expect(view.getByText('步骤 1/2')).toBeTruthy();
    expect(view.getByText('步骤 2/2')).toBeTruthy();
    expect(view.getByRole('button', { name: '更新香港入口 B' })).toBeTruthy();
    expect(view.queryByText(/^波次 /)).toBeNull();
  });
});
