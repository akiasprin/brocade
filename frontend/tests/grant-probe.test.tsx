import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { GrantProbeJob, GrantProbePlan, UserListItem } from '../src/api';
import { GrantProbePanel } from '../src/panes/users';

const user: UserListItem = {
  tenant_id: 'platform.acme',
  id: 'alice',
  status: 'active',
  created_at: '2026-08-31T00:00:00Z',
  created_revision: 582,
};

const plan: GrantProbePlan = {
  serving_revision: 582,
  serving_generation: 19,
  items: [
    {
      id: 'g-friendly:ipv4:vless',
      name: '伦敦入口',
      app_id: 'app-global',
      app_name: '全球线路',
      chain_id: 'ch-blue',
      ingress_id: 'in-green',
      family: 'ipv4',
      protocol: 'vless',
    },
    {
      id: 'g-friendly:ipv6:hysteria2',
      name: '伦敦入口 | QUIC | v6',
      app_id: 'app-global',
      app_name: '全球线路',
      chain_id: 'ch-blue',
      ingress_id: 'in-green',
      family: 'ipv6',
      protocol: 'hysteria2',
    },
  ],
};

const finishedJob: GrantProbeJob = {
  id: 'p1',
  tenant_id: user.tenant_id,
  user_id: user.id,
  serving_revision: plan.serving_revision,
  serving_generation: plan.serving_generation,
  status: 'completed',
  message: '网络拨测全部通过',
  created_at_unix_secs: 1,
  finished_at_unix_secs: 2,
  items: plan.items.map(item => ({ ...item, status: 'passed', ttfb_ms: 42, detail: null })),
};

const jsonResponse = (body: unknown) =>
  ({
    ok: true,
    status: 200,
    statusText: 'OK',
    json: async () => body,
  }) as Response;

