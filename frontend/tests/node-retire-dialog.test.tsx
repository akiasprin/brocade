import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem } from '../src/api';
import { draft } from '../src/draft';
import { navigate, returnTo } from '../src/forge/route';
import { NodesPane } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';
import { compiled, nodes as fleet, settings, snapshot } from './fixtures/ui-regression-data';

// 离开详情页只需要确认去向；真实路由会改写地址栏与历史，不在这里执行。
vi.mock('../src/forge/route', async importOriginal => ({
  ...(await importOriginal<typeof import('../src/forge/route')>()),
  navigate: vi.fn(() => true),
  returnTo: vi.fn(() => true),
}));

const NAME = fleet[0].name;
const ID = fleet[0].node_id;
const HOUR = 3_600_000;

type Call = { method: string; path: string; body: unknown };

/** 一台机器在 Console 里的样子；未给出的生命周期字段与运行中的机器一致。 */
function machine(overrides: Partial<NodeAgentStateItem> = {}): NodeAgentStateItem {
  return { ...fleet[0], retired_at: null, lifecycle_completed_at: null, ...overrides };
}

const retiring = (overrides: Partial<NodeAgentStateItem> = {}) =>
  machine({
    lifecycle_phase: 'retiring',
    retired_at: new Date(Date.now() - 12 * 60_000).toISOString(),
    lifecycle_deployment_id: 58,
    ...overrides,
  });

const retired = (overrides: Partial<NodeAgentStateItem> = {}) =>
  machine({
    lifecycle_phase: 'retired',
    retired_at: new Date(Date.now() - 3 * HOUR).toISOString(),
    lifecycle_completed_at: new Date(Date.now() - 2 * HOUR).toISOString(),
    ...overrides,
  });

const clients: QueryClient[] = [];
let server: {
  node: NodeAgentStateItem | null;
  deployments: { id: number; active: boolean; status: string; revision_id: number }[];
  deleteFails: boolean;
  calls: Call[];
};

beforeEach(() => {
  localStorage.clear();
  draft.init('node-retire-dialog');
  vi.mocked(navigate).mockClear();
  vi.mocked(returnTo).mockClear();
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  server = {
    node: machine(),
    deployments: [{ id: 58, active: true, status: 'running', revision_id: 7 }],
    deleteFails: false,
    calls: [],
  };
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? 'GET';
      if (method !== 'GET')
        server.calls.push({ method, path, body: init?.body ? JSON.parse(String(init.body)) : null });
      const now = new Date().toISOString();
      const node = server.node;
      if (method === 'PUT' && path === `/nodes/${ID}/status` && node) {
        const { status } = JSON.parse(String(init!.body)) as { status: 'active' | 'retired' };
        server.node =
          status === 'active'
            ? machine({ removal_status: null })
            : { ...node, lifecycle_phase: 'retiring', retired_at: node.retired_at ?? now, lifecycle_deployment_id: 58 };
        return Response.json({});
      }
      if (method === 'POST' && path === `/nodes/${ID}/lifecycle/abandon` && node) {
        server.node = {
          ...node,
          lifecycle_phase: 'abandoned',
          retired_at: node.retired_at ?? now,
          lifecycle_completed_at: now,
        };
        return Response.json({});
      }
      if (method === 'POST' && path === `/nodes/${ID}/lifecycle/removal` && node) {
        server.node = {
          ...node,
          lifecycle_phase: node.lifecycle_phase === 'active' ? 'retiring' : node.lifecycle_phase,
          retired_at: node.retired_at ?? now,
          removal_status: 'queued',
          removal_requested_at: now,
        };
        return Response.json({});
      }
      if (method === 'DELETE' && path === '/nodes') {
        if (server.deleteFails) return Response.json({ error: '机器仍被发布 #61 引用' }, { status: 409 });
        server.node = null;
        return Response.json({ revision_id: 8, removed_nodes: [ID], removed_chains: ['chain-a', 'chain-b'] });
      }
      if (path === '/nodes/agent-state') return Response.json({ nodes: server.node ? [server.node] : [] });
      if (path.startsWith('/deployments')) return Response.json({ deployments: server.deployments });
      if (path === '/settings') return Response.json(settings);
      if (path === '/model/snapshot') return Response.json(snapshot);
      if (path === '/revisions?limit=50') return Response.json({ current_revision: 7, revisions: [] });
      if (path.startsWith('/compile/')) return Response.json(compiled);
      if (path === '/certs') return Response.json({ groups: [], nodes: [] });
      return Response.json({ error: `Unexpected test request: ${path}` }, { status: 404 });
    }),
  );
});

afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  draft.clear();
  vi.unstubAllGlobals();
});

function mount(node: NodeAgentStateItem | null = server.node, role: 'system-admin' | 'readonly' = 'system-admin') {
  server.node = node;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  clients.push(client);
  for (const [key, value] of [
    [['nodes'], { nodes: node ? [node] : [] }],
    [['snapshot'], snapshot],
    [['snapshot', 'committed'], snapshot],
    [['settings'], settings],
    [['revisions'], { current_revision: 7, revisions: [] }],
    [['compile', 7], compiled],
    [['deployments'], { deployments: server.deployments }],
    [['certs'], { groups: [], nodes: [] }],
  ] as const)
    client.setQueryData(key, value);
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
    data: { drill: { p: 'node', id: ID, tab: 'config' } },
  };
  render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: { operator_id: 'retire-test', role, tenant_scope: null, token_prefix: null, masked_assets: false },
          initial: { node_count: 1, chain_group_count: [] },
        }}
      >
        <NodesPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return client;
}

const openMenu = () => fireEvent.click(screen.getByRole('button', { name: '更多操作' }));
const lifecycleItem = () => screen.getByRole('menuitem', { name: /^退役\/移出/ });

function openDialog() {
  openMenu();
  fireEvent.click(lifecycleItem());
  return screen.getByRole('dialog');
}

const option = (dialog: HTMLElement, name: RegExp) => within(dialog).getByRole('radio', { name }) as HTMLInputElement;
const forceSwitch = (dialog: HTMLElement) => within(dialog).getByRole('radiogroup', { name: '强制' });
const setForce = (dialog: HTMLElement, on: boolean) =>
  fireEvent.click(within(forceSwitch(dialog)).getByRole('radio', { name: on ? '开启' : '关闭' }));
const confirm = (dialog: HTMLElement, name: string) => fireEvent.click(within(dialog).getByRole('button', { name }));
const writes = () => server.calls.map(call => [call.method, call.path, call.body]);

