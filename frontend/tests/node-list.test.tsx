import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, Whoami } from '../src/api';
import type { NodesPane as NodesPaneComponent } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';
import { SCENES, agentState } from './fixtures/node-list-mockup-data';

let NodesPane: typeof NodesPaneComponent;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ NodesPane } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const admin: Whoami = {
  operator_id: 'operator',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};

const win: Win = {
  id: 1,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1000,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'list' } },
};

const fleet = (machines = SCENES.typical.machines()) =>
  machines.map(machine => agentState(machine, Math.floor(Date.now() / 1000)));

/** 典型场景：24 台在线与 3 台退役（洛杉矶 旧 GIA、首尔 旧机已停用；香港 旧 BGP 仍在退役中）。 */
function mount(nodes: NodeAgentStateItem[], who: Whoami = admin, bare = true) {
  let current = nodes;
  let finishRemoval: (() => void) | null = null;
  const fetcher = vi.fn((path: string, init?: RequestInit) => {
    if (path === '/nodes/agent-state') return Promise.resolve(Response.json({ nodes: current }));
    if (path === '/nodes' && init?.method === 'DELETE') {
      const { node_ids } = JSON.parse(String(init.body)) as { node_ids: string[] };
      return new Promise<Response>(resolve => {
        finishRemoval = () => {
          current = current.filter(node => !node_ids.includes(node.node_id));
          resolve(Response.json({ revision_id: 9, removed_nodes: node_ids, removed_chains: [] }));
        };
      });
    }
    return Promise.resolve(Response.json({ nodes: [] }));
  });
  vi.stubGlobal('fetch', fetcher);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['nodes'], { nodes });
  render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: nodes.length, chain_group_count: [] } }}>
        <NodesPane win={win} bare={bare} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return { fetcher, finishRemoval: () => finishRemoval?.() };
}

const cardNames = () => [...document.querySelectorAll('.ncard .nc-head > b')].map(element => element.textContent ?? '');
const button = (name: string | RegExp) => screen.getByRole('button', { name }) as HTMLButtonElement;
const checkbox = (name: string) => screen.getByRole('checkbox', { name }) as HTMLInputElement;

