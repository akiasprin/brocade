import { QueryClient, QueryClientProvider, skipToken, useQuery } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmInstance, CvmWorkspace } from '../../src/api';
import { navigate } from '../../src/forge/route';
import { useState } from 'react';
import { InstanceList, type InstanceListView } from '../../src/panes/cvm/instance-list';
import { baremetalWorkspace, cvmReadings, cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import { nicCatalog } from '../fixtures/cvm/network-data';

vi.mock('../../src/forge/route', () => ({ navigate: vi.fn() }));

const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function instanceLike(instance: CvmInstance, id: string, name: string) {
  const copy = structuredClone(instance);
  copy.desired.id = id;
  copy.desired.spec.name = name;
  return copy;
}

function mount(
  workspace: CvmWorkspace,
  options: {
    write?: (id: string, body: Record<string, unknown>) => Response | Promise<Response>;
    onCreateOnHost?: (id: string) => void;
    entryFailure?: boolean;
  } = {},
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  client.setQueryData(['cvm'], workspace);
  clients.push(client);
  const writes: { id: string; body: Record<string, unknown> }[] = [];
  const reads: string[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const url = String(input);
      if (init?.method === 'POST') {
        const id = decodeURIComponent(url.split('/')[2]);
        const body = JSON.parse(String(init.body));
        writes.push({ id, body });
        if (options.write) return options.write(id, body);
        throw new Error(`Unexpected list write to ${url}`);
      }
      reads.push(url);
      if (url === '/cvm/entries') {
        if (options.entryFailure) return Response.json({ error: '入口读取失败' }, { status: 503 });
        const board = entryBoard(workspace, 'list');
        const ssh = board.entries[0]?.mode;
        if (ssh && ssh.kind === 'l4_range') Object.assign(ssh, { listen_end: 30029, target_end: 29 });
        return Response.json(board);
      }
      if (url === '/cvm/networks') return Response.json(nicCatalog(workspace.can_manage_network));
      if (url === '/cvm/readings') return Response.json(cvmReadings(workspace));
      throw new Error(`Unexpected list read from ${url}`);
    }),
  );
  function ListContent() {
    const [view, setView] = useState<InstanceListView>({ filter: 'all', host: '', owner: '', search: '' });
    const current = useQuery<CvmWorkspace>({ queryKey: ['cvm'], queryFn: skipToken }).data ?? workspace;
    return (
      <InstanceList workspace={current} view={view} onViewChange={setView} onCreateOnHost={options.onCreateOnHost} />
    );
  }
  function ListHarness({ visible }: { visible: boolean }) {
    return <QueryClientProvider client={client}>{visible && <ListContent />}</QueryClientProvider>;
  }
  const view = render(<ListHarness visible />);
  return {
    writes,
    reads,
    client,
    remount: () => {
      view.rerender(<ListHarness visible={false} />);
      view.rerender(<ListHarness visible />);
    },
  };
}

it('groups real rows, combines status, owner, host and port filters, and opens the requested detail', async () => {
  const data = cvmWorkspace();
  const first = data.instances[0];
  const second = instanceLike(first, 'backup', 'backup-cache');
  second.node_id = 'tyo';
  second.user_id = null;
  data.hosts.push({ ...data.hosts[0], node_id: 'tyo', name: '东京' });
  data.instances.push(second);
  const create = vi.fn();
  mount(data, { onCreateOnHost: create });
  const home = await screen.findByRole('button', { name: /^TCP 203\.0\.113\.20:30022-30029 / });
  const table = within(screen.getByRole('table', { name: '按宿主分组的容器' }));
  const hongKong = table.getByRole('rowheader', { name: /香港 HKIX/ });
  expect(table.getByRole('rowheader', { name: /东京/ })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'backup-cache' })).toBeTruthy();
  fireEvent.click(home);
  expect(navigate).toHaveBeenLastCalledWith('cvm', { p: 'instance', id: first.desired.id, tab: 'network' });
  fireEvent.click(screen.getByRole('button', { name: first.desired.spec.name }));
  expect(navigate).toHaveBeenLastCalledWith('cvm', { p: 'instance', id: first.desired.id });
  fireEvent.click(within(hongKong).getByRole('button', { name: '在此创建' }));
  expect(create).toHaveBeenCalledWith(first.node_id);
  fireEvent.click(screen.getByRole('button', { name: /^运行中/ }));
  fireEvent.change(screen.getByRole('combobox', { name: '宿主' }), { target: { value: first.node_id } });
  fireEvent.change(screen.getByRole('searchbox', { name: '搜索容器' }), { target: { value: 'ALICE 30025' } });
  expect(screen.getByRole('button', { name: first.desired.spec.name })).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'backup-cache' })).toBeNull();
  fireEvent.change(screen.getByRole('combobox', { name: '使用者' }), { target: { value: 'unassigned' } });
  expect(screen.getByText('没有匹配的容器')).toBeTruthy();
  expect(screen.getByText(/可搜索名称、使用者、宿主、地址或端口/)).toBeTruthy();
});

