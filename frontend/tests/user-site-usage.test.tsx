import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  fetchUserOtherDomains,
  fetchUserSiteUsage,
  fetchUserSniffingFailedTargets,
  type UserOtherDomains,
  type UserSiteUsage,
  type UserSniffingFailedTargets,
} from '../src/api';
import type { SiteUsageChartSeries } from '../src/site-usage-chart';
import { domainGroups, mainDomain, targetItems, UserSiteUsageCard } from '../src/user-site-usage';

vi.mock('../src/api', async original => ({
  ...(await original<typeof import('../src/api')>()),
  fetchUserSiteUsage: vi.fn(),
  fetchUserOtherDomains: vi.fn(),
  fetchUserSniffingFailedTargets: vi.fn(),
}));
// ECharts 不在 jsdom 里绘制；替身只回显图收到的层与周期。
vi.mock('../src/site-usage-chart', () => ({
  default: ({
    series,
    period,
    focusKey,
    startUnixSecs,
    observedSinceUnixSecs,
    uncertainBeforeUnixSecs,
  }: {
    series: SiteUsageChartSeries[];
    period: string;
    focusKey: string | null;
    startUnixSecs: number;
    observedSinceUnixSecs: number | null;
    uncertainBeforeUnixSecs?: number | null;
  }) => (
    <div
      data-testid="site-usage-chart"
      data-period={period}
      data-focus={focusKey ?? ''}
      data-series={series.map(item => item.key).join(',')}
      data-slots={series[0]?.values.length ?? 0}
      data-start={startUnixSecs}
      data-observed-since={observedSinceUnixSecs ?? ''}
      data-uncertain-before={uncertainBeforeUnixSecs ?? ''}
      data-values={JSON.stringify(series[0]?.values)}
    />
  ),
}));
const fetchSites = vi.mocked(fetchUserSiteUsage);
const fetchDomains = vi.mocked(fetchUserOtherDomains);
const fetchTargets = vi.mocked(fetchUserSniffingFailedTargets);

// 2026-10-17 00:00 +08 起的当天，查询时刻 05:16；站点统计 02:22 开始。
const DAY_START = 1_792_166_400;
const NOW = DAY_START + 5 * 3600 + 16 * 60;
const row = (site_id: string, name: string, down: number, up = 0) => ({
  site_id,
  name,
  uplink_bytes: up,
  downlink_bytes: down,
});
const sites = (extra: Partial<UserSiteUsage> = {}): UserSiteUsage => ({
  collection_enabled: true,
  catalog_version: 1,
  period: 'day',
  range_start_unix_secs: DAY_START,
  range_end_unix_secs: NOW,
  period_end_unix_secs: DAY_START + 86_400,
  bucket_secs: 3600,
  reporting_nodes: 12,
  expected_nodes: 12,
  limited: false,
  latest_report_unix_secs: NOW - 30,
  observed_since_unix_secs: DAY_START - 86_400,
  rows: [
    row('speedtest', 'Speedtest', 760),
    row('openai', 'OpenAI / ChatGPT', 600, 100),
    row('tiktok', 'TikTok', 500),
    row('google', 'Google', 400),
    row('anthropic', 'Claude', 300),
    row('youtube', 'YouTube', 200),
    row('github', 'GitHub', 100),
    row('other', '其他站点', 9000, 1000),
    row('sniffing-failed', '嗅探失败', 300),
    row('unidentified', '未识别', 50),
  ],
  series: [
    {
      site_id: 'openai',
      points: [
        [DAY_START + 3 * 3600, 60, 400],
        [DAY_START + 4 * 3600, 40, 200],
      ],
    },
    { site_id: 'speedtest', points: [[DAY_START + 4 * 3600, 0, 760]] },
  ],
  ...extra,
});
const detail = <T extends 'domain' | 'target'>(
  key: T,
  values: string[],
  extra: Partial<UserOtherDomains> = {},
): T extends 'domain' ? UserOtherDomains : UserSniffingFailedTargets =>
  ({
    collection_enabled: true,
    top_n: 128,
    range_start_unix_secs: DAY_START,
    range_end_unix_secs: NOW,
    reporting_nodes: 12,
    expected_nodes: 12,
    limited: false,
    observed_since_unix_secs: DAY_START + 4 * 3600 + 57 * 60,
    first_record_unix_secs: values.length > 0 ? DAY_START + 2 * 3600 : null,
    latest_report_unix_secs: NOW,
    rows: values.map((value, index) => ({ [key]: value, uplink_bytes: 10, downlink_bytes: 1000 - index * 50 })),
    ...extra,
  }) as never;