describe('machine list title bar', () => {
  it('uses the list title bar shared with the other list pages', () => {
    mount(fleet());

    const panel = document.querySelector<HTMLElement>('.cardpage.node-cardpage > .panel.titled.node-list-panel');
    expect(panel?.dataset.pageTitle).toBe('true');
    const header = panel!.querySelector<HTMLElement>(':scope > header')!;
    expect(within(header).getByRole('heading', { level: 4, name: '机器' })).toBeTruthy();
    expect(
      within(header)
        .getAllByRole('button')
        .map(item => item.textContent),
    ).toEqual(['多选', '＋ 纳管机器']);
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.queryByRole('button', { name: '更多操作' })).toBeNull();
  });

  it('keeps retired machines at the end of the same grid instead of a second card', () => {
    mount(fleet());

    expect(document.querySelectorAll('.node-cardpage > .panel')).toHaveLength(1);
    expect(document.querySelectorAll('.ncards')).toHaveLength(1);
    const names = cardNames();
    expect(names).toHaveLength(27);
    expect(names.slice(-3)).toEqual(['洛杉矶 旧 GIA', '首尔 旧机', '香港 旧 BGP']);
    expect([...document.querySelectorAll('.ncard.off .nc-head > b')].map(name => name.textContent)).toEqual(
      names.slice(-3),
    );
  });

  it('picks live machines for retirement and stopped retired ones for removal in one selection', async () => {
    const page = mount(fleet());
    fireEvent.click(button('多选'));

    expect(screen.queryByRole('button', { name: '＋ 纳管机器' })).toBeNull();
    expect(button('退役下线').disabled).toBe(true);
    expect(button('移除选中').disabled).toBe(true);
    // 仍在退役中的机器还没有停用，不能勾选移除。
    expect(checkbox('选中 香港 旧 BGP').disabled).toBe(true);

    fireEvent.click(checkbox('选中 东京 IIJ'));
    fireEvent.click(checkbox('选中 香港 CMI'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    // 两类操作各数各的：退役只算在线机器，移除只算已停用的退役机器。
    expect(button('退役下线 2').disabled).toBe(false);
    expect(button('移除选中 1').disabled).toBe(false);

    fireEvent.click(button('退役下线 2'));
    await waitFor(() => expect(screen.queryAllByRole('checkbox')).toHaveLength(0));
    const retired = page.fetcher.mock.calls
      .filter(([path, init]) => init?.method === 'PUT' && path.endsWith('/status'))
      .map(([path, init]) => [path, JSON.parse(String(init?.body))]);
    expect(retired).toEqual([
      ['/nodes/hk-cmi/status', { status: 'retired' }],
      ['/nodes/jp-iij/status', { status: 'retired' }],
    ]);
    expect(page.fetcher.mock.calls.some(([path, init]) => path === '/nodes' && init?.method === 'DELETE')).toBe(false);
    expect(button('多选')).toBeTruthy();
  });

  it('removes the picked retired machines after confirming and keeps them checked while removing', async () => {
    const page = mount(fleet());
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    fireEvent.click(button('多选'));
    fireEvent.click(checkbox('选中 洛杉矶 旧 GIA'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    fireEvent.click(button('移除选中 2'));

    expect(confirm.mock.calls[0][0]).toContain('确定永久移除 2 台退役机器吗？');
    expect(confirm.mock.calls[0][0]).toContain('首尔 旧机、洛杉矶 旧 GIA');
    await waitFor(() => expect(button('移除中…').disabled).toBe(true));
    const removal = page.fetcher.mock.calls.find(([path, init]) => path === '/nodes' && init?.method === 'DELETE');
    expect(JSON.parse(String(removal?.[1]?.body))).toEqual({ node_ids: ['kr-old', 'us-old'] });
    expect(button('取消').disabled).toBe(true);
    expect(checkbox('选中 首尔 旧机').checked).toBe(true);
    expect(checkbox('选中 洛杉矶 旧 GIA').checked).toBe(true);

    await act(async () => page.finishRemoval());
    await waitFor(() => expect(screen.getByText('已移除 2 台机器，并清理 0 条关联线路。')).toBeTruthy());
    await waitFor(() => expect(cardNames()).toHaveLength(25));
    expect(cardNames().at(-1)).toBe('香港 旧 BGP');
    expect(screen.queryAllByRole('checkbox')).toHaveLength(0);

    // 没有可移除的退役机器以后，多选里只剩退役一种操作。
    fireEvent.click(button('多选'));
    expect(screen.queryByRole('button', { name: /移除选中/ })).toBeNull();
  });

  it('does not remove anything when the confirmation is declined', () => {
    const page = mount(fleet());
    vi.spyOn(window, 'confirm').mockReturnValue(false);
    fireEvent.click(button('多选'));
    fireEvent.click(checkbox('选中 首尔 旧机'));
    fireEvent.click(button('移除选中 1'));

    expect(page.fetcher.mock.calls.some(([path, init]) => path === '/nodes' && init?.method === 'DELETE')).toBe(false);
    expect(checkbox('选中 首尔 旧机').checked).toBe(true);
  });

  it('hides the tools from visitors and disables them for read-only operators', () => {
    mount(fleet(), {
      operator_id: 'public',
      role: 'readonly',
      tenant_scope: null,
      token_prefix: null,
      masked_assets: true,
    });
    expect(screen.queryByRole('button', { name: '多选' })).toBeNull();
    expect(screen.queryByRole('button', { name: '＋ 纳管机器' })).toBeNull();
    expect(cardNames()).toHaveLength(27);
    cleanup();

    mount(fleet(), { ...admin, operator_id: 'reviewer', role: 'readonly', masked_assets: true });
    expect(button('多选').disabled).toBe(true);
    expect(button('＋ 纳管机器').disabled).toBe(true);
  });

  it('shows the empty state without 多选 when there is no machine yet', () => {
    mount([]);
    expect(screen.getByText('还没有机器')).toBeTruthy();
    expect(screen.queryByRole('button', { name: '多选' })).toBeNull();
    expect(button('＋ 纳管机器').disabled).toBe(false);
  });

  it('keeps the same title bar and grid inside a window', () => {
    mount(fleet(), admin, false);
    expect(document.querySelector('.cardpage')).toBeNull();
    const panel = document.querySelector<HTMLElement>('.panel')!;
    expect(within(panel).getByRole('heading', { level: 4, name: '机器' })).toBeTruthy();
    expect(cardNames().slice(-3)).toEqual(['洛杉矶 旧 GIA', '首尔 旧机', '香港 旧 BGP']);
  });
});
