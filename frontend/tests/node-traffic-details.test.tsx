import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  fetchNodeInflowUsage,
  fetchNodeInflowDetails,
  fetchNodeSiteUsage,
  fetchNodeOtherDomains,
  fetchNodeSniffingFailedTargets,
  fetchUserSiteUsage,
  fetchSnapshot,
  type ConsoleSnapshot,
  type NodeInflowRow,
  type NodeInflowUsage,
  type NodeSiteUsage,
  type TrafficTimeRange,
} from '../src/api';
import { NodeInflowUsageCard, NodeTrafficDialog, trafficSources, trafficUserKeys } from '../src/node-traffic';
import { NodeSiteUsageCard } from '../src/user-site-usage';
import { Icon } from '../src/ui/icons';
import { trafficDateInput } from '../src/node-traffic-range';
import type { SiteUsageChartSeries } from '../src/site-usage-chart';

vi.mock('../src/api', async original => ({
  ...(await original<typeof import('../src/api')>()),
  fetchNodeInflowUsage: vi.fn(),
  fetchNodeInflowDetails: vi.fn(),
  fetchNodeSiteUsage: vi.fn(),
  fetchNodeOtherDomains: vi.fn(),
  fetchNodeSniffingFailedTargets: vi.fn(),
  fetchUserSiteUsage: vi.fn(),
  fetchSnapshot: vi.fn(),
}));
vi.mock('../src/user-traffic-realtime', async original => ({
  ...(await original<typeof import('../src/user-traffic-realtime')>()),
  NodeUserTrafficCard: ({ period, range }: { period: string; range?: TrafficTimeRange }) => (
    <section aria-label="实时流量" data-testid="live-card" data-period={period} data-range={JSON.stringify(range)} />
  ),
}));
vi.mock('../src/site-usage-chart', () => ({
  default: ({
    period,
    series,
    focusKey,
    observedSinceUnixSecs,
  }: {
    period: string;
    series: SiteUsageChartSeries[];
    focusKey: string | null;
    observedSinceUnixSecs: number | null;
  }) => (
    <div
      data-testid="site-chart"
      data-period={period}
      data-series={JSON.stringify(series)}
      data-focus={focusKey ?? ''}
      data-observed-since={observedSinceUnixSecs ?? ''}
    />
  ),
}));
let width = 1000;
let resize: () => void;
const start = 1792252800;
const row = (user: string, chain: string, total: number, extra: Partial<NodeInflowRow> = {}): NodeInflowRow => ({
  user_id: user,
  tenant_id: 'platform',
  source: 'chain',
  app_id: 'app',
  chain_id: chain,
  ingress_id: null,
  downlink_bytes: total,
  uplink_bytes: 0,
  has_gap: false,
  ...extra,
});
const inflow = (extra: Partial<NodeInflowUsage> = {}): NodeInflowUsage => ({
  period: 'day',
  range_start_unix_secs: start,
  range_end_unix_secs: start + 3600,
  period_end_unix_secs: start + 86400,
  latest_report_unix_secs: start + 3500,
  rows: [row('alice', 'a', 300), row('alice', 'b', 200), row('bob', 'b', 100)],
  idle_users: [],
  ...extra,
});
const outflow = (): NodeSiteUsage => ({
  collection_enabled: true,
  catalog_version: 1,
  period: 'day',
  range_start_unix_secs: start,
  range_end_unix_secs: start + 3600,
  period_end_unix_secs: start + 86400,
  bucket_secs: 3600,
  reporting_nodes: 1,
  expected_nodes: 1,
  limited: false,
  latest_report_unix_secs: start + 3500,
  observed_since_unix_secs: start,
  rows: [{ site_id: 'github', name: 'GitHub', uplink_bytes: 10, downlink_bytes: 90 }],
  series: [{ site_id: 'github', points: [[start, 10, 90]] }],
  top_users: [
    {
      site_id: 'github',
      users: [{ tenant_id: 'platform', user_id: 'alice', uplink_bytes: 10, downlink_bytes: 60 }],
      other_users: 2,
      other_uplink_bytes: 0,
      other_downlink_bytes: 30,
    },
  ],
});
const snapshot: ConsoleSnapshot = {
  node_egress_dns: [],
  redacted: false,
  snapshot: {
    revision: 1,
    apps: [
      {
        id: 'app',
        label: 'app',
        chains: [
          { id: 'a', name: '香港', tenant: 'platform', subscription_country: 'HK' },
          { id: 'b', name: '日本', tenant: 'platform', subscription_country: 'JP' },
        ],
        steps: [],
        ingresses: [],
        fronts: [],
        grants: [],
      },
    ],
  },
};
const mount = (element: React.ReactNode) => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return { ...render(<QueryClientProvider client={client}>{element}</QueryClientProvider>), client };
};
beforeEach(() => {
  width = 1000;
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({
    width,
    height: 300,
    left: 0,
    right: width,
    top: 0,
    bottom: 300,
    x: 0,
    y: 0,
    toJSON() {
      return {};
    },
  }));
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(callback: () => void) {
        resize = callback;
      }
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.mocked(fetchSnapshot).mockResolvedValue(snapshot);
  vi.mocked(fetchNodeInflowUsage).mockImplementation(async (_id, period) => ({ ...inflow(), period }));
  vi.mocked(fetchNodeInflowDetails).mockImplementation(async (node, tenant, user, period) => {
    const data = await fetchNodeInflowUsage(node, period);
    const rows = data.rows.filter(row => row.tenant_id === tenant && row.user_id === user);
    return {
      range_start_unix_secs: data.range_start_unix_secs,
      range_end_unix_secs: data.range_end_unix_secs,
      observed_since_unix_secs: start,
      latest_report_unix_secs: start + 3500,
      limited: false,
      accounting_mismatch: false,
      accounting_uplink_bytes: rows.reduce((sum, row) => sum + row.uplink_bytes, 0),
      accounting_downlink_bytes: rows.reduce((sum, row) => sum + row.downlink_bytes, 0),
      rows: rows.map(row => ({ ...row, source_ip: row.source === 'chain' ? '192.0.2.1' : null })),
    };
  });
  vi.mocked(fetchNodeSiteUsage).mockImplementation(async (_id, period) => ({ ...outflow(), period }));
  const detail = {
    collection_enabled: false,
    top_n: 128,
    range_start_unix_secs: start,
    range_end_unix_secs: start + 3600,
    reporting_nodes: 0,
    expected_nodes: 1,
    limited: false,
    observed_since_unix_secs: null,
    latest_report_unix_secs: null,
    rows: [],
  };
  vi.mocked(fetchNodeOtherDomains).mockResolvedValue(detail);
  vi.mocked(fetchNodeSniffingFailedTargets).mockResolvedValue(detail);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it('renders proportional flows, highlights by keyboard and exposes exact per-chain values', async () => {
  const { container } = mount(<NodeInflowUsageCard nodeId="n1" period="day" sourceOf={trafficSources(snapshot)} />);
  await screen.findByRole('img', { name: /用户到接入链的流向/ });
  const links = container.querySelectorAll('path.ntf-link');
  expect(links).toHaveLength(3);
  const userHeights = [...container.querySelectorAll('.ntf-node')]
    .slice(0, 2)
    .map(node => Number(node.getAttribute('height')));
  expect(userHeights[0] / userHeights[1]).toBeCloseTo(5);

  expect([...links].map(link => link.querySelector('title')?.textContent)).toContain(
    'alice → 香港 · 300 B · 下行 300 B · 上行 0 B',
  );
  const alice = screen.getByRole('button', { name: 'alice · 500 B' });
  expect(alice.querySelector('.user-generated-avatar')).toBeNull();
  expect(alice.getAttribute('title')).toBe('alice · 下行 500 B · 上行 0 B');
  fireEvent.focus(alice);
  expect(container.querySelectorAll('.ntf-link.dim')).toHaveLength(1);
  fireEvent.click(alice);
  const detail = await screen.findByRole('dialog', { name: 'alice · 流入明细' });
  // 两条链上的同一个来源只排一行，条形按流入统计里的链色分段。
  const sources = await within(detail).findByRole('list', { name: '来源' });
  expect(sources.querySelectorAll(':scope > li')).toHaveLength(1);
  expect(
    [...sources.querySelectorAll<HTMLElement>('.nid-meter > i')].map(segment => segment.style.background).sort(),
  ).toEqual(['var(--observe-1)', 'var(--observe-6)']);
  expect(within(detail).getByRole('group', { name: /接入链/ }).textContent).toContain('香港');
  expect(vi.mocked(fetchNodeInflowDetails)).toHaveBeenCalledWith('n1', 'platform', 'alice', 'day', undefined);
  expect(container.textContent).not.toContain('platform');
  fireEvent.blur(alice);
  expect(container.querySelectorAll('.ntf-link.dim')).toHaveLength(0);
});

it('switches to the mobile bars below 720px and supports focus and expansion', async () => {
  width = 719;
  mount(<NodeInflowUsageCard nodeId="n1" period="day" sourceOf={trafficSources(snapshot)} />);
  const legend = await screen.findByRole('group', { name: '接入链（点击聚焦）' });
  expect(screen.queryByRole('img', { name: /用户到接入链/ })).toBeNull();
  const japan = within(legend).getByRole('button', { name: /日本/ });
  fireEvent.click(japan);
  expect(japan.getAttribute('aria-pressed')).toBe('true');
  const alice = screen.getByRole('button', { name: 'alice · 500 B' });
  expect(within(alice).getByTitle('alice')).toBeTruthy();
  expect(screen.queryByText('platform')).toBeNull();
  fireEvent.click(alice);
  expect(alice.getAttribute('aria-haspopup')).toBe('dialog');
  const detail = await screen.findByRole('dialog', { name: 'alice · 流入明细' });
  expect(await within(detail).findByRole('list', { name: '来源' })).toBeTruthy();
  fireEvent.click(within(detail).getByRole('button', { name: '关闭流入明细' }));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: 'alice · 流入明细' })).toBeNull());
  act(() => {
    width = 720;
    resize();
  });
  expect(screen.getByRole('img', { name: /用户到接入链/ })).toBeTruthy();
});

