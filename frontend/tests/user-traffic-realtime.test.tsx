import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeTrafficHistory, SiteUsagePeriod, TrafficTimeRange, UserTrafficHistory } from '../src/api';
import { NodeRealtimeProvider } from '../src/node-realtime';
import {
  aggregateUserTraffic,
  fallbackSource,
  hourModel,
  nodeHistoryModel,
  mergeRetained,
  nextListOrder,
  NodeUserTrafficCard,
  UserRealtimeTrafficCard,
  type CardRow,
} from '../src/user-traffic-realtime';
import { Icon } from '../src/ui/icons';

// This suite verifies card aggregation and source labels; the real canvas is covered by
// browser checks. Letting the lazy chart import settle between tests makes jsdom timing-dependent.
vi.mock('../src/panes/telemetry', () => ({ ThroughputChart: () => null }));

class Events {
  static sources: Events[] = [];
  handlers = new Map<string, (event: { data: string }) => void>();
  closed = false;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) {
    Events.sources.push(this);
  }
  addEventListener(name: string, callback: (event: { data: string }) => void) {
    this.handlers.set(name, callback);
  }
  close() {
    this.closed = true;
  }
  send(name: string, value: unknown) {
    this.handlers.get(name)?.({ data: JSON.stringify(value) });
  }
}

// 1_760_000_025 落在 30 秒格的中间：格起点是 …010，窗口结束于 T-30 / T-60 / T-90 各落在前一格。
const T = 1_760_000_025;
const rate = (user = 'alice', ingress = 'entry', uplink = 1024, downlink = 2048, tenant = 'platform') => ({
  tenant_id: tenant,
  user_id: user,
  ingress_id: ingress,
  uplink_bytes_per_sec: uplink,
  downlink_bytes_per_sec: downlink,
});
const event = (node = 'n1', users = [rate()], all = true, report: Record<string, unknown> = {}) => ({
  node_id: node,
  received_at_unix_millis: Date.now(),
  sample: {
    sampled_at_unix_millis: Date.now(),
    user_traffic: { state: 'ready', all_users: all, elapsed_millis: 1000, users, ...report },
  },
});
const node = (id: string, users = [rate()]) => ({
  node_id: id,
  connected: true,
  active: true,
  samples: [event(id, users, false)],
});
const history = (
  nodes: Record<string, number[]>,
  series: Array<[string, string, Array<[number, number, number]>]> = [],
): UserTrafficHistory => ({
  range_start_unix_secs: T - 3_600,
  range_end_unix_secs: T,
  nodes: Object.entries(nodes).map(([node_id, window_ends_unix_secs]) => ({ node_id, window_ends_unix_secs })),
  series: series.map(([node_id, user, points]) => ({ node_id, tenant_id: 'platform', user_id: user, points })),
});

let served: UserTrafficHistory;
let servedNode: NodeTrafficHistory;
/** ['nodes'] 查询的回包；为 null 时该请求一直挂起（大多数用例不需要机器名称与地区）。 */
let servedNodes: { nodes: { node_id: string; name: string; public_ipv4_country: string | null }[] } | null;
const renderCard = (card: ReactNode) =>
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      {card}
    </QueryClientProvider>,
  );
const settle = () => act(() => vi.advanceTimersByTimeAsync(20));
const listItems = (list: HTMLElement) => within(list).queryAllByRole('listitem');
const names = (list: HTMLElement) => listItems(list).map(item => item.querySelector('.rut-name')?.textContent);
const item = (list: HTMLElement, name: string) => {
  const found = listItems(list).find(row => row.querySelector('.rut-name')?.textContent === name);
  if (!found) throw new Error(`row ${name} is missing`);
  return found;
};
const currentDown = (row: HTMLElement) => row.querySelector('.rut-cur .d')?.textContent;

