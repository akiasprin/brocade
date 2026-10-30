import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import {
  fetchChainUnlockProbes,
  type ChainUnlockProbeItem,
  type NodeUnlockProbe,
  type SnapshotApp,
  type UnlockResult,
} from '../src/api';
import { ChainUnlockTable } from '../src/panes/chain-unlock-table';
import { youtubeSplitResult } from './fixtures/youtube-unlock-data';
import { summarizeChainUnlock } from '../src/panes/chain-unlock-summary';

const result: UnlockResult = { service: 'gemini', status: 'available', code: 'web_available', region: 'JPN' };
const job: NodeUnlockProbe = {
  id: 'report',
  app_id: 'app',
  chain_id: 'one',
  node_id: 'n1',
  family: 'v4',
  state: 'complete',
  created_at: 1,
  completed_at: 2,
  results: [result],
  previous_results: [{ ...result, region: 'HKG' }],
};
const ipv6 = (patch: Partial<NodeUnlockProbe> = {}): NodeUnlockProbe => ({
  ...job,
  family: 'v6',
  previous_results: [],
  ...patch,
});
const item = (
  chain = 'one',
  v4: NodeUnlockProbe | null = job,
  v6: NodeUnlockProbe | null = ipv6(),
): ChainUnlockProbeItem => ({
  app_id: 'app',
  chain_id: chain,
  probes: { v4: v4 && { ...v4, chain_id: chain }, v6: v6 && { ...v6, chain_id: chain }, auto: null },
  capabilities: { family_mode: 'split', ipv6_available: true, ipv6_reason: null },
});
const cellOf = (entry: ChainUnlockProbeItem) =>
  summarizeChainUnlock(entry).cells.find(row => row.service === 'gemini')!;
const app = { id: 'app', label: '线路', ingresses: [{ chain: 'one' }, { chain: 'two' }] } as SnapshotApp;
const groups = [
  {
    app,
    chains: ['one', 'two'].map(id => ({
      chain: { id, name: id, tenant: 'platform' },
      disabled: false,
      tone: 'ok',
      title: '可用',
    })),
  },
];
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  vi.unstubAllGlobals();
});
function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  const open = vi.fn();
  const view = render(
    <QueryClientProvider client={client}>
      <ChainUnlockTable groups={groups} onOpen={open} />
    </QueryClientProvider>,
  );
  return { ...view, client, open };
}

it('loads both families in one paginated query and seeds their unchanged detail reports', async () => {
  const fetcher = vi.fn(async (url: string) =>
    Response.json(
      url.includes('after=') ? { chains: [item('two')], next_cursor: null } : { chains: [item()], next_cursor: 'one' },
    ),
  );
  vi.stubGlobal('fetch', fetcher);
  const view = show();
  await waitFor(() => expect(view.container.querySelectorAll('td[data-service="gemini"]')).toHaveLength(2));
  expect(fetcher.mock.calls.map(([url]) => url)).toEqual([
    '/chains/unlock-probes?limit=100',
    '/chains/unlock-probes?limit=100&after=one',
  ]);
  expect(view.container.querySelector('[data-service="gemini"]')?.textContent).toBe('JP');
  expect(view.getByRole('columnheader', { name: 'YouTube Premium' })).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: /one/ }));
  expect(view.open).toHaveBeenCalledWith('app', 'one');
  expect(view.client.getQueryData(['chain-unlock-probe', 'app', 'one', 'v4'])).toEqual(job);
  expect(view.client.getQueryData(['chain-unlock-probe', 'app', 'one', 'v6'])).toEqual(ipv6());
  expect(job.results[0].region).toBe('JPN');
});

it('shows different IPv6 regions visibly while retaining IPv4 as primary', async () => {
  const entry = item('one', job, ipv6({ results: [{ ...result, region: 'USA' }] }));
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ chains: [entry], next_cursor: null })),
  );
  const view = show();
  await view.findByText('v6 US');
  const cell = view.container.querySelector('[data-service="gemini"]')!;
  expect(cell.textContent).toBe('JPv6 US');
  expect(cell.getAttribute('title')).toContain('地区 JPN');
  expect(cell.getAttribute('title')).toContain('地区 USA');
  expect(cell.getAttribute('data-tone')).toBe('ok');
});

