import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SubscriptionViewer } from '../src/panes/subscription';
import { connectorFixture, subStoreTransport } from './fixtures/substore-data';
import { checkpoints, syncStatus } from '../src/panes/subscription-substore-state';

beforeEach(() =>
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} })),
);
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
function mount(scene = 'ready', writable = true, failSave = false, clashFails = false) {
  const transport = subStoreTransport(scene, writable);
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), 'https://console.example.test');
    if (url.pathname.includes('/subscription-connectors/')) {
      if (failSave && init?.method === 'PUT') return Response.json({ error: 'network failure' }, { status: 503 });
      return (await transport(url, init))!;
    }
    if (clashFails) return Response.json({ error: 'subscription unavailable' }, { status: 404 });
    const u = 'https://console.example.test/sub/v1/00000000-0000-4000-8000-000000000001/clash.yaml';
    return Response.json({
      url: u,
      urls: { both: u, v4: u, v6: u },
      template: 'Standard',
      haitun: null,
      remaining_bytes: 1024,
      reset_at: '2026-12-01T00:00:00Z',
    });
  });
  vi.stubGlobal('fetch', fetch);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const onClose = vi.fn();
  const v = render(
    <QueryClientProvider client={client}>
      <SubscriptionViewer tenant="platform.acme" user="alice" kind="clash" onClose={onClose} />
    </QueryClientProvider>,
  );
  return { ...v, fetch, client, onClose };
}
async function ticket(v: ReturnType<typeof mount>) {
  fireEvent.click(await v.findByRole('button', { name: /切换到 Sub-Store 后端同步/ }));
  await v.findByRole('region', { name: 'Sub-Store 后端同步' });
}