beforeEach(() => {
  Events.sources = [];
  vi.useFakeTimers({ now: T * 1000 });
  vi.stubGlobal('EventSource', Events);
  served = history({ n1: [] });
  servedNode = {
    range_start_unix_secs: T - 86_400,
    range_end_unix_secs: T,
    bucket_secs: 300,
    points: [[T - 300, 10, 20]],
    recorded_bytes: 900,
    has_gap: true,
    has_unplaced: false,
  };
  servedNodes = null;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path.includes('/traffic-history?')) return Response.json(servedNode);
      if (path.endsWith('/realtime/history')) return Response.json(served);
      if (path.endsWith('/nodes/agent-state') && servedNodes) return Response.json(servedNodes);
      return new Promise<Response>(() => {});
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('realtime user traffic data', () => {
  it('combines access and forwarding for one user while retaining source rates', () => {
    const rows = aggregateUserTraffic([
      rate(),
      {
        tenant_id: 'platform',
        user_id: 'alice',
        forwarding_rule_id: 7,
        uplink_bytes_per_sec: 100,
        downlink_bytes_per_sec: 200,
      },
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ up: 1124, down: 2248, forwardingUp: 100, forwardingDown: 200 });
  });

  it('uses actual forwarding windows and keeps unplaced legacy bytes out of the curve', () => {
    const end = Math.floor(T / 30) * 30;
    const model = hourModel({
      range_start_unix_secs: end - 60,
      range_end_unix_secs: end,
      nodes: [
        {
          node_id: 'n1',
          window_ends_unix_secs: [],
          windows: [],
          forwarding_windows: [
            [end - 60, end - 45],
            [end - 45, end - 30],
            [end - 30, end - 15],
            [end - 15, end],
          ],
        },
      ],
      series: [
        {
          node_id: 'n1',
          tenant_id: 'platform',
          user_id: 'alice',
          forwarding: true,
          points: [],
          windows: [
            [end - 60, end - 45, 15, 30],
            [end - 45, end - 30, 15, 30],
            [end - 30, end - 15, 15, 30],
            [end - 15, end, 15, 30],
          ],
          unplaced_uplink_bytes: 7,
          unplaced_downlink_bytes: 11,
        },
      ],
    });
    expect(model.rows.get('n1')).toMatchObject({
      up: [30, 30],
      down: [60, 60],
      hourUp: 67,
      hourDown: 131,
      unplaced: true,
    });
    expect(model.totalDown).toEqual([60, 60]);
  });

  it('does not turn missing forwarding collection into zero when access still reports', () => {
    const end = Math.floor(T / 30) * 30;
    const model = hourModel({
      range_start_unix_secs: end - 60,
      range_end_unix_secs: end,
      nodes: [
        {
          node_id: 'n1',
          window_ends_unix_secs: [],
          windows: [[end - 60, end]],
          forwarding_windows: [[end - 30, end]],
        },
      ],
      series: [
        { node_id: 'n1', tenant_id: 'platform', user_id: 'alice', points: [], windows: [[end - 60, end, 60, 120]] },
        {
          node_id: 'n1',
          tenant_id: 'platform',
          user_id: 'alice',
          forwarding: true,
          points: [],
          windows: [[end - 30, end, 30, 60]],
        },
      ],
    });
    expect(model.rows.get('n1')?.down).toEqual([null, 120]);
    expect(model.totalDown).toEqual([null, 120]);
  });
  it('groups one user across ingress labels without combining tenants', () => {
    const rows = aggregateUserTraffic([rate(), rate('alice', 'second'), rate('alice', 'entry', 1, 1, 'other')]);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ key: 'platform/alice', up: 2048, down: 4096 });
  });

  it('keeps idle windows at zero and uncollected windows empty', () => {
    const model = hourModel(
      history({ n1: [T - 90, T - 60, T - 30] }, [
        [
          'n1',
          'alice',
          [
            [T - 90, 30, 60],
            [T - 30, 3, 6],
          ],
        ],
      ]),
    );
    const row = model.rows.get('n1');
    const last = model.slots.length - 1;
    expect(row?.down.slice(last - 3)).toEqual([60, 0, 6, null]);
    expect(row?.down[0]).toBeNull();
    expect(row).toMatchObject({ hourDown: 66, hourUp: 33, lastActiveMs: (T - 30) * 1000 });
    expect(model.totalDown.slice(last - 3)).toEqual([60, 0, 6, null]);
  });

  it('lists every granted machine on the user page and sums machines in the total', () => {
    const model = hourModel(
      history({ n1: [T - 30], n2: [T - 30] }, [
        ['n1', 'alice', [[T - 30, 1, 2]]],
        ['n2', 'alice', [[T - 30, 3, 4]]],
      ]),
    );
    expect([...model.rows.keys()].sort()).toEqual(['n1', 'n2']);
    expect(model.totalDown[model.slots.length - 2]).toBe(6);
  });

  it('spreads a 45-second reading by its real duration instead of creating an empty bucket', () => {
    const end = Math.floor(T / 30) * 30;
    const input: UserTrafficHistory = {
      range_start_unix_secs: end - 90,
      range_end_unix_secs: end,
      nodes: [
        {
          node_id: 'n1',
          window_ends_unix_secs: [end - 45, end],
          windows: [
            [end - 90, end - 45],
            [end - 45, end],
          ],
          zero_windows: [],
        },
      ],
      series: [
        {
          node_id: 'n1',
          tenant_id: 'platform',
          user_id: 'alice',
          points: [[end, 90, 450]],
          windows: [[end - 45, end, 90, 450]],
        },
      ],
    };
    const model = hourModel(input);
    expect(model.totalDown).toEqual([0, 150, 300]);
    expect(model.rows.get('n1')).toMatchObject({ hourDown: 450, hourUp: 90 });
    expect(model.peak).toBe(12);
  });

  it('draws confirmed restart downtime as zero but leaves unknown intervals missing', () => {
    const end = Math.floor(T / 30) * 30;
    const input: UserTrafficHistory = {
      range_start_unix_secs: end - 120,
      range_end_unix_secs: end,
      nodes: [
        {
          node_id: 'n1',
          window_ends_unix_secs: [end - 60, end],
          windows: [[end - 90, end]],
          zero_windows: [[(end - 60) * 1000, (end - 30) * 1000]],
        },
      ],
      series: [
        {
          node_id: 'n1',
          tenant_id: 'platform',
          user_id: 'alice',
          points: [[end, 60, 600]],
          windows: [[end - 90, end, 60, 600]],
        },
      ],
    };
    const model = hourModel(input);
    expect(model.totalDown).toEqual([null, 300, 0, 300]);
    expect(model.totalUp).toEqual([null, 30, 0, 30]);
    expect(model.rows.get('n1')).toMatchObject({ hourDown: 600, hourUp: 60 });
  });

  it('clips boundary windows while retaining the right rate for a partial 30-second slot', () => {
    const end = Math.floor(T / 30) * 30 + 15;
    const input: UserTrafficHistory = {
      range_start_unix_secs: end - 30,
      range_end_unix_secs: end,
      nodes: [{ node_id: 'n1', window_ends_unix_secs: [end], windows: [[end - 60, end]], zero_windows: [] }],
      series: [
        {
          node_id: 'n1',
          tenant_id: 'platform',
          user_id: 'alice',
          points: [[end, 0, 600]],
          windows: [[end - 60, end, 0, 600]],
        },
      ],
    };
    const model = hourModel(input);
    expect(model.totalDown).toEqual([300, 300]);
    expect(model.rows.get('n1')?.hourDown).toBe(300);
    expect(model.peak).toBe(10);
  });

  it('retains rows between reports and zeroes them only when a report is complete', () => {
    const first = mergeRetained(new Map(), [{ key: 'a', name: 'a', detail: 't', down: 10, up: 1 }], true, 1_000);
    const idle = mergeRetained(first, [], true, 2_000);
    expect(idle.get('a')).toMatchObject({ down: 0, up: 0, lastActiveMs: 1_000 });
    const truncated = mergeRetained(first, [], false, 2_000);
    expect(truncated.get('a')).toMatchObject({ down: 10, up: 1 });
    const unknown = mergeRetained(first, [{ key: 'a', name: 'a', detail: 't', down: null, up: null }], false, 3_000);
    expect(unknown.get('a')).toMatchObject({ down: 10, up: 1, lastActiveMs: 1_000 });
  });

  it('keeps numeric positions fixed until the user confirms a reorder', () => {
    const row = (key: string, down: number, hourBytes = 0): CardRow => ({
      key,
      name: key,
      detail: '',
      down,
      up: 0,
      offline: false,
      hour: null,
      hourBytes,
      lastActiveMs: null,
    });
    const start = { keys: [], at: Number.NEGATIVE_INFINITY, sort: 'name' as const };
    const named = nextListOrder(start, [row('b', 10), row('a', 5)], 'name', false, 0);
    expect(named.keys).toEqual(['a', 'b']);
    expect(nextListOrder(named, [row('b', 99), row('a', 5)], 'name', false, 60_000)).toBe(named);
    expect(nextListOrder(named, [row('b', 1), row('c', 1), row('a', 1)], 'name', false, 1_000).keys).toEqual([
      'a',
      'b',
      'c',
    ]);
    const byDown = nextListOrder(named, [row('b', 10), row('a', 5)], 'down', false, 1_000);
    expect(byDown.keys).toEqual(['b', 'a']);
    const swapped = [row('b', 1), row('a', 50)];
    expect(nextListOrder(byDown, swapped, 'down', false, 3_000)).toBe(byDown);
    const confirmed = nextListOrder(byDown, swapped, 'down', true, 9_000);
    expect(confirmed.keys).toEqual(['a', 'b']);
    expect(nextListOrder(confirmed, swapped, 'down', false, 60_000)).toBe(confirmed);

    const unknown = [row('b', 0), row('a', 0)].map(value => ({ ...value, down: null, up: null }));
    const waiting = nextListOrder(start, unknown, 'down', false, 0);
    expect(waiting.keys).toEqual(['a', 'b']);
    // Loading history before the first live frame must not immediately raise a stale-rank prompt.
    expect(nextListOrder(waiting, [row('b', 50), row('a', 1)], 'down', false, 1_000).keys).toEqual(['b', 'a']);
  });
});