it('keeps tenants distinct, includes forwarding and unattributed bytes, and distinguishes missing reports', async () => {
  vi.mocked(fetchNodeInflowUsage).mockResolvedValue(
    inflow({
      rows: [
        row('alice', 'a', 300),
        row('alice', 'a', 50, { tenant_id: 'platform.other', has_gap: true }),
        row('alice', '', 80, { source: 'forwarding', app_id: null, chain_id: null }),
        row('alice', '', 40, { source: 'unattributed', app_id: null, chain_id: null }),
      ],
    }),
  );
  const { container, unmount } = mount(
    <NodeInflowUsageCard nodeId="n1" period="day" sourceOf={trafficSources(snapshot)} />,
  );
  await screen.findByRole('img', { name: /用户到接入链/ });
  expect(container.querySelectorAll('.ntf-label.left')).toHaveLength(2);
  expect(screen.getByRole('button', { name: 'alice · 420 B' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'alice · 50 B' })).toBeTruthy();
  expect(container.textContent).not.toContain('platform');
  const forwarding = screen.getByRole('button', { name: /端口转发/ });
  const expected = render(<Icon of="forwarding" size={13} />).container.querySelector('svg')!.innerHTML;
  expect(forwarding.querySelector('svg')?.innerHTML).toBe(expected);
  expect(forwarding.querySelector('.traffic-source-icon')?.getAttribute('aria-hidden')).toBe('true');
  expect(forwarding.querySelector('.geo-flag')).toBeNull();
  const unattributed = screen.getByRole('button', { name: /未按接入链记录/ });
  expect(unattributed.querySelector('.traffic-source-icon')).toBeNull();
  expect(screen.getByText('采集有缺口，仅含已记录的流量。')).toBeTruthy();
  act(() => {
    width = 719;
    resize();
  });
  const legend = screen.getByRole('group', { name: '接入链（点击聚焦）' });
  expect(
    within(legend)
      .getByRole('button', { name: /端口转发/ })
      .querySelector('.traffic-source-icon'),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'alice · 420 B' }));
  const detail = await screen.findByRole('dialog', { name: 'alice · 流入明细' });
  expect(await within(detail).findByText('端口转发 · 不区分来源')).toBeTruthy();
  unmount();
  vi.mocked(fetchNodeInflowUsage).mockResolvedValue(inflow({ rows: [], latest_report_unix_secs: null }));
  mount(<NodeInflowUsageCard nodeId="n2" period="day" />);
  expect(await screen.findByText('本机尚无流量上报，不能视为零流量。')).toBeTruthy();
});

