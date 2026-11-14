import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SubscriptionViewer } from '../src/panes/subscription';
import { ClashRulesEditor, rulesKey } from '../src/panes/clash-rules/editor';
import type { ClashRulesView } from '../src/api';
import { clashRulesTransport } from './fixtures/clash-rules-data';
import { subStoreTransport } from './fixtures/substore-data';
const originalScroll = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'scrollIntoView');
beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: vi.fn() });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  if (originalScroll) Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', originalScroll);
  else Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
});
function mount(mode: 'conflict' | 'readonly' | 'unavailable') {
  const rules = clashRulesTransport(mode !== 'readonly');
  const connector = subStoreTransport('empty', true);
  let advanced = false;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'https://fixture.invalid');
      if (url.pathname.endsWith('/clash-rules') && mode === 'unavailable')
        return Response.json({ error: '规则暂时无法读取' }, { status: 503 });
      if (url.pathname.endsWith('/clash-rules') && init?.method === 'PUT' && mode === 'conflict') {
        advanced = true;
        return Response.json({ error: '规则已在其他窗口更新，请重新载入后再保存' }, { status: 409 });
      }
      const response = rules(url, init);
      if (response) {
        if (advanced && (!init?.method || init.method === 'GET')) {
          const value = await response.json();
          value.version = 2;
          value.rules.custom = [
            { id: 'server-one', kind: 'domain', value: 'server.example.invalid', target: 'direct' },
          ];
          return Response.json(value);
        }
        return response;
      }
      const substore = await connector(url, init);
      if (substore) return substore;
      if (url.pathname.endsWith('/clash-subscription'))
        return Response.json({
          urls: {
            both: 'https://sub.example.invalid/clash.yaml',
            v4: 'https://sub.example.invalid/clash.yaml?family=v4',
            v6: 'https://sub.example.invalid/clash.yaml?family=v6',
          },
          template: 'Standard',
          haitun: null,
          remaining_bytes: 1024,
          reset_at: '2026-12-01T00:00:00Z',
        });
      throw new Error(`Unexpected fixture request: ${url.pathname}`);
    }),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SubscriptionViewer tenant="example" user="alice" kind="clash" onClose={() => undefined} />
    </QueryClientProvider>,
  );
}
it('keeps a conflicting draft and reloads only after explicit discard', async () => {
  const v = mount('conflict');
  fireEvent.click(await v.findByRole('button', { name: '规则' }));
  const input = await v.findByRole('textbox', { name: '添加域名、IP 段或进程' });
  fireEvent.change(input, { target: { value: 'draft.example.invalid' } });
  fireEvent.keyDown(input, { key: 'Enter' });
  fireEvent.click(v.getByRole('button', { name: '保存规则' }));
  await v.findByText('规则已在其他窗口更新，请重新载入后再保存');
  expect(v.getByText('draft.example.invalid', { exact: true })).toBeTruthy();
  fireEvent.click(v.getByRole('button', { name: '重新载入规则' }));
  await v.findByRole('dialog', { name: '放弃尚未保存的更改？' });
  fireEvent.click(v.getByRole('button', { name: '放弃并离开' }));
  await v.findByText('server.example.invalid', { exact: true });
  expect(v.queryByText('draft.example.invalid', { exact: true })).toBeNull();
  await waitFor(() => expect((v.getByRole('button', { name: '保存规则' }) as HTMLButtonElement).disabled).toBe(true));
});
it('keeps ordinary rules read-only for a read-only actor', async () => {
  const v = mount('readonly');
  fireEvent.click(await v.findByRole('button', { name: '规则' }));
  const input = await v.findByRole('textbox', { name: '添加域名、IP 段或进程' });
  expect(input.matches(':disabled')).toBe(true);
  expect((v.getByRole('button', { name: '保存规则' }) as HTMLButtonElement).disabled).toBe(true);
});
it('keeps the subscription available when the separate preferences endpoint fails', async () => {
  const v = mount('unavailable');
  fireEvent.click(await v.findByRole('button', { name: '规则' }));
  await v.findByText('规则暂时无法读取');
  expect(v.getByRole('img', { name: 'Clash 订阅地址二维码' })).toBeTruthy();
  expect(v.getByRole('button', { name: '重试' })).toBeTruthy();
});

