import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchNodeInflowDetails, type NodeInflowDetails, type NodeInflowDetailRow } from '../src/api';
import { NodeInflowDetailsDialog } from '../src/node-inflow-details';
import { parseTrafficRange, trafficDateInput } from '../src/node-traffic-range';
import { copyText } from '../src/ui/platform';

vi.mock('../src/api', async original => ({
  ...(await original<typeof import('../src/api')>()),
  fetchNodeInflowDetails: vi.fn(),
}));
vi.mock('../src/ui/platform', async original => ({
  ...(await original<typeof import('../src/ui/platform')>()),
  copyText: vi.fn(async () => true),
}));
const start = 1792454400;
const IPV6 = '2001:db8:4c1a:4a29:9017:1598:ffdf:c0ba';
const row = (
  ip: string | null,
  down: number,
  up: number,
  chain = 'a',
  source: NodeInflowDetailRow['source'] = 'chain',
): NodeInflowDetailRow => ({
  tenant_id: 'platform.acme',
  user_id: 'alice',
  source,
  app_id: source === 'chain' ? 'app' : null,
  chain_id: source === 'chain' ? chain : null,
  ingress_id: null,
  source_ip: ip,
  downlink_bytes: down,
  uplink_bytes: up,
  has_gap: false,
});
const data = (extra: Partial<NodeInflowDetails> = {}): NodeInflowDetails => ({
  range_start_unix_secs: start,
  // 当天等口径止于查询时刻，秒数不进读数栏。
  range_end_unix_secs: start + 317,
  observed_since_unix_secs: start,
  latest_report_unix_secs: start + 300,
  limited: false,
  accounting_mismatch: false,
  accounting_downlink_bytes: 420,
  accounting_uplink_bytes: 180,
  rows: [
    row('192.0.2.1', 210, 90),
    row('192.0.2.1', 100, 0, 'b'),
    row(IPV6, 140, 120),
    row(null, 30, 10),
    row(null, 70, 30, '', 'forwarding'),
  ],
  source_countries: [
    { ip: '192.0.2.1', country: 'CN' },
    { ip: IPV6, country: 'JP' },
  ],
  source_operators: [{ ip: '192.0.2.1', operator: 'chinanet' }],
  ...extra,
});
const CHAINS: Record<string, { name: string; country: string; color: string }> = {
  a: { name: '香港 01', country: 'HK', color: 'rgb(1, 2, 3)' },
  b: { name: '日本 02', country: 'JP', color: 'rgb(4, 5, 6)' },
};
// Chain keys are JSON [app, chain, ingress]; forwarding and unattributed rows use their kind.
const chainOf = (key: string) => (key.startsWith('[') ? CHAINS[(JSON.parse(key) as string[])[1]] : undefined);
const mount = (range?: { start_unix_secs: number; end_unix_secs: number }) => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <NodeInflowDetailsDialog
        nodeId="n1"
        nodeName="香港测试机器"
        tenantId="platform.acme"
        userId="alice"
        period="day"
        range={range}
        sourceOf={key => ({ key, name: chainOf(key)?.name ?? key, country: chainOf(key)?.country ?? null })}
        colorOf={key => chainOf(key)?.color}
        onClose={() => {}}
      />
    </QueryClientProvider>,
  );
};
const items = (list: HTMLElement) => [...list.querySelectorAll<HTMLElement>(':scope > li')];
beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: true, addEventListener() {}, removeEventListener() {} }));
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(data());
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it('ranks each source address once across chains, with the bytes of each chain, and copies the exact address', async () => {
  mount();
  const dialog = await screen.findByRole('dialog', { name: 'alice · 流入明细' });
  const list = await within(dialog).findByRole('list', { name: '来源' });
  expect(within(dialog).getByRole('heading', { name: 'alice' })).toBeTruthy();
  expect(dialog.textContent).toContain('香港测试机器 · 流入明细');
  expect(dialog.textContent).not.toContain('platform.acme');
  const readout = within(dialog).getByRole('region', { name: '流入合计' });
  expect(readout.textContent).toContain('当天 · 10-20 08:00–08:05 · UTC+8');
  expect(readout.textContent).toContain('800 B');
  expect(readout.textContent).toMatch(/来源 IP\s*2/);
  expect(readout.textContent).toMatch(/接入链\s*2/);

  // 地址按合计排行；同一地址的两条链合在一行，条形按链分段着色。观测没能拆开的部分排在最后。
  const rows = items(list);
  expect(
    rows.map(item => item.querySelector('code')?.textContent ?? item.querySelector('.nid-unknown')?.textContent),
  ).toEqual(['192.0.2.1', IPV6, '端口转发 · 不区分来源', '未记录来源']);
  expect(rows[0].textContent).toContain('中国电信');
  expect(rows[0].textContent).toContain('400 B');
  expect(rows[0].textContent).toContain('50.0%');
  expect([...rows[0].querySelectorAll<HTMLElement>('.nid-meter > i')].map(segment => segment.style.background)).toEqual(
    ['rgb(1, 2, 3)', 'rgb(4, 5, 6)'],
  );
  expect(rows[0].getAttribute('title')).toBe('香港 01 300 B · 日本 02 100 B · 上行 90 B · 下行 310 B');
  expect(rows[1].textContent).toContain('日本');
  expect(rows[1].querySelector('.user-presence-iid')?.textContent).toBe(':9017:1598:ffdf:c0ba');
  expect(rows[3].querySelector('.nid-unknown')?.getAttribute('title')).toContain('不按 IP 均摊');

  // 上行改变排序、条形与占比。
  fireEvent.click(within(dialog).getByRole('button', { name: '上行' }));
  expect(items(list)[0].textContent).toContain(IPV6.slice(0, 19));
  expect(items(list)[0].textContent).toContain('120 B');

  // 点图例只看这条链的来源，每行只算这条链上的流量。
  fireEvent.click(within(dialog).getByRole('button', { name: '合计' }));
  const legend = within(dialog).getByRole('group', { name: '接入链（点击只看这条链的来源）' });
  const japan = within(legend).getByRole('button', { name: /日本 02/ });
  fireEvent.click(japan);
  expect(japan.getAttribute('aria-pressed')).toBe('true');
  expect(items(list)).toHaveLength(1);
  expect(items(list)[0].textContent).toContain('100 B');
  expect(items(list)[0].textContent).toContain('100.0%');
  fireEvent.click(japan);
  expect(items(list)).toHaveLength(4);

  fireEvent.click(within(list).getByRole('button', { name: '复制来源 IP 192.0.2.1' }));
  await waitFor(() => expect(copyText).toHaveBeenCalledWith('192.0.2.1'));
  expect(vi.mocked(fetchNodeInflowDetails)).toHaveBeenCalledWith('n1', 'platform.acme', 'alice', 'day', undefined);
});

