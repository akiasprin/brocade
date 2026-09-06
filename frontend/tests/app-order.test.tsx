import { beforeEach, describe, expect, it } from 'vitest';
import { createApp, reorderApps, reorderChains, upsertApp } from '../src/api';
import { draft } from '../src/draft';

describe('线路与链顺序草稿', () => {
  beforeEach(() => {
    draft.init('app-order-test');
    draft.clear();
  });

  it('拖动线路只保留最终完整顺序', async () => {
    await reorderApps(['line-b', 'line-a', 'line-c']);
    await reorderApps(['line-c', 'line-b', 'line-a']);

    expect(draft.ops()).toEqual([{ op: 'reorder_apps', ids: ['line-c', 'line-b', 'line-a'] }]);
  });

  it('同一草稿内修改刚创建的线路仍保留 create-only 语义', async () => {
    await createApp({ id: 'app-premium', label: '最初名称' });
    await upsertApp({ id: 'app-premium', label: '修改后的名称' });
    expect(draft.ops()).toEqual([{ op: 'create_app', app: { id: 'app-premium', label: '修改后的名称' } }]);
  });

  it('每条线路各自只保留一份最终链顺序', async () => {
    await reorderChains('line-a', ['chain-b', 'chain-a', 'chain-c']);
    await reorderChains('line-b', ['chain-z', 'chain-y']);
    await reorderChains('line-a', ['chain-c', 'chain-b', 'chain-a']);

    expect(draft.ops()).toEqual([
      { op: 'reorder_chains', app_id: 'line-b', ids: ['chain-z', 'chain-y'] },
      { op: 'reorder_chains', app_id: 'line-a', ids: ['chain-c', 'chain-b', 'chain-a'] },
    ]);
  });

  it('升级时只移除旧按钮留下的 swap 草稿', () => {
    localStorage.setItem(
      'brocade-console:draft:v2:legacy-order-test',
      JSON.stringify([
        {
          key: 'app-swap:line-a:line-b:1',
          label: '旧线路交换',
          op: { op: 'swap_apps', left_id: 'line-a', right_id: 'line-b' },
        },
        {
          key: 'chain-swap:line-a:chain-a:chain-b:2',
          label: '旧链交换',
          op: {
            op: 'swap_chains',
            app_id: 'line-a',
            left_id: 'chain-a',
            right_id: 'chain-b',
          },
        },
        {
          key: 'app:line-c',
          label: '线路 line-c',
          op: { op: 'upsert_app', app: { id: 'line-c', label: 'Line C' } },
        },
      ]),
    );

    draft.init('legacy-order-test');
    expect(draft.ops()).toEqual([{ op: 'upsert_app', app: { id: 'line-c', label: 'Line C' } }]);
  });

  it('模型 ID 上线后不回放仍引用旧 ID 的 v1 草稿', () => {
    localStorage.setItem(
      'brocade-console:draft:v1:pre-model-id-test',
      JSON.stringify([
        {
          key: 'chain:app-1/c-1',
          label: '链 c-1',
          op: {
            op: 'upsert_chain',
            app_id: 'app-1',
            chain: { id: 'c-1', tenant_id: 'platform.acme', name: '旧链' },
          },
        },
      ]),
    );

    draft.init('pre-model-id-test');
    expect(draft.ops()).toEqual([]);
  });
});
