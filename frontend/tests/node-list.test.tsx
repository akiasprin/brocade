import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, NodeNicView, NodeLoadView, NodeTrafficItem, Whoami } from '../src/api';
import type { NodesPane as NodesPaneComponent } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';
import { SCENES, agentState, nicView, trafficItem, loadListView } from './fixtures/node-roster-data';

let NodesPane: typeof NodesPaneComponent;
let announceNodeRemoval: (text: string) => void;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: media.includes('max-width: 820px'),
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ NodesPane } = await import('../src/panes/nodes'));
  ({ announceNodeRemoval } = await import('../src/panes/node-lifecycle'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const admin: Whoami = {
  operator_id: 'operator',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};

const win: Win = {
  id: 1,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1000,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'list' } },
};

const fleet = (machines = SCENES.typical.machines()) =>
  machines.map(machine => agentState(machine, Math.floor(Date.now() / 1000)));

it('shows live NIC rates and gaps without replacing the historical curve', async () => {
  class Events {
    static sources: Events[] = [];
    handlers = new Map<string, (event: { data: string }) => void>();
    constructor(readonly url: string) {
      Events.sources.push(this);
    }
    addEventListener(name: string, callback: (event: { data: string }) => void) {
      this.handlers.set(name, callback);
    }
    close() {}
    send(name: string, value: unknown) {
      this.handlers.get(name)?.({ data: JSON.stringify(value) });
    }
  }
  vi.stubGlobal('EventSource', Events);
  const machine = SCENES.calm.machines()[0];
  const now = Math.floor(Date.now() / 1000);
  mount(fleet([machine]), admin, true, { nic: [nicView(machine, now, 24)], traffic: [] });
  const source = Events.sources.find(source => source.url === '/realtime/nodes/events')!;
  await waitFor(() => expect(document.querySelector('.node-card-rate.none')).toBeNull());
  const curve = document.querySelector('.node-card-comet')!.innerHTML;
  act(() =>
    source.send('sample', {
      node_id: machine.id,
      received_at_unix_millis: Date.now() + 4_000,
      sample: { rx_bytes_per_sec: 15_000_000, tx_bytes_per_sec: 5_000_000, has_gap: false },
    }),
  );
  expect(document.querySelector('.node-card-rate')!.textContent).toContain('160');
  expect(document.querySelector('.node-card-rate')!.getAttribute('title')).toContain('实时采样');
  expect(document.querySelector('.node-card-comet')!.innerHTML).toBe(curve);
  act(() =>
    source.send('sample', {
      node_id: machine.id,
      received_at_unix_millis: Date.now() + 4_001,
      sample: { rx_bytes_per_sec: 0, tx_bytes_per_sec: 0, has_gap: true },
    }),
  );
  expect(document.querySelector('.node-card-rate')!.textContent).toBe('—');
});

/** 典型场景：24 台在线与 3 台退役（洛杉矶 旧 GIA、首尔 旧机已停用；香港 旧 BGP 仍在退役中）。 */
function mount(
  nodes: NodeAgentStateItem[],
  who: Whoami = admin,
  bare = true,
  metrics: { nic: NodeNicView[]; traffic: NodeTrafficItem[]; health?: NodeLoadView[] } = { nic: [], traffic: [] },
) {
  const fetcher = vi.fn((path: string) => {
    if (path === '/node-traffic') return Promise.resolve(Response.json({ nodes: metrics.traffic }));
    if (path === '/load/nodes?windows=6') return Promise.resolve(Response.json({ nodes: metrics.health ?? [] }));
    if (path.startsWith('/load/nodes/nic')) return Promise.resolve(Response.json({ nodes: metrics.nic }));
    if (path === '/nodes/agent-state') return Promise.resolve(Response.json({ nodes }));
    return Promise.resolve(Response.json({ nodes: [] }));
  });
  vi.stubGlobal('fetch', fetcher);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['nodes'], { nodes });
  render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: nodes.length, chain_group_count: [] } }}>
        <NodesPane win={win} bare={bare} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return { fetcher, client };
}

const cardNames = () =>
  [...document.querySelectorAll('.node-card .node-card-name')].map(element => element.textContent ?? '');
const button = (name: string | RegExp) => screen.getByRole('button', { name }) as HTMLButtonElement;