describe('⋯ menu', () => {
  it.each([
    ['active', machine(), '仅退役，或退役后从机器列表移出', false],
    ['retiring', retiring(), '退役进行中；可以安排移出', false],
    ['retired', retired(), '已退役；可以从机器列表移出', false],
    ['abandoned', retired({ lifecycle_phase: 'abandoned' }), '已退役；可以从机器列表移出', false],
    ['retiring with removal queued', retiring({ removal_status: 'queued' }), '已安排：停用完成后自动移出', true],
    ['retired with removal queued', retired({ removal_status: 'queued' }), '已安排：正在从机器列表移出', true],
  ] as const)('ends with a single 退役/移出 entry for a %s machine', (_, node, note, disabled) => {
    mount(node);
    openMenu();

    const menu = screen.getByRole('menu');
    const items = within(menu).getAllByRole('menuitem');
    expect(items.at(-1)).toBe(lifecycleItem());
    expect(lifecycleItem().textContent).toBe(`退役/移出${note}`);
    expect((lifecycleItem() as HTMLButtonElement).disabled).toBe(disabled);
    for (const gone of [/^退役机器/, /^恢复机器/, /^强制退役/, /^查看发布/, /^重建停用发布/]) {
      expect(within(menu).queryByRole('menuitem', { name: gone })).toBeNull();
    }
    // 与退役无关的维护项保留在菜单里。
    expect(within(menu).getByRole('menuitem', { name: /^强制完成 Xray 交接/ })).toBeTruthy();
  });

  it('keeps the entry disabled for operators without system access', () => {
    mount(machine(), 'readonly');
    openMenu();
    expect((lifecycleItem() as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('退役/移出 dialog', () => {
  it('opens an active machine on 仅退役 with 强制 on and focus on the confirmation', () => {
    mount();
    const dialog = openDialog();

    expect(dialog.getAttribute('aria-label')).toBe(`退役 ${NAME}`);
    expect(option(dialog, /^仅退役/).checked).toBe(true);
    expect(option(dialog, /^退役且移出/).checked).toBe(false);
    expect(within(forceSwitch(dialog)).getByRole('radio', { name: '开启' }).getAttribute('aria-checked')).toBe('true');
    expect(dialog.textContent).toContain('将暂停经过它的 2 条线路：');
    expect(document.activeElement).toBe(within(dialog).getByRole('button', { name: '退役' }));

    fireEvent.click(option(dialog, /^退役且移出/));
    expect(dialog.textContent).toContain('将删除经过它的 2 条线路：');
    expect(within(dialog).getByRole('button', { name: '退役且移出' }).className).toContain('solid');
  });

  it.each([retiring(), retired(), retired({ lifecycle_phase: 'abandoned' })])(
    'keeps the retirement choice when polling advances to $lifecycle_phase',
    async node => {
      const client = mount();
      const dialog = openDialog();
      const retirement = within(dialog).getByRole('button', { name: '退役', exact: true }) as HTMLButtonElement;

      server.node = node;
      await act(() => client.invalidateQueries({ queryKey: ['nodes'] }));
      await waitFor(() => expect(dialog.getAttribute('aria-label')).toBe(`移出 ${NAME}`));

      expect(option(dialog, /^仅退役/).checked).toBe(true);
      expect(option(dialog, /^仅退役/).disabled).toBe(true);
      expect(option(dialog, /^移出/).checked).toBe(false);
      expect(retirement.disabled).toBe(true);
      fireEvent.click(retirement);
      expect(writes()).toEqual([]);

      fireEvent.click(option(dialog, /^移出/));
      expect((within(dialog).getByRole('button', { name: '移出', exact: true }) as HTMLButtonElement).disabled).toBe(
        false,
      );
    },
  );

  it('opens a finished retirement on 移出 only, without 强制, and focuses 取消', () => {
    mount(retired());
    const dialog = openDialog();

    expect(dialog.getAttribute('aria-label')).toBe(`移出 ${NAME}`);
    expect(option(dialog, /^仅退役/).disabled).toBe(true);
    expect(option(dialog, /^仅退役/).closest('label')!.textContent).toContain('已完成');
    expect(option(dialog, /^移出/).checked).toBe(true);
    expect(within(dialog).queryByRole('radiogroup', { name: '强制' })).toBeNull();
    expect(document.activeElement).toBe(within(dialog).getByRole('button', { name: '取消' }));
  });

  it('warns that an offline machine will never report back when 强制 is off', () => {
    mount(machine({ runtime_report_fresh: false, last_poll_at: new Date(Date.now() - 3 * HOUR).toISOString() }));
    const dialog = openDialog();

    expect(dialog.textContent).toContain('失联 3 小时');
    expect(dialog.textContent).not.toContain('关闭强制会一直等到它重新上线');
    setForce(dialog, false);
    expect(dialog.textContent).toContain('这台已失联 3 小时，关闭强制会一直等到它重新上线。');
  });

  it('moves the 强制 switch with the arrow keys', () => {
    mount();
    const dialog = openDialog();
    const on = within(forceSwitch(dialog)).getByRole('radio', { name: '开启' });
    fireEvent.keyDown(on, { key: 'ArrowRight' });
    const off = within(forceSwitch(dialog)).getByRole('radio', { name: '关闭' });
    expect(off.getAttribute('aria-checked')).toBe('true');
    expect(document.activeElement).toBe(off);
  });

  it('returns focus to ⋯ when closed with Escape', async () => {
    mount();
    openDialog();
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('button', { name: '更多操作' })));
    expect(server.calls).toEqual([]);
  });
});

describe('dialog actions', () => {
  it('仅退役 with 强制 force-completes the retirement', async () => {
    mount();
    confirm(openDialog(), '退役');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(writes()).toEqual([['POST', `/nodes/${ID}/lifecycle/abandon`, { unregister_warp: true }]]);
  });

  it('仅退役 without 强制 starts the ordinary teardown', async () => {
    mount();
    const dialog = openDialog();
    setForce(dialog, false);
    confirm(dialog, '退役');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(writes()).toEqual([['PUT', `/nodes/${ID}/status`, { status: 'retired' }]]);
  });

  it('退役且移出 without 强制 asks the Console to remove the machine after retirement', async () => {
    mount();
    const dialog = openDialog();
    fireEvent.click(option(dialog, /^退役且移出/));
    setForce(dialog, false);
    confirm(dialog, '退役且移出');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(writes()).toEqual([['POST', `/nodes/${ID}/lifecycle/removal`, {}]]);
    // 横幅随后提示完成后会移出。
    await waitFor(() => expect(screen.getByRole('region', { name: '退役状态' }).textContent).toContain('完成后移出'));
  });

  it('退役且移出 with 强制 force-retires, removes and goes back to the machine list', async () => {
    mount();
    const dialog = openDialog();
    fireEvent.click(option(dialog, /^退役且移出/));
    confirm(dialog, '退役且移出');
    await waitFor(() => expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'list' }));
    expect(writes()).toEqual([
      ['POST', `/nodes/${ID}/lifecycle/abandon`, { unregister_warp: true }],
      ['DELETE', '/nodes', { node_ids: [ID] }],
    ]);
    expect(returnTo).toHaveBeenCalledTimes(1);
    // 返回被拦下（这里没有真的离开）时，原地说明已移出，而不是「没有这台机器」。
    await waitFor(() => expect(screen.getByRole('status').textContent).toBe(`已移出 ${NAME}，删除 2 条线路`));
  });

  it('keeps the dialog open on 移出 and offers a retry when removal fails after the forced retirement', async () => {
    server.deleteFails = true;
    mount();
    const dialog = openDialog();
    fireEvent.click(option(dialog, /^退役且移出/));
    confirm(dialog, '退役且移出');

    await waitFor(() => expect(dialog.textContent).toContain('机器仍被发布 #61 引用'));
    await waitFor(() => expect(dialog.getAttribute('aria-label')).toBe(`移出 ${NAME}`));
    expect(within(dialog).getByRole('button', { name: '移出' })).toBeTruthy();
    expect(returnTo).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole('button', { name: '取消' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    const banner = screen.getByRole('region', { name: '退役状态' });
    expect(banner.textContent).toContain('强制退役');
    expect(banner.textContent).toContain('机器仍被发布 #61 引用');

    server.deleteFails = false;
    fireEvent.click(within(banner).getByRole('button', { name: '移出' }));
    await waitFor(() => expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'list' }));
    expect(writes().filter(([method]) => method === 'DELETE')).toHaveLength(2);
  });

  it('a retiring machine defaults to 移出 with 强制: force-complete, then remove', async () => {
    mount(retiring());
    const dialog = openDialog();
    expect(dialog.getAttribute('aria-label')).toBe(`移出 ${NAME}`);
    expect(option(dialog, /^仅退役/).closest('label')!.textContent).toContain('进行中');
    confirm(dialog, '移出');
    await waitFor(() => expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'list' }));
    expect(writes()).toEqual([
      ['POST', `/nodes/${ID}/lifecycle/abandon`, { unregister_warp: true }],
      ['DELETE', '/nodes', { node_ids: [ID] }],
    ]);
  });

  it('a retiring machine without 强制 schedules removal after the teardown', async () => {
    mount(retiring());
    const dialog = openDialog();
    setForce(dialog, false);
    confirm(dialog, '移出');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(writes()).toEqual([['POST', `/nodes/${ID}/lifecycle/removal`, {}]]);
  });

  it('a retired machine is removed directly', async () => {
    mount(retired());
    confirm(openDialog(), '移出');
    await waitFor(() => expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'list' }));
    expect(writes()).toEqual([['DELETE', '/nodes', { node_ids: [ID] }]]);
  });
});