it('keeps the list operable when the entry feed fails', async () => {
  const data = cvmWorkspace();
  mount(data, { entryFailure: true });
  expect(await screen.findByText('入口读取失败')).toBeTruthy();
  const instance = data.instances[0];
  fireEvent.click(screen.getByRole('button', { name: instance.desired.spec.name }));
  expect(navigate).toHaveBeenCalledWith('cvm', { p: 'instance', id: instance.desired.id });
  const more = { name: `${instance.desired.spec.name} 更多操作` };
  fireEvent.click(screen.getByRole('button', more));
  fireEvent.click(within(screen.getByRole('dialog', more)).getByRole('menuitem', { name: /^删除容器/ }));
  const deletion = within(screen.getByRole('dialog', { name: '删除容器' }));
  expect(deletion.getByText('撤销该容器的全部入站方式（明细暂不可用）')).toBeTruthy();
});

it('hides private fields and operations for a redacted account', async () => {
  const data = cvmWorkspace();
  data.redacted = true; // Exercise the client boundary with data that still contains an address.
  const address = data.instances[0].report!.access_address!;
  const { reads } = mount(data);
  expect((await screen.findAllByText('/ 100 GiB')).length).toBeGreaterThan(0);
  expect(screen.getByText('已脱敏')).toBeTruthy();
  expect(screen.queryByRole('checkbox')).toBeNull();
  expect(screen.queryByRole('button', { name: /更多操作|复制 SSH|终端/ })).toBeNull();
  expect(reads).not.toContain('/cvm/entries');
  fireEvent.change(screen.getByRole('searchbox', { name: '搜索容器' }), { target: { value: address } });
  expect(screen.getByText('没有匹配的容器')).toBeTruthy();
});

it('filters staged and in-flight publishing rows without offering another power change', () => {
  const data = cvmWorkspace();
  const first = data.instances[0];
  Object.assign(first, { revision_id: 41, published_revision_id: 40, operation_kind: 'entries' });
  const locked = instanceLike(first, 'locked', '发布中的容器');
  locked.blocking_deployment_id = 7;
  const active = instanceLike(cvmWorkspace().instances[0], 'active', '运行容器');
  data.instances.push(locked, active);
  mount(data);
  fireEvent.click(screen.getByRole('button', { name: '待发布 2' }));
  expect(screen.getByRole('button', { name: first.desired.spec.name })).toBeTruthy();
  expect(screen.getByRole('button', { name: '发布中的容器' })).toBeTruthy();
  expect(screen.getByText('发布中')).toBeTruthy();
  expect(screen.queryByRole('button', { name: '运行容器' })).toBeNull();
  expect(screen.queryByRole('button', { name: '关机' })).toBeNull();
});