it.each(['v4', 'v6'] as const)(
  'omits the only-%s badge while retaining family facts in the tooltip',
  async normalFamily => {
    const fallback: UnlockResult = {
      ...result,
      region: 'HKG',
      dns_parts: [
        {
          part: 'service',
          kind: 'fallback',
          exits: ['n1'],
          paths: [{ exit: 'n1', normal_family: normalFamily, fallback: 'machine', rules: [] }],
        },
      ],
    };
    const entry = normalFamily === 'v4'
      ? item('one', job, ipv6({ results: [fallback] }))
      : item('one', { ...job, results: [fallback] }, ipv6());
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ chains: [entry], next_cursor: null })));
    const view = show();
    await waitFor(() => expect(view.container.querySelector('[data-service="gemini"]')).toBeTruthy());
    const cell = view.container.querySelector('[data-service="gemini"]')!;
    expect(cell.textContent).toBe('JP');
    expect(cell.querySelector('.cut-v6')).toBeNull();
    expect(cell.getAttribute('title')).toContain(`常规流量仅 ${normalFamily === 'v4' ? 'IPv4' : 'IPv6'}`);
    expect(cell.getAttribute('title')).toContain('实际连接经 DNS 回退取得');
    expect(cell.getAttribute('data-tone')).toBe('ok');
  },
);

it.each([
  ['restricted', 'access_denied'],
  ['unsupported_region', 'region_unsupported'],
  ['failed', 'timeout'],
  ['failed', 'network_error'],
  ['partial', 'originals_only'],
] as const)('uses the warning semicircle for IPv6 %s / %s', (status, code) => {
  expect(cellOf(item('one', job, ipv6({ results: [{ ...result, status, code }] }))).verdict.tone).toBe('part');
});

it('distinguishes both restricted from one family restricted or missing', () => {
  const restricted = {
    ...job,
    results: [{ ...result, status: 'restricted' as const, code: 'access_denied' as const }],
  };
  expect(cellOf(item('one', restricted)).verdict.tone).toBe('part');
  expect(cellOf(item('one', restricted, ipv6({ results: restricted.results }))).verdict).toMatchObject({tone:'no', label:'访问受限'});
  expect(cellOf(item('one', job, null)).verdict.tone).toBe('part');
  const v6only = cellOf(item('one', null, ipv6()));
  expect(v6only.verdict.tone).toBe('part');
  expect(v6only.regionFamily).toBe('v6');
});

it.each([
  ['available', 'no_ipv6_dns'],
  ['restricted', 'no_ipv6_dns'],
  ['failed', 'no_ipv6_dns'],
  ['partial', 'no_ipv6_dns'],
  ['available', 'no_ipv6_egress'],
  ['restricted', 'no_ipv6_egress'],
  ['failed', 'no_ipv6_egress'],
  ['partial', 'no_ipv6_egress'],
] as const)('keeps the exact IPv4 %s verdict when IPv6 is skipped for %s', (status, code) => {
  const v4 = { ...job, results: [{ ...result, status }] };
  const noDns = ipv6({ results: [{ ...result, status: 'skipped', code, region: null }] });
  const cell = cellOf(item('one', v4, noDns));
  expect(cell.verdict.tone).toBe({ available: 'ok', restricted: 'no', failed: 'unknown', partial: 'part' }[status]);
  expect(cell.region).toBe('JP');
  expect(cell.ipv6Region).toBeNull();
  expect(cell.title).toContain('采用 IPv4 结果');
});

it('host observations cannot hide an actual restricted IPv6 path result', () => {
 const entry=item('one',job,ipv6({results:[{...result,status:'restricted',code:'access_denied'}]}));
 entry.capabilities={family_mode:'split',ipv6_available:false,ipv6_reason:'链路出口无 IPv6 地址'};
 const summary=summarizeChainUnlock(entry);const cell=cellOf(entry);
 expect(cell.verdict.tone).toBe('part');expect(cell.title).toContain('IPv6：访问受限');
 expect(summary.detail).toContain('IPv6');
});

it('reports temporary family readiness separately from failed service detection',()=>{
 const entry=item('one',job,ipv6({results:[],path_error:'family_unavailable'}));
 expect(summarizeChainUnlock(entry).state).toBe('检测未就绪');
});

it('renders IPv4 availability as green for a chain without an IPv6 exit', async () => {
  const entry = item('one', job, ipv6({ results: [{ ...result, status: 'skipped', code: 'no_ipv6_egress' }] }));
  entry.capabilities.ipv6_available = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ chains: [entry], next_cursor: null })),
  );
  const view = show();
  await waitFor(() =>
    expect(view.container.querySelector('[data-service="gemini"]')?.getAttribute('data-tone')).toBe('ok'),
  );
});

