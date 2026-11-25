import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ComponentProps } from 'react';
import type { CvmMonitoring, CvmNicState, CvmWorkspace } from '../../src/api';
import { navigate } from '../../src/forge/route';
import { InstanceDetail } from '../../src/panes/cvm/detail';
import { baremetalWorkspace, cvmReadings, cvmWorkspace } from '../fixtures/cvm/data';

vi.mock('../../src/forge/route', () => ({ navigate: vi.fn() }));

const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function mount(
  workspace: CvmWorkspace,
  reply?: (body: unknown, attempt: number) => Response,
  bindings: CvmNicState['bindings'] = [],
  controlled?: Pick<ComponentProps<typeof InstanceDetail>, 'initialTab' | 'onTabChange'>,
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const writes: unknown[] = [];
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      const body: unknown = JSON.parse(String(init.body));
      writes.push(body);
      return reply?.(body, writes.length) ?? Response.json(workspace.instances[0]);
    }
    if (url.endsWith('/entries')) return Response.json([]);
    if (url.endsWith('/network')) return Response.json({ bindings });
    if (url === '/cvm/readings') return Response.json(cvmReadings(workspace));
    if (url.endsWith('/monitoring'))
      return Response.json({
        state: 'unavailable',
        error: 'runtime-secret 192.0.2.21',
        events: [{ at_unix_millis: Date.now(), kind: 'guest_unreachable', detail: 'volume-secret 192.0.2.21' }],
        issues: [],
        health: null,
        latest: null,
        samples: [],
        samples_truncated: false,
        range_start_unix_millis: Date.now() - 3600000,
        range_end_unix_millis: Date.now(),
      } satisfies CvmMonitoring);
    return Response.json({ error: 'unexpected test request' }, { status: 404 });
  });
  vi.stubGlobal('fetch', fetch);
  const view = (data: CvmWorkspace) => (
    <QueryClientProvider client={client}>
      <button>外部操作</button>
      <main className="fg-view" tabIndex={-1}>
        <div>
          <InstanceDetail
            instance={data.instances[0]}
            workspace={data}
            topBar={<div role="status">统一待发布条</div>}
            networkDraftCount={2}
            networkEditor={<input aria-label="网络编辑" />}
            {...controlled}
          />
        </div>
      </main>
    </QueryClientProvider>
  );
  const result = render(view(workspace));
  return { rerender: (data: CvmWorkspace) => result.rerender(view(data)), writes, fetch };
}

