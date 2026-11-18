import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { TunnelProbeList, TunnelProbeRun, TunnelProbeView } from '../src/api';

/* Each chart keeps the option it was last given, so the tests read the series the panel drew. */
const chartMock = vi.hoisted(() => ({ options: [] as Array<Record<string, unknown>> }));

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  init: vi.fn(() => ({
    setOption(option: Record<string, unknown>) {
      chartMock.options.push(option);
    },
    resize: vi.fn(),
    dispose: vi.fn(),
  })),
}));
vi.mock('echarts/charts', () => ({ CustomChart: {}, LineChart: {}, ScatterChart: {} }));
vi.mock('echarts/components', () => ({
  GridComponent: {},
  MarkAreaComponent: {},
  MarkLineComponent: {},
  TooltipComponent: {},
}));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let TunnelProbePanel: typeof import('../src/tunnel-probe').TunnelProbePanel;
let TunnelProbeActions: typeof import('../src/tunnel-probe').TunnelProbeActions;
let TunnelProbeDraftAction: typeof import('../src/tunnel-probe').TunnelProbeDraftAction;
let useTunnelProbeControls: typeof import('../src/tunnel-probe').useTunnelProbeControls;
let groupTunnelProbeRuns: typeof import('../src/tunnel-probe').groupTunnelProbeRuns;
let tunnelProbeImpact: typeof import('../src/tunnel-probe').tunnelProbeImpact;
let draft: typeof import('../src/draft').draft;

beforeAll(async () => {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  ({
    TunnelProbePanel,
    TunnelProbeActions,
    TunnelProbeDraftAction,
    useTunnelProbeControls,
    groupTunnelProbeRuns,
    tunnelProbeImpact,
  } = await import('../src/tunnel-probe'));
  ({ draft } = await import('../src/draft'));
});

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

const timeoutRun: TunnelProbeRun = {
  ...run,
  id: 40,
  status: 'failed',
  result: 'timeout',
  ttfb_ms: null,
  http_status: null,
  exit_ip: null,
  exit_loc: null,
  error_code: 'timeout',
  error_detail: '10 秒内未收到首字节',
  finished_at_unix_secs: 1_699_999_500,
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
    health_reason: { kind: 'low-success-rate', succeeded: 3, samples: 5 },
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
  slow_ttfb_ms: 300,
  bucket_secs: 360,
  summary: {
    window_secs: 86_400,
    total: 5,
    succeeded: 4,
    success_rate: 0.8,
    p50_ms: 88,
    p95_ms: 240,
    failures: 1,
    timeouts: 1,
    connect_failures: 0,
    target_failures: 0,
    exit_ips: 2,
  },
  points: [
    { run_id: 39, finished_at_unix_secs: 1_699_999_000, result: 'ok', ttfb_ms: 76 },
    { run_id: 40, finished_at_unix_secs: 1_699_999_500, result: 'timeout', ttfb_ms: null },
    { run_id: 41, finished_at_unix_secs: 1_700_000_002, result: 'ok', ttfb_ms: 88 },
  ],
  recent_runs: [run, timeoutRun],
};

const capability = { available: true, version: '26.4.25', reason: null, concurrency: 30 };
const inventory: TunnelProbeList = {
  origin: 'console',
  endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace',
  retention_days: 7,
  items: [view.item],
};

const jsonResponse = (body: unknown, status = 200) =>
  ({ ok: status >= 200 && status < 300, status, statusText: 'OK', json: async () => body }) as Response;

function wrapper() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
}

/** The detail page owns the window; the panel only reports a switch. */
function Panel({
  outboundId = run.outbound_id,
  draftSupported = true,
  editable = true,
}: {
  outboundId?: string;
  draftSupported?: boolean;
  editable?: boolean;
}) {
  const [windowSecs, setWindowSecs] = useState(86_400);
  const controls = useTunnelProbeControls({
    tenantId: run.tenant_id,
    outboundId,
    draftSupported,
    editable,
    windowSecs,
  });
  return (
    <>
      <header>
        <TunnelProbeDraftAction controls={controls} />
        <TunnelProbeActions controls={controls} />
      </header>
      <TunnelProbePanel controls={controls} windowSecs={windowSecs} onWindowChange={setWindowSecs} />
    </>
  );
}