it('folds users without losing their traffic and makes all users available', async () => {
  vi.mocked(fetchNodeInflowUsage).mockResolvedValue(
    inflow({ rows: Array.from({ length: 15 }, (_, i) => row(`u${i}`, 'a', 100)) }),
  );
  const { container } = mount(<NodeInflowUsageCard nodeId="n1" period="day" />);
  const expand = await screen.findByRole('button', { name: '展开全部 15 位用户' });
  expect(container.querySelectorAll('path.ntf-link')).toHaveLength(11);
  expect(screen.getByRole('button', { name: /其余 5 位.*500 B/ })).toBeTruthy();
  fireEvent.click(expand);
  expect(container.querySelectorAll('path.ntf-link')).toHaveLength(15);
});

it('keeps the shared statistics range before the icon-only close button in the compact header', async () => {
  vi.stubGlobal(
    'EventSource',
    class {
      addEventListener() {}
      close() {}
    },
  );
  mount(<NodeTrafficDialog nodeId="n1" name="香港机器" country="HK" onClose={() => undefined} />);
  await screen.findByRole('img', { name: /用户到接入链/ });
  const dialog = screen.getByRole('dialog', { name: '香港机器 · 用量' });
  const header = dialog.querySelector<HTMLElement>('.node-usage-header')!;
  const range = within(header).getByRole('group', { name: '流量统计范围' });
  const close = within(header).getByRole('button', { name: '关闭机器用量' });
  expect(range.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(close.textContent).toBe('');
  expect(close.querySelector('svg')).toBeTruthy();
  expect(within(header).getByRole('img', { name: 'HK 地区旗' }).classList.contains('square')).toBe(true);
  expect(header.querySelector('p')).toBeNull();
  expect(dialog.querySelector('.node-usage-body')?.querySelector('[aria-label="流量统计范围"]')).toBeNull();
  expect(screen.queryByText('机器用量 · 实时与流入、流出统计')).toBeNull();
  const live = screen.getByTestId('live-card');
  expect(screen.getAllByRole('group')).toHaveLength(1);
  fireEvent.click(within(range).getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchNodeInflowUsage).toHaveBeenCalledWith('n1', 'week'));
  await waitFor(() => expect(fetchNodeSiteUsage).toHaveBeenCalledWith('n1', 'week'));
  expect(fetchNodeOtherDomains).toHaveBeenCalledWith('n1', 'week');
  expect(fetchNodeSniffingFailedTargets).toHaveBeenCalledWith('n1', 'week');
  expect(within(range).getByRole('button', { name: '当周' }).getAttribute('aria-pressed')).toBe('true');
  expect(screen.getByTestId('live-card')).toBe(live);
  expect(live.dataset.period).toBe('week');
  expect(fetchUserSiteUsage).not.toHaveBeenCalled();
});