it('anchors restart confirmation and requires an exact name for clean deletion', async () => {
  const data = cvmWorkspace();
  const instance = data.instances[0];
  const name = instance.desired.spec.name;
  const { writes } = mount(data, { write: () => Response.json({ error: '保存失败' }, { status: 409 }) });
  await screen.findByRole('button', { name: /^TCP 203\.0\.113\.20:30022-30029 / });
  const trigger = screen.getByRole('button', { name: '重启' });
  trigger.focus();
  fireEvent.click(trigger);
  const restart = screen.getByRole('dialog', { name: '重启' });
  expect(restart.classList.contains('popover')).toBe(true);
  expect(within(restart).getByText(/短暂中断连接/)).toBeTruthy();
  expect(writes).toHaveLength(0);
  fireEvent.keyDown(restart, { key: 'Escape' });
  await waitFor(() => expect(document.activeElement).toBe(trigger));
  expect(screen.queryByRole('dialog', { name: '重启' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: `${name} 更多操作` }));
  fireEvent.click(
    within(screen.getByRole('dialog', { name: `${name} 更多操作` })).getByRole('menuitem', { name: /^删除容器/ }),
  );
  const modal = screen.getByRole('dialog', { name: '删除容器' });
  expect(modal.classList.contains('modal')).toBe(true);
  const deletion = within(modal);
  expect(deletion.getByText(/销毁 .* 系统盘及其数据/)).toBeTruthy();
  expect(deletion.getByText(/撤销 TCP 203\.0\.113\.20:30022-30029/)).toBeTruthy();
  expect(deletion.getByText('移除 2 个出站方式')).toBeTruthy();
  expect(deletion.getByText(/香港 HKIX · 清理确认后释放 1 vCPU/)).toBeTruthy();
  const save = deletion.getByRole<HTMLButtonElement>('button', { name: '保存删除容器' });
  expect(save.disabled).toBe(true);
  expect(save.classList.contains('danger')).toBe(false);
  fireEvent.change(deletion.getByRole('textbox', { name: '输入实例名称确认' }), { target: { value: name } });
  fireEvent.click(save);
  expect(await deletion.findByText('保存失败 · 保存失败')).toBeTruthy();
  expect(writes).toEqual([
    {
      id: instance.desired.id,
      body: {
        request_id: expect.any(String),
        expected_generation: instance.desired.generation,
        action: 'delete',
        delete_data: true,
      },
    },
  ]);
});

it('stages a lifecycle change from its confirmation without leaving the list', async () => {
  const data = cvmWorkspace();
  const instance = data.instances[0];
  const { writes } = mount(data, {
    write: () => {
      const saved = structuredClone(instance);
      saved.desired.generation += 1;
      saved.desired.restart_epoch += 1;
      saved.revision_id = 41;
      saved.operation_kind = 'restart';
      return Response.json(saved);
    },
  });
  fireEvent.click(screen.getByRole('button', { name: '重启' }));
  const card = within(screen.getByRole('dialog', { name: '重启' }));
  expect(writes).toHaveLength(0);
  fireEvent.click(card.getByRole('button', { name: '保存变更' }));
  expect(await screen.findByRole('button', { name: '待发布 1' })).toBeTruthy();
  expect(screen.queryByRole('dialog', { name: '重启' })).toBeNull();
  expect(writes).toEqual([
    {
      id: instance.desired.id,
      body: { request_id: expect.any(String), expected_generation: instance.desired.generation, action: 'restart' },
    },
  ]);
  expect(navigate).not.toHaveBeenCalled();
});

