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

});
