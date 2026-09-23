import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { XrayRelease, XrayReleaseView } from '../src/api';
import { XrayReleaseSection } from '../src/panes/xray-release';

const OLD_A = 'a'.repeat(64);
const OLD_B = 'b'.repeat(64);
const NEXT = 'c'.repeat(64);
const RELEASE_ID = 'd'.repeat(64);

const emptyView = (): XrayReleaseView => ({
  available_release_id: RELEASE_ID,
  available_xrays: [{ arch: 'x86_64', sha256: NEXT }],
  xray_version: '26.9.1',
  console_version: '0.2.0',
  build_commit: 'test',
  history: [],
  next_history_before_id: null,
  releases: [],
});

const nodes = {
  nodes: [
    {
      node_id: 'edge-a',
      name: '台北入口',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: { xray_installed_sha256: OLD_A, xray_running_sha256: OLD_A },
    },
    {
      node_id: 'edge-b',
      name: '东京中继',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: { xray_installed_sha256: OLD_B, xray_running_sha256: OLD_B },
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
      name: '已经升级',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: { xray_installed_sha256: NEXT, xray_running_sha256: NEXT },
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

const clients: QueryClient[] = [];
function mount(view: XrayReleaseView) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Number.POSITIVE_INFINITY },
      mutations: { retry: false },
    },
  });
  clients.push(client);
  client.setQueryData(['xray-releases'], view);
  client.setQueryData(['nodes'], nodes);
  return render(
    <QueryClientProvider client={client}>
      <XrayReleaseSection editable />
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

describe('Xray 版本发布', () => {
  it('只允许有可靠摘要的机器入选，并把灰度机器显式写入创建请求', async () => {
    const view = emptyView();
    let sent: unknown;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        expect(String(input)).toBe('/xray-releases');
        expect(init?.method).toBe('POST');
        sent = JSON.parse(String(init?.body));
        return json(view);
      }),
    );
    mount(view);

    expect((screen.getByLabelText('发布到 旧 Agent') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('发布到 离线机器') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('发布到 已经升级') as HTMLInputElement).disabled).toBe(false);
    fireEvent.click(screen.getByLabelText('发布到 已经升级'));
    expect((screen.getByLabelText('设 已经升级 为灰度机器') as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByLabelText('发布到 已经升级'));
    fireEvent.click(screen.getByLabelText('发布到 未运行 Xray'));
    expect((screen.getByLabelText('设 未运行 Xray 为灰度机器') as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByLabelText('发布到 未运行 Xray'));
    fireEvent.click(screen.getByLabelText('发布到 台北入口'));
    fireEvent.click(screen.getByLabelText('发布到 东京中继'));
    fireEvent.click(screen.getByLabelText('设 东京中继 为灰度机器'));
    fireEvent.change(screen.getByLabelText('Xray 发布说明'), { target: { value: '验证新内核' } });
    fireEvent.click(screen.getByRole('button', { name: '创建 Xray 发布' }));
    fireEvent.click(screen.getByRole('button', { name: '创建发布' }));

    await waitFor(() =>
      expect(sent).toEqual({
        idempotency_key: expect.any(String),
        release_id: RELEASE_ID,
        nodes: ['edge-a', 'edge-b'],
        canary_node: 'edge-b',
        batch_size: 10,
        note: '验证新内核',
      }),
    );
  });

  it('只有灰度成功后才提供扩波确认', async () => {
    const release: XrayRelease = {
      id: 7,
      release_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'running',
      active: true,
      confirmed_wave: 1,
      batch_size: 10,
      note: null,
      created_at: '2026-09-12T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: null,
      events: [],
      targets: [
        {
          node_id: 'edge-a',
          wave: 1,
          status: 'succeeded',
          attempt: 1,
          before_sha256: OLD_A,
          desired_sha256: NEXT,
          arch: 'x86_64',
          error: null,
          reported_performed_update: true,
          reported_xray_enabled: true,
          reported_installed_sha256: NEXT,
          reported_running_sha256: NEXT,
          retryable: false,
          dispatched_at: '2026-09-12T00:01:00Z',
          finished_at: '2026-09-12T00:02:00Z',
        },
        {
          node_id: 'edge-b',
          wave: 2,
          status: 'pending',
          attempt: 1,
          before_sha256: OLD_B,
          desired_sha256: null,
          arch: null,
          error: null,
          reported_performed_update: null,
          reported_xray_enabled: null,
          reported_installed_sha256: null,
          reported_running_sha256: null,
          retryable: false,
          dispatched_at: null,
          finished_at: null,
        },
      ],
    };
    const view = {
      ...emptyView(),
      history: [
        {
          id: release.id,
          release_id: release.release_id,
          version: release.version,
          status: release.status,
          active: release.active,
          confirmed_wave: release.confirmed_wave,
          batch_size: release.batch_size,
          note: null,
          created_at: release.created_at,
          created_by: release.created_by,
          halted_at: null,
          finished_at: null,
          target_count: 2,
          succeeded_count: 1,
          problem_count: 0,
        },
      ],
      releases: [release],
    };
    let requested = '';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        requested = String(input);
        expect(init?.method).toBe('POST');
        return json(view);
      }),
    );
    mount(view);

    fireEvent.click(screen.getByRole('button', { name: '扩大到第 2 波' }));
    fireEvent.click(screen.getByRole('button', { name: '确认第 2 波' }));

    await waitFor(() => expect(requested).toBe('/xray-releases/7/confirm'));
  });

  it('历史列表按需加载审计详情并用游标追加更早记录', async () => {
    const release: XrayRelease = {
      id: 7,
      release_id: RELEASE_ID,
      version: '26.9.1',
      artifacts: [{ arch: 'x86_64', sha256: NEXT }],
      status: 'succeeded',
      active: false,
      confirmed_wave: 1,
      batch_size: 10,
      note: null,
      created_at: '2026-09-12T00:00:00Z',
      created_by: 'admin',
      halted_at: null,
      finished_at: '2026-09-12T00:02:00Z',
      targets: [],
      events: [],
    };
    const summary = {
      id: release.id,
      release_id: release.release_id,
      version: release.version,
      status: release.status,
      active: release.active,
      confirmed_wave: release.confirmed_wave,
      batch_size: release.batch_size,
      note: release.note,
      created_at: release.created_at,
      created_by: release.created_by,
      halted_at: release.halted_at,
      finished_at: release.finished_at,
      target_count: 1,
      succeeded_count: 1,
      problem_count: 0,
    };
    const view: XrayReleaseView = {
      ...emptyView(),
      history: [summary],
      next_history_before_id: 7,
      releases: [release],
    };
    const requested: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        requested.push(url);
        if (url === '/xray-releases/7') {
          return json({
            ...release,
            events: [
              {
                id: 1,
                kind: 'created',
                node_id: null,
                wave: 1,
                actor: 'admin',
                detail: {},
                created_at: release.created_at,
              },
            ],
          });
        }
        if (url === '/xray-releases/history?before_id=7') {
          return json({
            history: [{ ...summary, id: 6, version: '26.8.1' }],
            next_history_before_id: null,
          });
        }
        throw new Error(`unexpected request ${url}`);
      }),
    );
    mount(view);

    fireEvent.click(screen.getByRole('button', { name: '查看审计' }));
    await screen.findByRole('table', { name: 'Xray 发布 7 审计事件' });
    expect(screen.getByText('created')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '加载更早记录' }));
    await screen.findByText('#6 · 26.8.1');
    expect(requested).toEqual(['/xray-releases/7', '/xray-releases/history?before_id=7']);
  });
});