/** Answers capability and inventory reads; everything else goes to `rest`. */
const routes =
  (rest: (input: string, init?: RequestInit) => Response) => async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input) === '/tunnel-probes/capability') return jsonResponse(capability);
    if (String(input) === '/tunnel-probes') return jsonResponse(inventory);
    return rest(String(input), init);
  };

afterEach(() => {
  cleanup();
  draft.clear();
  chartMock.options.length = 0;
  vi.unstubAllGlobals();
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
});

describe('Console tunnel probes', () => {
  it('states the health rule outcome and the measured readings', async () => {
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse(view))));
    const screen = render(<Panel />, { wrapper: wrapper() });

    expect(await screen.findByText('变慢')).toBeTruthy();
    expect(screen.getByText('最近 5 次成功 3 次')).toBeTruthy();
    const metrics = screen.container.querySelector('.tnd-metrics') as HTMLElement;
    expect(within(metrics).getByText('可用率').nextElementSibling?.textContent).toBe('80%');
    expect(within(metrics).getByText('P95').nextElementSibling?.textContent).toBe('240ms');
    expect(within(metrics).getByText('超时 1')).toBeTruthy();
    expect(within(metrics).getByText('203.0.113.8')).toBeTruthy();
    expect(within(metrics).getByText(/24 小时内 2 个 IP/)).toBeTruthy();
    expect(screen.getByText('10 秒内未收到首字节')).toBeTruthy();
    expect(await screen.findByText(/Console → cp\.cloudflare\.com\/cdn-cgi\/trace/)).toBeTruthy();
    expect(screen.getByText('每 5 分钟 · 超时 10 秒')).toBeTruthy();
  });

  it('draws failure buckets as line breaks, full-height shading and an aligned status lane', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(1_700_000_300_000);
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse(view))));
    render(<Panel />, { wrapper: wrapper() });

    await vi.waitFor(() => expect(chartMock.options.length).toBeGreaterThan(0));
    const option = chartMock.options.at(-1) as {
      series: Array<{ type: string; data: unknown[]; markLine?: { data: unknown[] } }>;
    };
    const [line, , lane] = option.series;
    expect(line.data).toContainEqual([1_699_999_500_000, null]);
    const [slow, median] = line.markLine?.data ?? [];
    expect(slow).toMatchObject({ yAxis: 300 });
    // The median covers the observed runs only: from the first point to the window end.
    expect(median).toMatchObject([{ coord: [1_699_999_000_000, 88] }, {}]);
    expect(lane.type).toBe('custom');
    const bucketStart = Math.floor(1_699_999_500 / 360) * 360_000;
    expect(lane.data).toContainEqual([bucketStart + 180_000, bucketStart, bucketStart + 360_000]);
    expect((line as unknown as { markArea: { data: unknown[] } }).markArea.data).toContainEqual([
      expect.objectContaining({ xAxis: bucketStart, itemStyle: expect.objectContaining({ opacity: 0.1 }) }),
      { xAxis: bucketStart + 360_000 },
    ]);
  });

  it('shows the execution phase and sends durable cancellation for an active run', async () => {
    const active: TunnelProbeRun = {
      ...run,
      id: 99,
      trigger: 'manual',
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
      recent_runs: [active, ...view.recent_runs],
    };
    const fetchMock = vi.fn(
      routes((input, init) =>
        input === '/tunnel-probe-runs/99' && init?.method === 'DELETE'
          ? jsonResponse({ ...active, cancel_requested: true })
          : jsonResponse(activeView),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(<Panel />, { wrapper: wrapper() });

    const progress = await screen.findByText(/启动 Xray · 第 3 步，共 4 步/);
    expect(progress.closest('[role="status"]')).not.toBeNull();
    expect(screen.queryByRole('button', { name: '立即拨测' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith('/tunnel-probe-runs/99', expect.objectContaining({ method: 'DELETE' })),
    );
  });

  it('queues a manual probe against Serving', async () => {
    const fetchMock = vi.fn(
      routes((input, init) =>
        input === '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs' && init?.method === 'POST'
          ? jsonResponse(
              { run: { ...run, id: 101, status: 'queued', phase: 'queued', result: null }, reused: false },
              202,
            )
          : jsonResponse(view),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(<Panel />, { wrapper: wrapper() });

    const button = (await screen.findByRole('button', { name: '立即拨测' })) as HTMLButtonElement;
    expect(screen.queryByRole('button', { name: '拨测草稿' })).toBeNull();
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs',
        expect.objectContaining({ method: 'POST', body: '{"source":"serving","ops":[]}' }),
      ),
    );
  });

  it('offers the draft only when it edits this tunnel, and labels the frozen fingerprint in history', async () => {
    const draftRun: TunnelProbeRun = {
      ...run,
      id: 102,
      trigger: 'manual',
      source: 'draft',
      serving_generation: null,
      draft_sha256: '1234567890abcdef'.repeat(4),
    };
    const draftView: TunnelProbeView = { ...view, recent_runs: [draftRun, ...view.recent_runs] };
    const fetchMock = vi.fn(
      routes((input, init) =>
        input === '/tenants/platform.acme/tunnels/custom-1111-1111/probe-runs' && init?.method === 'POST'
          ? jsonResponse({ run: { ...draftRun, status: 'queued', phase: 'queued' }, reused: false }, 202)
          : jsonResponse(draftView),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    draft.push({
      op: 'upsert_external_outbound',
      outbound: {
        tenant_id: run.tenant_id,
        id: run.outbound_id,
        name: run.outbound_name,
        address: 'edge.example.com',
        port: 8443,
        protocol: { t: 'anytls', v: { credential: '<redacted>' } },
        security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
      },
    });
    const screen = render(<Panel />, { wrapper: wrapper() });

    expect(await screen.findByText('草稿 12345678')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '更多拨测操作' }));
    const button = screen.getByRole('menuitem', { name: /拨测草稿/ }) as HTMLButtonElement;
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    await vi.waitFor(() => {
      const post = fetchMock.mock.calls.find(([, init]) => init?.method === 'POST');
      const body = JSON.parse(String(post?.[1]?.body));
      expect(body.source).toBe('draft');
      expect(body.ops).toHaveLength(1);
    });
  });

  it('never offers a draft probe the Console cannot run', async () => {
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse(view))));
    draft.push({
      op: 'upsert_external_outbound',
      outbound: {
        tenant_id: run.tenant_id,
        id: run.outbound_id,
        name: run.outbound_name,
        address: 'edge.example.com',
        port: 8443,
        protocol: { t: 'anytls', v: { credential: '<redacted>' } },
        security: { t: 'none' },
      },
    });
    const screen = render(<Panel draftSupported={false} />, { wrapper: wrapper() });

    const button = (await screen.findByRole('button', { name: '立即拨测' })) as HTMLButtonElement;
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    expect(screen.queryByRole('button', { name: '拨测草稿' })).toBeNull();
  });

  it('does not disclose executor capability or the probe target from a read-only panel', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL) => jsonResponse(view));
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(<Panel editable={false} />, { wrapper: wrapper() });

    expect(await screen.findByText('变慢')).toBeTruthy();
    expect((screen.getByRole('button', { name: '立即拨测' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('button', { name: '修改' }) as HTMLButtonElement).disabled).toBe(true);
    const reads = fetchMock.mock.calls.map(([input]) => String(input));
    expect(reads).not.toContain('/tunnel-probes/capability');
    expect(reads).not.toContain('/tunnel-probes');
    expect(screen.queryByText(/Console →/)).toBeNull();
  });

  it('keeps an unpublished tunnel in an explicit pre-release state', async () => {
    const fetchMock = vi.fn(
      routes((input, init) =>
        input === '/tenants/platform.acme/tunnels/future-edge/probe-runs' && init?.method === 'POST'
          ? jsonResponse(
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
            )
          : jsonResponse({ message: 'not in Serving' }, 404),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const screen = render(<Panel outboundId="future-edge" />, { wrapper: wrapper() });

    expect(await screen.findByText('未发布')).toBeTruthy();
    expect(screen.getByText('发布后开始定时拨测')).toBeTruthy();
    expect(screen.getByText('发布后可启用')).toBeTruthy();
    expect(screen.queryByRole('group', { name: '拨测统计范围' })).toBeNull();
    const button = screen.getByRole('button', { name: '拨测草稿' }) as HTMLButtonElement;
    await vi.waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/tenants/platform.acme/tunnels/future-edge/probe-runs',
        expect.objectContaining({ method: 'POST', body: '{"source":"draft","ops":[]}' }),
      ),
    );
  });
});