describe('retirement banner', () => {
  const banner = () => screen.getByRole('region', { name: '退役状态' });

  it('is absent while the machine is active', () => {
    mount();
    expect(screen.queryByRole('region', { name: '退役状态' })).toBeNull();
  });

  it('shows the teardown progress and its actions while retiring', async () => {
    mount(retiring());
    expect(banner().textContent).toContain('退役中');
    expect(banner().textContent).toContain('停用发布 #58');
    const steps = within(banner()).getAllByRole('listitem');
    expect(steps.map(step => [step.textContent, step.dataset.state ?? null])).toEqual([
      ['创建停用发布', 'done'],
      ['等待机器回报', 'now'],
      ['停用完成', null],
    ]);
    expect(steps[1].getAttribute('aria-current')).toBe('step');

    fireEvent.click(within(banner()).getByRole('button', { name: /查看发布 #58/ }));
    expect(navigate).toHaveBeenCalledWith('deploy');

    fireEvent.click(within(banner()).getByRole('button', { name: '强制完成' }));
    await waitFor(() =>
      expect(writes()).toEqual([['POST', `/nodes/${ID}/lifecycle/abandon`, { unregister_warp: true }]]),
    );
  });

  it('restores a retiring machine', async () => {
    mount(retiring());
    fireEvent.click(within(banner()).getByRole('button', { name: '恢复' }));
    await waitFor(() => expect(writes()).toEqual([['PUT', `/nodes/${ID}/status`, { status: 'active' }]]));
    await waitFor(() => expect(screen.queryByRole('region', { name: '退役状态' })).toBeNull());
  });

  it('offers to rebuild a missing teardown and adds the removal step when one is queued', async () => {
    server.deployments = [{ id: 58, active: false, status: 'cancelled', revision_id: 7 }];
    mount(retiring({ removal_status: 'queued' }));
    expect(banner().textContent).toContain('完成后移出');
    expect(banner().className).toContain('bad');
    expect(
      within(banner())
        .getAllByRole('listitem')
        .map(step => step.textContent),
    ).toEqual(['创建停用发布', '等待机器回报', '停用完成', '移出']);
    expect(within(banner()).queryByRole('button', { name: /查看发布/ })).toBeNull();
    fireEvent.click(within(banner()).getByRole('button', { name: '重建停用发布' }));
    await waitFor(() => expect(writes()).toEqual([['PUT', `/nodes/${ID}/status`, { status: 'retired' }]]));
  });

  it('suggests forcing completion for an offline retiring machine', () => {
    mount(retiring({ runtime_report_fresh: false, last_poll_at: new Date(Date.now() - 3 * HOUR).toISOString() }));
    expect(banner().textContent).toContain('这台已失联 3 小时，回报不会到来，可以强制完成。');
    expect(within(banner()).getByRole('button', { name: '强制完成' }).className).toContain('node-life-urgent');
  });

  it('lists the paused routes of a retired machine and restores it', async () => {
    mount(retired());
    expect(banner().textContent).toContain('已退役');
    expect(banner().textContent).toContain('机器已回报停用');
    expect(banner().textContent).toContain('经过它的 2 条线路已暂停：线路 A、线路 B。');
    expect(within(banner()).queryByRole('button', { name: '移出' })).toBeNull();
    fireEvent.click(within(banner()).getByRole('button', { name: '恢复' }));
    await waitFor(() => expect(writes()).toEqual([['PUT', `/nodes/${ID}/status`, { status: 'active' }]]));
  });

  it('shows a failed automatic removal and removes by hand', async () => {
    mount(
      retired({ lifecycle_phase: 'abandoned', removal_status: 'failed', removal_last_error: '对象存储暂时不可用' }),
    );
    expect(banner().textContent).toContain('强制退役');
    expect(within(banner()).getByRole('alert').textContent).toBe('自动移出失败：对象存储暂时不可用。可以手动移出。');
    fireEvent.click(within(banner()).getByRole('button', { name: '移出' }));
    await waitFor(() => expect(writes()).toEqual([['DELETE', '/nodes', { node_ids: [ID] }]]));
  });

  it('shows a retrying removal without offering a second one', () => {
    mount(retired({ removal_status: 'queued', removal_last_error: '对象存储暂时不可用' }));
    expect(banner().textContent).toContain('正在移出');
    expect(banner().textContent).toContain('移出重试中：对象存储暂时不可用');
    expect(within(banner()).queryByRole('button', { name: '移出' })).toBeNull();
    expect(within(banner()).getByRole('button', { name: '恢复' }).getAttribute('title')).toContain('撤销已安排的移出');
  });

  it('disables the actions for read-only operators', () => {
    mount(retiring(), 'readonly');
    for (const name of ['恢复', '强制完成']) {
      expect((within(banner()).getByRole('button', { name }) as HTMLButtonElement).disabled).toBe(true);
    }
  });
});

describe('removal handled by the Console', () => {
  it('goes back to the list once a machine with a queued removal disappears', async () => {
    const client = mount(retired({ removal_status: 'queued' }));
    expect(returnTo).not.toHaveBeenCalled();

    server.node = null;
    await act(() => client.invalidateQueries({ queryKey: ['nodes'] }));
    await waitFor(() => expect(returnTo).toHaveBeenCalledWith('nodes', { p: 'list' }));
    expect(returnTo).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('status').textContent).toBe(`已移出 ${NAME}`);
    expect(screen.queryByText(/没有这台机器/)).toBeNull();
  });

  it('still reports an unknown machine that was never scheduled for removal', async () => {
    const client = mount(retired());
    server.node = null;
    await act(() => client.invalidateQueries({ queryKey: ['nodes'] }));
    await waitFor(() => expect(screen.getByText(/没有这台机器/)).toBeTruthy());
    expect(returnTo).not.toHaveBeenCalled();
  });
});