it('opens card actions as a popover and uses the existing authorization flow', async () => {
  const data = cvmWorkspace();
  const instance = data.instances[0];
  const args = { name: `${instance.desired.spec.name} 更多操作` };
  const { writes } = mount(data, { write: () => Response.json(instance) });
  const more = screen.getByRole('button', args);
  more.focus();
  fireEvent.click(more);
  const menu = screen.getByRole('dialog', args);
  expect(menu.classList.contains('popover')).toBe(true);
  fireEvent.click(within(menu).getByRole('menuitem', { name: /^重启/ }));
  const confirmation = screen.getByRole('dialog', { name: '重启' });
  expect(confirmation.classList.contains('popover')).toBe(true);
  expect(screen.queryByRole('dialog', args)).toBeNull();
  fireEvent.click(within(confirmation).getByRole('button', { name: '取消' }));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '重启' })).toBeNull());
  fireEvent.click(more);
  fireEvent.click(within(screen.getByRole('dialog', args)).getByRole('menuitem', { name: /^使用授权/ }));
  const authorization = within(screen.getByRole('dialog', { name: '容器使用授权' }));
  expect(screen.queryByRole('dialog', args)).toBeNull();
  expect(writes).toHaveLength(0);
  fireEvent.change(authorization.getByRole('combobox', { name: '使用用户' }), { target: { value: '' } });
  fireEvent.click(authorization.getByRole('button', { name: '收回授权' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toEqual({
    id: instance.desired.id,
    body: {
      request_id: expect.any(String),
      expected_generation: instance.desired.generation,
      action: 'assign',
      user_id: null,
    },
  });
});

it('filters attention rows and offers in-place retry and verification under each host', async () => {
  const data = cvmWorkspace();
  const first = data.instances[0];
  const failed = instanceLike(first, 'failed', '故障容器');
  const unknown = instanceLike(cvmWorkspace('unknown').instances[0], 'unknown', '待核查容器');
  failed.status = 'failed';
  failed.report!.error = '磁盘空间不足';
  unknown.node_id = 'offline';
  data.hosts.push({ ...data.hosts[0], node_id: 'offline', name: '东京', fresh: false });
  data.instances.push(failed, unknown);
  const { writes } = mount(data, {
    write: id => {
      const saved = structuredClone(data.instances.find(item => item.desired.id === id)!);
      saved.status = 'pending';
      return Response.json(saved);
    },
  });
  const list = within(screen.getByRole('region', { name: '容器清单' }));
  fireEvent.click(list.getByRole('button', { name: '需处理 2' }));
  expect(list.queryByRole('button', { name: first.desired.spec.name })).toBeNull();
  expect(list.getByRole('rowheader', { name: /香港 HKIX/ })).toBeTruthy();
  expect(list.getByRole('rowheader', { name: /东京/ })).toBeTruthy();
  expect(list.getByText('磁盘空间不足')).toBeTruthy();
  expect(list.getByText(/宿主能力已过期/)).toBeTruthy();
  expect(list.getByRole('button', { name: '重试' })).toBeTruthy();
  fireEvent.click(list.getByRole('button', { name: '故障容器 更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /^复制为新容器/ }));
  expect(navigate).toHaveBeenCalledWith('cvm', { p: 'new', copy: failed.desired.id });
  // Choosing an item closes the menu, like the shared action menus.
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '故障容器 更多操作' })).toBeNull());
  fireEvent.change(list.getByRole('searchbox', { name: '搜索容器' }), { target: { value: '东京' } });
  expect(list.queryByRole('button', { name: '重试' })).toBeNull();
  fireEvent.click(list.getByRole('button', { name: '核查' }));
  const check = screen.getByRole('dialog', { name: '核查' });
  expect(check.classList.contains('popover')).toBe(true);
  expect(writes).toHaveLength(0);
  fireEvent.click(within(check).getByRole('button', { name: '确认核查' }));
  expect(await list.findByText('已提交核查与重试')).toBeTruthy();
  expect(writes).toEqual([
    {
      id: unknown.desired.id,
      body: { request_id: expect.any(String), expected_generation: unknown.desired.generation, action: 'retry' },
    },
  ]);
});

it('offers release instead of clean deletion for a bare-metal card', () => {
  const data = baremetalWorkspace();
  mount(data);
  const more = { name: 'baremetal-workspace 更多操作' };
  fireEvent.click(screen.getByRole('button', more));
  const menu = within(screen.getByRole('dialog', more));
  expect(menu.queryByRole('menuitem', { name: /开机|关机|重启|使用授权|删除/ })).toBeNull();
  fireEvent.click(menu.getByRole('menuitem', { name: /^解除纳管/ }));
  const release = within(screen.getByRole('dialog', { name: '解除纳管' }));
  expect(release.getByText(/保留宿主操作系统和磁盘/)).toBeTruthy();
  expect(release.queryByText('系统盘')).toBeNull();
});

it('keeps owner authorization and clean deletion out of an ordinary user’s card menu', () => {
  const data = cvmWorkspace('list', 'user');
  const args = { name: `${data.instances[0].desired.spec.name} 更多操作` };
  mount(data);
  fireEvent.click(screen.getByRole('button', args));
  const menu = within(screen.getByRole('dialog', args));
  expect(menu.getByRole('menuitem', { name: /^重启/ })).toBeTruthy();
  expect(menu.queryByRole('menuitem', { name: /使用授权|删除|复制为新容器/ })).toBeNull();
});