const domains = (extra: Partial<UserOtherDomains> = {}) =>
  detail(
    'domain',
    [
      'a1.ydycdn.com',
      'b2.ydycdn.com',
      'www.nodeseek.com',
      'cdn.jsdelivr.net',
      'shop.example.com.cn',
      'img.example.com.cn',
      'one.example',
      'two.example',
      'three.example',
      'four.example',
    ],
    extra,
  );
const targets = () =>
  detail('target', [
    'tcp://192.0.2.1:443',
    'udp://[2001:db8::1]:443',
    'udp://198.51.100.7:53',
    'tcp://203.0.113.9:2002',
  ]);

const card = (client = new QueryClient({ defaultOptions: { queries: { retry: false } } })) =>
  render(
    <QueryClientProvider client={client}>
      <UserSiteUsageCard tenantId="root" userId="alice" />
    </QueryClientProvider>,
  );
beforeEach(() => {
  fetchSites.mockReset().mockResolvedValue(sites());
  fetchDomains.mockReset().mockResolvedValue(domains());
  fetchTargets.mockReset().mockResolvedValue(targets());
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => setTimeout(() => callback(0), 0));
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('reads categories and both detail lists for one calendar period and stays mounted when switching', async () => {
  card();
  await screen.findByText('Speedtest');
  for (const fetcher of [fetchSites, fetchDomains, fetchTargets]) {
    expect(fetcher).toHaveBeenCalledWith('root', 'alice', 'day');
  }
  const panel = screen.getByRole('region', { name: '站点流量' });
  const filter = within(panel).getByRole('group', { name: '站点统计范围' });
  expect([...filter.querySelectorAll('button')].map(button => button.textContent)).toEqual(['当天', '当周', '当月']);
  // 筛选在正文的合计行里，不在标题条
  expect(panel.querySelector('header .segsw')).toBeNull();
  expect(panel.querySelector('.site-usage-head .segsw')).toBe(filter);
  fetchSites.mockImplementation(() => new Promise(() => {}));
  fireEvent.click(within(filter).getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchSites).toHaveBeenLastCalledWith('root', 'alice', 'week'));
  expect(fetchDomains).toHaveBeenLastCalledWith('root', 'alice', 'week');
  expect(fetchTargets).toHaveBeenLastCalledWith('root', 'alice', 'week');
  expect(screen.getByRole('region', { name: '站点流量' })).toBe(panel);
  expect(screen.getByText('Speedtest')).toBeTruthy();
});

it('stacks the top five sites, merges the rest into one layer and keeps unsorted categories apart', async () => {
  card();
  const chart = await screen.findByTestId('site-usage-chart');
  expect(chart.dataset.series).toBe('speedtest,openai,tiktok,google,anthropic,rest,other,sniffing-failed,unidentified');
  expect(chart.dataset.slots).toBe('24');
  const list = screen.getByRole('region', { name: '各站点流量' });
  const rest = within(list).getByRole('button', { name: /其余 2 类站点/ });
  expect(rest.getAttribute('aria-expanded')).toBe('false');
  expect(within(list).queryByText('YouTube')).toBeNull();
  fireEvent.click(rest);
  expect(within(list).getByText('YouTube')).toBeTruthy();
  expect(within(list).getByText('未归类')).toBeTruthy();
  fireEvent.mouseEnter(within(list).getByText('TikTok').closest('li')!);
  expect(chart.dataset.focus).toBe('tiktok');
  fireEvent.mouseEnter(within(list).getByText('YouTube').closest('li')!);
  expect(chart.dataset.focus).toBe('rest');
});