it('loads machine usage only when opened and releases its live stream when Escape closes the dialog', async () => {
  const close = vi.fn();
  const opened = vi.fn();
  vi.stubGlobal(
    'EventSource',
    class {
      constructor(url: string) {
        opened(url);
      }
      addEventListener() {}
      close = close;
    },
  );
  function Launcher() {
    const [open, setOpen] = useState(false);
    return (
      <>
        <button onClick={() => setOpen(true)}>香港机器</button>
        {open && <NodeTrafficDialog nodeId="n1" name="香港机器" country="HK" onClose={() => setOpen(false)} />}
      </>
    );
  }
  mount(<Launcher />);
  const opener = screen.getByRole('button', { name: '香港机器' });
  expect(fetchSnapshot).not.toHaveBeenCalled();
  expect(fetchNodeInflowUsage).not.toHaveBeenCalled();
  opener.focus();
  fireEvent.click(opener);
  const dialog = await screen.findByRole('dialog', { name: '香港机器 · 用量' });
  await within(dialog).findByRole('img', { name: /用户到接入链/ });
  expect(within(dialog).getByTestId('live-card')).toBeTruthy();
  expect(opened).toHaveBeenCalledExactlyOnceWith('/realtime/nodes/n1/events');
  expect(document.body.style.overflow).toBe('hidden');
  fireEvent.keyDown(document, { key: 'Escape' });
  await waitFor(() => expect(screen.queryByRole('dialog', { hidden: true })).toBeNull());
  expect(close).toHaveBeenCalledOnce();
  await waitFor(() => expect(document.activeElement).toBe(opener));
  expect(document.body.style.overflow).not.toBe('hidden');
});