describe('durable connector UI', () => {
  it('maps checkpoint evidence without treating conversion as a client receipt', () => {
    expect(checkpoints(connectorFixture('ready'))).toEqual(['ok', 'ok', 'ok']);
    expect(checkpoints(connectorFixture('partial'))).toEqual(['ok', 'bad', 'track']);
    expect(checkpoints(connectorFixture('running'))).toEqual(['ok', 'live', 'track']);
    expect(checkpoints(connectorFixture('empty'))).toEqual(['empty', 'empty', 'empty']);
    expect(checkpoints(connectorFixture('superseded'))).toEqual(['ok', 'ok', 'track']);
    expect(syncStatus(connectorFixture('superseded'))).toMatchObject({ label: '等待核验', tone: 'muted' });
    expect(syncStatus(connectorFixture('inactive')).label).toBe('已暂停');
    const changed = connectorFixture('failed');
    changed.config!.version++;
    changed.config!.registered = false;
    expect(syncStatus(changed).label).toBe('未同步');
  });
  it('pushes settings inside the original dialog without exposing saved private paths', async () => {
    const v = mount();
    await ticket(v);
    expect(v.queryByText('G58')).toBeNull();
    expect(v.queryByText('客户端更新')).toBeNull();
    fireEvent.click(v.getByRole('button', { name: '设置' }));
    await v.findByText('Sub-Store 设置');
    expect(v.getAllByRole('dialog')).toHaveLength(1);
    expect(v.getByLabelText('后端地址').getAttribute('placeholder')).toContain('已保存');
    expect((v.getByLabelText('后端地址') as HTMLInputElement).value).toBe('');
  });
  it('keeps connector history and cleanup reachable when an inactive user has no Clash subscription', async () => {
    const v = mount('inactive', true, false, true);
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: '设置' }));
    await v.findByRole('button', { name: '清理远端来源…' });
    expect(v.getByRole('button', { name: '同步记录' })).toBeTruthy();
  });
  it('preserves unsaved secret inputs on failure and guards closing', async () => {
    const v = mount('ready', true, true);
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: '设置' }));
    fireEvent.change(v.getByLabelText('后端地址'), { target: { value: 'https://new.example.test/private-path' } });
    fireEvent.click(v.getByRole('button', { name: '保存' }));
    await v.findByText('network failure');
    expect((v.getByLabelText('后端地址') as HTMLInputElement).value).toBe('https://new.example.test/private-path');
    fireEvent.click(v.getAllByRole('button', { name: '关闭' }).find(b => b.classList.contains('sub-dialog-close'))!);
    await v.findByText('放弃未保存的更改？');
    expect(v.onClose).not.toHaveBeenCalled();
  });
  it('validates first connection then saves and queues the server task', async () => {
    const v = mount('empty');
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: '连接 Sub-Store' }));
    fireEvent.change(v.getByLabelText('后端地址'), { target: { value: 'file:///etc/passwd' } });
    fireEvent.click(v.getByRole('button', { name: '连接并同步' }));
    await v.findByText('请输入完整的 http(s) 后端地址');
    fireEvent.change(v.getByLabelText('后端地址'), { target: { value: 'http://127.0.0.1:3001/private' } });
    fireEvent.click(v.getByRole('button', { name: '连接并同步' }));
    await v.findByRole('region', { name: 'Sub-Store 后端同步' });
    expect(v.fetch.mock.calls.some(([u, i]) => String(u).endsWith('/sub-store') && i?.method === 'PUT')).toBe(true);
    expect(v.fetch.mock.calls.some(([u, i]) => String(u).endsWith('/sub-store/sync') && i?.method === 'POST')).toBe(
      true,
    );
  });
  it('opens persistent task details and retries with the parent identity', async () => {
    const v = mount('partial');
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: /失败：/ }));
    fireEvent.click(await v.findByRole('button', { name: /登记同步，失败：Sub-Store 拉取返回错误/ }));
    expect(await v.findByText('原因：Sub-Store 拉取返回错误')).toBeTruthy();
    expect(v.queryByText('部分完成')).toBeNull();
    fireEvent.click(await v.findByRole('button', { name: '重试同步' }));
    await waitFor(() =>
      expect(v.fetch.mock.calls.some(([u, i]) => String(u).endsWith('/runs/1049/retry') && i?.method === 'POST')).toBe(
        true,
      ),
    );
    expect(v.getAllByRole('dialog')).toHaveLength(1);
  });
  it('shows the latest automatic success first and puts failures in the second tab', async () => {
    const v = mount('history');
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: /订阅数据核验一致：/ }));
    await v.findByRole('button', { name: /周期核验，订阅数据核验一致/ });
    expect(v.getByRole('button', { name: '最近结果' }).getAttribute('aria-pressed')).toBe('true');
    expect(v.getByRole('button', { name: /周期核验，已忽略/ })).toBeTruthy();
    expect(v.getByRole('button', { name: /周期核验，失败：连接 Sub-Store 超时/ })).toBeTruthy();
    expect(v.queryByText('无变化核验')).toBeNull();
    expect(v.queryByText('部分完成')).toBeNull();
    fireEvent.click(v.getByRole('button', { name: '异常' }));
    expect(await v.findByRole('button', { name: /周期核验，失败：连接 Sub-Store 超时/ })).toBeTruthy();
    expect(v.queryByRole('button', { name: /周期核验，已忽略/ })).toBeNull();
    expect(v.queryByRole('button', { name: /周期核验，订阅数据核验一致/ })).toBeNull();
    expect(v.queryByRole('button', { name: /拉取验证，订阅数据核验一致/ })).toBeNull();
    fireEvent.click(v.getByRole('button', { name: '忽略' }));
    expect(await v.findByRole('button', { name: /周期核验，已忽略/ })).toBeTruthy();
    expect(v.queryByRole('button', { name: /周期核验，失败：/ })).toBeNull();
  });
  it('shows a clear empty state when there are no warnings or errors', async () => {
    const v = mount('ready');
    await ticket(v);
    fireEvent.click(v.getByRole('button', { name: /同步成功：/ }));
    await v.findByRole('button', { name: /登记同步，同步成功/ });
    fireEvent.click(v.getByRole('button', { name: '异常' }));
    await v.findByText('暂无异常');
    fireEvent.click(v.getByRole('button', { name: '忽略' }));
    await v.findByText('暂无忽略记录');
  });
  it.each(['readonly', 'inactive'])('disables writes for %s and keeps observations visible', async scene => {
    const v = mount(scene);
    await ticket(v);
    expect((v.getByRole('button', { name: '立即同步' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(v.getByRole('button', { name: '设置' }));
    await v.findByText('Sub-Store 设置');
    if (scene === 'readonly') expect(v.queryByRole('button', { name: '保存' })).toBeNull();
  });
});