describe('machine user traffic card', () => {
  it('does not present old access-only Agent reports as complete forwarding traffic', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" />
      </NodeRealtimeProvider>,
    );
    await settle();
    act(() =>
      Events.sources[0].send(
        'sample',
        event('n1', [rate()], true, { forwarding_expected: true, forwarding_supported: false }),
      ),
    );
    expect(screen.getByText('转发实时待升级 Agent')).toBeTruthy();
    expect(screen.getByRole('region', { name: '当前速率' }).textContent).toContain('—');
  });
  it('shows selected history as bars without the matrix and retains readings when the channel drops', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" />
      </NodeRealtimeProvider>,
    );
    await settle();
    const overview = screen.getByRole('region', { name: '实时流量' });
    expect(within(overview).getByRole('region', { name: '所选区间历史流量' }).textContent).toContain('900B');
    expect(within(overview).getByText('当天 · 每 5 分钟 · 已观测窗口均值')).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByRole('group', { name: '排序' })).toBeNull();
    expect(screen.queryByRole('searchbox')).toBeNull();
    const now = screen.getByRole('region', { name: '当前速率' });
    act(() => Events.sources[0].send('sample', event('n1', [rate('alice')])));
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toContain('24.6');
    expect(now.querySelector('.rut-hero-sub')?.textContent).toContain('共 1 位');
    act(() => vi.advanceTimersByTime(1_000));
    act(() => Events.sources[0].send('status', { node_id: 'n1', connected: false }));
    expect(screen.getByText('实时通道未连接')).toBeTruthy();
    expect(screen.getByText(/当前速率停在/)).toBeTruthy();
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toContain('24.6');
  });

  it('merges the current ledger by chain without restoring an ingress grid', async () => {
    const sourceOf = (key: string) => ({ key: key.endsWith('extra') ? 'ingress:c1' : key, name: key, country: null });
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" sourceOf={sourceOf} />
      </NodeRealtimeProvider>,
    );
    await settle();
    const rates = Array.from({ length: 26 }, (_, i) => rate(`u${String(i).padStart(2, '0')}`, `c${i % 7}`, i + 1, 100));
    act(() => Events.sources[0].send('sample', event('n1', [...rates, rate('u01', 'extra', 5, 10)])));
    const chains = screen.getByRole('region', { name: '当前经过的接入链' });
    expect(within(chains).getAllByRole('listitem')).toHaveLength(7);
    expect([...chains.querySelectorAll('.rut-source-name')].map(entry => entry.textContent)).toEqual([
      'ingress:c0',
      'ingress:c1',
      'ingress:c2',
      'ingress:c3',
      'ingress:c4',
      'ingress:c5',
      'ingress:c6',
    ]);
    expect(chains.textContent).not.toContain('其余');
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByRole('button', { name: '当前' })).toBeNull();
  });

  it('keeps chain names in alphabetical order and retains zero-rate entries between complete reports', async () => {
    const labels: Record<string, string> = { 'ingress:a': 'Alpha', 'ingress:b': 'beta', 'ingress:z': 'Zulu' };
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" sourceOf={key => ({ key, name: labels[key] ?? key, country: null })} />
      </NodeRealtimeProvider>,
    );
    await settle();
    const chains = screen.getByRole('region', { name: '当前经过的接入链' });
    const chainNames = () => [...chains.querySelectorAll('.rut-source-name')].map(entry => entry.textContent);
    const report = async (rates: ReturnType<typeof rate>[]) => {
      await act(() => vi.advanceTimersByTimeAsync(1_000));
      act(() => Events.sources[0].send('sample', event('n1', rates)));
      await act(() => vi.advanceTimersByTimeAsync(1_000));
    };

    await report([rate('alice', 'z', 0, 900), rate('alice', 'b', 0, 100), rate('alice', 'a', 0, 10)]);
    expect(chainNames()).toEqual(['Alpha', 'beta', 'Zulu']);
    const originalRows = within(chains).getAllByRole('listitem');

    // The same user's next report drops two sources; keep their identities at zero.
    await report([rate('alice', 'a', 0, 2000)]);
    expect(chainNames()).toEqual(['Alpha', 'beta', 'Zulu']);
    expect(within(chains).getAllByRole('listitem')).toEqual(originalRows);
    expect(originalRows[1].querySelector('b')?.textContent).toBe('0');
    expect(originalRows[2].querySelector('b')?.textContent).toBe('0');

    // A complete empty report confirms all users are idle, rather than deleting their chains.
    await report([]);
    expect(chainNames()).toEqual(['Alpha', 'beta', 'Zulu']);
    expect(
      within(chains)
        .getAllByRole('listitem')
        .map(entry => entry.querySelector('b')?.textContent),
    ).toEqual(['0', '0', '0']);

    await report([rate('bob', 'z', 0, 3000)]);
    expect(chainNames()).toEqual(['Alpha', 'beta', 'Zulu']);
    expect(originalRows[0].querySelector('b')?.textContent).toBe('0');
    expect(originalRows[2].querySelector('b')?.textContent).not.toBe('0');
  });

  it('keeps chain identities visible without showing unknown reports as measured zeroes', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" />
      </NodeRealtimeProvider>,
    );
    await settle();
    act(() => Events.sources[0].send('sample', event('n1', [rate('alice', 'first')])));
    const chains = screen.getByRole('region', { name: '当前经过的接入链' });
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    act(() => Events.sources[0].send('sample', event('n1', [rate('bob', 'second')], true, { truncated: true })));
    await act(() => vi.advanceTimersByTimeAsync(1_000));

    const first = within(chains).getByText('接入点 first').closest('li')!;
    expect(first.querySelector('b')?.textContent).toBe('—');
    expect(within(chains).getByText('接入点 second')).toBeTruthy();
    expect(screen.getByText('部分速率合计')).toBeTruthy();
  });

  it('marks forwarding sources with the shared icon alongside chain region flags', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard
          nodeId="n1"
          sourceOf={key => (key === 'forwarding' ? fallbackSource(key) : { key, name: '日本接入链', country: 'JP' })}
        />
      </NodeRealtimeProvider>,
    );
    await settle();
    const forwarded = { ...rate('alice', 'entry', 100, 200), forwarding_rule_id: 42 };
    act(() => Events.sources[0].send('sample', event('n1', [rate(), forwarded])));
    const sources = screen.getByRole('region', { name: '当前经过的接入链' });
    const forwarding = within(sources).getByText('端口转发').closest('.rut-source')!;
    const expected = render(<Icon of="forwarding" size={13} />).container.querySelector('svg')!.innerHTML;
    expect(forwarding.querySelector('.traffic-source-icon')?.getAttribute('aria-hidden')).toBe('true');
    expect(forwarding.querySelector('svg')?.innerHTML).toBe(expected);
    expect(forwarding.querySelector('.geo-flag')).toBeNull();
    expect(within(sources).getByRole('img', { name: 'JP 地区旗' })).toBeTruthy();
  });

  it('does not present a reset counter baseline as zero traffic', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" />
      </NodeRealtimeProvider>,
    );
    await settle();
    act(() => Events.sources[0].send('sample', event('n1', [], true, { has_gap: true })));
    expect(screen.getByText('正在建立速率基线')).toBeTruthy();
    const now = screen.getByRole('region', { name: '当前速率' });
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toBe('—');
    expect([...now.querySelectorAll('.usage-io dd:not(.usage-share)')].map(cell => cell.textContent)).toEqual([
      '—',
      '—',
    ]);
  });

  it('retains recent machine rates only for warming, not a counter gap or disabled observation', async () => {
    renderCard(
      <NodeRealtimeProvider nodeId="n1">
        <NodeUserTrafficCard nodeId="n1" />
      </NodeRealtimeProvider>,
    );
    await settle();
    const source = Events.sources[0];
    act(() => source.send('sample', event()));
    const now = screen.getByRole('region', { name: '当前速率' });
    const reading = now.querySelector('.user-usage-hero-value')?.textContent;
    act(() => {
      vi.advanceTimersByTime(1_000);
      source.send('sample', event('n1', [], true, { state: 'warming' }));
    });
    expect(screen.getByText('采样恢复中 · 保留最近读数')).toBeTruthy();
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toBe(reading);

    act(() => source.send('sample', event('n1', [], true, { has_gap: true })));
    expect(screen.getByText('正在建立速率基线')).toBeTruthy();
    expect(screen.getByRole('region', { name: '当前速率' }).querySelector('.user-usage-hero-value')?.textContent).toBe(
      '—',
    );

    act(() =>
      source.send('snapshot', {
        policy: { enabled: false },
        nodes: [{ ...node('n1'), samples: [event('n1', [], true, { state: 'warming' })] }],
      }),
    );
    expect(screen.getByText('实时观测已关闭')).toBeTruthy();
    expect(screen.getByRole('region', { name: '当前速率' }).querySelector('.user-usage-hero-value')?.textContent).toBe(
      '—',
    );
  });
});