it('uses the opening range for every machine usage source and keeps the range switch interactive', async () => {
  vi.stubGlobal(
    'EventSource',
    class {
      addEventListener() {}
      close() {}
    },
  );
  mount(<NodeTrafficDialog nodeId="n1" name="香港机器" country="HK" initialPeriod="month" onClose={() => undefined} />);
  await screen.findByRole('img', { name: /用户到接入链/ });
  for (const source of [
    fetchNodeInflowUsage,
    fetchNodeSiteUsage,
    fetchNodeOtherDomains,
    fetchNodeSniffingFailedTargets,
  ]) {
    expect(source).toHaveBeenCalledWith('n1', 'month');
    expect(vi.mocked(source).mock.calls.every(([, period]) => period === 'month')).toBe(true);
  }
  const range = screen.getByRole('group', { name: '流量统计范围' });
  fireEvent.click(within(range).getByRole('button', { name: '当天' }));
  await waitFor(() => expect(fetchNodeInflowUsage).toHaveBeenCalledWith('n1', 'day'));
  await waitFor(() => expect(fetchNodeSiteUsage).toHaveBeenCalledWith('n1', 'day'));
});

it('applies a UTC+8 custom range to all statistics and restores unfiltered presets', async () => {
  vi.stubGlobal(
    'EventSource',
    class {
      addEventListener() {}
      close() {}
    },
  );
  mount(<NodeTrafficDialog nodeId="n1" name="香港机器" country="HK" onClose={() => undefined} />);
  await screen.findByRole('img', { name: /用户到接入链/ });
  const scope = screen.getByRole('group', { name: '流量统计范围' });
  fireEvent.click(within(scope).getByRole('button', { name: '自定义' }));
  const until = Math.floor(Date.now() / 30_000) * 30;
  fireEvent.change(screen.getByLabelText('开始时间（UTC+8）'), { target: { value: trafficDateInput(until - 600) } });
  fireEvent.change(screen.getByLabelText('结束时间（UTC+8）'), { target: { value: trafficDateInput(until) } });
  fireEvent.click(screen.getByRole('button', { name: '应用' }));
  for (const load of [
    fetchNodeInflowUsage,
    fetchNodeSiteUsage,
    fetchNodeOtherDomains,
    fetchNodeSniffingFailedTargets,
  ]) {
    await waitFor(() =>
      expect(load).toHaveBeenCalledWith('n1', 'day', { start_unix_secs: until - 600, end_unix_secs: until }),
    );
  }
  expect(screen.getByTestId('live-card')).toBeTruthy();
  expect(screen.getByTestId('live-card').dataset.range).toBe(
    JSON.stringify({ start_unix_secs: until - 600, end_unix_secs: until }),
  );
  fireEvent.click(within(scope).getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchNodeInflowUsage).toHaveBeenCalledWith('n1', 'week'));
  expect(screen.getByTestId('live-card').dataset.period).toBe('week');
  expect(screen.getByTestId('live-card').dataset.range).toBeUndefined();
});

it('waits for machine access metadata and keeps model failures visible', async () => {
  let finish: ((value: ConsoleSnapshot) => void) | undefined;
  vi.mocked(fetchSnapshot).mockReturnValueOnce(
    new Promise(resolve => {
      finish = resolve;
    }),
  );
  vi.stubGlobal(
    'EventSource',
    class {
      addEventListener() {}
      close() {}
    },
  );
  const view = mount(<NodeTrafficDialog nodeId="n1" name="香港机器" country="HK" onClose={() => undefined} />);
  expect(screen.getByRole('status', { name: '加载机器用量' })).toBeTruthy();
  expect(fetchNodeInflowUsage).not.toHaveBeenCalled();
  await act(async () => {
    finish?.(snapshot);
  });
  await screen.findByRole('img', { name: /用户到接入链/ });
  view.unmount();

  vi.mocked(fetchSnapshot).mockRejectedValueOnce(new Error('模型暂不可用'));
  mount(<NodeTrafficDialog nodeId="n2" name="日本机器" country="JP" onClose={() => undefined} />);
  await screen.findByText('模型暂不可用');
  expect(screen.queryByTestId('live-card')).toBeNull();
  expect(screen.getByRole('button', { name: '关闭机器用量' })).toBeTruthy();
});

