import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, NodeNicView, NodeLoadView, NodeTrafficItem, Whoami } from '../src/api';
import type { NodesPane as NodesPaneComponent } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';
import { SCENES, agentState, nicView, trafficItem, loadListView } from './fixtures/node-roster-data';

let NodesPane: typeof NodesPaneComponent;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: media.includes('max-width: 820px'),
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ NodesPane } = await import('../src/panes/nodes'));
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
      received_at_unix_millis: Date.now(),
      sample: { rx_bytes_per_sec: 15_000_000, tx_bytes_per_sec: 5_000_000, has_gap: false },
    }),
  );
  expect(document.querySelector('.node-card-rate')!.textContent).toContain('160');
  expect(document.querySelector('.node-card-rate')!.getAttribute('title')).toContain('实时采样');
  expect(document.querySelector('.node-card-comet')!.innerHTML).toBe(curve);
  act(() =>
    source.send('sample', {
      node_id: machine.id,
      received_at_unix_millis: Date.now() + 1,
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
  let current = nodes;
  let finishRemoval: (() => void) | null = null;
  const fetcher = vi.fn((path: string, init?: RequestInit) => {
    if (path === '/node-traffic') return Promise.resolve(Response.json({ nodes: metrics.traffic }));
    if (path === '/load/nodes?windows=6') return Promise.resolve(Response.json({ nodes: metrics.health ?? [] }));
    if (path.startsWith('/load/nodes/nic')) return Promise.resolve(Response.json({ nodes: metrics.nic }));
    if (path === '/nodes/agent-state') return Promise.resolve(Response.json({ nodes: current }));
    if (path === '/nodes' && init?.method === 'DELETE') {
      const { node_ids } = JSON.parse(String(init.body)) as { node_ids: string[] };
      return new Promise<Response>(resolve => {
        finishRemoval = () => {
          current = current.filter(node => !node_ids.includes(node.node_id));
          resolve(Response.json({ revision_id: 9, removed_nodes: node_ids, removed_chains: [] }));
        };
      });
    }
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
  return { fetcher, client, finishRemoval: () => finishRemoval?.() };
}

const cardNames = () =>
  [...document.querySelectorAll('.node-card .node-card-name')].map(element => element.textContent ?? '');
const button = (name: string | RegExp) => screen.getByRole('button', { name }) as HTMLButtonElement;
const checkbox = (name: string) => screen.getByRole('checkbox', { name }) as HTMLInputElement;

describe('machine list title bar', () => {
  it('uses the icon title bar shared with the usage page', () => {
    mount(fleet());

    const panel = document.querySelector<HTMLElement>('.nd-sheet.node-cardpage .node-list-panel');
    const header = panel!.querySelector<HTMLElement>(':scope > header')!;
    expect(within(header).getByRole('heading', { level: 1, name: '机器' })).toBeTruthy();
    expect(
      within(header)
        .getAllByRole('button')
        .map(item => item.textContent),
    ).toEqual(['多选', '＋ 纳管机器']);
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

  it('picks live machines for retirement and stopped retired ones for removal in one selection', async () => {
    const page = mount(fleet());
    fireEvent.click(button('多选'));

    expect(screen.queryByRole('button', { name: '＋ 纳管机器' })).toBeNull();
    expect(button('退役下线').disabled).toBe(true);
    expect(button('移除选中').disabled).toBe(true);
    // 仍在退役中的机器还没有停用，不能勾选移除。
    expect(checkbox('选中 香港 旧 BGP').disabled).toBe(true);

    fireEvent.click(checkbox('选中 东京 IIJ'));
    fireEvent.click(checkbox('选中 香港 CMI'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    // 两类操作各数各的：退役只算在线机器，移除只算已停用的退役机器。
    expect(button('退役下线 2').disabled).toBe(false);
    expect(button('移除选中 1').disabled).toBe(false);

    fireEvent.click(button('退役下线 2'));
    await waitFor(() => expect(screen.queryAllByRole('checkbox')).toHaveLength(0));
    const retired = page.fetcher.mock.calls
      .filter(([path, init]) => init?.method === 'PUT' && path.endsWith('/status'))
      .map(([path, init]) => [path, JSON.parse(String(init?.body))]);
    expect(retired).toEqual([
      ['/nodes/hk-cmi/status', { status: 'retired' }],
      ['/nodes/jp-iij/status', { status: 'retired' }],
    ]);
    expect(page.fetcher.mock.calls.some(([path, init]) => path === '/nodes' && init?.method === 'DELETE')).toBe(false);
    expect(button('多选')).toBeTruthy();
  });

  it('removes the picked retired machines after confirming and keeps them checked while removing', async () => {
    const page = mount(fleet());
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    fireEvent.click(button('多选'));
    fireEvent.click(checkbox('选中 洛杉矶 旧 GIA'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    fireEvent.click(button('移除选中 2'));

    expect(confirm.mock.calls[0][0]).toContain('确定永久移除 2 台退役机器吗？');
    expect(confirm.mock.calls[0][0]).toContain('首尔 旧机、洛杉矶 旧 GIA');
    await waitFor(() => expect(button('移除中…').disabled).toBe(true));
    const removal = page.fetcher.mock.calls.find(([path, init]) => path === '/nodes' && init?.method === 'DELETE');
    expect(JSON.parse(String(removal?.[1]?.body))).toEqual({ node_ids: ['kr-old', 'us-old'] });
    expect(button('取消').disabled).toBe(true);
    expect(checkbox('选中 首尔 旧机').checked).toBe(true);
    expect(checkbox('选中 洛杉矶 旧 GIA').checked).toBe(true);

    await act(async () => page.finishRemoval());
    await waitFor(() => expect(screen.getByText('已移除 2 台机器，并清理 0 条关联线路。')).toBeTruthy());
    await waitFor(() => expect(cardNames()).toHaveLength(25));
    expect(cardNames().at(-1)).toBe('香港 旧 BGP');
    expect(screen.queryAllByRole('checkbox')).toHaveLength(0);

    // 没有可移除的退役机器以后，多选里只剩退役一种操作。
    fireEvent.click(button('多选'));
    expect(screen.queryByRole('button', { name: /移除选中/ })).toBeNull();
  });

  it('does not remove anything when the confirmation is declined', () => {
    const page = mount(fleet());
    vi.spyOn(window, 'confirm').mockReturnValue(false);
    fireEvent.click(button('多选'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    fireEvent.click(button('移除选中 1'));

    expect(page.fetcher.mock.calls.some(([path, init]) => path === '/nodes' && init?.method === 'DELETE')).toBe(false);
    expect(checkbox('选中 首尔 旧机').checked).toBe(true);
  });

  it('hides the tools from visitors and disables them for read-only operators', () => {
    mount(fleet(), {
      operator_id: 'public',
      role: 'readonly',
      tenant_scope: null,
      token_prefix: null,
      masked_assets: true,
    });
    expect(screen.queryByRole('button', { name: '多选' })).toBeNull();
    expect(screen.queryByRole('button', { name: '＋ 纳管机器' })).toBeNull();
    expect(cardNames()).toHaveLength(27);
    cleanup();

    mount(fleet(), { ...admin, operator_id: 'reviewer', role: 'readonly', masked_assets: true });
    expect(button('多选').disabled).toBe(true);
    expect(button('＋ 纳管机器').disabled).toBe(true);
  });

  it('shows the empty state without 多选 when there is no machine yet', () => {
    mount([]);
    expect(screen.getByText('还没有机器')).toBeTruthy();
    expect(screen.queryByRole('button', { name: '多选' })).toBeNull();
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
    expect(button(`查看 ${machine.name} 的机器详情`).getAttribute('aria-describedby')).toBe(
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
  it.each(['stale', 'gap', 'missing', 'offline', 'retired', 'invalid-totals', 'future'] as const)(
    'does not report %s resource observations as healthy',
    async kind => {
      const now = Math.floor(Date.now() / 1000);
      const view = loadListView(machine, now);
      const nodes = fleet([machine]);
      if (kind === 'stale' || kind === 'future')
        view.series = view.series.map(sample => ({
          ...sample,
          window_start_unix_secs: sample.window_start_unix_secs + (kind === 'stale' ? -300 : 300),
          window_end_unix_secs: sample.window_end_unix_secs + (kind === 'stale' ? -300 : 300),
        }));
      if (kind === 'gap') view.series.at(-1)!.has_gap = true;
      if (kind === 'missing') view.series = [];
      if (kind === 'offline') {
        nodes[0].runtime_report_fresh = false;
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