const wrapper = () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('user grant probe', () => {
  it('submits only frozen item ids and renders the real-traffic contract', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/users/platform.acme/alice/grant-probes' && init?.method === 'POST') {
        return jsonResponse({ job: finishedJob, reused: false });
      }
      if (path === '/users/platform.acme/alice/grant-probes') return jsonResponse(plan);
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(<GrantProbePanel user={user} />, { wrapper: wrapper() });

    expect(view.getByText('网络拨测')).toBeTruthy();
    expect(await view.findByText('伦敦入口')).toBeTruthy();
    const results = view.container.querySelectorAll<HTMLElement>('.grant-probe-result');
    const matrix = view.container.querySelector<HTMLElement>('.grant-probe-matrix');
    expect(results).toHaveLength(2);
    // 三种协议各有 V4/V6 两格。CSS 读取同一个槽位数，不会在新增协议后仍按旧的四列排版。
    expect(matrix?.children).toHaveLength(6);
    expect(matrix?.style.getPropertyValue('--grant-probe-slot-count')).toBe('6');
    expect(view.container.querySelectorAll('.grant-probe-result.missing')).toHaveLength(0);
    const tableHead = view.container.querySelector('.grant-probe-table-head');
    expect(tableHead).toBeTruthy();
    expect(tableHead?.querySelectorAll('.grant-probe-matrix-head > span')).toHaveLength(6);
    expect(tableHead?.textContent).toContain('线路 / Route');
    expect(tableHead?.textContent).toContain('AnyTLS');
    expect(results[0].dataset).toMatchObject({ protocol: 'VLESS', stack: 'V4' });
    expect(results[1].dataset).toMatchObject({ protocol: 'Hysteria2', stack: 'V6' });
    expect((view.getByRole('button', { name: '重试失败项' }) as HTMLButtonElement).disabled).toBe(true);
    expect(view.getByText(/真实用户凭据/)).toBeTruthy();
    expect(view.getByText(/计入该用户用量/)).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '拨测' }));

    await view.findAllByText('42ms');
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === 'POST');
    expect(post).toBeTruthy();
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      item_ids: ['g-friendly:ipv4:vless', 'g-friendly:ipv6:hysteria2'],
    });
    expect(String(post?.[1]?.body)).not.toContain('uuid');
    expect(String(post?.[1]?.body)).not.toContain('server');
  });

  it('keeps actions disabled when the local Xray capability is unavailable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        jsonResponse({
          available: false,
          version: null,
          reason: 'Console 未安装或无法执行拨测 Xray',
          concurrency: 30,
        }),
      ),
    );
    const view = render(<GrantProbePanel user={user} />, { wrapper: wrapper() });

    expect(await view.findByText('Console 未安装或无法执行拨测 Xray')).toBeTruthy();
    expect((view.getByRole('button', { name: '拨测全部' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('shows the credential-free Serving plan to readonly visitors without calling execution APIs', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/users/platform.acme/alice/grant-probes' && !init?.method) return jsonResponse(plan);
      throw new Error(`readonly visitor called unexpected request ${init?.method ?? 'GET'} ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(<GrantProbePanel user={user} readOnly />, { wrapper: wrapper() });

    expect(await view.findByText('伦敦入口')).toBeTruthy();
    expect(view.getByText(/只读查看/)).toBeTruthy();
    expect((view.getByRole('button', { name: '拨测全部' }) as HTMLButtonElement).disabled).toBe(true);
    expect((view.getByRole('button', { name: '拨测' }) as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe('/users/platform.acme/alice/grant-probes');
  });

  it('keeps the complete authorization matrix visible after probing only one row', async () => {
    const multiPlan: GrantProbePlan = {
      ...plan,
      items: [
        ...plan.items,
        {
          id: 'g-tokyo:ipv4:vless',
          name: '东京入口',
          app_id: 'app-standard',
          app_name: '标准线路',
          chain_id: 'ch-tokyo',
          ingress_id: 'in-tokyo',
          family: 'ipv4',
          protocol: 'vless',
        },
      ],
    };
    const rowJob: GrantProbeJob = {
      ...finishedJob,
      id: 'p-row',
      items: plan.items.map(item => ({ ...item, status: 'passed', ttfb_ms: 31, detail: null })),
    };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/users/platform.acme/alice/grant-probes' && init?.method === 'POST') {
        return jsonResponse({ job: rowJob, reused: false });
      }
      if (path === '/users/platform.acme/alice/grant-probes') return jsonResponse(multiPlan);
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(<GrantProbePanel user={user} />, { wrapper: wrapper() });

    expect(await view.findByText('东京入口')).toBeTruthy();
    fireEvent.click(view.getAllByRole('button', { name: '拨测' })[0]);

    await view.findAllByText('31ms');
    expect(view.getByText('东京入口')).toBeTruthy();
    const idle = view.getByText('东京入口').closest('.grant-probe-row')?.querySelector('.grant-probe-result.idle');
    expect(idle?.textContent).not.toContain('尚未验证');
    expect(idle?.getAttribute('aria-label')).toContain('尚未验证');
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === 'POST');
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      item_ids: ['g-friendly:ipv4:vless', 'g-friendly:ipv6:hysteria2'],
    });
  });

  it('polls a running job to completion when the SSE stream delivers no snapshots', async () => {
    class SilentEventSource {
      onerror: (() => void) | null = null;
      addEventListener() {}
      close() {}
    }
    vi.stubGlobal('EventSource', SilentEventSource);
    const runningJob: GrantProbeJob = {
      ...finishedJob,
      id: 'p-live',
      status: 'running',
      message: null,
      finished_at_unix_secs: null,
      items: plan.items.map((item, index) => ({
        ...item,
        status: index === 0 ? 'running' : 'waiting',
        ttfb_ms: null,
        detail: null,
      })),
    };
    const polledJob: GrantProbeJob = {
      ...finishedJob,
      id: runningJob.id,
      items: plan.items.map(item => ({ ...item, status: 'passed', ttfb_ms: 44, detail: null })),
    };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (path === '/users/platform.acme/alice/grant-probes' && init?.method === 'POST') {
        return jsonResponse({ job: runningJob, reused: false });
      }
      if (path === '/users/platform.acme/alice/grant-probes') return jsonResponse(plan);
      if (path === '/grant-probes/p-live') return jsonResponse(polledJob);
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(<GrantProbePanel user={user} />, { wrapper: wrapper() });

    expect(await view.findByText('伦敦入口')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    expect(await view.findByText('连接中…')).toBeTruthy();
    expect(await view.findAllByText('44ms', {}, { timeout: 3_000 })).toHaveLength(2);
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/grant-probes/p-live')).toBe(true);
  });
});