it('missing actual IPv6 results remain visible regardless of host observations',()=>{
 for(const ipv6_available of [false,null,true]) {
  const entry=item('one',job,null);entry.capabilities.ipv6_available=ipv6_available;
  expect(cellOf(entry).verdict.tone).toBe('part');
  expect(summarizeChainUnlock(entry).state).toBe('部分未检测');
 }
});

it.each(['running', 'expired', 'client_unavailable'] as const)(
  'preserves and dims previous results for %s',
  async state => {
    const previous: NodeUnlockProbe = {
      ...job,
      results: [],
      previous_results: job.results,
      previous_completed_at: 2,
      state: state === 'client_unavailable' ? 'complete' : state,
      path_error: state === 'client_unavailable' ? state : null,
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ chains: [item('one', previous)], next_cursor: null })),
    );
    const view = show();
    await waitFor(() => expect(view.container.querySelector('[data-service="gemini"]')).toBeTruthy());
    const cell = view.container.querySelector('[data-service="gemini"]')!;
    expect(cell.hasAttribute('data-dim')).toBe(true);
    expect(cell.closest('tr')?.lastElementChild?.getAttribute('title')).toContain('的结果');
  },
);

it('a new timeout cannot be hidden behind an older IPv6 no-AAAA report', () => {
  const v6 = ipv6({
    state: 'expired',
    results: [],
    previous_results: [{ ...result, status: 'skipped', code: 'no_ipv6_dns' }],
  });
  expect(cellOf(item('one', job, v6)).verdict.tone).toBe('part');
});

it('keeps obsolete results bright and ignores previous failed comparisons', () => {
  expect(cellOf(item('one', { ...job, state: 'obsolete' })).dim).toBe(false);
  const cell = cellOf(item('one', { ...job, previous_results: [{ ...result, status: 'failed', code: 'timeout' }] }));
  expect(cell.change).toBeNull();
});

it('keeps automatic routing independent from both family slots', async () => {
  const automatic = {
    ...item(),
    capabilities: { family_mode: 'automatic' as const, ipv6_available: null, ipv6_reason: null },
    probes: { v4: null, v6: null, auto: { ...job, family: null, automatic: true } },
  };
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ chains: [automatic], next_cursor: null })),
  );
  const view = show();
  await view.findByText('不分族');
  expect(cellOf(automatic).verdict.tone).toBe('ok');
  fireEvent.click(view.getByRole('button', { name: /one/ }));
  expect(view.client.getQueryData(['chain-unlock-probe', 'app', 'one', 'auto'])).toEqual(automatic.probes.auto);
});

it('a single-stack original-path result stays green despite an old family prerequisite failure', () => {
  for (const ipv6_available of [false, true]) {
    const entry: ChainUnlockProbeItem = {
      ...item('one', { ...job, path_error: 'family_unavailable', results: [] }),
      capabilities: { family_mode: 'automatic', automatic_ready: true, ipv6_available, ipv6_reason: null },
      probes: {
        ...item().probes,
        v4: { ...job, path_error: 'family_unavailable', results: [] },
        auto: { ...job, family: null, automatic: true },
      },
    };
    const summary = summarizeChainUnlock(entry);
    expect(summary.automatic).toBe(true);
    expect(summary.state).toBeNull();
    expect(summary.tone).toBe('fresh');
    expect(cellOf(entry).verdict.tone).toBe('ok');
    expect(cellOf(entry).title).not.toContain('需更新配置');
  }
});

it('shows read failures explicitly and allows retry', async () => {
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce(Response.json({ error: '暂不可用' }, { status: 503 }))
    .mockImplementation(async () => Response.json({ chains: [item()], next_cursor: null }));
  vi.stubGlobal('fetch', fetcher);
  const view = show();
  await view.findByText('暂不可用');
  expect(view.queryByText('尚未检测')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: '重试' }));
  await waitFor(() => expect(view.container.querySelector('[data-service="gemini"]')).toBeTruthy());
});

it('rejects mismatched family slots and wrong chain reports', async () => {
  const wrongFamily = item('one', { ...job, family: 'v6' });
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ chains: [wrongFamily], next_cursor: null })),
  );
  await expect(fetchChainUnlockProbes()).rejects.toThrow('地址族');
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      Response.json({
        chains: [{ ...item(), probes: { ...item().probes, v6: { ...ipv6(), chain_id: 'other' } } }],
        next_cursor: null,
      }),
    ),
  );
  await expect(fetchChainUnlockProbes()).rejects.toThrow('线路不一致');
});

