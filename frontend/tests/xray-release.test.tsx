import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { XrayRelease, XrayReleaseView } from '../src/api';
import { XrayReleaseTab, useXrayRelease } from '../src/panes/xray-release';

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
      runtime_versions: {
        xray: 'Xray 26.8.1',
        xray_installed_sha256: OLD_A,
        xray_running_sha256: OLD_A,
      },
    },
    {
      node_id: 'edge-b',
      name: '东京中继',
      lifecycle_phase: 'active',
      desired_poll_fresh: true,
      runtime_report_fresh: true,
      runtime_versions: {
        xray: 'Xray 26.8.1',
        xray_installed_sha256: OLD_B,
        xray_running_sha256: OLD_B,
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
  it('像 Agent 一样勾选升级范围后直接批准，请求不包含波次', async () => {
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

    await waitFor(() =>
      expect(sent).toEqual({
        idempotency_key: expect.any(String),
        release_id: RELEASE_ID,
        nodes: ['edge-a', 'edge-b'],
        note: null,
      }),
    );
    expect(screen.queryByText(/灰度|分批|波次|发布记录/)).toBeNull();
  });

  it('进行中只显示每台机器的升级状态，不提供扩波和历史', async () => {
    const release: XrayRelease = {
      id: 7,
      release_id: RELEASE_ID,
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
          reported_xray_enabled: true,
          reported_installed_sha256: NEXT,
          reported_running_sha256: NEXT,
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
          reported_xray_enabled: true,
          reported_installed_sha256: OLD_B,
          reported_running_sha256: OLD_B,
          retryable: true,
          dispatched_at: '2026-09-12T00:01:00Z',
          finished_at: '2026-09-12T00:02:00Z',
        },
      ],
    };
    const view = { ...emptyView(), releases: [release] };
    let requested = '';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        requested = String(input);
        return json(view);
      }),
    );
    mount(view);

    expect(screen.getByText('本次已升级', { selector: '.cgc-st' })).toBeTruthy();
    expect(screen.getByText('失败 · 已恢复', { selector: '.cgc-st' })).toBeTruthy();
    // 失败原因写在机器名下；进行中不能再勾选新的机器。
    expect(screen.getByText('启动检查失败')).toBeTruthy();
    expect(screen.queryByRole('group', { name: '升级范围' })).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByRole('button', { name: /波/ })).toBeNull();
    expect(screen.queryByRole('button', { name: '查看审计' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    await waitFor(() => expect(requested).toBe('/xray-releases/7/targets/edge-b/retry'));
  });
});