it('saves eligible batch power changes, skips stopped and bare-metal rows, and replays only an uncertain request', async () => {
  const data = cvmWorkspace('list', 'user');
  const first = data.instances[0];
  const second = instanceLike(first, 'unstable', '结果需核查');
  const stopped = instanceLike(cvmWorkspace('stopped').instances[0], 'stopped', '已经关机');
  const metal = baremetalWorkspace('baremetal-detail', 'user');
  metal.hosts[0].node_id = 'baremetal';
  metal.instances[0].node_id = 'baremetal';
  metal.instances[0].desired.id = 'baremetal-instance';
  data.hosts.push(metal.hosts[0]);
  data.instances.push(second, stopped, metal.instances[0]);
  let secondAttempts = 0;
  const { writes, remount } = mount(data, {
    write: id => {
      if (id === second.desired.id && secondAttempts++ === 0)
        return Response.json({ error: '请求超时' }, { status: 503 });
      const saved = structuredClone(data.instances.find(item => item.desired.id === id)!);
      saved.desired.generation += 1;
      saved.desired.power = 'stopped';
      saved.revision_id = 42;
      return Response.json(saved);
    },
  });
  expect(screen.queryByRole('combobox', { name: '使用者' })).toBeNull();
  expect(screen.queryByRole('button', { name: '查看发布计划' })).toBeNull();
  fireEvent.click(screen.getByRole('checkbox', { name: '选择当前清单中的容器' }));
  const toolbar = screen.getByText('已选 4 个容器').parentElement!;
  expect(within(toolbar).queryByRole('button', { name: /删除/ })).toBeNull();
  fireEvent.click(within(toolbar).getByRole('button', { name: '关机' }));
  const dialog = within(screen.getByRole('dialog', { name: '批量关机' }));
  expect(dialog.getByText('跳过 · 已关机')).toBeTruthy();
  expect(dialog.getByText('跳过 · 裸机不支持此操作')).toBeTruthy();
  fireEvent.click(dialog.getByRole('button', { name: '保存变更' }));
  expect(await dialog.findByText('结果未确认 · 请求超时')).toBeTruthy();
  expect(writes.map(item => item.id)).toEqual([first.desired.id, second.desired.id]);
  fireEvent.click(dialog.getByText('关闭', { selector: 'button' }));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '批量关机' })).toBeNull());
  remount();
  expect(screen.queryByRole('button', { name: '重启' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '重试原请求' }));
  const resumed = within(screen.getByRole('dialog', { name: '批量关机' }));
  fireEvent.click(resumed.getByRole('button', { name: '重试原请求' }));
  await waitFor(() => expect(resumed.getAllByText('已保存')).toHaveLength(2));
  expect(writes).toHaveLength(3);
  expect(writes[2]).toEqual(writes[1]);
});