it('groups other-site hosts by main domain and lists failed targets with protocol and port', async () => {
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  await within(other).findByText('ydycdn.com');
  expect(within(other).getAllByText('2 个主机')).toHaveLength(2);
  expect(within(other).getByText('example.com.cn')).toBeTruthy();
  // 单个主机时子域名淡色显示，主域名完整
  expect(within(other).getByText('www.').closest('.site-usage-dname')?.textContent).toBe('www.nodeseek.com');
  const failed = screen.getByRole('region', { name: '嗅探失败明细' });
  await within(failed).findByText('192.0.2.1');
  const rows = [...failed.querySelectorAll('.site-usage-drow .site-usage-dname')].map(item => item.textContent);
  expect(rows).toEqual([
    'TCP192.0.2.1:443HTTPS',
    'UDP[2001:db8::1]:443QUIC',
    'UDP198.51.100.7:53DNS',
    'TCP203.0.113.9:2002',
  ]);
  expect(failed.querySelector('a')).toBeNull();
});

it('lists six detail rows by default, all on request, and collapses to a preview', async () => {
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  await within(other).findByText('ydycdn.com');
  expect(other.querySelectorAll('.site-usage-drow')).toHaveLength(6);
  fireEvent.click(within(other).getByRole('button', { name: '显示全部 8 个主域名' }));
  expect(other.querySelectorAll('.site-usage-drow')).toHaveLength(8);
  fireEvent.click(within(other).getByRole('button', { name: '收起' }));
  expect(other.querySelectorAll('.site-usage-drow')).toHaveLength(6);
  const head = within(other).getByRole('button', { name: /其他站点/ });
  fireEvent.click(head);
  expect(head.getAttribute('aria-expanded')).toBe('false');
  expect(other.querySelectorAll('.site-usage-drow')).toHaveLength(0);
  expect(head.textContent).toContain('ydycdn.com、example.com.cn、nodeseek.com 等 8 个主域名');
  // 排行里的「其他站点」直接展开并定位到明细
  fireEvent.click(within(screen.getByRole('region', { name: '各站点流量' })).getByRole('button', { name: /其他站点/ }));
  expect(head.getAttribute('aria-expanded')).toBe('true');
});

it('states how much of a category its detail covers', async () => {
  fetchDomains.mockResolvedValue(
    domains({ top_n: 10, limited: true, range_start_unix_secs: DAY_START + 3600, reporting_nodes: 10 }),
  );
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  const note = await within(other).findByText(/覆盖/);
  expect(note.textContent).toBe(
    '最早记录：今天 02:00 · 覆盖 8 KiB，占其他站点 78.5% · 只保留前 10 个主机 · 10 / 12 台入口已上报 · 计数受限，部分流量未计入',
  );
});

