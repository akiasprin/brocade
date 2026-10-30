import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type {
  ChainUnlockProbeItem,
  NodeAgentStateItem,
  NodeUnlockProbe,
  UnlockDnsPath,
  UnlockDnsPartPath,
  UnlockResult,
} from '../src/api';
import { NodeUnlockPanel } from '../src/panes/node-unlock';
import { unlockReport } from '../src/panes/unlock-report';
import { summarizeChainUnlock } from '../src/panes/chain-unlock-summary';
import { DnsRuleMembers } from '../src/panes/unlock-dns';

const path: UnlockDnsPath = {
  exit: 'exit-one',
  normal_family: 'v4',
  fallback: 'machine',
  rules: [
    {
      selector: { t: 'geosite', v: ['category-ai-!cn'] },
      resolution: {
        address: '198.51.100.53',
        port: 53,
        transport: 'tcp',
        address_strategy: 'use_ip',
        fallback: 'machine',
      },
    },
  ],
};
const result: UnlockResult = { service: 'chatgpt', status: 'available', code: 'web_available', region: 'SG' };
const evidence = (path: UnlockDnsPath, kind: UnlockDnsPartPath['kind'] = 'fallback'): UnlockDnsPartPath[] => [
  { part: 'service', kind, paths: [path], exits: [path.exit] },
];
const job = (family: 'v4' | 'v6', reading: UnlockResult): NodeUnlockProbe => ({
  id: family,
  node_id: 'entry',
  family,
  state: 'complete',
  created_at: 1,
  completed_at: 2,
  results: [reading],
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('shows the actual racing member and keeps each member attempt count', () => {
  const view = render(
    <DnsRuleMembers
      rule={{
        ...path.rules[0],
        resolution: { ...path.rules[0].resolution, address: ['192.0.2.53', '2001:db8::53'] },
        selected_member: 2,
        members: [
          { index: 1, outcome: 'error', other: false, attempts: 5, elapsed_ms: 4000, rcode: 2, cache: 'miss' },
          { index: 2, outcome: 'answer', other: false, attempts: 2, elapsed_ms: 280, cache: 'miss' },
        ],
      }}
    />,
  );
  expect(view.container.textContent).toContain('192.0.2.53 · 请求失败 · 5 次 · 4000ms · RCODE 2');
  expect(view.container.textContent).toContain('采用 · 2001:db8::53 · 有地址 · 2 次 · 280ms');
});

it('uses measured fallback evidence for UseIP and keeps old reports unclassified', () => {
  expect(unlockReport(job('v6', result)).rows[0].route).toBe('normal');
  expect(unlockReport(job('v6', { ...result, dns_path: path })).rows[0].route).toBe('normal');
  expect(unlockReport(job('v6', { ...result, dns_parts: evidence(path) })).rows[0]).toMatchObject({
    route: 'fallback',
    region: 'SG',
  });
  expect(unlockReport(job('v4', { ...result, dns_parts: evidence(path) })).rows[0].route).toBe('normal');
});

it('keeps the reading but excludes fallback from both available count and denominator', async () => {
  const report = job('v6', { ...result, dns_parts: evidence(path) });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      Response.json(
        url.endsWith('/capabilities')
          ? { family_mode: 'split', ipv6_available: null, ipv6_reason: null }
          : url.includes('family=v6')
            ? report
            : job('v4', result),
      ),
    ),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      <NodeUnlockPanel
        node={{ node_id: 'entry', agent_protocol_version: 57, lifecycle_phase: 'active' } as NodeAgentStateItem}
        editable={false}
      />
    </QueryClientProvider>,
  );
  fireEvent.click(await view.findByRole('button', { name: 'IPv6' }));
  await view.findByLabelText('可用 0 / 12');
  const tile = view.getByRole('article', { name: 'ChatGPT' });
  expect(tile.getAttribute('data-route')).toBe('fallback');
  expect(within(tile).getByText('SG')).toBeTruthy();
  expect(within(tile).getByText('可用')).toBeTruthy();
  fireEvent.click(within(tile).getByRole('button', { name: 'ChatGPT · 回退结果的解析路径' }));
  const detail = await view.findByRole('region', { name: 'ChatGPT · 回退结果的解析路径' });
  expect(detail.textContent).toContain('198.51.100.53:53');
  expect(detail.textContent).toContain('UseIP');
  expect(detail.textContent).toContain('本次实际连接的 DNS 解析与拨号');
});

