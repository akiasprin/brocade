import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
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
  it('未上报变体的旧 Agent 可升级基础版，显式 CVM 保持阻止降级', () => {
    const qc = client();
    qc.setQueryData(['nodes'], {
      nodes: [undefined, null, 'base', 'cvm'].map((flavor, index) => ({
        node_id: `edge-${index}`,
        name: `机器 ${index}`,
        agent_version: `brocade-agent/${'b'.repeat(64)}`,
        lifecycle_phase: 'active',
        desired_poll_fresh: true,
        runtime_versions: flavor === undefined ? {} : { agent_flavor: flavor },
      })),
    });
    qc.setQueryData(['binary-releases', 'agent'], {
      available: {
        agent_flavor: 'base',
        component: 'agent',
        version: 'test',
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
    for (const index of [0, 1, 2]) {
      expect((screen.getByLabelText(`升级 机器 ${index}`) as HTMLInputElement).disabled).toBe(false);
    }
    expect((screen.getByLabelText('升级 机器 3') as HTMLInputElement).disabled).toBe(true);
  });

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
    fireEvent.click(await screen.findByRole('button', { name: /edge · 已升级/ }));
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

  it('历史将按授权强制退役显示为绿色已升级，保留异常后自动恢复审计', async () => {
    const release = {
      id: 11,
      component: 'xray',
      build_id: digest,
      version: '26.10.1',
      status: 'succeeded',
      created_by: 'admin',
      created_at: '2026-10-16T00:00:00Z',
      succeeded_count: 2,
      target_count: 2,
      problem_count: 0,
      note: null,
      targets: [
        {
          node_id: 'force-node',
          status: 'succeeded',
          attempt: 1,
          before_sha256: digest,
          desired_sha256: 'b'.repeat(64),
          verification: 'receipt',
          error: null,
          reported_forced_retirement: true,
          recovered_after_dirty: false,
        },
        {
          node_id: 'recovered-node',
          status: 'succeeded',
          attempt: 1,
          before_sha256: digest,
          desired_sha256: 'b'.repeat(64),
          verification: 'receipt',
          error: null,
          reported_forced_retirement: false,
          recovered_after_dirty: true,
        },
      ],
      events: [
        {
          id: 1,
          kind: 'target-recovered-after-dirty',
          node_id: 'recovered-node',
          actor: null,
          detail: { attempt: 1, previous_error: '动态转发恢复仍在重试' },
          created_at: '2026-10-16T00:01:00Z',
        },
      ],
      next_event_before_id: null,
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/binary-releases/xray/history') return json({ items: [release], next_before_id: null });
        if (path === '/binary-releases/xray/11') return json(release);
        throw new Error(`unexpected request ${path}`);
      }),
    );
    render(
      <QueryClientProvider client={client()}>
        <BinaryReleaseHistory component="xray" latestId={11} />
      </QueryClientProvider>,
    );

    fireEvent.click(screen.getByRole('button', { name: '发布历史' }));
    fireEvent.click(await screen.findByRole('button', { name: /#11/ }));
    const upgraded = await screen.findByRole('button', { name: /force-node · 已升级/ });
    expect(within(upgraded).getByText('已升级').classList.contains('st-ok')).toBe(true);
    expect(screen.queryByText('强制退役完成')).toBeNull();
    expect(screen.getByRole('button', { name: /recovered-node · 自动恢复完成/ })).toBeTruthy();
    expect(screen.getByText(/异常后自动恢复完成 · recovered-node/)).toBeTruthy();
    expect(screen.getByText('恢复前异常：动态转发恢复仍在重试')).toBeTruthy();
  });
});