it('shows the first actual record instead of the retention boundary or current collection window', async () => {
  const weekStart = DAY_START - 5 * 86_400;
  const retainedSince = DAY_START - 7 * 86_400 + 6 * 3600;
  fetchSites.mockResolvedValue(sites({ period: 'month', range_start_unix_secs: DAY_START - 16 * 86_400 }));
  fetchDomains.mockResolvedValue(
    domains({
      range_start_unix_secs: retainedSince,
      observed_since_unix_secs: NOW - 60,
      first_record_unix_secs: retainedSince + 3 * 3600,
    }),
  );
  fetchTargets.mockResolvedValue(
    detail('target', ['tcp://192.0.2.1:443'], {
      range_start_unix_secs: DAY_START - 3 * 86_400,
      observed_since_unix_secs: retainedSince,
      first_record_unix_secs: DAY_START - 2 * 86_400 + 3600,
    }),
  );
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  expect((await within(other).findByText(/覆盖/)).textContent).toMatch(/^最早记录：10-10 09:00/);
  const failed = screen.getByRole('region', { name: '嗅探失败明细' });
  expect((await within(failed).findByText(/覆盖/)).textContent).toMatch(/^最早记录：10-15 01:00/);
  expect(within(other).queryByText(/起保留|起采集/)).toBeNull();

  fetchSites.mockResolvedValue(sites({ period: 'week', range_start_unix_secs: weekStart }));
  fetchDomains.mockResolvedValue(
    domains({
      range_start_unix_secs: weekStart,
      observed_since_unix_secs: NOW - 60,
      first_record_unix_secs: weekStart + 2 * 3600,
    }),
  );
  fireEvent.click(screen.getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchDomains).toHaveBeenLastCalledWith('root', 'alice', 'week'));
  await waitFor(() => expect(within(other).getByText(/覆盖/).textContent).toMatch(/^最早记录：10-12 02:00/));
});

it.each([null, undefined])('does not infer a first record when the actual timestamp is %s', async first => {
  fetchDomains.mockResolvedValue(domains({ first_record_unix_secs: first }));
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  const note = await within(other).findByText(/覆盖/);
  expect(note.textContent).not.toMatch(/最早记录|起采集|起保留/);
});

it('does not show a first record for empty detail rows', async () => {
  fetchDomains.mockResolvedValue(domains({ rows: [], first_record_unix_secs: null }));
  card();
  const other = await screen.findByRole('region', { name: '其他站点明细' });
  const note = await within(other).findByText(/暂无记录/);
  expect(note.textContent).not.toMatch(/最早记录|起采集|起保留/);
});

it('retains midnight history when collection later establishes a new continuous baseline', async () => {
  const resumed = DAY_START + 105 * 60;
  fetchSites.mockResolvedValue(
    sites({
      observed_since_unix_secs: resumed,
      rows: [row('youtube', 'YouTube', 200)],
      series: [
        {
          site_id: 'youtube',
          points: [
            [DAY_START, 0, 150],
            [DAY_START + 2 * 3600, 0, 50],
          ],
        },
      ],
    }),
  );
  const { container } = card();
  const chart = await screen.findByTestId('site-usage-chart');
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('200B');
  expect(chart.dataset.start).toBe(String(DAY_START));
  expect(chart.dataset.observedSince).toBe('');
  expect(chart.dataset.uncertainBefore).toBe(String(resumed));
  expect(JSON.parse(chart.dataset.values!)[0]).toBe(150);
  expect(JSON.parse(chart.dataset.values!)[2]).toBe(50);
  expect(screen.queryByText(/观测起点/)).toBeNull();
  expect(screen.getByText('站点采集可能有缺口').title).toContain('此前已有记录继续计入');
});

it('marks a high sniffing-failure share, uncertain historical coverage and missing reporters', async () => {
  fetchSites.mockResolvedValue(
    sites({
      reporting_nodes: 10,
      observed_since_unix_secs: DAY_START + 2 * 3600 + 22 * 60,
      rows: [row('youtube', 'YouTube', 100), row('sniffing-failed', '嗅探失败', 100)],
    }),
  );
  card();
  const list = await screen.findByRole('region', { name: '各站点流量' });
  expect(within(list).getByText('50.0%', { selector: '.site-usage-pct.warn' })).toBeTruthy();
  expect(screen.getByText('站点采集可能有缺口').classList.contains('warn')).toBe(true);
  const status = screen.getByRole('status');
  expect(status.classList.contains('warn')).toBe(true);
  expect(status.textContent).toBe('10 / 12 台入口已上报');
});

