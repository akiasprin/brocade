import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { BinaryReleaseHistory } from '../src/panes/binary-release-history';
import { BinaryReleaseTab, useBinaryRelease } from '../src/panes/binary-release';

const digest = 'a'.repeat(64);
const json = (body: unknown) => new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
const clients: QueryClient[] = [];
function client() {
  const value = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  clients.push(value);
  value.setQueryData(['nodes'], { nodes: [] });
  return value;
}
afterEach(() => {
  cleanup();
  clients.forEach(value => value.clear());
  clients.length = 0;
  vi.unstubAllGlobals();
});

describe('二进制发布共用视图', () => {
  it('完成回执刷新机器摘要，关闭的历史不额外请求', async () => {
    const qc = client();
    const old = 'b'.repeat(64);
    const fleet = (sha: string) => ({
      nodes: [
        { node_id: 'edge', name: '入口', agent_version: sha, lifecycle_phase: 'active', desired_poll_fresh: true },
      ],
    });
    const target = {
      node_id: 'edge',
      status: 'dispatched',
      before_sha256: old,
      desired_sha256: digest,
      attempt: 1,
      finished_at: null,
    };
    const view = {
      available: {
        component: 'agent',
        version: '0.2.0',
        build_id: digest,
        source: 'embedded',
        artifacts: [{ arch: 'x86_64', sha256: digest }],
      },
      current: { id: 9, active: true, status: 'running', targets: [target] },
      legacy_approval: null,
    };
    qc.setQueryData(['nodes'], fleet(old));
    qc.setQueryData(['binary-releases', 'agent'], view);
    const paths: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        paths.push(path);
        if (path === '/nodes/agent-state') return json(fleet(digest));
        if (path === '/binary-releases/agent')
          return json({
            ...view,
            current: {
              ...view.current,
              active: false,
              status: 'succeeded',
              targets: [{ ...target, status: 'succeeded', finished_at: '2026-10-07T00:00:00Z' }],
            },
          });
        throw new Error(`unexpected request ${path}`);
      }),
    );
    function Harness() {
      const release = useBinaryRelease('agent');
      return <span>一致 {release.counts.current}</span>;
    }
    render(
      <QueryClientProvider client={qc}>
        <Harness />
      </QueryClientProvider>,
    );
    await act(async () => {
      await qc.refetchQueries({ queryKey: ['binary-releases', 'agent'] });
    });
    expect(await screen.findByText('一致 1')).toBeTruthy();
    expect(paths).toEqual(['/binary-releases/agent', '/nodes/agent-state']);
  });

  it('历史、详情、逐次证据各自按需读取，翻页使用游标', async () => {
    const calls: string[] = [];
    const target = {
      node_id: 'edge',
      status: 'succeeded',
      attempt: 2,
      before_sha256: digest,
      desired_sha256: 'b'.repeat(64),
      verification: 'observed',
      error: null,
    };
    const release = {
      id: 7,
      component: 'agent',
      build_id: digest,
      version: '0.2.0',
      status: 'succeeded',
      created_by: 'admin',
      created_at: '2026-10-07T00:00:00Z',
      succeeded_count: 1,
      target_count: 1,
      problem_count: 0,
      note: null,
      targets: [target],
      events: [],
      next_event_before_id: null,
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        calls.push(path);
        if (path === '/binary-releases/agent/history') return json({ items: [release], next_before_id: 7 });
        if (path === '/binary-releases/agent/history?before_id=7') return json({ items: [], next_before_id: null });
        if (path === '/binary-releases/agent/7') return json(release);
        if (path === '/binary-releases/agent/7/targets/edge/attempts')
          return json([
            {
              attempt: 1,
              status: 'failed-recovered',
              verification: 'receipt',
              error: '下载超时',
              started_at: null,
              finished_at: null,
            },
            {
              attempt: 2,
              status: 'succeeded',
              verification: 'observed',
              error: null,
              started_at: null,
              finished_at: null,
            },
          ]);
        throw new Error(`unexpected request ${path}`);
      }),
    );
    render(
      <QueryClientProvider client={client()}>
        <BinaryReleaseHistory component="agent" latestId={7} />
      </QueryClientProvider>,
    );
    expect(calls).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: '发布历史' }));
    fireEvent.click(await screen.findByRole('button', { name: /#7/ }));
    fireEvent.click(await screen.findByRole('button', { name: /edge · 本次已升级/ }));
    expect(await screen.findByText('下载超时')).toBeTruthy();
    expect(screen.getByText('第 1 次 · 失败 · 原版本运行')).toBeTruthy();
    expect(screen.getByText('观测确认 · 未领取')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '更早的发布' }));
    await waitFor(() =>
      expect(calls).toEqual([
        '/binary-releases/agent/history',
        '/binary-releases/agent/7',
        '/binary-releases/agent/7/targets/edge/attempts',
        '/binary-releases/agent/history?before_id=7',
      ]),
    );
    expect(screen.queryByRole('button', { name: /回滚/ })).toBeNull();
  });

  it('Agent 离线时摘要相同也不显示已一致，非法摘要不能选择', () => {
    const qc = client();
    qc.setQueryData(['nodes'], {
      nodes: [
        {
          node_id: 'stale',
          name: '离线机器',
          agent_version: digest,
          lifecycle_phase: 'active',
          desired_poll_fresh: false,
        },
        {
          node_id: 'invalid',
          name: '旧标识机器',
          agent_version: '0.2.0',
          lifecycle_phase: 'active',
          desired_poll_fresh: true,
        },
      ],
    });
    qc.setQueryData(['binary-releases', 'agent'], {
      available: {
        component: 'agent',
        version: '0.2.0',
        build_id: digest,
        source: 'embedded',
        artifacts: [{ arch: 'x86_64', sha256: digest }],
      },
      current: null,
      legacy_approval: null,
    });
    function Harness() {
      const release = useBinaryRelease('agent');
      return <BinaryReleaseTab release={release} editable editing onEditingChange={() => {}} />;
    }
    render(
      <QueryClientProvider client={qc}>
        <Harness />
      </QueryClientProvider>,
    );
    expect(screen.queryByText('已一致', { selector: '.cgc-st' })).toBeNull();
    expect((screen.getByLabelText('升级 离线机器') as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByLabelText('升级 旧标识机器') as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText('还没有上报有效的 Agent 摘要')).toBeTruthy();
  });
});
