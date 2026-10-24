import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { DataRetentionView } from '../src/api';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { DataRetentionSection } = await import('../src/panes/settings');

const view: DataRetentionView = {
  policies: [
    {
      table_name: 'node_ping_probe_samples',
      title: 'Ping 探测样本',
      description: '机器到探测目标的逐次延迟。',
      category: 'history',
      ttl_hours: 168,
      default_ttl_hours: 168,
      min_ttl_hours: 1,
      max_ttl_hours: 87600,
      customized: false,
      estimated_rows: 17_117_236,
      total_bytes: 1_671_168_000,
      estimated_dead_rows: 200,
      last_vacuum_unix_secs: null,
      cleanup: null,
    },
    {
      table_name: 'usage_report_receipts',
      title: '用量上报回执',
      description: '重复上报的短期幂等响应。',
      category: 'temporary',
      ttl_hours: 168,
      default_ttl_hours: 168,
      min_ttl_hours: 1,
      max_ttl_hours: 87600,
      customized: false,
      estimated_rows: 12,
      total_bytes: 65_536,
      estimated_dead_rows: 0,
      last_vacuum_unix_secs: null,
      cleanup: null,
    },
  ],
};

function Harness() {
  const query = useQuery({ queryKey: ['data-retention'], queryFn: async () => view, initialData: view });
  return <DataRetentionSection editable data={query.data} />;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it('groups only catalogued bounded tables and saves a shorter TTL after confirmation', async () => {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  const confirm = vi.fn(() => true);
  vi.stubGlobal('confirm', confirm);
  const fetch = vi.fn(async (_path: string, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body));
    const next = {
      policies: view.policies.map(policy =>
        policy.table_name === 'node_ping_probe_samples'
          ? { ...policy, ttl_hours: body.ttl_hours, customized: true }
          : policy,
      ),
    };
    return Response.json(next);
  });
  vi.stubGlobal('fetch', fetch);

  render(
    <QueryClientProvider client={client}>
      <Harness />
    </QueryClientProvider>,
  );

  const details = document.getElementById('set-data-retention') as HTMLDetailsElement;
  expect(details.open).toBe(false);
  fireEvent.click(details.querySelector('summary')!);
  expect(details.open).toBe(true);
  expect(screen.getByText('历史数据 · 1 张表')).toBeDefined();
  expect(screen.getByText('临时数据 · 1 张表')).toBeDefined();
  expect(screen.getByText('node_ping_probe_samples')).toBeDefined();
  expect(screen.queryByText('users')).toBeNull();

  const row = screen.getByText('node_ping_probe_samples').closest<HTMLElement>('[role="listitem"]')!;
  const ttl = within(row).getByLabelText('Ping 探测样本 TTL');
  expect((ttl as HTMLInputElement).value).toBe('7');
  fireEvent.change(ttl, { target: { value: '3' } });
  fireEvent.click(details.querySelector('summary')!);
  expect(details.open).toBe(false);
  fireEvent.click(details.querySelector('summary')!);
  expect((ttl as HTMLInputElement).value).toBe('3');
  fireEvent.click(within(row).getByRole('button', { name: '保存' }));

  expect(confirm).toHaveBeenCalledWith(expect.stringContaining('永久删除'));
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  expect(fetch.mock.calls[0][0]).toBe('/data-retention/node_ping_probe_samples');
  expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toEqual({ ttl_hours: 72 });
  await waitFor(() => expect((screen.getByLabelText('Ping 探测样本 TTL') as HTMLInputElement).value).toBe('3'));
});