it('expands a destination to its top users on this machine and folds the remaining users', async () => {
  mount(<NodeSiteUsageCard nodeId="n1" period="day" />);
  const site = await screen.findByRole('button', { name: /GitHub/ });
  fireEvent.click(site);
  const users = screen.getByLabelText('GitHub的主要用户');
  expect(users.textContent).toContain('alice');
  expect(users.textContent).toContain('70 B');
  expect(users.textContent).toContain('其余 2 位用户 · 30 B');
  expect(screen.queryByRole('button', { name: '当天' })).toBeNull();
});

it('includes forwarding in outflow totals and its own chart layer and expands its main users', async () => {
  const data = outflow();
  data.rows.push({ site_id: 'forwarding', name: '端口转发', uplink_bytes: 30, downlink_bytes: 70 });
  data.series.push({ site_id: 'forwarding', points: [[start, 30, 70]] });
  data.observed_since_unix_secs = start + 1800;
  data.top_users.push({
    site_id: 'forwarding',
    users: [{ tenant_id: 'platform', user_id: 'bob', uplink_bytes: 20, downlink_bytes: 50 }],
    other_users: 2,
    other_uplink_bytes: 10,
    other_downlink_bytes: 20,
  });
  vi.mocked(fetchNodeSiteUsage).mockResolvedValue(data);
  const { container } = mount(<NodeSiteUsageCard nodeId="n1" period="day" />);
  const forwarding = await screen.findByRole('button', { name: /端口转发.*100.*50\.0%/ });
  const ranking = screen.getByRole('region', { name: '各类型流量' });
  expect(within(ranking).getAllByRole('button')[0]).toBe(forwarding);
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('200B');
  expect(container.querySelector('.site-usage-label')?.textContent).toContain('1 类站点 · 端口转发');
  expect(screen.queryByRole('button', { name: /其余.*类站点/ })).toBeNull();
  const expected = render(<Icon of="forwarding" size={18} />).container.querySelector('svg')!.innerHTML;
  expect(forwarding.querySelector('svg')?.innerHTML).toBe(expected);
  expect(forwarding.querySelector('.site-brand-icon')?.getAttribute('aria-hidden')).toBe('true');
  const chart = await screen.findByTestId('site-chart');
  const series: SiteUsageChartSeries[] = JSON.parse(chart.getAttribute('data-series')!);
  expect(series[0].key).toBe('forwarding');
  const layer = series.find(item => item.key === 'forwarding')!;
  expect(layer.name).toBe('端口转发');
  expect(layer.values[0]).toBe(100);
  expect(layer.values[1]).toBeNull();
  expect(chart.getAttribute('data-observed-since')).toBe('');
  fireEvent.focus(forwarding);
  expect(chart.getAttribute('data-focus')).toBe('forwarding');
  fireEvent.click(forwarding);
  const users = screen.getByLabelText('端口转发的主要用户');
  expect(users.textContent).toContain('bob');
  expect(users.textContent).toContain('70 B');
  expect(users.textContent).toContain('其余 2 位用户 · 30 B');
});

it('shows forwarding without authenticated ingress or site collection and explains missing hourly detail', async () => {
  vi.mocked(fetchNodeSiteUsage).mockResolvedValue({
    ...outflow(),
    collection_enabled: false,
    reporting_nodes: 0,
    expected_nodes: 0,
    latest_report_unix_secs: null,
    observed_since_unix_secs: null,
    rows: [{ site_id: 'forwarding', name: '端口转发', uplink_bytes: 30, downlink_bytes: 70 }],
    series: [{ site_id: 'forwarding', points: [] }],
    top_users: [],
  });
  const { container } = mount(<NodeSiteUsageCard nodeId="n1" period="week" />);
  await screen.findByRole('button', { name: /端口转发.*100.*100\.0%/ });
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('100B');
  expect(screen.getByText('站点采集已关闭 · 转发仍按账本统计')).toBeTruthy();
  expect(screen.getByText('端口转发有 100 B 已计入总量，小时明细已不完整，未分配到图中。')).toBeTruthy();
  expect(screen.queryByText(/尚无认证代理入口/)).toBeNull();
  expect(fetchNodeSiteUsage).toHaveBeenCalledWith('n1', 'week');
  const scope = screen.getByText('流出统计').closest('header')?.querySelector('[title]');
  expect(scope?.getAttribute('title')).toContain('端口转发作为独立类型计入');
  expect(scope?.getAttribute('title')).not.toContain('不含端口转发');
});

