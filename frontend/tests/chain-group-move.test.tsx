import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { moveChain, reorderChains, upsertChain, upsertGrant, type SnapshotApp } from '../src/api';
import { draft } from '../src/draft';
import { ChainGroupMove } from '../src/panes/chain-group-move';
import { ChainsPane } from '../src/panes/chains';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const chain = { id: 'chain-one', name: '香港 01', tenant: 'platform' };
const source: SnapshotApp = {
  id: 'app-one',
  label: 'APAC Standard',
  chains: [chain],
  ingresses: [],
  steps: [],
  grants: [],
  fronts: [],
};
const target: SnapshotApp = { ...source, id: 'app-two', label: '新分组', chains: [] };
beforeEach(() => {
  draft.init('chain-move-test');
  draft.clear();
  vi.stubGlobal(
    'fetch',
    vi.fn(() => {
      throw new Error('movement must remain in draft');
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function form() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <ChainGroupMove apps={[source, target]} app={source} chain={chain} />
    </QueryClientProvider>,
  );
}

it('换组及其后的排序、名称与授权在同一个草稿内回放', async () => {
  await moveChain(source.id, chain.id, target.id);
  await reorderChains(target.id, [chain.id]);
  await upsertChain(target.id, { id: chain.id, tenant_id: chain.tenant, name: '新名字', subscription_country: 'HK' });
  await upsertGrant({
    app_id: target.id,
    tenant_id: chain.tenant,
    user_id: 'alice',
    ingress_id: 'ingress-one',
    enabled: false,
  });
  expect(draft.ops().map(op => op.op)).toEqual(['move_chain', 'reorder_chains', 'upsert_chain', 'upsert_grant']);
  expect(fetch).not.toHaveBeenCalled();
});

it('反复跨组再返回原组保留完整顺序，不合并掉前置移动', async () => {
  for (const [from, to] of [
    ['app-one', 'app-two'],
    ['app-two', 'app-one'],
    ['app-one', 'app-two'],
  ])
    await moveChain(from, chain.id, to);
  expect(draft.ops()).toHaveLength(3);
  expect(new Set(draft.snapshot().map(entry => entry.key)).size).toBe(3);
});

it('预览未完成时重复同一次移动不会追加无法回放的操作', async () => {
  await moveChain(source.id, chain.id, target.id);
  await moveChain(source.id, chain.id, target.id);
  expect(draft.ops()).toHaveLength(1);
});

it('详情入口说明计费和发布边界，选择已有空分组后保存草稿', async () => {
  const view = form();
  fireEvent.click(view.getByRole('button', { name: '移动到分组' }));
  expect(view.getByText(/历史用量保留/)).toBeTruthy();
  const select = view.getByRole('combobox', { name: '目标分组' });
  expect((select as HTMLSelectElement).options).toHaveLength(3);
  expect(view.getByRole('button', { name: '保存到草稿' }).hasAttribute('disabled')).toBe(true);
  fireEvent.change(select, { target: { value: target.id } });
  fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() =>
    expect(draft.ops()).toEqual([
      { op: 'move_chain', app_id: source.id, chain_id: chain.id, target_app_id: target.id },
    ]),
  );
});

it('直接新建分组时先校验名称，按创建后移动的顺序保存', async () => {
  const view = form();
  fireEvent.click(view.getByRole('button', { name: '移动到分组' }));
  fireEvent.change(view.getByRole('combobox', { name: '目标分组' }), { target: { value: '__new_group__' } });
  expect(view.getByRole('button', { name: '保存到草稿' }).hasAttribute('disabled')).toBe(true);
  fireEvent.change(view.getByRole('textbox', { name: '新分组名称' }), { target: { value: ' 香港专线 ' } });
  fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() => expect(draft.ops()).toHaveLength(2));
  const create = draft.ops()[0];
  expect(create).toMatchObject({ op: 'create_app', app: { label: '香港专线' } });
  if (create.op !== 'create_app') throw new Error('expected create first');
  expect(draft.ops()[1]).toEqual({
    op: 'move_chain',
    app_id: source.id,
    chain_id: chain.id,
    target_app_id: create.app.id,
  });
});

it('换组后跨分组监听引用仍阻止单独删除被引用的链', () => {
  const otherChain = { ...chain, id: 'chain-two', name: '被引用的链' };
  const apps: SnapshotApp[] = [
    {
      ...source,
      steps: [
        {
          chain: chain.id,
          node: 'entry',
          accept: null,
          hop_in: null,
          rules: [
            {
              m: { t: 'any' },
              a: {
                t: 'reuse_listener',
                listener: { chain: otherChain.id, node: 'relay' },
                dial: { t: 'overlay' },
                pool: { t: 'none' },
              },
            },
          ],
        },
      ],
    },
    { ...target, chains: [otherChain] },
  ];
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['snapshot'], {
    snapshot: { revision: 1, apps, nodes: [], external_outbounds: [] },
    node_egress_dns: [],
    redacted: false,
  });
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['e2e-probes'], { chains: [] });
  const win = { id: 1, key: 'tab:chains', title: '线路', data: { drill: { p: 'list' } } } as unknown as Win;
  const view = render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'root',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 0, chain_group_count: [] },
        }}
      >
        <ChainsPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  expect(view.getByText('1 处监听引用')).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: /^多选$/ }));
  fireEvent.click(view.getByRole('checkbox', { name: '选中 被引用的链' }));
  expect(view.getByRole('button', { name: /^删除链 1$/ }).hasAttribute('disabled')).toBe(true);
});