describe('probe history grouping', () => {
  const scheduled = (id: number, changes: Partial<TunnelProbeRun> = {}): TunnelProbeRun => ({
    ...run,
    id,
    finished_at_unix_secs: 1_700_000_000 - id * 300,
    ...changes,
  });

  it('keeps failures separated after filtering out the intervening success', async () => {
    const recent = [
      scheduled(1, { ...timeoutRun, id: 1, finished_at_unix_secs: 1_700_000_000 }),
      scheduled(2),
      scheduled(3, { ...timeoutRun, id: 3, finished_at_unix_secs: 1_699_999_100 }),
    ];
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse({ ...view, recent_runs: recent }))));
    const screen = render(<Panel />, { wrapper: wrapper() });
    await screen.findByRole('group', { name: '拨测记录筛选' });
    fireEvent.click(screen.getByRole('button', { name: '只看失败' }));
    expect(screen.getAllByText('超时')).toHaveLength(2);
    expect(screen.queryByText('2 次超时')).toBeNull();
  });

  it('groups matching records and exposes the original HTTP status and configuration', async () => {
    const recent = [scheduled(1), scheduled(2)];
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse({ ...view, recent_runs: recent }))));
    const screen = render(<Panel />, { wrapper: wrapper() });
    const label = await screen.findByText('2 次成功');
    const group = label.closest('details')!;
    expect(within(group.querySelector('summary')!).getByText('HTTP 200')).toBeTruthy();
    expect(within(group).queryByText('HTTP 204')).toBeNull();
    fireEvent.click(group.querySelector('summary')!);
    expect(group.querySelectorAll('article')).toHaveLength(2);
    expect(group.querySelector('article')?.title).toBe('Serving G7 · r12');
  });

  it('splits version, HTTP, exit, manual, draft and scheduling-gap boundaries', () => {
    for (const change of [
      { serving_generation: 8 },
      { topology_revision: 13 },
      { settings_revision: 13 },
      { http_status: 201 },
      { ttfb_ms: null },
      { exit_ip: '203.0.113.9' },
      { trigger: 'manual' as const },
      { source: 'draft' as const },
      { finished_at_unix_secs: 1_699_990_000 },
    ]) {
      expect(groupTunnelProbeRuns([scheduled(1), scheduled(2, change)], 300, 300)).toHaveLength(2);
    }
  });

  it('keeps canceled and unsupported records out of the failure filter', async () => {
    const recent = [scheduled(1, { result: 'canceled', status: 'canceled' }), scheduled(2, { result: 'unsupported' })];
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse({ ...view, recent_runs: recent }))));
    const screen = render(<Panel />, { wrapper: wrapper() });
    await screen.findByRole('group', { name: '拨测记录筛选' });
    fireEvent.click(screen.getByRole('button', { name: '只看失败' }));
    expect(screen.getByText('最近 2 次记录中没有失败。')).toBeTruthy();
  });

  it('renders an empty history without inventing readings', async () => {
    const empty: TunnelProbeView = {
      ...view,
      item: { ...view.item, health: 'unknown', health_reason: { kind: 'no-samples' }, latest_run: null, policy: null },
      points: [],
      recent_runs: [],
      summary: {
        ...view.summary,
        total: 0,
        succeeded: 0,
        success_rate: null,
        p50_ms: null,
        p95_ms: null,
        failures: 0,
        exit_ips: 0,
      },
    };
    vi.stubGlobal('fetch', vi.fn(routes(() => jsonResponse(empty))));
    const screen = render(<Panel />, { wrapper: wrapper() });
    expect(await screen.findByText('暂无结论')).toBeTruthy();
    expect(screen.getByText('还没有拨测记录。')).toBeTruthy();
    expect(screen.container.querySelector('.tnd-metrics')?.textContent).not.toContain('null');
    expect(screen.container.querySelector('.tnd-metrics')?.textContent).not.toContain('0%');
  });

  it('does not turn reliability failures into a claim about latency or all business traffic', () => {
    expect(tunnelProbeImpact(view)).toContain('出现失败');
    expect(tunnelProbeImpact(view)).not.toContain('首字节偏慢');
    expect(tunnelProbeImpact({ ...view, item: { ...view.item, health: 'down' } })).toContain('可能受影响');
    expect(tunnelProbeImpact({ ...view, item: { ...view.item, health: 'paused' } })).toBeNull();
  });
});
