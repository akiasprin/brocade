import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TunnelProbeList, TunnelProbeRun, TunnelProbeView } from '../src/api';
import { TunnelProbePanel, TunnelProbeSettingsSection } from '../src/tunnel-probe';

const run: TunnelProbeRun = {
  id: 41,
  tenant_id: 'platform.acme',
  outbound_id: 'custom-1111-1111',
  outbound_name: '供应商出口',
  protocol: 'anytls',
  trigger: 'scheduled',
  source: 'serving',
  topology_revision: 12,
  serving_generation: 7,
  draft_sha256: null,
  settings_revision: 12,
  timeout_secs: 10,
  status: 'succeeded',
  phase: 'finished',
  result: 'ok',
  ttfb_ms: 88,
  http_status: 200,
  exit_ip: '203.0.113.8',
  exit_loc: 'TW',
  attempt_count: 1,
  error_code: null,
  error_detail: null,
  queued_at_unix_secs: 1_700_000_000,
  started_at_unix_secs: 1_700_000_001,
  finished_at_unix_secs: 1_700_000_002,
  cancel_requested: false,
};

const view: TunnelProbeView = {
  item: {
    tenant_id: run.tenant_id,
    outbound_id: run.outbound_id,
    name: run.outbound_name,
    protocol: run.protocol,
    supported: true,
    unsupported_reason: null,
    health: 'degraded',
    policy: {
      enabled: true,
      interval_secs: 300,
      timeout_secs: 10,
      next_run_at_unix_secs: 1_700_000_300,
      updated_at_unix_secs: 1_700_000_000,
    },
    latest_run: run,
  },
  retention_days: 7,
  summary: {
    window_secs: 86_400,
    total: 5,
    succeeded: 4,
    success_rate: 0.8,
    p50_ms: 88,
    p95_ms: 240,
    failures: 1,
  },
  points: [
    { run_id: 39, finished_at_unix_secs: 1_699_999_000, result: 'ok', ttfb_ms: 76 },
    { run_id: 40, finished_at_unix_secs: 1_699_999_500, result: 'timeout', ttfb_ms: null },
    { run_id: 41, finished_at_unix_secs: 1_700_000_002, result: 'ok', ttfb_ms: 88 },
  ],
  recent_runs: [run, { ...run, id: 40, status: 'failed', result: 'timeout', ttfb_ms: null, http_status: null }],
};

const jsonResponse = (body: unknown, status = 200) =>
  ({ ok: status >= 200 && status < 300, status, statusText: 'OK', json: async () => body }) as Response;