describe('machine list title bar', () => {
  it('keeps machine flags free of maintenance badges without fetching per-machine maintenance views', () => {
    const nodes = fleet().slice(0, 4);
    nodes[0].ip_maintenance = { configured: true, enabled: true };
    nodes[1].ip_maintenance = { configured: true, enabled: false };
    nodes[2].ip_maintenance = { configured: false, enabled: false };
    delete nodes[3].ip_maintenance;
    const { fetcher } = mount(nodes);
    for (const node of nodes) {
      const card = screen.getByRole('button', { name: `查看 ${node.name || node.node_id} 的机器详情` });
      expect(card.querySelector('.node-card-flag')).not.toBeNull();
      expect(within(card).queryByRole('img', { name: /自动养护/ })).toBeNull();
    }
    expect(fetcher.mock.calls.some(([path]) => path.includes('/ip-maintenance'))).toBe(false);
  });

  it('uses the icon title bar shared with the usage page', () => {
    mount(fleet());

    const panel = document.querySelector<HTMLElement>('.nd-sheet.node-cardpage .node-list-panel');
    const header = panel!.querySelector<HTMLElement>(':scope > header')!;
    expect(within(header).getByRole('heading', { level: 1, name: '机器' })).toBeTruthy();
    expect(
      within(header)
        .getAllByRole('button')
        .map(item => item.textContent),
    ).toEqual(['＋ 纳管机器']);
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.queryByRole('button', { name: '更多操作' })).toBeNull();
  });

  it('keeps retired machines at the end of the same grid instead of a second section', () => {
    mount(fleet());

    expect(document.querySelectorAll('.node-list-panel')).toHaveLength(1);
    expect(document.querySelectorAll('.node-card-grid')).toHaveLength(1);
    const names = cardNames();
    expect(names).toHaveLength(27);
    expect(names.slice(-3)).toEqual(['洛杉矶 旧 GIA', '首尔 旧机', '香港 旧 BGP']);
    expect([...document.querySelectorAll('.node-card.off .node-card-name')].map(name => name.textContent)).toEqual(
      names.slice(-3),
    );
  });

  it('only opens machines: no checkboxes, selection or batch retire and remove', () => {
    const page = mount(fleet());

    expect(screen.queryAllByRole('checkbox')).toHaveLength(0);
    for (const name of ['多选', '取消', /退役下线/, /移除选中/]) {
      expect(screen.queryByRole('button', { name })).toBeNull();
    }
    // 每张卡片（含退役中与已退役）都只是进入详情的按钮，退役与移出在详情的 ⋯ 菜单里逐台确认。
    const cards = [...document.querySelectorAll<HTMLElement>('.node-card')];
    expect(cards).toHaveLength(27);
    expect(cards.every(card => card.tagName === 'BUTTON' && !card.classList.contains('picked'))).toBe(true);
    expect(page.fetcher.mock.calls.some(([path]) => path.includes('/status') || path === '/nodes')).toBe(false);
  });

  it('marks a retirement that will end in removal on its card', () => {
    const nodes = fleet();
    nodes.find(node => node.node_id === 'hk-old')!.removal_status = 'queued';
    // 旧字段留在运行中的机器上不算已安排：恢复会撤销这次移出。
    nodes.find(node => node.node_id === 'jp-iij')!.removal_status = 'queued';
    mount(nodes);

    const retiring = button('查看 香港 旧 BGP 的机器详情');
    expect(retiring.querySelector('.node-card-status')?.textContent).toBe('退役中 · 完成后移出');
    expect(button('查看 东京 IIJ 的机器详情').textContent).not.toContain('完成后移出');
  });

  it('confirms a removal made from the detail page once the list is back', async () => {
    vi.useFakeTimers();
    try {
      mount(fleet());
      act(() => announceNodeRemoval('已移出 首尔 旧机，删除 2 条线路'));
      const notice = screen.getByRole('status');
      expect(notice.textContent).toContain('已移出 首尔 旧机，删除 2 条线路');
      // 提示浮在网格之上，不占据卡片流。
      expect(notice.closest('.node-list-body')).toBeNull();
      act(() => vi.advanceTimersByTime(6_600));
      expect(screen.queryByRole('status')).toBeNull();

      act(() => announceNodeRemoval('已移出 洛杉矶 旧 GIA'));
      fireEvent.click(button('关闭提示'));
      expect(screen.queryByRole('status')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('hides the tools from visitors and disables them for read-only operators', () => {
    mount(fleet(), {
      operator_id: 'public',
      role: 'readonly',
      tenant_scope: null,
      token_prefix: null,
      masked_assets: true,
    });
    expect(screen.queryByRole('button', { name: '＋ 纳管机器' })).toBeNull();
    expect(cardNames()).toHaveLength(27);
    cleanup();

    mount(fleet(), { ...admin, operator_id: 'reviewer', role: 'readonly', masked_assets: true });
    expect(button('＋ 纳管机器').disabled).toBe(true);
  });

  it('shows the empty state with only the provisioning entry when there is no machine yet', () => {
    mount([]);
    expect(screen.getByText('还没有机器')).toBeTruthy();
    expect(button('＋ 纳管机器').disabled).toBe(false);
  });

  it('keeps the same title bar and cards inside a window', () => {
    mount(fleet(), admin, false);
    expect(document.querySelector('.cardpage')).toBeNull();
    const panel = document.querySelector<HTMLElement>('.node-list-panel')!;
    expect(within(panel).getByRole('heading', { level: 1, name: '机器' })).toBeTruthy();
    expect(cardNames().slice(-3)).toEqual(['洛杉矶 旧 GIA', '首尔 旧机', '香港 旧 BGP']);
  });
});

describe('machine card readings', () => {
  const machine = SCENES.typical.machines().find(machine => machine.id === 'mo-ctm')!;
  it('distinguishes a measured zero from missing, stale and gapped rates', async () => {
    const now = Math.floor(Date.now() / 1000);
    const view = nicView(machine, now, 24);
    const page = mount(fleet([machine]), admin, true, { nic: [view], traffic: [trafficItem(machine, now)] });
    await waitFor(() => expect(document.querySelector('.node-card-rate.none')).toBeNull());
    for (const change of ['zero', 'stale', 'gap'] as const) {
      const copy = { ...view, series: view.series.map(sample => ({ ...sample })) };
      const last = copy.series.at(-1)!;
      if (change === 'zero') {
        last.nic_rx_bps = 0;
        last.nic_tx_bps = 0;
      }
      if (change === 'stale')
        copy.series = copy.series.map(sample => ({
          ...sample,
          window_start_unix_secs: sample.window_start_unix_secs - 180,
          window_end_unix_secs: sample.window_end_unix_secs - 180,
        }));
      if (change === 'gap') last.has_gap = true;
      await act(async () => page.client.setQueryData(['node-nic-list', 'windows', 24], { nodes: [copy] }));
      if (change === 'zero') {
        await waitFor(() => expect(document.querySelector('.node-card-rate')?.textContent).toMatch(/^0/));
        expect(document.querySelector('.node-card-rate.none')).toBeNull();
      } else await waitFor(() => expect(document.querySelector('.node-card-rate.none')?.textContent).toBe('—'));
    }
    expect(
      page.fetcher.mock.calls.some(([path]) => path.includes('/ping-probe/') || path.startsWith('/load/nodes/mo-ctm')),
    ).toBe(false);
  });
  it('labels the billing cycle consistently and keeps a whole-number 100 GiB quota intact', async () => {
    const now = Math.floor(Date.now() / 1000);
    const traffic = {
      ...trafficItem(machine, now),
      cycle_kind: 'yearly' as const,
      total_bytes: String(90 * 1024 ** 3),
      quota_bytes: String(100 * 1024 ** 3),
    };
    mount(fleet([machine]), admin, true, { nic: [nicView(machine, now, 24)], traffic: [traffic] });
    await waitFor(() => expect(document.querySelector('.node-card-percent')?.textContent).toBe('90%'));
    expect(document.querySelector('.node-card-month')?.textContent).toContain('本周期');
    expect(document.querySelector('.node-card-month')?.textContent).not.toMatch(/本月|本年/);
    expect(document.querySelector('.node-card-quota')?.textContent?.trim()).toBe('/ 100 GiB');
    expect(document.querySelector('.node-card-month')?.getAttribute('title')).toContain('UTC');
  });
  it('keeps unavailable accounting unknown rather than labeling it unlimited', async () => {
    mount(fleet([machine]));
    await waitFor(() => expect(document.querySelector('.node-card-month b.none')?.textContent).toBe('—'));
    expect(document.querySelector('.node-card-corner')).toBeNull();
    expect(document.querySelector('.node-card-quota')).toBeNull();
  });
});

describe('comet and health-ring cards', () => {
  const machine = SCENES.typical.machines().find(machine => machine.id === 'mo-ctm')!;
  const readings = () => [...document.querySelectorAll('.node-card-resource b')].map(el => el.textContent);
  it.each([
    [1.17, 75, 85],
    [2.5, 50, 65],
    [5, 5, 20],
    [10, 5, 20],
    [100, 5, 20],
    [500, 5, 20],
  ])('keeps %s Mbit/s at an appropriate height and expands for high traffic', async (mbps, minTop, maxTop) => {
    const now = Math.floor(Date.now() / 1000);
    const view = nicView(machine, now, 24);
    view.series = view.series.map(sample => ({
      ...sample,
      nic_rx_bps: mbps * 1e6 * 0.6,
      nic_tx_bps: mbps * 1e6 * 0.4,
    }));
    mount(fleet([machine]), admin, true, { nic: [view], traffic: [] });
    await waitFor(() => expect(document.querySelector('.node-card-comet-now')).toBeTruthy());
    const top = parseFloat(document.querySelector<HTMLElement>('.node-card-comet-now')!.style.top);
    expect(top).toBeGreaterThan(minTop);
    expect(top).toBeLessThan(maxTop);
    const curve = document.querySelector('.node-card-comet .trail')!.getAttribute('d')!;
    const firstY = Number(/^M0 ([\d.]+)/.exec(curve)![1]);
    expect((firstY / 46) * 100).toBeCloseTo(top, 1);
    const rate = document.querySelector('.node-card-rate')!;
    expect(parseFloat(rate.textContent!)).toBe(mbps);
    expect(rate.querySelector('small')?.textContent).toBe('Mbit/s');
  });
  it('keeps zero-traffic curves visible, breaks explicit and absent windows and drops unknown endpoints', async () => {
    const now = Math.floor(Date.now() / 1000);
    const view = nicView(machine, now, 24);
    view.series = view.series.map(sample => ({ ...sample, nic_rx_bps: 0, nic_tx_bps: 0 }));
    const page = mount(fleet([machine]), admin, true, { nic: [view], traffic: [] });
    await waitFor(() => expect(document.querySelector('.node-card-comet-now')).toBeTruthy());
    expect(document.querySelectorAll('.node-card-comet .trail')).toHaveLength(1);
    expect(document.querySelector('.node-card-comet .trail')?.getAttribute('d')).toContain('45.50');
    const gapped = { ...view, series: view.series.map(sample => ({ ...sample })) };
    gapped.series[10].has_gap = true;
    gapped.series.splice(16, 2);
    gapped.series.at(-1)!.has_gap = true;
    await act(async () => page.client.setQueryData(['node-nic-list', 'windows', 24], { nodes: [gapped] }));
    await waitFor(() => expect(document.querySelectorAll('.node-card-comet .trail')).toHaveLength(3));
    expect(document.querySelector('.node-card-comet-now')).toBeNull();
    expect(document.querySelector('.node-card-rate.none')?.textContent).toBe('—');
  });
  it('shows CPU, memory and disk usage with their own warning thresholds', async () => {
    const now = Math.floor(Date.now() / 1000);
    const view = loadListView(machine, now);
    view.host = { ...view.host!, mem_total_bytes: 1000, disk_total_bytes: 1000 };
    view.series = view.series.map(sample => ({
      ...sample,
      cpu_user_pct: 80,
      cpu_sys_pct: 0,
      cpu_softirq_pct: 0,
      mem_available_bytes: 150,
      disk_free_bytes: 30,
    }));
    const page = mount(fleet([machine]), admin, true, {
      nic: [nicView(machine, now, 24)],
      traffic: [],
      health: [view],
    });
    await waitFor(() => expect(readings()).toEqual(['80%', '85%', '97%']));
    expect(document.querySelectorAll('.node-card-resource.warn')).toHaveLength(2);
    expect(document.querySelectorAll('.node-card-resource.bad')).toHaveLength(1);
    expect(button(`查看 ${machine.name} 的机器详情`).getAttribute('aria-describedby')?.split(' ')).toContain(
      document.querySelector('.node-card-resources')!.id,
    );
    expect(page.fetcher.mock.calls.filter(([path]) => path === '/load/nodes?windows=6')).toHaveLength(1);
    expect(
      page.fetcher.mock.calls.some(
        ([path]) => /^\/load\/nodes\/[^?]/.test(path) && !path.startsWith('/load/nodes/nic'),
      ),
    ).toBe(false);
  });
  it('bounds the average to 3 minutes and uses the latest disk observation', async () => {
    const now = Math.floor(Date.now() / 1000);
    const view = loadListView(machine, now);
    view.host = { ...view.host!, mem_total_bytes: 1000, disk_total_bytes: 1000 };
    view.series = view.series.map((sample, index) => ({
      ...sample,
      cpu_user_pct: 0,
      cpu_sys_pct: 0,
      cpu_softirq_pct: 0,
      mem_available_bytes: 1000,
      disk_free_bytes: index === 5 ? 1000 : 0,
    }));
    view.series.unshift({
      ...view.series[0],
      window_start_unix_secs: now - 630,
      window_end_unix_secs: now - 600,
      cpu_user_pct: 100,
      mem_available_bytes: 0,
    });
    mount(fleet([machine]), admin, true, { nic: [], traffic: [], health: [view] });
    await waitFor(() => expect(readings()).toEqual(['0%', '0%', '0%']));
    expect(document.querySelectorAll('.node-card-resource.none')).toHaveLength(0);
  });
  it.each([-600, 600])('shows accepted resource windows with a %i-second browser clock offset', async offset => {
    const now = Math.floor(Date.now() / 1000);
    const view = loadListView(machine, now);
    view.host = { ...view.host!, mem_total_bytes: 1000, disk_total_bytes: 1000 };
    view.series = view.series.map(sample => ({
      ...sample,
      window_start_unix_secs: sample.window_start_unix_secs + offset,
      window_end_unix_secs: sample.window_end_unix_secs + offset,
      cpu_user_pct: 20,
      cpu_sys_pct: 5,
      cpu_softirq_pct: 0,
      mem_available_bytes: 600,
      disk_free_bytes: 700,
    }));
    const page = mount(fleet([machine]), admin, true, { nic: [], traffic: [], health: [view] });
    await waitFor(() => expect(readings()).toEqual(['25%', '40%', '30%']));
    const next = { ...view, series: view.series.map(sample => ({ ...sample, cpu_user_pct: 30 })) };
    await act(async () => page.client.setQueryData(['node-load-list', 'windows', 6], { nodes: [next] }));
    await waitFor(() => expect(readings()).toEqual(['35%', '40%', '30%']));
  });
  it.each(['gap', 'missing', 'offline', 'retired', 'invalid-totals'] as const)(
    'does not report %s resource observations as healthy',
    async kind => {
      const now = Math.floor(Date.now() / 1000);
      const view = loadListView(machine, now);
      const nodes = fleet([machine]);
      if (kind === 'gap') view.series.at(-1)!.has_gap = true;
      if (kind === 'missing') view.series = [];
      if (kind === 'offline') {
        nodes[0].runtime_report_fresh = false;
        nodes[0].desired_poll_fresh = false;
        nodes[0].last_poll_at = new Date(Date.now() - 300_000).toISOString();
      }
      if (kind === 'retired') {
        nodes[0].lifecycle_phase = 'retired';
        nodes[0].retired_at = new Date().toISOString();
      }
      if (kind === 'invalid-totals') view.host = { ...view.host!, mem_total_bytes: 0, disk_total_bytes: 0 };
      mount(nodes, admin, true, { nic: [], traffic: [], health: [view] });
      await waitFor(() =>
        expect(document.querySelector('.node-card-resources')?.getAttribute('data-state')).not.toBe('pending'),
      );
      expect(readings().slice(kind === 'invalid-totals' ? 1 : 0)).toEqual(
        kind === 'invalid-totals' ? ['—', '—'] : ['—', '—', '—'],
      );
    },
  );
});