it('distinguishes unsupported, empty, disabled and failed reports', async () => {
  fetchSites.mockResolvedValue(sites({ reporting_nodes: 0, latest_report_unix_secs: null, rows: [], series: [] }));
  const unsupported = card();
  await screen.findByText(/尚无站点观测数据/);
  expect(screen.getByRole('group', { name: '站点统计范围' })).toBeTruthy();
  unsupported.unmount();

  fetchSites.mockResolvedValue(sites({ rows: [], series: [] }));
  fetchDomains.mockResolvedValue(domains({ rows: [] }));
  fetchTargets.mockResolvedValue(detail('target', []));
  const empty = card();
  await screen.findByText('10-17 周六 · 当天没有观测到流量');
  expect(screen.getByRole('group', { name: '站点统计范围' })).toBeTruthy();
  expect(screen.getByText('当天没有站点记录')).toBeTruthy();
  expect(screen.queryByTestId('site-usage-chart')).toBeNull();
  empty.unmount();

  fetchSites.mockResolvedValue(sites({ collection_enabled: false }));
  const disabled = card();
  expect((await screen.findByText('采集已关闭 · 显示已保留的数据')).classList.contains('rut-status')).toBe(true);
  disabled.unmount();

  fetchSites.mockRejectedValue(new Error('unavailable'));
  card();
  expect((await screen.findByRole('alert')).textContent).toContain('读取失败');
});

it('puts forwarding first in the user ranking and chart and includes it in totals and shares', async () => {
  fetchSites.mockResolvedValue(
    sites({
      rows: [row('github', 'GitHub', 900), row('forwarding', '端口转发', 70, 30)],
      series: [
        { site_id: 'github', points: [[DAY_START + 3 * 3600, 0, 900]] },
        { site_id: 'forwarding', points: [[DAY_START + 3600, 30, 70]] },
      ],
      observed_since_unix_secs: DAY_START + 2 * 3600,
    }),
  );
  const { container } = card();
  const ranking = await screen.findByRole('region', { name: '各站点流量' });
  expect([...ranking.querySelectorAll('.site-usage-name')].map(item => item.textContent)).toEqual([
    '端口转发',
    'GitHub',
  ]);
  const forwarding = within(ranking).getByText('端口转发').closest('.site-usage-row')!;
  expect(forwarding.textContent).toContain('10.0%');
  expect(forwarding.querySelector('.site-brand-icon svg')).toBeTruthy();
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('1000B');
  expect(container.querySelector('.site-usage-label')?.textContent).toContain('1 类站点 · 端口转发');
  expect(screen.getByText('站点采集可能有缺口')).toBeTruthy();
  const chart = await screen.findByTestId('site-usage-chart');
  expect(chart.dataset.series).toBe('forwarding,github');
  fireEvent.mouseEnter(forwarding.closest('li')!);
  expect(chart.dataset.focus).toBe('forwarding');
  expect(screen.getByText('站点流量').closest('header')?.querySelector('[title]')?.getAttribute('title')).toContain(
    '端口转发作为独立类型计入',
  );
});

it.each([true, false])('shows forwarding-only user traffic when site collection is %s', async enabled => {
  fetchSites.mockResolvedValue(
    sites({
      collection_enabled: enabled,
      reporting_nodes: 0,
      expected_nodes: 0,
      latest_report_unix_secs: null,
      observed_since_unix_secs: null,
      rows: [row('forwarding', '端口转发', 70, 30)],
      series: [{ site_id: 'forwarding', points: [] }],
    }),
  );
  fetchDomains.mockResolvedValue(domains({ rows: [] }));
  fetchTargets.mockResolvedValue(detail('target', []));
  const { container } = card();
  expect(await screen.findByText('端口转发')).toBeTruthy();
  expect(container.querySelector('.site-usage-total')?.textContent).toBe('100B');
  expect(
    screen.getByText(enabled ? '无认证代理入口 · 转发按账本统计' : '站点采集已关闭 · 转发仍按账本统计'),
  ).toBeTruthy();
  expect(screen.getByText('端口转发有 100 B 已计入总量，小时明细已不完整，未分配到图中。')).toBeTruthy();
  expect(screen.queryByText(/尚无已记录的站点或端口转发流量/)).toBeNull();
  expect((await screen.findByTestId('site-usage-chart')).dataset.series).toBe('forwarding');
});