function wrapper() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('Console tunnel probes', () => {
  it('renders measured metrics and preserves failures as chart discontinuities', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) =>
        String(input) === '/tunnel-probes/capability'
          ? jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 })
          : jsonResponse(view),
      ),
    );
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported editable />,
      { wrapper: wrapper() },
    );

    expect(await screen.findByText('线路拨测')).toBeTruthy();
    expect(screen.getByText('80%')).toBeTruthy();
    expect(screen.getByText('240 ms')).toBeTruthy();
    expect(screen.container.querySelectorAll('.tunnel-probe-chart .failure')).toHaveLength(1);
    expect(screen.container.querySelectorAll('.tunnel-probe-chart .line')).toHaveLength(2);
    expect(screen.container.querySelectorAll('.tunnel-probe-chart .reading')).toHaveLength(2);
    expect(screen.container.querySelectorAll('.tunnel-probe-chart .p95')).toHaveLength(1);
    expect(screen.getByText('超时')).toBeTruthy();
    expect(screen.getByText(/起点/).textContent).toContain('Console');
  });

  it('shows all execution phases and sends durable cancellation for an active run', async () => {
    const active: TunnelProbeRun = {
      ...run,
      id: 99,
      status: 'running',
      phase: 'starting-xray',
      result: null,
      ttfb_ms: null,
      http_status: null,
      finished_at_unix_secs: null,
    };
    const activeView: TunnelProbeView = {
      ...view,
      item: { ...view.item, latest_run: active },
      recent_runs: [active],
    };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/tunnel-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (String(input) === '/tunnel-probe-runs/99' && init?.method === 'DELETE') {
        return jsonResponse({ ...active, cancel_requested: true });
      }
      return jsonResponse(activeView);
    });
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported editable />,
      { wrapper: wrapper() },
    );

    expect(await screen.findByText('冻结配置')).toBeTruthy();
    expect(screen.getByText('启动 Xray')).toBeTruthy();
    expect(screen.getByText('请求落点')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith('/tunnel-probe-runs/99', expect.objectContaining({ method: 'DELETE' })),
    );
  });

  it('queues a manual probe against the selected Serving tunnel', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/tunnel-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (String(input) === '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs' && init?.method === 'POST') {
        return jsonResponse(
          {
            run: {
              ...run,
              id: 101,
              status: 'queued',
              phase: 'queued',
              result: null,
              ttfb_ms: null,
              http_status: null,
              finished_at_unix_secs: null,
            },
            reused: false,
          },
          202,
        );
      }
      return jsonResponse(view);
    });
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported editable />,
      { wrapper: wrapper() },
    );

    const button = (await screen.findByRole('button', { name: '拨测 Serving' })) as HTMLButtonElement;
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs',
        expect.objectContaining({ method: 'POST', body: '{"source":"serving","ops":[]}' }),
      ),
    );
  });

  it('queues the browser draft explicitly and labels its frozen fingerprint in history', async () => {
    const draftRun: TunnelProbeRun = {
      ...run,
      id: 102,
      trigger: 'manual',
      source: 'draft',
      serving_generation: null,
      draft_sha256: '1234567890abcdef'.repeat(4),
    };
    const draftView: TunnelProbeView = { ...view, recent_runs: [draftRun, ...view.recent_runs] };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/tunnel-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (String(input) === '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs' && init?.method === 'POST') {
        return jsonResponse({ run: { ...draftRun, status: 'queued', phase: 'queued' }, reused: false }, 202);
      }
      return jsonResponse(draftView);
    });
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported editable />,
      { wrapper: wrapper() },
    );

    expect(await screen.findByText('草稿 12345678')).toBeTruthy();
    fireEvent.change(screen.getByRole('combobox', { name: '拨测目标' }), { target: { value: 'draft' } });
    fireEvent.click(screen.getByRole('button', { name: '拨测草稿' }));
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs',
        expect.objectContaining({ method: 'POST', body: '{"source":"draft","ops":[]}' }),
      ),
    );
  });

  it('does not disclose executor capability from a read-only tunnel panel', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL) => jsonResponse(view));
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported editable={false} />,
      { wrapper: wrapper() },
    );

    expect(await screen.findByText('线路拨测')).toBeTruthy();
    expect((screen.getByRole('button', { name: '拨测 Serving' }) as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/tunnel-probes/capability')).toBe(false);
  });

  it('keeps Serving available when only the current draft changes the tunnel to WARP', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) =>
        String(input) === '/tunnel-probes/capability'
          ? jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 })
          : jsonResponse(view),
      ),
    );
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId={run.outbound_id} draftSupported={false} editable />,
      { wrapper: wrapper() },
    );

    const button = (await screen.findByRole('button', { name: '拨测 Serving' })) as HTMLButtonElement;
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    const select = screen.getByRole('combobox', { name: '拨测目标' }) as HTMLSelectElement;
    expect(select.value).toBe('serving');
    expect((screen.getByRole('option', { name: '草稿（不支持）' }) as HTMLOptionElement).disabled).toBe(true);
  });

  it('keeps an unpublished tunnel in an explicit pre-release state', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/tunnel-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (String(input) === '/tenants/platform.acme/tunnels/future-edge/probe-runs' && init?.method === 'POST') {
        return jsonResponse(
          {
            run: {
              ...run,
              id: 103,
              outbound_id: 'future-edge',
              source: 'draft',
              serving_generation: null,
              draft_sha256: 'a'.repeat(64),
              status: 'queued',
              phase: 'queued',
              result: null,
            },
            reused: false,
          },
          202,
        );
      }
      return jsonResponse({ message: 'not in Serving' }, 404);
    });
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(
      <TunnelProbePanel tenantId={run.tenant_id} outboundId="future-edge" draftSupported editable />,
      { wrapper: wrapper() },
    );

    expect(await screen.findByText('可拨测当前草稿')).toBeTruthy();
    expect(screen.getByText(/定时监测仍需在发布后启用/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '拨测草稿' }));
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/tenants/platform.acme/tunnels/future-edge/probe-runs',
        expect.objectContaining({ method: 'POST', body: '{"source":"draft","ops":[]}' }),
      ),
    );
  });

  it('lists only Console-supported tunnels in Settings', async () => {
    const list: TunnelProbeList = {
      origin: 'console',
      endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace',
      retention_days: 7,
      items: [
        view.item,
        {
          ...view.item,
          outbound_id: 'warp-8f3a-2d71',
          name: 'Cloudflare WARP',
          protocol: 'warp',
          supported: false,
          unsupported_reason: '需要独立的 Console 身份',
          health: 'unknown',
          policy: null,
          latest_run: null,
        },
      ],
    };
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/tunnel-probes/capability') {
        return jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });
      }
      if (init?.method === 'PUT') return jsonResponse({ ...view.item.policy, enabled: false });
      return jsonResponse(list);
    });
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(<TunnelProbeSettingsSection editable />, { wrapper: wrapper() });

    expect(await screen.findByText('隧道定时监测')).toBeTruthy();
    const vendor = (await screen.findByRole('checkbox', {
      name: '供应商出口 定时监测',
    })) as HTMLInputElement;
    expect(vendor.checked).toBe(true);
    expect(screen.queryByRole('checkbox', { name: 'Cloudflare WARP 定时监测' })).toBeNull();
    fireEvent.click(vendor);
    await vi.waitFor(() => {
      const put = fetchMock.mock.calls.find(([, init]) => init?.method === 'PUT');
      expect(JSON.parse(String(put?.[1]?.body))).toEqual({ enabled: false, interval_secs: 300, timeout_secs: 10 });
    });
  });
});