it('does not send an invalid TTL', () => {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  const fetch = vi.fn();
  vi.stubGlobal('fetch', fetch);
  render(
    <QueryClientProvider client={client}>
      <Harness />
    </QueryClientProvider>,
  );
  fireEvent.click(document.querySelector('#set-data-retention > summary')!);
  const ttl = screen.getByLabelText('Ping 探测样本 TTL');
  fireEvent.change(ttl, { target: { value: '0' } });
  expect(screen.getByText(/TTL 需为/)).toBeDefined();
  const row = screen.getByText('node_ping_probe_samples').closest<HTMLElement>('[role="listitem"]')!;
  expect((within(row).getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
  expect(fetch).not.toHaveBeenCalled();
});

function cleanupView(
  overrides: Partial<NonNullable<DataRetentionView['policies'][number]['cleanup']>> = {},
): DataRetentionView {
  const now = Math.floor(Date.now() / 1000);
  return {
    policies: [
      {
        ...view.policies[0],
        last_vacuum_unix_secs: now - 30,
        cleanup: {
          ttl_hours: 168,
          finished_at_unix_secs: now - 10,
          last_success_unix_secs: now - 10,
          deleted_rows: 42,
          error_code: null,
          has_backlog: false,
          next_run_unix_secs: now + 3600,
          ...overrides,
        },
      },
    ],
  };
}

it('shows successful cleanup, measured backlog and database vacuum estimates', () => {
  const client = new QueryClient();
  const mounted = render(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable={false} data={cleanupView()} />
    </QueryClientProvider>,
  );
  expect(screen.getByText('上轮无过期积压')).toBeDefined();
  expect(screen.getByText(/清理 42 行/)).toBeDefined();
  expect(screen.getByText(/待回收约 200 行/)).toBeDefined();
  expect(screen.getByText(/最近空间回收/)).toBeDefined();
  expect((screen.getByLabelText('Ping 探测样本 TTL') as HTMLInputElement).disabled).toBe(true);
  mounted.rerender(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable={false} data={cleanupView({ has_backlog: true })} />
    </QueryClientProvider>,
  );
  expect(screen.getByText('存在过期积压，将继续分批清理')).toBeDefined();
  expect(screen.queryByText('上轮无过期积压')).toBeNull();
});

it('distinguishes failed, unknown and outdated observations without resetting unsaved TTL input', () => {
  const client = new QueryClient();
  const mounted = render(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable data={cleanupView({ has_backlog: null })} />
    </QueryClientProvider>,
  );
  const details = document.getElementById('set-data-retention') as HTMLDetailsElement;
  fireEvent.click(details.querySelector('summary')!);
  const ttl = screen.getByLabelText('Ping 探测样本 TTL') as HTMLInputElement;
  fireEvent.change(ttl, { target: { value: '3' } });
  expect(screen.getByText('过期积压尚未确认，将重新检查')).toBeDefined();
  mounted.rerender(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable data={cleanupView({ error_code: 'statement_timeout', has_backlog: null })} />
    </QueryClientProvider>,
  );
  expect(details.open).toBe(true);
  expect(ttl.value).toBe('3');
  expect(screen.getByText('清理超时')).toBeDefined();
  expect(screen.getByText(/清理 42 行/)).toBeDefined();
  expect(screen.queryByText('上轮无过期积压')).toBeNull();
  mounted.rerender(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable data={cleanupView({ ttl_hours: 24, has_backlog: false })} />
    </QueryClientProvider>,
  );
  expect(screen.getByText('保留期已更新，等待重新检查')).toBeDefined();
  expect(screen.queryByText('上轮无过期积压')).toBeNull();
});

it('shows overdue cleanup and first-run state explicitly', () => {
  const client = new QueryClient();
  const mounted = render(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable={false} data={cleanupView({ next_run_unix_secs: 1 })} />
    </QueryClientProvider>,
  );
  expect(screen.getByText('清理延迟')).toBeDefined();
  mounted.rerender(
    <QueryClientProvider client={client}>
      <DataRetentionSection editable={false} data={{ policies: [view.policies[0]] }} />
    </QueryClientProvider>,
  );
  expect(screen.getByText('等待首次清理')).toBeDefined();
  expect(screen.getByText('暂无成功清理记录')).toBeDefined();
});
