import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, createChain, createTenant, createUser, reorderApps, reorderChains, upsertApp } from '../src/api';
import { draft } from '../src/draft';

const writes: { path: string; body: unknown }[] = [];

describe('线路与链顺序保存', () => {
  beforeEach(() => {
    draft.init('app-order-test');
    draft.clear();
    writes.length = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string, init?: RequestInit) => {
        writes.push({ path, body: init?.body ? JSON.parse(String(init.body)) : null });
        return Response.json({ revision_id: writes.length });
      }),
    );
  });

  afterEach(() => vi.unstubAllGlobals());

  it('拖动线路按手势顺序即时写入完整顺序', async () => {
    await reorderApps(['line-b', 'line-a', 'line-c']);
    await reorderApps(['line-c', 'line-b', 'line-a']);

    expect(draft.ops()).toEqual([]);
    expect(writes).toEqual([
      { path: '/apps/order', body: { ids: ['line-b', 'line-a', 'line-c'] } },
      { path: '/apps/order', body: { ids: ['line-c', 'line-b', 'line-a'] } },
    ]);
  });

  it('同一草稿内修改刚创建的线路仍保留 create-only 语义', async () => {
    await createApp({ id: 'app-premium', label: '最初名称' });
    await upsertApp({ id: 'app-premium', label: '修改后的名称' });
    expect(draft.ops()).toEqual([{ op: 'create_app', app: { id: 'app-premium', label: '修改后的名称' } }]);
  });

  it('既有线路改名和创建用户即时写库，租户创建仍留在草稿', async () => {
    await upsertApp({ id: 'line-a', label: '新名称' });
    await createUser({ tenant_id: 'platform.acme', id: 'alice' });
    await createTenant({ id: 'tenant-next', name: '下一租户' });

    expect(writes).toEqual([
      { path: '/apps', body: { id: 'line-a', label: '新名称' } },
      { path: '/users', body: { tenant_id: 'platform.acme', id: 'alice' } },
    ]);
    expect(draft.ops()).toEqual([{ op: 'create_tenant', tenant: { id: 'tenant-next', name: '下一租户' } }]);
  });

  it('每条线路的链顺序各自即时写入', async () => {
    await reorderChains('line-a', ['chain-b', 'chain-a', 'chain-c']);
    await reorderChains('line-b', ['chain-z', 'chain-y']);
    await reorderChains('line-a', ['chain-c', 'chain-b', 'chain-a']);

    expect(draft.ops()).toEqual([]);
    expect(writes).toEqual([
      { path: '/apps/line-a/chains/order', body: { ids: ['chain-b', 'chain-a', 'chain-c'] } },
      { path: '/apps/line-b/chains/order', body: { ids: ['chain-z', 'chain-y'] } },
      { path: '/apps/line-a/chains/order', body: { ids: ['chain-c', 'chain-b', 'chain-a'] } },
    ]);
  });

  it('含未创建对象的顺序继续留在草稿中', async () => {
    await createApp({ id: 'line-new', label: '新线路' });
    await createChain('line-new', {
      id: 'chain-new',
      tenant_id: 'platform.acme',
      name: '新链',
    });
    await reorderApps(['line-a', 'line-new']);
    await reorderChains('line-new', ['chain-new']);

    expect(writes).toEqual([]);
    expect(draft.ops()).toEqual([
      { op: 'create_app', app: { id: 'line-new', label: '新线路' } },
      {
        op: 'create_chain',
        app_id: 'line-new',
        chain: { id: 'chain-new', tenant_id: 'platform.acme', name: '新链' },
      },
      { op: 'reorder_apps', ids: ['line-a', 'line-new'] },
      { op: 'reorder_chains', app_id: 'line-new', ids: ['chain-new'] },
    ]);
  });
});