it('folds only addresses, keeps unsplit traffic visible and drops the composition for a single chain', async () => {
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(
    data({
      rows: [...Array.from({ length: 12 }, (_, index) => row(`192.0.2.${index + 1}`, 120 - index, 0)), row(null, 1, 0)],
      source_countries: [],
      source_operators: [],
      range_start_unix_secs: start + 30,
      range_end_unix_secs: start + 330,
    }),
  );
  const range = { start_unix_secs: start + 30, end_unix_secs: start + 330 };
  mount(range);
  const list = await screen.findByRole('list', { name: '来源' });
  expect(screen.getByRole('region', { name: '流入合计' }).textContent).toContain('自定义 · 10-20 08:00:30–08:05:30');
  expect(screen.queryByRole('group', { name: /接入链/ })).toBeNull();
  expect(items(list)).toHaveLength(12);
  const more = within(list).getByRole('button', { name: /其余 2 个来源 · 219 B/ });
  expect(items(list).at(-1)!.textContent).toContain('未记录来源');
  expect(items(list)[0].textContent).toContain('位置未知');
  fireEvent.click(more);
  expect(items(list)).toHaveLength(14);
  expect(items(list).at(-1)!.textContent).toContain('未记录来源');
  fireEvent.click(within(list).getByRole('button', { name: '收起' }));
  expect(items(list)).toHaveLength(12);
  expect(vi.mocked(fetchNodeInflowDetails)).toHaveBeenCalledWith('n1', 'platform.acme', 'alice', 'day', range);
});

