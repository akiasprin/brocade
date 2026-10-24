import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ForwardingEntitlement, ForwardingProbe, ForwardingRule } from '../src/api';
import { ForwardingRuleProbe, forwardingProbeKey } from '../src/panes/forwarding-probe';

const rule = {
  id: 42,
  tenant_id: 't',
  user_id: 'alice',
  node_id: 'n1',
  target_host: 'example.com',
  target_port: 443,
  listen_port: 30000,
  protocol: 'tcp',
  enabled: true,
  runtime_status: 'active',
  updated_at: 'v1',
} as ForwardingRule;
const entry = {
  node_id: 'n1',
  node_name: '东京入口',
  enabled: true,
  quota_suspended: false,
  desired_generation: 2,
} as ForwardingEntitlement;
const result = {
  id: 'test',
  rule_id: 42,
  rule_updated_at: 'v1',
  generation: 2,
  state: 'complete',
  created_at: 1792368000,
  result: { status: 'reachable', code: 'connected', connect_ms: 12, dns_ms: 3, resolved_ip: '1.1.1.1' },
} satisfies ForwardingProbe;

function show(probe?: ForwardingProbe, manage = true, currentRule = rule, currentEntry = entry) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      <ForwardingRuleProbe rule={currentRule} entry={currentEntry} result={probe} canManage={manage} />
    </QueryClientProvider>,
  );
  return { ...view, client };
}
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('does not test on mount; one click sends one destination-free request', async () => {
  const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
    Response.json({ ...result, state: 'queued', result: null }),
  );
  vi.stubGlobal('fetch', fetch);
  const view = show();
  expect(fetch).not.toHaveBeenCalled();
  fireEvent.click(view.getByRole('button', { name: '测试延迟' }));
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  expect(fetch.mock.calls[0]?.[0]).toBe('/users/t/alice/forwarding/42/tcp-ping');
  const init = fetch.mock.calls[0]?.[1] as RequestInit | undefined;
  expect(init?.method).toBe('POST');
  expect(JSON.parse(String(init?.body))).toEqual({});
  await waitFor(() =>
    expect(view.client.getQueryData(forwardingProbeKey('t', 'alice'))).toEqual([
      { ...result, state: 'queued', result: null },
    ]),
  );
  expect(view.queryByRole('combobox')).toBeNull();
  expect(view.queryByRole('textbox')).toBeNull();
});

it('shows handshake time, DNS and actual IP separately without claiming application verification', () => {
  const view = show(result);
  expect(view.getByText(/TCP 连通/)).toBeTruthy();
  expect(view.getByText(/12 ms/)).toBeTruthy();
  expect(view.getByText('DNS 3 ms')).toBeTruthy();
  expect(view.getByText('1.1.1.1')).toBeTruthy();
  expect(view.getByText(/东京入口 → 目标端口/)).toBeTruthy();
  expect(view.queryByText(/完整转发路径|TLS|HTTP/)).toBeNull();
});

it('zero handshake latency is a real reading; absence is not zero', () => {
  const view = show({ ...result, result: { ...result.result, connect_ms: 0, dns_ms: null } });
  expect(view.getByText(/0 ms/)).toBeTruthy();
  expect(view.queryByText(/DNS/)).toBeNull();
});

it('running disables repeat clicks', () => {
  const view = show({ ...result, state: 'running', result: null });
  expect((view.getByRole('button', { name: '连接中…' }) as HTMLButtonElement).disabled).toBe(true);
});

it.each([
  ['timeout', '超时或远程地址非 TCP 监听'],
  ['connect-failed', 'TCP 建连失败，请检查远程 TCP 监听和网络连通性'],
] as const)('shows the %s failure reason without a fabricated latency and permits manual retry', (code, message) => {
  const view = show({
    ...result,
    result: { status: 'failed', code, connect_ms: null, dns_ms: null, resolved_ip: '1.1.1.1' },
  });
  expect(view.getByRole('status').textContent).toBe(message);
  expect(view.getByText(message).className).toContain('bad');
  expect(view.queryByText(/TCP 连通|\d+ ms/)).toBeNull();
  expect((view.getByRole('button', { name: '测试延迟' }) as HTMLButtonElement).disabled).toBe(false);
});

it('hides a stale result after edit or quota pause; read-only has no action', () => {
  const view = show(result, true, { ...rule, updated_at: 'v2' });
  expect(view.queryByText(/TCP 连通/)).toBeNull();
  view.unmount();
  const paused = show(result, true, rule, { ...entry, quota_suspended: true });
  expect(paused.queryByText(/TCP 连通/)).toBeNull();
  expect((paused.getByRole('button', { name: '测试延迟' }) as HTMLButtonElement).disabled).toBe(true);
  paused.unmount();
  expect(show(result, false).queryByRole('button')).toBeNull();
});

it('network failure stays local and allows retry rather than automatic target probing', async () => {
  const fetch = vi.fn(async () => new Response('unavailable', { status: 503 }));
  vi.stubGlobal('fetch', fetch);
  const view = show();
  fireEvent.click(view.getByRole('button', { name: '测试延迟' }));
  await waitFor(() => expect(view.getByText(/unavailable|503/)).toBeTruthy());
  expect(fetch).toHaveBeenCalledTimes(1);
  expect((view.getByRole('button', { name: '测试延迟' }) as HTMLButtonElement).disabled).toBe(false);
});

it('有效期暂停后隐藏旧读数并禁止发送新的拨测', () => {
  const view = show(result, true, { ...rule, runtime_status: 'service-paused' });
  expect(view.queryByText(/TCP 连通/)).toBeNull();
  expect((view.getByRole('button', { name: '测试延迟' }) as HTMLButtonElement).disabled).toBe(true);
});