it('does not carry one user profile into another user while loading', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const user = (name: string) => (
    <QueryClientProvider client={client}>
      <UserSiteUsageCard tenantId="root" userId={name} />
    </QueryClientProvider>
  );
  const view = render(user('alice'));
  await screen.findByText('Speedtest');
  fetchSites.mockImplementation(() => new Promise(() => {}));
  fetchDomains.mockImplementation(() => new Promise(() => {}));
  fetchTargets.mockImplementation(() => new Promise(() => {}));
  await act(async () => view.rerender(user('bob')));
  await waitFor(() => expect(fetchSites).toHaveBeenCalledWith('root', 'bob', 'day'));
  expect(screen.queryByText('Speedtest')).toBeNull();
  expect(screen.queryByText('ydycdn.com')).toBeNull();
});

it('derives main domains and target labels without guessing sites', () => {
  expect(mainDomain('lf26-dy-cdn-tos.bytegecko.com')).toBe('bytegecko.com');
  expect(mainDomain('www.example.co.uk')).toBe('example.co.uk');
  expect(mainDomain('static.example.cn')).toBe('example.cn');
  expect(mainDomain('localhost')).toBe('localhost');
  expect(mainDomain('static.school.k12.ca.us')).toBe('school.k12.ca.us');
  expect(mainDomain('img.alice.github.io')).toBe('alice.github.io');
  expect(mainDomain('img.bob.github.io')).toBe('bob.github.io');
  expect(mainDomain('cdn.alice.blogspot.com')).toBe('alice.blogspot.com');
  expect(mainDomain('www.city.kawasaki.jp')).toBe('city.kawasaki.jp');
  const groups = domainGroups([
    { domain: 'a.cdn.example', uplink_bytes: 1, downlink_bytes: 9 },
    { domain: 'b.cdn.example', uplink_bytes: 1, downlink_bytes: 4 },
    { domain: 'big.example', uplink_bytes: 0, downlink_bytes: 20 },
  ]);
  expect(groups.map(group => [group.main, group.hosts.length, group.total])).toEqual([
    ['big.example', 1, 20],
    ['cdn.example', 2, 15],
  ]);
  expect(targetItems([{ target: 'udp://[2001:db8::1]:853', uplink_bytes: 0, downlink_bytes: 1 }])[0]).toMatchObject({
    protocol: 'UDP',
    host: '[2001:db8::1]',
    port: '853',
    portName: 'DoT',
  });
});

it('keeps independent details and the period filter available without fixed-category observations', async () => {
  fetchSites.mockResolvedValue(sites({ collection_enabled: false, reporting_nodes: 0, rows: [], series: [] }));
  card();
  expect(await screen.findByText('ydycdn.com')).toBeTruthy();
  expect(await screen.findByText('192.0.2.1')).toBeTruthy();
  fireEvent.click(within(screen.getByRole('group', { name: '站点统计范围' })).getByRole('button', { name: '当月' }));
  await waitFor(() => expect(fetchDomains).toHaveBeenLastCalledWith('root', 'alice', 'month'));
  expect(fetchTargets).toHaveBeenLastCalledWith('root', 'alice', 'month');
});

it('shows detail failures even when there are no category rows, and keeps successful details on site failure', async () => {
  fetchSites.mockRejectedValue(new Error('unavailable'));
  fetchTargets.mockRejectedValue(new Error('unavailable'));
  card();
  expect(await screen.findByText('ydycdn.com')).toBeTruthy();
  expect(await screen.findByText('嗅探失败明细读取失败，请稍后重试。')).toBeTruthy();
  expect(screen.getByText('站点统计读取失败，请稍后重试。')).toBeTruthy();
  expect(screen.getByRole('group', { name: '站点统计范围' })).toBeTruthy();
});

