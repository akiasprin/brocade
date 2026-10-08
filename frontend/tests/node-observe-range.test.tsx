import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, Whoami } from '../src/api';
import * as route from '../src/forge/route';
import { SessionProvider } from '../src/session';
import { customLoadRange } from '../src/ui/observe-range';
import type { Win } from '../src/wm/store';

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  connect: vi.fn(),
  init: vi.fn(() => ({
    setOption: vi.fn(),
    showLoading: vi.fn(),
    hideLoading: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
    dispatchAction: vi.fn(),
    group: '',
  })),
}));
vi.mock('echarts/charts', () => ({ CustomChart: {}, LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, MarkLineComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let NodesPane: typeof import('../src/panes/nodes').NodesPane;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  ({ NodesPane } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.history.replaceState(null, '', '/');
});

const who: Whoami = {
  operator_id: 'operator',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};

const emptyLoadOverview = {
  node_id: 'n1',
  range_start_unix_secs: 0,
  range_end_unix_secs: 0,
  reported_at_unix_secs: null,
  clock_skew_secs: null,
  host: null,
  latest_sample: null,
  processes: [],
  series: {
    window_start_unix_secs: [],
    window_end_unix_secs: [],
    has_gap: [],
    cpu_user_pct: [],
    cpu_sys_pct: [],
    cpu_softirq_pct: [],
    cpu_peak_pct: [],
    cpu_steal_pct: [],
    load1: [],
    mem_available_bytes: [],
    swap_used_bytes: [],
    oom_kills: [],
    disk_free_bytes: [],
    disk_inode_free_pct: [],
    nic_rx_bps: [],
    nic_tx_bps: [],
    nic_rx_drop: [],
    nic_tx_drop: [],
    nic_err: [],
    conntrack_count: [],
    uptime_secs: [],
  },
};

/* The three observation reads answer for any range; everything else stays pending. */
function respond(path: string): Promise<Response> {
  if (path.startsWith('/load/nodes/n1/overview?')) return Promise.resolve(Response.json(emptyLoadOverview));
  if (path.startsWith('/usage/node-series?')) {
    return Promise.resolve(Response.json({ since: '', month_start: '', nodes: [] }));
  }
  if (path.startsWith('/ping-probe/nodes/n1/series?'))
    return Promise.resolve(Response.json({ node_id: 'n1', targets: [] }));
  return new Promise<Response>(() => undefined);
}

const winWith = (drill: Record<string, unknown>): Win => ({
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
  data: { drill },
});

function mount(drill: Record<string, unknown>) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => respond(String(input))),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(['nodes'], { nodes: [{ node_id: 'n1', name: '东京' } as NodeAgentStateItem] });
  const tree = (next: Record<string, unknown>) => (
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: 1, chain_group_count: [] } }}>
        <NodesPane win={winWith(next)} />
      </SessionProvider>
    </QueryClientProvider>
  );
  const view = render(tree(drill));
  return { rerender: (next: Record<string, unknown>) => view.rerender(tree(next)) };
}

/** What the router writes into history.state; previousRouteHash() reads `fromHash` from it. */
const enterFrom = (fromHash: string, hash: string) =>
  window.history.replaceState({ brocadeRoute: { index: 2, scrollTop: 0, fromHash } }, '', hash);

describe('machine detail observation range in the address', () => {
  it('shows the fixed range from the address and writes a changed range as one history entry', async () => {
    const navigateInPlace = vi.spyOn(route, 'navigateInPlace').mockReturnValue(true);
    mount({ p: 'node', id: 'n1', from: 1_700_000_000, to: 1_700_000_600 });

    const fixed = customLoadRange(1_700_000_000, 1_700_000_600);
    fireEvent.click(screen.getByRole('button', { name: `观测时间范围：${fixed.menuLabel}` }));
    fireEvent.click(screen.getByRole('option', { name: '近 6 小时' }));

    // The address changes only after the three reads are cached, so the panels switch together.
    await waitFor(() => expect(navigateInPlace).toHaveBeenCalledWith('nodes', { p: 'node', id: 'n1', range: '6h' }));
    expect(navigateInPlace).toHaveBeenCalledTimes(1);
  });

  it('offers a return button when the previous history entry is another range of this machine', () => {
    const returnTo = vi.spyOn(route, 'returnTo').mockReturnValue(true);
    enterFrom('#/nodes/node/n1', '#/nodes/node/n1?range=6h');
    mount({ p: 'node', id: 'n1', range: '6h' });

    fireEvent.click(screen.getByRole('button', { name: '返回上一个时间范围：近 1 小时' }));
    expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'node', id: 'n1' });
  });

  it('does not offer the return button for history that came from another page or machine', () => {
    enterFrom('#/nodes', '#/nodes/node/n1?range=6h');
    mount({ p: 'node', id: 'n1', range: '6h' });
    expect(screen.queryByRole('button', { name: /返回上一个时间范围/ })).toBeNull();
    cleanup();

    enterFrom('#/nodes/node/n2?range=24h', '#/nodes/node/n1?range=6h');
    mount({ p: 'node', id: 'n1', range: '6h' });
    expect(screen.queryByRole('button', { name: /返回上一个时间范围/ })).toBeNull();
  });

  it('returns to the observation tab when history changes only the range', () => {
    const { rerender } = mount({ p: 'node', id: 'n1', tab: 'chains' });
    expect(screen.getByRole('tab', { name: /规则/ }).getAttribute('aria-selected')).toBe('true');

    rerender({ p: 'node', id: 'n1', tab: 'chains', range: '6h' });
    expect(screen.getByRole('tab', { name: /观测/ }).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByRole('button', { name: '观测时间范围：近 6 小时' })).toBeTruthy();
  });
});