function moreItem(name: RegExp) {
  fireEvent.click(screen.getByRole('button', { name: '实例更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name }));
}

it('steps between listed instances without taking arrows from tabs, inputs, or outside focus', () => {
  const data = cvmWorkspace('detail');
  const next = structuredClone(data.instances[0]);
  next.desired.id = 'cvm-next';
  data.instances.push(next);
  mount(data);
  expect(screen.getByText('统一待发布条')).toBeTruthy();
  expect(screen.getByRole('button', { name: '上一个容器' }).hasAttribute('disabled')).toBe(true);
  expect(screen.getByRole('tab', { name: '网络' }).getAttribute('aria-description')).toBe('2 处未保存的网络变更');
  fireEvent.keyDown(screen.getByRole('tab', { name: '概览' }), { key: 'ArrowDown' });
  expect(screen.getByRole('tab', { name: '网络' }).getAttribute('aria-selected')).toBe('true');
  fireEvent.keyDown(screen.getByRole('textbox', { name: '网络编辑' }), { key: 'ArrowDown' });
  fireEvent.keyDown(screen.getByRole('button', { name: '外部操作' }), { key: 'ArrowDown' });
  expect(navigate).not.toHaveBeenCalled();
  fireEvent.keyDown(screen.getByRole('main'), { key: 'ArrowDown' });
  expect(navigate).toHaveBeenCalledWith('cvm', { p: 'instance', id: 'cvm-next', tab: 'network' });
  fireEvent.click(screen.getByRole('tab', { name: '概览' }));
  fireEvent.click(screen.getByRole('button', { name: '下一个容器' }));
  expect(navigate).toHaveBeenLastCalledWith('cvm', { p: 'instance', id: 'cvm-next' });
});

it('uses the routed tab and refuses an artifacts deep link for ordinary users', () => {
  const data = cvmWorkspace('detail', 'user');
  const onTabChange = vi.fn();
  const { fetch } = mount(data, undefined, [], { initialTab: 'artifacts', onTabChange });
  expect(screen.getByRole('tab', { name: '概览' }).getAttribute('aria-selected')).toBe('true');
  expect(screen.queryByRole('tab', { name: '产物' })).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  expect(onTabChange).toHaveBeenCalledWith('network');
  expect(fetch.mock.calls.some(([url]) => url.includes('/artifacts'))).toBe(false);
});

it('requires the exact displayed name and describes the destructive consequences before saving', async () => {
  const data = cvmWorkspace('detail');
  const { writes } = mount(data);
  moreItem(/^删除/);
  const dialog = await screen.findByRole('dialog', { name: '删除容器？' });
  const confirm = within(dialog).getByRole('button', { name: '保存删除变更' });
  expect(within(dialog).getByRole('list').textContent).toContain('永久销毁系统盘');
  expect(within(dialog).getByRole('list').textContent).toContain('撤销访问入口，卸载网卡');
  expect(within(dialog).getByRole('list').textContent).toContain('释放该容器在宿主上的资源分配');
  expect(confirm.hasAttribute('disabled')).toBe(true);
  const name = within(dialog).getByRole('textbox', { name: '输入完整容器名称以确认删除' });
  fireEvent.change(name, { target: { value: `${data.instances[0].desired.spec.name} ` } });
  fireEvent.click(confirm);
  expect(writes).toHaveLength(0);
  fireEvent.change(name, { target: { value: data.instances[0].desired.spec.name } });
  fireEvent.click(confirm);
  await waitFor(() => expect(writes).toMatchObject([{ action: 'delete', delete_data: true }]));
});

it('releases bare metal separately without requesting a VM name or erasing disks', async () => {
  const { writes } = mount(baremetalWorkspace());
  moreItem(/^解除纳管/);
  const dialog = await screen.findByRole('dialog', { name: '解除纳管？' });
  expect(within(dialog).queryByRole('textbox')).toBeNull();
  expect(within(dialog).getByRole('list').textContent).toContain('保留宿主操作系统、本地磁盘和数据');
  fireEvent.click(within(dialog).getByRole('button', { name: '保存解除纳管' }));
  await waitFor(() => expect(writes).toMatchObject([{ action: 'release_host' }]));
  expect(writes[0]).not.toHaveProperty('delete_data');
});

it('withdraws with a frozen idempotency key and generation even if workspace refresh shows it committed', async () => {
  const data = cvmWorkspace('detail');
  data.owners.push(
    { tenant_id: data.instances[0].tenant_id, user_id: 'bob', name: 'Bob' },
    { tenant_id: 'elsewhere', user_id: 'eve', name: 'Eve' },
  );
  const revoked = structuredClone(data);
  const instance = revoked.instances[0];
  instance.user_id = null;
  instance.desired.generation += 1;
  instance.status = 'pending';
  instance.operation_kind = 'assign';
  const { rerender, writes } = mount(data, (_body, attempt) =>
    attempt === 1 ? Response.json({ error: 'uncertain' }, { status: 503 }) : Response.json(instance),
  );
  moreItem(/^使用授权/);
  const dialog = await screen.findByRole('dialog', { name: '容器使用授权' });
  const choose = within(dialog).getByRole('combobox', { name: '使用用户' });
  expect(within(choose).getByRole('option', { name: 'Bob' }).hasAttribute('disabled')).toBe(true);
  expect(within(choose).queryByRole('option', { name: 'Eve' })).toBeNull();
  expect(within(dialog).getByRole('list').textContent).toContain('立即失去控制台访问；收回授权不可撤回');
  fireEvent.change(choose, { target: { value: '' } });
  fireEvent.click(within(dialog).getByRole('button', { name: '收回授权' }));
  const retry = await within(dialog).findByRole('button', { name: '重试原请求' });
  rerender(revoked);
  expect(choose.hasAttribute('disabled')).toBe(true);
  expect(retry.hasAttribute('disabled')).toBe(false);
  fireEvent.click(retry);
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[0]).toMatchObject({ action: 'assign', user_id: null, expected_generation: 1 });
  expect(writes[1]).toEqual(writes[0]);
});

it('prevents reassigning an unassigned container while published shutdown and cleanup are unconfirmed', async () => {
  const data = cvmWorkspace('detail');
  const instance = data.instances[0];
  instance.user_id = null;
  instance.operation_kind = 'assign';
  instance.revision_id = 2;
  instance.published_revision_id = 1;
  instance.desired.generation = 2;
  instance.desired.power = 'stopped';
  instance.desired.spec.interfaces = instance.desired.spec.interfaces.filter(nic => nic.purpose === 'contact');
  instance.desired.spec.routing.routes = [];
  const { writes } = mount(data);
  moreItem(/^使用授权/);
  const dialog = await screen.findByRole('dialog', { name: '容器使用授权' });
  expect(within(dialog).getByText('请先发布收回授权，并等待关机和网络清理确认。')).toBeTruthy();
  expect(within(dialog).getByRole('combobox', { name: '使用用户' }).hasAttribute('disabled')).toBe(true);
  expect(within(dialog).getByRole('button', { name: '保存使用授权' }).hasAttribute('disabled')).toBe(true);
  expect(writes).toHaveLength(0);
});

it.each([true, false])('gates a completed withdrawal on outstanding NIC bindings: %s', async withdrawing => {
  const data = cvmWorkspace('stopped');
  const instance = data.instances[0];
  instance.user_id = null;
  instance.operation_kind = 'assign';
  instance.desired.spec.interfaces = instance.desired.spec.interfaces.filter(nic => nic.purpose === 'contact');
  instance.desired.spec.routing.routes = [];
  const { writes } = mount(
    data,
    undefined,
    withdrawing
      ? [
          {
            consumer_fresh: true,
            provider_fresh: false,
            id: 'old-binding',
            interface_id: 'nic-business',
            transport: 'xray',
            generation: 1,
            phase: 'withdrawing',
            consumer: null,
            provider: null,
            error: null,
          },
        ]
      : [],
  );
  moreItem(/^使用授权/);
  const dialog = await screen.findByRole('dialog', { name: '容器使用授权' });
  const choice = within(dialog).getByRole('combobox', { name: '使用用户' });
  if (withdrawing) {
    expect(await within(dialog).findByText('正在等待访问入口和业务网卡清理；确认完成后才能授权。')).toBeTruthy();
    expect(choice.hasAttribute('disabled')).toBe(true);
    expect(writes).toHaveLength(0);
  } else {
    await waitFor(() => expect(choice.hasAttribute('disabled')).toBe(false));
    fireEvent.change(choice, { target: { value: 'alice' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '保存使用授权' }));
    await waitFor(() => expect(writes).toMatchObject([{ action: 'assign', user_id: 'alice' }]));
  }
});

it.each(['user', 'readonly', 'public'])('hides artifact requests and unauthorized actions for %s', async role => {
  // Deliberately pass an unredacted administrator payload to exercise the presentation boundary.
  const data = cvmWorkspace('detail');
  data.can_write = false;
  data.can_publish = false;
  data.can_use = role === 'user';
  data.redacted = role !== 'user';
  const { fetch } = mount(data);
  expect(screen.queryByRole('tab', { name: '产物' })).toBeNull();
  expect(screen.queryByRole('button', { name: /产物/ })).toBeNull();
  expect(screen.queryByRole('button', { name: '实例更多操作' })).toBeNull();
  if (data.redacted) {
    expect(screen.queryByRole('button', { name: '终端' })).toBeNull();
    expect(screen.queryByRole('button', { name: '关机' })).toBeNull();
    expect(screen.queryByRole('button', { name: '查看登录密码' })).toBeNull();
    await screen.findByText('近 1 小时没有采样记录。');
    expect(screen.queryByText(/runtime-secret|volume-secret|192\.0\.2\.21|op-first/)).toBeNull();
    fireEvent.click(screen.getByRole('tab', { name: '监控与诊断' }));
    await screen.findByText('实例监控采集失败。');
    expect(screen.queryByText(/runtime-secret|volume-secret|192\.0\.2\.21|op-first/)).toBeNull();
    expect(fetch.mock.calls.some(([url]) => url.endsWith('/entries'))).toBe(false);
  } else {
    expect(screen.getByRole('button', { name: '查看登录密码' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '关机' })).toBeTruthy();
  }
  expect(fetch.mock.calls.some(([url]) => url.includes('/artifacts'))).toBe(false);
});