it('rejects non-advancing pagination and continues through three immediate pages', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ chains: [item()], next_cursor: 'one' })),
  );
  await expect(fetchChainUnlockProbes('one')).rejects.toThrow('分页没有前进');
  const fetcher = vi.fn(async (url: string) =>
    Response.json(
      url.includes('after=two')
        ? { chains: [item('three')], next_cursor: null }
        : url.includes('after=one')
          ? { chains: [item('two')], next_cursor: 'two' }
          : { chains: [item()], next_cursor: 'one' },
    ),
  );
  vi.stubGlobal('fetch', fetcher);
  show();
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(3));
});

it('also ignores no-AAAA for YouTube webpage when no diagnostic subpage could be fetched', () => {
  const v4 = { ...job, results: [youtubeSplitResult] };
  const v6 = ipv6({ results: [{ service: 'youtube_premium', status: 'skipped', code: 'no_ipv6_dns', region: null }] });
  const summary = summarizeChainUnlock(item('one', v4, v6));
  for (const service of ['youtube', 'youtube_premium']) {
    const cell = summary.cells.find(cell => cell.service === service)!;
    expect(cell.verdict.tone).toBe(service === 'youtube' ? 'ok' : 'info');
    expect(cell.title).toContain('无解析地址，采用 IPv4 结果');
  }
});

it.each(['CN', 'MO'])('keeps the %s Premium notice when the other family failed', region => {
  const notice = structuredClone(youtubeSplitResult);
  notice.region = region;
  notice.youtube!.premium.privacy_region = region;
  for (const code of ['network_error', 'timeout'] as const)
    for (const noticeFamily of ['v4', 'v6'] as const) {
      const failure: UnlockResult = { service: 'youtube_premium', status: 'failed', code, region: null };
      const entry = item(
        'one',
        { ...job, results: [noticeFamily === 'v4' ? notice : failure], previous_results: [] },
        ipv6({ results: [noticeFamily === 'v6' ? notice : failure] }),
      );
      const before = structuredClone(entry);
      const cell = summarizeChainUnlock(entry).cells.find(row => row.service === 'youtube_premium')!;
      expect(cell.verdict).toMatchObject({ tone: 'info', label: '提示 · 地区不支持' });
      expect(cell.region).toBe(region);
      expect(cell.title).toContain(`${noticeFamily === 'v4' ? 'IPv6' : 'IPv4'}：检测失败`);
      expect(cell.title).toContain(code === 'network_error' ? '网络连接失败' : '请求超时');
      expect(entry).toEqual(before);
    }
});

it('does not replace a usable or explicitly blocked other family with a Premium notice', () => {
  for (const [status, code, tone] of [
    ['available', 'web_available', 'part'],
    ['restricted', 'access_denied', 'no'],
  ] as const) {
    const entry = item('one', { ...job, results: [youtubeSplitResult] }, ipv6({
      results: [{ service: 'youtube_premium', status, code, region: status === 'available' ? 'JP' : null }],
    }));
    expect(summarizeChainUnlock(entry).cells.find(row => row.service === 'youtube_premium')!.verdict.tone).toBe(tone);
  }
});

it('renders CN Premium with the neutral notice icon and retains the IPv6 error in its explanation', async () => {
  const notice = structuredClone(youtubeSplitResult);
  notice.region = 'CN';
  notice.youtube!.premium.privacy_region = 'CN';
  const entry = item('one', { ...job, results: [notice] }, ipv6({
    results: [{ service: 'youtube_premium', status: 'failed', code: 'network_error', region: null }],
  }));
  vi.stubGlobal('fetch', vi.fn(async () => Response.json({ chains: [entry], next_cursor: null })));
  const view = show();
  await waitFor(() => expect(view.container.querySelector('[data-service="youtube_premium"]')?.getAttribute('data-tone')).toBe('info'));
  const cell = view.container.querySelector('[data-service="youtube_premium"]')!;
  expect(cell.textContent).toBe('CN');
  expect(cell.getAttribute('title')).toContain('IPv4：提示 · 地区不支持');
  expect(cell.getAttribute('title')).toContain('IPv6：检测失败');
  expect(cell.getAttribute('title')).toContain('网络连接失败');
});