it('states incomplete observations in one line and keeps the ledger values behind the mismatch note', async () => {
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(
    data({
      rows: [{ ...row('192.0.2.1', 300, 100), has_gap: true }, row(null, 120, 80, '', 'unattributed')],
      limited: true,
      accounting_mismatch: true,
    }),
  );
  mount();
  const status = (await screen.findByText(/来源记录不完整/)).closest('p')!;
  expect(status.getAttribute('role')).toBe('status');
  expect(status.textContent).toContain('来源记录不完整，仅含已记录部分；所选时段有采集缺口；与用量账本不一致');
  const help = within(status).getByRole('button', { name: '与用量账本不一致' });
  const note = document.getElementById(help.getAttribute('aria-controls')!)!;
  expect(note.hidden).toBe(true);
  fireEvent.click(help);
  expect(help.getAttribute('aria-expanded')).toBe('true');
  expect(note.hidden).toBe(false);
  expect(note.textContent).toContain('上行 180 B、下行 420 B');
  expect(screen.getByText('未按接入链记录', { selector: '.nid-unknown' })).toBeTruthy();
});

it('keeps masked addresses apart, without copy, and locates them only when the mask is unambiguous', async () => {
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(
    data({
      rows: [
        row('203.0.***.***', 300, 0),
        row('203.0.***.***', 200, 0),
        row('198.51.***.***', 120, 0, 'b'),
        row('2001:db8:***', 60, 0),
      ],
      source_countries: [
        { ip: '203.0.***.***', country: 'CN' },
        { ip: '203.0.***.***', country: 'US' },
        { ip: '198.51.***.***', country: 'JP' },
        { ip: '2001:db8:***', country: 'SG' },
      ],
      source_operators: [],
    }),
  );
  mount();
  const list = await screen.findByRole('list', { name: '来源' });
  const rows = items(list);
  expect(rows).toHaveLength(4);
  expect(rows.map(item => item.querySelector('.nid-net')?.textContent)).toEqual([
    '位置未知',
    '位置未知',
    '日本',
    '新加坡',
  ]);
  expect(within(list).queryByRole('button', { name: /复制来源 IP/ })).toBeNull();
  const readout = screen.getByRole('region', { name: '流入合计' });
  expect(readout.textContent).not.toContain('来源 IP');
  expect(readout.textContent).toMatch(/接入链\s*2/);
});

it('says once that nothing was recorded, and never turns a failed request into empty traffic', async () => {
  vi.mocked(fetchNodeInflowDetails).mockRejectedValueOnce(new Error('offline'));
  mount();
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('明细读取失败');
  expect(screen.queryByText(/没有已记录的流入/)).toBeNull();
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(data({ rows: [] }));
  fireEvent.click(within(alert).getByRole('button', { name: '重试' }));
  expect(await screen.findByText('当天 · 10-20 08:00–08:05，没有已记录的流入。')).toBeTruthy();
  expect(screen.queryByRole('region', { name: '流入合计' })).toBeNull();
  expect(screen.queryByRole('list', { name: '来源' })).toBeNull();
});

it('interprets time inputs at UTC+8 and rejects inverted, future, oversized and partial windows', () => {
  const now = 1792458000;
  expect(trafficDateInput(start)).toBe('2026-10-20T08:00:00');
  expect(parseTrafficRange(trafficDateInput(start), trafficDateInput(start + 300), now)).toEqual({
    start_unix_secs: start,
    end_unix_secs: start + 300,
  });
  for (const [from, to] of [
    ['', ''],
    [trafficDateInput(start + 300), trafficDateInput(start)],
    [trafficDateInput(start), trafficDateInput(now + 300)],
    [trafficDateInput(start - 32 * 86400), trafficDateInput(start)],
    [trafficDateInput(start + 1), trafficDateInput(start + 300)],
  ]) {
    expect(typeof parseTrafficRange(from, to, now)).toBe('string');
  }
});

it('merges source segments across historical groups of the same chain', async () => {
  vi.mocked(fetchNodeInflowDetails).mockResolvedValue(
    data({
      rows: [{ ...row('192.0.2.1', 100, 0), app_id: 'old' }, row('192.0.2.1', 200, 0)],
      accounting_downlink_bytes: 300,
      accounting_uplink_bytes: 0,
    }),
  );
  mount();
  const list = await screen.findByRole('list', { name: '来源' });
  expect(items(list)).toHaveLength(1);
  expect(items(list)[0].querySelectorAll('.nid-meter > i')).toHaveLength(1);
  expect(screen.queryByRole('group', { name: /接入链（点击只看这条链的来源）/ })).toBeNull();
});