it('does not calculate coverage against a different period while the chart is refreshing', async () => {
  card();
  await screen.findByText(/占其他站点/);
  fetchSites.mockImplementation(() => new Promise(() => {}));
  fireEvent.click(within(screen.getByRole('group', { name: '站点统计范围' })).getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchDomains).toHaveBeenLastCalledWith('root', 'alice', 'week'));
  expect(screen.queryByText(/占其他站点/)).toBeNull();
});

it('marks retained site data when a refresh fails', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  card(client);
  await screen.findByText('Speedtest');
  fetchSites.mockRejectedValue(new Error('unavailable'));
  await act(() => client.invalidateQueries({ queryKey: ['user-site-usage'] }));
  expect(await screen.findByText('站点统计刷新失败，当前显示上次读取的数据。')).toBeTruthy();
  expect(screen.getByText('Speedtest')).toBeTruthy();
});

it('keeps site identity colors across range/rank changes using the separated monthly palette', async () => {
  const monthRows = [
    row('youtube', 'YouTube', 5000),
    row('openai', 'OpenAI / ChatGPT', 4000),
    row('google', 'Google', 3000),
    row('github', 'GitHub', 2000),
    row('anthropic', 'Claude', 1000),
  ];
  fetchSites.mockImplementation(async (_tenant, _user, period) =>
    sites({
      period,
      rows:
        period === 'month'
          ? monthRows
          : period === 'day'
            ? monthRows.slice(0, 3)
            : [
                row('google', 'Google', 10000),
                row('youtube', 'YouTube', 2000),
                row('openai', 'OpenAI / ChatGPT', 1000),
              ],
    }),
  );
  card();
  await screen.findByText('YouTube');
  const swatches = () =>
    Object.fromEntries(
      [...document.querySelectorAll('.site-usage-item')].map(element => [
        element.querySelector('.site-usage-name')?.textContent,
        element.querySelector<HTMLElement>('.site-usage-swatch')?.style.background,
      ]),
    );
  await waitFor(() => expect(swatches()['YouTube']).toContain('--observe-1'));
  expect(swatches()['OpenAI / ChatGPT']).toContain('--observe-6');
  expect(swatches()['Google']).toContain('--observe-4');
  const before = swatches();
  fireEvent.click(within(screen.getByRole('group', { name: '站点统计范围' })).getByRole('button', { name: '当周' }));
  await waitFor(() => expect(fetchSites).toHaveBeenCalledWith('root', 'alice', 'week'));
  await waitFor(() => expect(document.querySelector('.site-usage-name')?.textContent).toBe('Google'));
  for (const site of ['YouTube', 'OpenAI / ChatGPT', 'Google']) expect(swatches()[site]).toBe(before[site]);
});

it('keeps a newly dominant daily site in the neutral group without reusing a featured color', async () => {
  const monthRows = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map((id, i) => row(id, `Site ${id}`, 1000 - i * 100));
  fetchSites.mockImplementation(async (_tenant, _user, period) =>
    sites({
      period,
      rows:
        period === 'month' ? monthRows : [row('g', 'Site g', 2000), row('a', 'Site a', 500), row('b', 'Site b', 400)],
    }),
  );
  card();
  await waitFor(() => expect(screen.getByTestId('site-usage-chart').dataset.series).toBe('a,b,rest'));
  const list = screen.getByRole('region', { name: '各站点流量' });
  expect(within(list).queryByText('Site g')).toBeNull();
  fireEvent.click(within(list).getByRole('button', { name: /其余 1 类站点/ }));
  expect(within(list).getByText('Site g')).toBeTruthy();
  fireEvent.mouseEnter(within(list).getByText('Site g').closest('li')!);
  expect(screen.getByTestId('site-usage-chart').dataset.focus).toBe('rest');
});