it('keeps site coverage warnings independent of forwarding volume', async () => {
  const data = outflow();
  data.rows.push(
    { site_id: 'sniffing-failed', name: '嗅探失败', uplink_bytes: 0, downlink_bytes: 100 },
    { site_id: 'forwarding', name: '端口转发', uplink_bytes: 0, downlink_bytes: 10000 },
  );
  data.series.push(
    { site_id: 'sniffing-failed', points: [[start, 0, 100]] },
    { site_id: 'forwarding', points: [[start, 0, 10000]] },
  );
  vi.mocked(fetchNodeSiteUsage).mockResolvedValue(data);
  mount(<NodeSiteUsageCard nodeId="n1" period="day" />);
  const ranking = await screen.findByRole('region', { name: '各类型流量' });
  const failed = within(ranking).getByRole('button', { name: /嗅探失败/ });
  expect(failed.querySelector('.site-usage-pct')?.classList.contains('warn')).toBe(true);
  const chart = await screen.findByTestId('site-chart');
  const series: SiteUsageChartSeries[] = JSON.parse(chart.getAttribute('data-series')!);
  expect(series.find(item => item.key === 'sniffing-failed')?.color.token).toBe('--gold');
});

it('shows request errors with retry instead of presenting them as an empty period', async () => {
  vi.mocked(fetchNodeInflowUsage).mockRejectedValue(new Error('forbidden'));
  mount(<NodeInflowUsageCard nodeId="n1" period="day" />);
  expect((await screen.findAllByRole('alert'))[0].textContent).toContain('读取失败');
  expect(screen.queryByText('所选范围没有已记录的流入流量。')).toBeNull();
  vi.mocked(fetchNodeInflowUsage).mockResolvedValue(inflow());
  fireEvent.click(screen.getAllByRole('button', { name: '重试' })[0]);
  await screen.findByRole('img', { name: /用户到接入链/ });
});

it('counts authorized users once across several ingress grants and excludes other machines', () => {
  const model = structuredClone(snapshot);
  model.snapshot.apps[0].ingresses = [2001, 2002, 2003].map((port, index) => ({
    id: `entry-${index}`,
    chain: 'a',
    node: index === 2 ? 'other' : 'n1',
    port,
    bind: '0.0.0.0',
    projection: {},
    guard: {
      no_private: false,
      no_bittorrent: false,
      no_mail: false,
      no_udp_amplification: false,
      tcp_and_quic_only: false,
      cn_source_block: 'off',
    },
    identity: {},
    wires: {},
  }));
  model.snapshot.apps[0].grants = [
    { tenant: 'platform', user: 'alice', ingress: 'entry-0' },
    { tenant: 'platform', user: 'bob', ingress: 'entry-1' },
  ];
  model.snapshot.apps[0].grants.push({ tenant: 'platform', user: 'alice', ingress: 'entry-1' });
  expect(trafficUserKeys(model, 'n1')).toEqual(['platform/alice', 'platform/bob']);
});

it('keeps a moved chain in one source with its current name and unchanged totals', async () => {
  vi.mocked(fetchNodeInflowUsage).mockImplementation(async (_node, period) => ({
    ...inflow(),
    period,
    rows: [row('alice', 'a', 100, { app_id: 'app-old' }), row('alice', 'a', 200)],
  }));
  const { container } = mount(<NodeInflowUsageCard nodeId="n1" period="month" sourceOf={trafficSources(snapshot)} />);
  await screen.findByRole('img', { name: /用户到接入链的流向/ });
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('300 B');
  expect(container.textContent).toContain('1 个来源');
  expect(screen.queryByText('接入链 a')).toBeNull();
  expect(screen.getAllByText('香港').length).toBeGreaterThan(0);
  expect(container.querySelectorAll('path.ntf-link')).toHaveLength(1);
  fireEvent.click(screen.getByRole('button', { name: 'alice · 300 B' }));
  const detail = await screen.findByRole('dialog', { name: 'alice · 流入明细' });
  const sources = await within(detail).findByRole('list', { name: '来源' });
  expect(within(detail).queryByRole('group', { name: /接入链/ })).toBeNull();
  expect(sources.querySelectorAll('.nid-meter > i')).toHaveLength(1);
  expect(
    [...sources.querySelectorAll<HTMLElement>('.nid-meter > i')].every(
      part => part.style.background === 'var(--observe-1)',
    ),
  ).toBe(true);
});