it('confirms each eligible batch deletion, keeps skips frozen, and replays only the original uncertain delete', async () => {
  const data = cvmWorkspace();
  const first = data.instances[0];
  const uncertain = instanceLike(first, 'uncertain', '回复丢失');
  const denied = instanceLike(first, 'denied', '被服务端拒绝');
  const changed = instanceLike(first, 'changed', '稍后发生变化');
  const pending = instanceLike(first, 'pending', '正在发布');
  pending.node_id = 'tyo';
  pending.revision_id = 40;
  pending.published_revision_id = 39;
  pending.blocking_deployment_id = 9;
  const metal = baremetalWorkspace().instances[0];
  metal.desired.id = 'baremetal-instance';
  data.hosts.push({ ...data.hosts[0], node_id: 'tyo', name: '东京' });
  data.instances.push(uncertain, denied, changed, pending, metal);
  let respondFirst!: (response: Response) => void;
  let uncertainAttempts = 0;
  const deleted = (instance: CvmInstance) => {
    const saved = structuredClone(instance);
    saved.desired.generation += 1;
    saved.desired.power = 'deleted';
    saved.revision_id = 41;
    return saved;
  };
  const { writes, client, remount } = mount(data, {
    write: id => {
      if (id === first.desired.id) return new Promise<Response>(resolve => (respondFirst = resolve));
      if (id === denied.desired.id) return Response.json({ error: '写入冲突' }, { status: 409 });
      if (id === uncertain.desired.id && uncertainAttempts++ === 0)
        return Response.json({ error: '回执丢失' }, { status: 503 });
      if (id === uncertain.desired.id) return Response.json(deleted(uncertain));
      throw new Error(`Ineligible delete: ${id}`);
    },
  });
  fireEvent.click(screen.getByRole('checkbox', { name: '选择当前清单中的容器' }));
  const toolbar = within(screen.getByRole('group', { name: '批量操作' }));
  expect(toolbar.getByText('已选 6 个容器')).toBeTruthy();
  expect(within(toolbar.getByRole('list', { name: '所选容器的宿主分布' })).getByText('东京 1 个')).toBeTruthy();
  fireEvent.click(toolbar.getByRole('button', { name: '批量删除…' }));
  const dialog = within(screen.getByRole('dialog', { name: '批量删除容器' }));
  expect(dialog.getByText('跳过 · 裸机不支持此操作')).toBeTruthy();
  expect(dialog.getByText('跳过 · 发布单已锁定')).toBeTruthy();
  expect(dialog.getAllByRole('textbox')).toHaveLength(4);
  const save = dialog.getByRole<HTMLButtonElement>('button', { name: '保存批量删除容器' });
  expect(save.disabled).toBe(true);
  for (const instance of [first, uncertain, denied, changed]) {
    const name = instance.desired.spec.name;
    const input = dialog.getByRole('textbox', { name: `输入 ${name} 确认` });
    fireEvent.change(input, { target: { value: `${name} ` } });
    expect(save.disabled).toBe(true);
    fireEvent.change(input, { target: { value: name } });
  }
  expect(save.disabled).toBe(false);
  expect(writes).toHaveLength(0);
  fireEvent.click(save);
  await waitFor(() => expect(writes).toHaveLength(1));
  const advanced = structuredClone(data);
  advanced.instances.find(item => item.desired.id === changed.desired.id)!.desired.generation += 1;
  const nowUnlocked = advanced.instances.find(item => item.desired.id === pending.desired.id)!;
  nowUnlocked.blocking_deployment_id = null;
  nowUnlocked.published_revision_id = nowUnlocked.revision_id;
  act(() => {
    client.setQueryData(['cvm'], advanced);
    respondFirst(Response.json(deleted(first)));
  });
  expect(await dialog.findByText('结果未确认 · 回执丢失')).toBeTruthy();
  expect(dialog.getByText('保存失败 · 写入冲突')).toBeTruthy();
  expect(dialog.getByText('跳过 · 配置已变化，请重新选择')).toBeTruthy();
  expect(dialog.getByText('跳过 · 发布单已锁定')).toBeTruthy();
  expect(dialog.getByText('已保存')).toBeTruthy();
  expect(writes.map(item => item.id)).toEqual([first.desired.id, uncertain.desired.id, denied.desired.id]);
  expect(writes.every(item => item.body.action === 'delete' && item.body.delete_data === true)).toBe(true);
  fireEvent.click(dialog.getByText('关闭', { selector: 'button' }));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '批量删除容器' })).toBeNull());
  const refreshed = client.getQueryData<CvmWorkspace>(['cvm'])!;
  client.setQueryData(['cvm'], {
    ...refreshed,
    instances: refreshed.instances.map(item => (item.desired.id === uncertain.desired.id ? deleted(item) : item)),
  });
  remount();
  fireEvent.click(screen.getByRole('button', { name: '重试原请求' }));
  const resumed = within(screen.getByRole('dialog', { name: '批量删除容器' }));
  expect(resumed.queryByRole('textbox')).toBeNull();
  fireEvent.click(resumed.getByRole('button', { name: '重试原请求' }));
  await waitFor(() => expect(resumed.getAllByText('已保存')).toHaveLength(2));
  expect(resumed.getByText('保存失败 · 写入冲突')).toBeTruthy();
  expect(writes).toHaveLength(4);
  expect(writes[3]).toEqual(writes[1]);
});