it('explains stop using the actual DNS strategy without inventing a policy restriction', () => {
  const skipped: UnlockResult = {
    ...result,
    status: 'skipped',
    code: 'no_ipv6_dns',
    region: null,
    dns_parts: evidence({ ...path, fallback: 'stop' }),
  };
  expect(unlockReport(job('v6', skipped)).rows[0].verdict.reason).toBe(
    '自定义 DNS 本次仅返回 IPv4，失败处理为停止连接',
  );
  skipped.dns_parts = evidence({
    ...path,
    fallback: 'stop',
    rules: [
      { ...path.rules[0], resolution: { ...path.rules[0].resolution, address_strategy: 'use_ipv4', fallback: 'stop' } },
    ],
  });
  expect(unlockReport(job('v6', skipped)).rows[0].verdict.reason).toBe('DNS 策略只解析 IPv4');
});

it('keeps the ordinary family and removes fallback restrictions from the list verdict', () => {
  const item: ChainUnlockProbeItem = {
    app_id: 'app',
    chain_id: 'chain',
    capabilities: { family_mode: 'split', ipv6_available: null, ipv6_reason: null },
    probes: {
      auto: null,
      v4: job('v4', result),
      v6: job('v6', {
        ...result,
        status: 'unsupported_region',
        code: 'region_unsupported',
        region: 'MO',
        dns_parts: evidence(path),
      }),
    },
  };
  const cell = summarizeChainUnlock(item).cells[0];
  expect(cell).toMatchObject({
    onlyFamily: 'v4',
    region: 'SG',
    ipv6Region: null,
    verdict: { tone: 'ok', label: '可用' },
  });
  expect(cell.title).toContain('IPv6：地区不支持');
  expect(cell.title).toContain('实际连接经 DNS 回退');
  item.probes.v4 = job('v4', { ...result, status: 'restricted', code: 'access_denied' });
  item.probes.v6 = job('v6', { ...result, status: 'restricted', code: 'access_denied' });
  expect(summarizeChainUnlock(item).cells[0].verdict).toMatchObject({ tone: 'no', label: '访问受限' });
});

it('keeps partial and unknown evidence in availability counts and separates YouTube subitems', () => {
  for (const kind of ['partial', 'unknown'] as const)
    expect(unlockReport(job('v6', { ...result, dns_parts: evidence(path, kind) })).rows[0].route).toBe('normal');
  const v6Path = { ...path, normal_family: 'v6' as const };
  const youtube: UnlockResult = {
    service: 'youtube_premium',
    status: 'available',
    code: 'web_available',
    region: 'SG',
    youtube: {
      webpage: {
        status: 'available',
        code: 'web_available',
        http_status: 200,
        page_region: 'SG',
        topbar_region: 'SG',
        privacy_region: 'SG',
      },
      premium: {
        status: 'available',
        code: 'web_available',
        http_status: 200,
        page_region: 'TR',
        topbar_region: 'TR',
        privacy_region: 'TR',
      },
      cdn: { node: 'hkg01s01', airport: 'HKG', country: 'HK', error: null, http_status: 200, elapsed_ms: 10 },
    },
    dns_parts: [
      { part: 'webpage', kind: 'fallback', paths: [v6Path], exits: ['exit-one'] },
      { part: 'premium', kind: 'direct', paths: [], exits: ['exit-two'] },
      { part: 'cdn', kind: 'partial', paths: [v6Path], exits: ['exit-one', 'exit-two'] },
    ],
  };
  const rows = unlockReport(job('v4', youtube)).rows;
  expect(rows.find(row => row.service === 'youtube')).toMatchObject({ route: 'fallback', region: 'SG' });
  expect(rows.find(row => row.service === 'youtube_premium')).toMatchObject({ route: 'normal', region: 'TR' });
});