describe('user realtime traffic card', () => {
  it('keeps recent rates through authorization renewal without extending their freshness on warming frames', async () => {
    renderCard(<UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => id} />);
    const source = Events.sources[0];
    act(() => source.send('snapshot', { policy: { enabled: true }, nodes: [node('n1')] }));
    await settle();
    const now = screen.getByRole('region', { name: '当前速率' });
    const reading = now.querySelector('.user-usage-hero-value')?.textContent;
    const list = screen.getByRole('list', { name: '各入口机器流量' });

    act(() => source.send('reset', 'authorization_refresh'));
    expect(source.closed).toBe(true);
    const renewed = Events.sources[1];
    act(() => {
      vi.advanceTimersByTime(1_000);
      renewed.send('snapshot', {
        policy: { enabled: true },
        nodes: [{ ...node('n1'), samples: [event('n1', [], false, { state: 'warming' })] }],
      });
    });
    expect(screen.queryByText('正在建立速率基线')).toBeNull();
    expect(screen.getByText('采样恢复中 · 保留最近读数')).toBeTruthy();
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toBe(reading);
    expect(currentDown(item(list, 'n1'))).toBe('16.4Kbit/s');

    act(() => {
      vi.advanceTimersByTime(14_000);
      renewed.send('sample', event('n1', [], false, { state: 'warming' }));
      vi.advanceTimersByTime(1_000);
    });
    expect(screen.getByText('正在建立速率基线')).toBeTruthy();
    expect(now.querySelector('.user-usage-hero-value')?.textContent).toBe('—');

    act(() => renewed.send('sample', event('n1', [rate('alice', 'entry', 10, 100)])));
    expect(screen.getByText('实时 · 每秒更新')).toBeTruthy();
    expect(currentDown(item(list, 'n1'))).toBe('800bit/s');
  });

  it("does not reuse another user's recent reading while their baseline warms", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const card = (userId: string) => (
      <QueryClientProvider client={client}>
        <UserRealtimeTrafficCard tenantId="platform" userId={userId} nameOf={id => id} />
      </QueryClientProvider>
    );
    const view = render(card('alice'));
    act(() => Events.sources[0].send('snapshot', { policy: { enabled: true }, nodes: [node('n1')] }));
    await settle();
    view.rerender(card('bob'));
    act(() =>
      Events.sources[1].send('snapshot', {
        policy: { enabled: true },
        nodes: [{ ...node('n1'), samples: [event('n1', [], false, { state: 'warming' })] }],
      }),
    );
    expect(screen.getByText('正在建立速率基线')).toBeTruthy();
    expect(screen.getByRole('region', { name: '当前速率' }).querySelector('.user-usage-hero-value')?.textContent).toBe(
      '—',
    );
  });

  it('labels forwarding only on the machine that actually carries it, including idle rules', async () => {
    served = history({ n1: [], n2: [] });
    renderCard(<UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => id} />);
    act(() =>
      Events.sources[0].send('snapshot', {
        policy: { enabled: true },
        nodes: [
          {
            ...node('n1'),
            samples: [event('n1', [rate()], false, { forwarding_expected: false, forwarding_supported: true })],
          },
          {
            ...node('n2'),
            samples: [event('n2', [rate()], false, { forwarding_expected: true, forwarding_supported: true })],
          },
        ],
      }),
    );
    await settle();
    const list = screen.getByRole('list', { name: '各入口机器流量' });
    expect(item(list, 'n1').textContent).not.toContain('含转发');
    expect(item(list, 'n2').textContent).toContain('含转发');
  });

  it('marks entry machines with their region flag and keeps the hour chart and four sorts', async () => {
    servedNodes = {
      nodes: [
        { node_id: 'n1', name: '东京 A', public_ipv4_country: 'JP' },
        { node_id: 'n2', name: 'n2', public_ipv4_country: null },
      ],
    };
    served = history({ n1: [T - 30], n2: [T - 30] }, [['n1', 'alice', [[T - 30, 10, 20]]]]);
    renderCard(<UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => id} />);
    act(() => Events.sources[0].send('snapshot', { policy: { enabled: true }, nodes: [node('n1'), node('n2')] }));
    await settle();
    const list = screen.getByRole('list', { name: '各入口机器流量' });
    expect(within(item(list, 'n1')).getByRole('img', { name: 'JP 地区旗' })).toBeTruthy();
    // 没有国别的机器与机器详情页头一样不画底板，只占位
    expect(item(list, 'n2').querySelector('.nd-idplate')).toBeNull();
    expect(item(list, 'n2').querySelector('.rut-mark-slot')).toBeTruthy();
    expect(item(list, 'n1').querySelector('.rut-meta')?.textContent).toBe('1 个接入点 · 30 B');
    // 用户详情上方没有重复的曲线，合计图与四档排序保留
    expect(screen.getByRole('region', { name: '近 1 小时' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '下行' })).toBeTruthy();
    expect(screen.queryByRole('searchbox', { name: '搜索机器' })).toBeNull();
  });

  it('sums only the selected user across machines and keeps the last rates when they expire', async () => {
    served = history({ n1: [T - 30], n2: [T - 30] }, [['n1', 'alice', [[T - 30, 10, 20]]]]);
    renderCard(<UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => `机器 ${id}`} />);
    expect(Events.sources[0].url).toBe('/users/platform/alice/realtime/events');
    act(() =>
      Events.sources[0].send('snapshot', {
        policy: { enabled: true },
        nodes: [node('n1', [rate(), rate('bob')]), node('n2')],
      }),
    );
    await settle();
    const list = screen.getByRole('list', { name: '各入口机器流量' });
    expect(names(list)).toEqual(['机器 n1', '机器 n2']);
    expect(screen.queryByText('bob')).toBeNull();
    expect(screen.getByRole('region', { name: '当前速率' }).textContent).toContain('32.8');

    act(() => vi.advanceTimersByTime(16_000));
    expect(screen.getByText('实时通道未连接')).toBeTruthy();
    expect(names(list)).toEqual(['机器 n1', '机器 n2']);
    expect(currentDown(item(list, '机器 n1'))).toBe('16.4Kbit/s');
  });

  it('updates a delayed machine independently and never counts retained offline or missing readings as current', async () => {
    served = history({ n1: [], n2: [] });
    renderCard(<UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => id} />);
    const received = Date.now();
    const delayed = (down: number, ago: number) => ({
      ...event('n2', [rate('alice', 'entry', 0, down)], false),
      received_at_unix_millis: received - ago,
      // Machine clocks are not a common time axis. Only Console receipt orders observations.
      sample: {
        ...event('n2', [rate('alice', 'entry', 0, down)], false).sample,
        sampled_at_unix_millis: received - 3_600_000,
      },
    });
    act(() =>
      Events.sources[0].send('snapshot', {
        policy: { enabled: true },
        nodes: [node('n1', [rate('alice', 'entry', 0, 1024)]), { ...node('n2'), samples: [delayed(1024, 4000)] }],
      }),
    );
    await settle();
    const list = screen.getByRole('list', { name: '各入口机器流量' });
    const panel = screen.getByRole('region', { name: '当前速率' });
    expect(currentDown(item(list, 'n2'))).toBe('8.19Kbit/s');
    expect(item(list, 'n2').textContent).toContain('4 秒前收到');
    act(() => Events.sources[0].send('sample', delayed(2048, 2000)));
    act(() => vi.advanceTimersByTime(1000));
    // n1 remains the largest receipt timestamp, but n2's newer rate must still appear.
    expect(currentDown(item(list, 'n2'))).toBe('16.4Kbit/s');
    expect(panel.querySelector('.user-usage-hero-value')?.textContent).toContain('24.6');
    expect(item(list, 'n2').textContent).toContain('3 秒前收到');
    // A late replay cannot replace n2 with an older high reading.
    act(() => Events.sources[0].send('sample', delayed(10240, 3000)));
    act(() => vi.advanceTimersByTime(1000));
    expect(currentDown(item(list, 'n2'))).toBe('16.4Kbit/s');
    act(() => Events.sources[0].send('status', { node_id: 'n2', connected: false, active: false }));
    expect(currentDown(item(list, 'n2'))).toBe('16.4Kbit/s');
    expect(panel.querySelector('.user-usage-hero-value')?.textContent).toContain('8.19');
    expect(panel.textContent).toContain('部分速率合计');
    expect(panel.textContent).toContain('1 / 2 台有读数');
    act(() => Events.sources[0].send('sample', event('n2', [], false, { has_gap: true })));
    expect(panel.querySelector('.user-usage-hero-value')?.textContent).toContain('8.19');
    expect(item(list, 'n2').textContent).toContain('上次读数');
  });

  it('distinguishes an old Agent from idle and unsubscribes on navigation', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <UserRealtimeTrafficCard tenantId="platform" userId="alice" nameOf={id => id} />
      </QueryClientProvider>,
    );
    act(() =>
      Events.sources[0].send('snapshot', {
        policy: { enabled: true },
        nodes: [{ ...node('n1'), samples: [{ ...event(), sample: { sampled_at_unix_millis: Date.now() } }] }],
      }),
    );
    await settle();
    expect(screen.getByText('当前 Agent 尚未支持用户速率')).toBeTruthy();
    view.rerender(
      <QueryClientProvider client={client}>
        <UserRealtimeTrafficCard tenantId="platform" userId="bob" nameOf={id => id} />
      </QueryClientProvider>,
    );
    expect(Events.sources[0].closed).toBe(true);
    expect(screen.getByText('正在建立速率基线')).toBeTruthy();
    view.unmount();
    expect(Events.sources[1].closed).toBe(true);
  });
});