async function mountCachedEditor(newer = false) {
  const transport = clashRulesTransport(true);
  const endpoint = new URL('https://fixture.invalid/users/example/alice/clash-rules');
  const cached: ClashRulesView = await transport(endpoint)!.json();
  const query = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  query.setQueryData(rulesKey('example', 'alice'), cached);
  const writes: { version: number; rules: ClashRulesView['rules'] }[] = [];
  const advance = (value: string) => {
    const write = {
      version: current.version,
      rules: { ...current.rules, custom: [{ id: value, kind: 'domain', value, target: 'direct' }] },
    };
    return transport(endpoint, { method: 'PUT', body: JSON.stringify(write) })!;
  };
  let current = cached;
  if (newer) current = await advance('newest.example.invalid').json();
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), 'https://fixture.invalid');
    if (init?.method === 'PUT') writes.push(JSON.parse(String(init.body)));
    const response = transport(url, init);
    if (!response) throw new Error(`Unexpected fixture request: ${url.pathname}`);
    return response;
  });
  const v = render(
    <QueryClientProvider client={query}>
      <ClashRulesEditor tenant="example" user="alice" onClose={() => undefined} onGuard={() => undefined} />
    </QueryClientProvider>,
  );
  await v.findByRole('button', { name: '保存规则' });
  return {
    v,
    query,
    writes,
    async refresh(value: string) {
      current = await advance(value).json();
      await act(() => query.refetchQueries({ queryKey: rulesKey('example', 'alice') }));
    },
  };
}
function addRule(v: ReturnType<typeof render>, value: string) {
  const input = v.getByRole('textbox', { name: '添加域名、IP 段或进程' });
  fireEvent.change(input, { target: { value } });
  fireEvent.keyDown(input, { key: 'Enter' });
}
it('adopts the latest fetched rules over a clean cache and saves with that version', async () => {
  const { v, writes } = await mountCachedEditor(true);
  await v.findByText('newest.example.invalid', { exact: true });
  expect((v.getByRole('button', { name: '保存规则' }) as HTMLButtonElement).disabled).toBe(true);
  addRule(v, 'local.example.invalid');
  fireEvent.click(v.getByRole('button', { name: '保存规则' }));
  await v.findByText('规则已保存，更新订阅后生效。', { exact: true });
  expect(writes).toHaveLength(1);
  expect(writes[0].version).toBe(1);
  expect(writes[0].rules.custom.map(rule => rule.value)).toEqual(['newest.example.invalid', 'local.example.invalid']);
});
it('preserves dirty rules on refetch and requires confirmation to reload the new version', async () => {
  const { v, refresh, writes } = await mountCachedEditor();
  addRule(v, 'draft.example.invalid');
  await refresh('remote.example.invalid');
  await v.findByText('规则已在其他窗口更新，当前更改已保留。重新载入会放弃当前更改。');
  expect(v.getByText('draft.example.invalid', { exact: true })).toBeTruthy();
  expect(v.queryByText('remote.example.invalid', { exact: true })).toBeNull();
  fireEvent.click(v.getByRole('button', { name: '重新载入规则' }));
  await v.findByRole('dialog', { name: '放弃尚未保存的更改？' });
  fireEvent.click(v.getByRole('button', { name: '取消' }));
  await waitFor(() => expect(document.querySelector('.confirm-card')).toBeNull());
  expect(v.getByText('draft.example.invalid', { exact: true })).toBeTruthy();
  fireEvent.click(v.getByRole('button', { name: '重新载入规则' }));
  fireEvent.click(await v.findByRole('button', { name: '放弃并离开' }));
  await v.findByText('remote.example.invalid', { exact: true });
  expect(v.queryByText('draft.example.invalid', { exact: true })).toBeNull();
  expect(v.queryByRole('button', { name: '重新载入规则' })).toBeNull();
  expect(writes).toHaveLength(0);
});
it('adopts a newer version after restoring a draft and clears stale probe hits', async () => {
  const { v, refresh } = await mountCachedEditor();
  const probe = v.getByRole('textbox', { name: '试一试：域名、IP 或进程名' });
  fireEvent.change(probe, { target: { value: 'example.invalid' } });
  fireEvent.click(v.getByRole('button', { name: '试一试' }));
  await waitFor(() => expect(v.container.querySelector('[data-hit]')).not.toBeNull());
  await refresh('remote.example.invalid');
  await v.findByText('remote.example.invalid', { exact: true });
  await waitFor(() => expect(v.container.querySelector('[data-hit]')).toBeNull());
  addRule(v, 'draft.example.invalid');
  await refresh('newest.example.invalid');
  fireEvent.click(v.getByRole('button', { name: '还原' }));
  await v.findByText('newest.example.invalid', { exact: true });
  expect(v.queryByText('draft.example.invalid', { exact: true })).toBeNull();
  expect(v.queryByText('remote.example.invalid', { exact: true })).toBeNull();
});