describe('machine history period', () => {
  it('keeps gaps, rates and recorded totals independent of bucket width', () => {
    const model = nodeHistoryModel({
      ...servedNode,
      bucket_secs: 3600,
      points: [
        [T - 3600, null, null],
        [T, 10, 20],
      ],
    });
    expect(model.totalUp).toEqual([null, 300]);
    expect(model.totalDown).toEqual([null, 600]);
    expect(model.recordedBytes).toBe(900);
    expect(model.slots).toEqual([T - 3600, T]);
  });

  it('reloads every selection without reopening the live stream or displaying the previous range', async () => {
    const requests: URL[] = [];
    let finish: ((response: Response) => void) | undefined;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (!url.pathname.endsWith('/traffic-history')) return new Promise<Response>(() => {});
        requests.push(url);
        if (url.searchParams.get('period') === 'week')
          return new Promise<Response>(resolve => {
            finish = resolve;
          });
        return Response.json({ ...servedNode, bucket_secs: url.searchParams.get('period') === 'month' ? 3600 : 300 });
      }),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const card = (period: SiteUsagePeriod, range?: TrafficTimeRange) => (
      <QueryClientProvider client={client}>
        <NodeRealtimeProvider nodeId="n1">
          <NodeUserTrafficCard nodeId="n1" period={period} range={range} />
        </NodeRealtimeProvider>
      </QueryClientProvider>
    );
    const view = render(card('day'));
    await settle();
    const live = screen.getByRole('region', { name: '当前速率' });
    act(() => Events.sources[0].send('sample', event()));
    const reading = live.textContent;
    view.rerender(card('week'));
    await settle();
    expect(screen.queryByText('当天 · 每 5 分钟 · 已观测窗口均值')).toBeNull();
    expect(screen.getByRole('region', { name: '所选区间历史流量' }).textContent).not.toContain('900B');
    await act(async () => {
      finish!(Response.json({ ...servedNode, bucket_secs: 900, recorded_bytes: 1800 }));
    });
    await settle();
    expect(screen.getByText('当周 · 每 15 分钟 · 已观测窗口均值')).toBeTruthy();
    view.rerender(card('month'));
    await settle();
    expect(screen.getByText('当月 · 每 1 小时 · 已观测窗口均值')).toBeTruthy();
    const range = { start_unix_secs: T - 600, end_unix_secs: T - 30 };
    view.rerender(card('day', range));
    await settle();
    const last = requests.at(-1)!;
    expect(last.searchParams.get('start_unix_secs')).toBe(String(range.start_unix_secs));
    expect(last.searchParams.get('end_unix_secs')).toBe(String(range.end_unix_secs));
    expect(requests.map(url => url.searchParams.get('period'))).toEqual(['day', 'week', 'month', 'day']);
    expect(screen.getByText('自选区间 · 每 5 分钟 · 已观测窗口均值')).toBeTruthy();
    expect(screen.getByRole('region', { name: '当前速率' })).toBe(live);
    expect(live.textContent).toBe(reading);
    expect(Events.sources).toHaveLength(1);
    expect(Events.sources[0].closed).toBe(false);
  });
});
