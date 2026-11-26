import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { CvmPane } from '../../src/panes/cvm';
import { navigate, type Loc } from '../../src/forge/route';
import { useState } from 'react';
import TerminalWindow from '../../src/panes/cvm/terminal-window';
import { SessionProvider } from '../../src/session';
import { routeForViewer } from '../../src/forge/access';
import type { CvmWorkspace, CvmMonitoring, CvmEntry, Whoami } from '../../src/api';
import type { CvmNetworkEditRequest } from '../../src/api/cvm-network-edit';
import { nicCatalog, nicState } from '../fixtures/cvm/network-data';
import type { Win } from '../../src/wm/store';
import { baremetalWorkspace, cvmReadings, cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import { artifactHistory } from '../fixtures/cvm/artifact-data';

vi.mock('../../src/panes/cvm/monitoring-charts', () => ({
  default: () => <div>监控曲线</div>,
  CvmMetricHistory: () => <div>概览历史</div>,
}));
vi.mock('../../src/forge/route', () => ({ navigate: vi.fn() }));
vi.mock('../../src/panes/cvm/terminal', () => ({
  default: ({ onClose }: { onClose: () => void }) => (
    <div role="dialog" aria-label="实例终端会话">
      <button onClick={onClose}>关闭终端</button>
    </div>
  ),
}));
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
const who: Whoami = {
  operator_id: 'admin',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};
function mount(
  data: CvmWorkspace,
  drill: Loc['drill'] = {},
  identity = who,
  failure?: number,
  monitoring?: CvmMonitoring,
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const writes: { url: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const url = String(input);
      if (url.endsWith('/artifacts/preview'))
        return Response.json({ instance: data.instances[0], artifacts: artifactHistory(), no_op: false });
      if (init?.method === 'PUT' && /^\/cvm\/hosts\/[^/]+$/.test(url)) {
        const body = JSON.parse(String(init.body));
        writes.push({ url, body });
        if (failure) return Response.json({ error: '宿主身份已被其他操作更新，请刷新后重试' }, { status: failure });
        const next = structuredClone(data);
        const node = decodeURIComponent(url.split('/')[3]);
        const host = [...next.hosts, ...(next.available_hosts ?? [])].find(item => item.node_id === node);
        if (!host?.can_manage_host) return Response.json({ error: '无权管理宿主' }, { status: 403 });
        if (next.instances.some(item => item.node_id === node && item.reserved))
          return Response.json({ error: '请先删除该宿主上的实例，并等待系统盘和网络资源清理完成' }, { status: 409 });
        host.host_revision += 1;
        next.hosts = [...next.hosts.filter(item => item.node_id !== node), ...(body.enabled ? [host] : [])];
        next.available_hosts = [
          ...(next.available_hosts ?? []).filter(item => item.node_id !== node),
          ...(!body.enabled ? [host] : []),
        ];
        Object.assign(data, next);
        return Response.json(next);
      }
      if (init?.method === 'PUT' && url.endsWith('/terminal')) {
        const body = JSON.parse(String(init.body));
        writes.push({ url, body });
        if (failure) return Response.json({ error: 'WebShell 设置已更新，请刷新后重试' }, { status: failure });
        const next = structuredClone(data);
        const host = next.hosts.find(host => url.includes(encodeURIComponent(host.node_id)));
        if (!host?.can_manage_terminal) return Response.json({ error: '无权管理 WebShell' }, { status: 403 });
        host.terminal_enabled = body.enabled;
        host.terminal_revision = host.terminal_revision + 1;
        Object.assign(data, next);
        return Response.json(next);
      }
      if (url.endsWith('/credential'))
        return Response.json({ instance_id: data.instances[0].desired.id, revision: 1, password: 'F'.repeat(32) });
      if (init?.method === 'PUT' && url.endsWith('/network-edit?preview=true'))
        return Response.json({ instance: data.instances[0], mounted_interfaces: {}, input_sha256: 'a'.repeat(64) });
      if (init?.method === 'POST' || (init?.method === 'PUT' && url.endsWith('/network-edit'))) {
        writes.push({ url, body: JSON.parse(String(init.body)) });
        return Response.json({ error: '执行结果尚未确认' }, { status: failure ?? 503 });
      }
      if (url === '/cvm/readings') return Response.json(cvmReadings(data));
      if (url.startsWith('/cvm/chain-egress')) return Response.json({ chains: [], supported: false });
      if (url === '/cvm') return Response.json(data);
      if (url === '/cvm/networks') return Response.json(nicCatalog(data.can_manage_network));
      if (url === '/cvm/entries') return Response.json(entryBoard(data, 'list'));
      if (url.endsWith('/entries')) return Response.json(entryBoard(data, 'list').entries);
      if (url.endsWith('/network')) return Response.json(nicState(data.instances[0], data.redacted));
      if (url.endsWith('/tcp-pings')) return Response.json([]);
      if (url.split('?')[0].endsWith('/monitoring'))
        return Response.json(
          monitoring ??
            ({
              state: 'unavailable',
              error: null,
              latest: null,
              health: null,
              samples_truncated: false,
              issues: [],
              events: [],
              range_start_unix_millis: Date.now() - 3600000,
              range_end_unix_millis: Date.now(),
              samples: [],
            } satisfies CvmMonitoring),
        );
      if (url === '/whoami') return Response.json(identity);
      throw new Error(`Unexpected 容器 request ${url}`);
    }),
  );
  const win: Win = {
    id: 1,
    key: 'tab:cvm',
    title: '容器',
    x: 0,
    y: 0,
    w: 1200,
    h: 900,
    z: 1,
    min: false,
    home: 'desk',
    data: {},
  };
  const route = vi.mocked(navigate).mockClear();
  function RoutedPane() {
    // The production shell feeds the new drill back into the same pane on route changes.
    const [current, setCurrent] = useState<Loc['drill']>(drill);
    route.mockImplementation((nav, next) => {
      if (nav === 'cvm') setCurrent(routeForViewer({ nav, drill: next }, identity).drill);
      return true;
    });
    return <CvmPane win={{ ...win, data: { drill: current } }} />;
  }
  render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who: identity, initial: { node_count: 1, chain_group_count: [] } }}>
        <RoutedPane />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return { writes, client };
}
async function fillCreate() {
  fireEvent.change(await screen.findByRole('textbox', { name: '容器名称' }), { target: { value: 'my-cvm' } });
}
it('separates instance and host pages without removing creation and terminal management', async () => {
  const data = cvmWorkspace('list');
  mount(data);
  const instances = await screen.findByRole('tab', { name: '容器' });
  expect(instances.getAttribute('aria-selected')).toBe('true');
  expect(screen.queryByRole('table', { name: '宿主' })).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  const hosts = screen.getByRole('table', { name: '宿主' });
  const row = within(hosts).getByRole('button', { name: data.hosts[0].name });
  expect(row.getAttribute('aria-expanded')).toBe('false');
  expect(screen.getByText('1 / 8', { exact: true })).toBeTruthy();
  expect(screen.getByText('会话数未上报')).toBeTruthy();
  expect(screen.getByRole('switch', { name: `${data.hosts[0].name} WebShell` })).toBeTruthy();
  fireEvent.click(row);
  expect(row.getAttribute('aria-expanded')).toBe('true');
  expect(screen.getByText(/网卡上限/)).toBeTruthy();
  expect(await screen.findByText('线路出口', { exact: false })).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '在此创建' }));
  expect(navigate).toHaveBeenLastCalledWith('cvm', { p: 'new', host: data.hosts[0].node_id });
  expect(screen.getByRole('heading', { name: '创建容器' })).toBeTruthy();
});

it('removes only the host identity and lets an administrator explicitly add it back', async () => {
  const data = cvmWorkspace('new');
  const node = data.hosts[0].node_id;
  const name = data.hosts[0].name;
  const { writes, client } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name }));
  fireEvent.click(screen.getByRole('button', { name: '移除宿主…' }));
  const dialog = screen.getByRole('dialog', { name: '移除宿主' });
  expect(writes).toEqual([]);
  fireEvent.click(within(dialog).getByRole('button', { name: '确认移除' }));
  await screen.findByText('尚无宿主');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(client.getQueryData<CvmWorkspace>(['cvm'])?.hosts).toHaveLength(0);
  expect(writes[0]).toEqual({ url: `/cvm/hosts/${node}`, body: { enabled: false, expected_revision: 0 } });
  fireEvent.click(screen.getByRole('tab', { name: '容器' }));
  expect((screen.getByRole('button', { name: '创建容器' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  fireEvent.change(screen.getByRole('combobox', { name: '添加宿主' }), { target: { value: node } });
  await screen.findByRole('button', { name });
  expect(writes[1]).toEqual({ url: `/cvm/hosts/${node}`, body: { enabled: true, expected_revision: 1 } });
  expect(screen.queryByRole('combobox', { name: '添加宿主' })).toBeNull();
});

it('keeps an occupied host visible and prevents a removal request', async () => {
  const data = cvmWorkspace('list');
  const { writes } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name: data.hosts[0].name }));
  const remove = screen.getByRole<HTMLButtonElement>('button', { name: '移除宿主…' });
  expect(remove.disabled).toBe(true);
  expect(screen.getByText(/等待资源清理完成/)).toBeTruthy();
  fireEvent.click(remove);
  expect(writes).toEqual([]);
});

it('shows the server conflict when removal is rejected after confirmation', async () => {
  const data = cvmWorkspace('new');
  const name = data.hosts[0].name;
  mount(data, {}, who, 409);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name }));
  fireEvent.click(screen.getByRole('button', { name: '移除宿主…' }));
  fireEvent.click(screen.getByRole('button', { name: '确认移除' }));
  expect((await screen.findByRole('alert')).textContent).toContain('宿主身份已被其他操作更新');
  expect(screen.getByRole('button', { name })).toBeTruthy();
});

it('does not offer host identity management to an ordinary user', async () => {
  const data = cvmWorkspace('list', 'user');
  mount(data, {}, { ...who, role: 'user' });
  await screen.findByRole('region', { name: '容器清单' });
  expect(screen.queryByRole('tab', { name: '宿主' })).toBeNull();
  expect(routeForViewer({ nav: 'cvm', drill: { p: 'hosts' } }, { ...who, role: 'user' })).toEqual({ nav: 'cvm' });
  expect(screen.queryByRole('button', { name: '移除宿主…' })).toBeNull();
  expect(screen.queryByRole('button', { name: /网络配置日志/ })).toBeNull();
  expect(screen.queryByRole('combobox', { name: '添加宿主' })).toBeNull();
});

it('moves between page tabs with arrow and boundary keys', async () => {
  mount(cvmWorkspace('list'));
  const instances = await screen.findByRole('tab', { name: '容器' });
  const hosts = screen.getByRole('tab', { name: '宿主' });
  fireEvent.keyDown(instances, { key: 'End' });
  const networks = screen.getByRole('tab', { name: '网络' });
  expect(networks.getAttribute('aria-selected')).toBe('true');
  fireEvent.keyDown(networks, { key: 'Home' });
  expect(instances.getAttribute('aria-selected')).toBe('true');
  fireEvent.keyDown(instances, { key: 'ArrowRight' });
  expect(hosts.getAttribute('aria-selected')).toBe('true');
  fireEvent.keyDown(hosts, { key: 'Home' });
  expect(instances.getAttribute('aria-selected')).toBe('true');
});

it('counts only the host’s visible instances and excludes confirmed deletions', async () => {
  const data = cvmWorkspace('list');
  const foreign = structuredClone(data.instances[0]);
  foreign.node_id = 'other-host';
  foreign.desired.id = 'other-instance';
  foreign.desired.spec.name = 'other-workspace';
  data.instances.push(foreign);
  const removed = structuredClone(data.instances[0]);
  removed.desired.id = 'deleted-instance';
  removed.desired.power = 'deleted';
  data.instances.push(removed);
  mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name: data.hosts[0].name }));
  const instances = screen.getByRole('heading', { name: '工作负载', level: 3 }).parentElement!;
  expect(within(instances).getAllByRole('button')).toHaveLength(1);
  expect(within(instances).getByRole('button', { name: new RegExp(data.instances[0].desired.spec.name) })).toBeTruthy();
  expect(screen.queryByText('能力报告新鲜')).toBeNull();
  expect(screen.queryByText('等待新的能力报告')).toBeNull();
});

it.each(['stale', 'error'] as const)('does not advertise creation from a %s host report', async state => {
  const data = cvmWorkspace('list');
  if (state === 'stale') data.hosts[0].fresh = false;
  else data.hosts[0].error = '执行能力暂不可用';
  mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.getByRole('status').textContent).toBe(state === 'stale' ? '能力已过期' : '不可用');
  expect(screen.queryByText('可创建', { exact: true })).toBeNull();
});

it('lists only machines with the container stack, never plain proxy machines', async () => {
  const data = cvmWorkspace('list');
  const proxy = structuredClone(data.hosts[0]);
  proxy.node_id = 'proxy-without-stack';
  proxy.name = '仅代理机器';
  proxy.error = 'CVM Rust executor configuration is unavailable';
  proxy.capabilities = { ...proxy.capabilities, backend: '', create: false };
  data.hosts.push(proxy);
  mount(data);
  expect(await screen.findByRole('region', { name: '容器清单' })).toBeTruthy();
  expect(screen.queryByText('仅代理机器')).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  const hosts = screen.getByRole('table', { name: '宿主' });
  expect(within(hosts).queryByText('仅代理机器')).toBeNull();
  expect(within(hosts).queryByText('执行能力不可用')).toBeNull();
});

it('keeps an empty host tab available without claiming an instance exists', async () => {
  mount(cvmWorkspace('empty'));
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.getByText('尚无宿主')).toBeTruthy();
  expect(screen.queryByRole('table', { name: '按宿主分组的容器' })).toBeNull();
});

it('locks creation fields and retries the identical request after an uncertain response', async () => {
  const { writes } = mount(cvmWorkspace('new'), { p: 'new' });
  await fillCreate();
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  await screen.findByText(/上次创建结果尚未确认/);
  expect(screen.getByRole('textbox', { name: '容器名称' }).matches(':disabled')).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: '重试保存创建变更' }));
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[0].body.request_id).toBeTruthy();
  expect(writes[1]).toEqual(writes[0]);
});

it('does not render stale actual power as successful, and preserves the final contact', async () => {
  const data = cvmWorkspace('unknown');
  mount(data, { p: 'instance', id: 'cvm-alice' });
  expect((await screen.findByRole('status')).textContent).toBe('待核查');
  expect(screen.queryByText('运行中')).toBeNull();
});
it('keeps cleanup available on an unhealthy machine while disabling new runtime changes', async () => {
  const data = cvmWorkspace('unknown');
  data.hosts[0].fresh = false;
  data.hosts[0].error = 'network gateway unavailable';
  mount(data, { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  expect((screen.getByRole('menuitem', { name: /^删除/ }) as HTMLButtonElement).disabled).toBe(false);
  fireEvent.keyDown(screen.getByRole('menu'), { key: 'Escape' });
  expect((screen.getByRole('button', { name: '核查并重试原任务' }) as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  expect((screen.getByRole('button', { name: '挂载网卡' }) as HTMLButtonElement).disabled).toBe(true);
});
it.each(['reader', 'public'])('keeps read-only %s identities outside creation while allowing detail', operatorId => {
  const viewer: Whoami = { ...who, operator_id: operatorId, role: 'readonly', masked_assets: true };
  expect(routeForViewer({ nav: 'cvm', drill: { p: 'new' } }, viewer)).toEqual({ nav: 'cvm' });
  const location = { nav: 'cvm' as const, drill: { p: 'instance', id: 'cvm-alice' } };
  expect(routeForViewer(location, viewer)).toEqual(location);
});

it('shows guests the read-only list but not the host or network administration routes', async () => {
  const { writes } = mount(
    cvmWorkspace('list', 'public'),
    {},
    {
      ...who,
      operator_id: 'public',
      role: 'readonly',
      masked_assets: true,
    },
  );
  await screen.findByRole('table', { name: '按宿主分组的容器' });
  expect(screen.queryByRole('button', { name: '创建容器' })).toBeNull();
  expect(screen.queryByRole('tab', { name: '宿主' })).toBeNull();
  expect(screen.queryByRole('tab', { name: '网络' })).toBeNull();
  expect(screen.queryByRole('table', { name: '宿主' })).toBeNull();
  expect(writes).toEqual([]);
});

it('lets guests inspect redacted details and monitoring without instance writes or terminal access', async () => {
  const { writes } = mount(
    cvmWorkspace('detail', 'public'),
    { p: 'instance', id: 'cvm-alice' },
    {
      ...who,
      operator_id: 'public',
      role: 'readonly',
      masked_assets: true,
    },
  );
  await screen.findByRole('tab', { name: '概览' });
  for (const name of ['实例更多操作', '终端']) expect(screen.queryByRole('button', { name })).toBeNull();
  expect(screen.getByText('连接地址已脱敏。')).toBeTruthy();
  expect(screen.queryByText('192.0.2.21/32')).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  const network = within(screen.getByRole('tabpanel', { name: '网络' }));
  expect(network.getByText('访问地址已脱敏。')).toBeTruthy();
  expect(network.queryByRole('button', { name: '挂载网卡' })).toBeNull();
  expect(network.queryByText(/GreenVPS|remote-ingress|203\.0\.113\.20/)).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '监控与诊断' }));
  expect(await screen.findByText('尚无实例监控数据。宿主更新采集能力后会自动上报。')).toBeTruthy();
  expect(writes).toEqual([]);
});

it('CPU and memory are independent choices and survive a system image change', async () => {
  const data = cvmWorkspace('new');
  data.hosts[0].capabilities.images.push({
    ...data.hosts[0].capabilities.images[0],
    id: 'ubuntu',
    name: 'Ubuntu',
    version: '26.04',
  });
  const { writes } = mount(data, { p: 'new' });
  await fillCreate();
  fireEvent.change(screen.getByRole('spinbutton', { name: 'CPU（核）' }), { target: { value: '3' } });
  fireEvent.change(screen.getByRole('spinbutton', { name: '内存（MiB）' }), { target: { value: '768' } });
  fireEvent.click(screen.getByRole('button', { name: /Ubuntu 26.04/ }));
  expect((screen.getByRole('spinbutton', { name: 'CPU（核）' }) as HTMLInputElement).value).toBe('3');
  expect((screen.getByRole('spinbutton', { name: '内存（MiB）' }) as HTMLInputElement).value).toBe('768');
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body.resources).toEqual({ vcpus: 3, memory_bytes: 768 * 1024 ** 2, root_disk_bytes: 3 * 1024 ** 3 });
  expect(writes[0].body.profile_id).toBeUndefined();
});

it('shows actual running state and requires confirmation before a durable restart', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  // 运行状态是标题图标角上的状态灯，与机器详情一致。
  await screen.findByRole('img', { name: '运行中' });
  expect(screen.queryByText(/期望运行/)).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '重启' }));
  const dialog = screen.getByRole('dialog', { name: '重启实例' });
  expect(writes).toHaveLength(0);
  fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body.action).toBe('restart');
});

it('offers independent CPU and memory editing only after shutdown', async () => {
  const { writes } = mount(cvmWorkspace('stopped'), { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /修改配置/ }));
  const dialog = screen.getByRole('dialog', { name: '修改配置' });
  fireEvent.change(within(dialog).getByRole('spinbutton', { name: 'CPU（核）' }), { target: { value: '3' } });
  fireEvent.change(within(dialog).getByRole('spinbutton', { name: '内存（MiB）' }), { target: { value: '768' } });
  const save = within(dialog).getByRole('button', { name: '保存变更' }) as HTMLButtonElement;
  expect(save.disabled).toBe(false);
  expect(writes).toHaveLength(0);
  fireEvent.click(save);
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ action: 'configure', vcpus: 3, memory_bytes: 768 * 1024 ** 2 });
  expect(writes[0].body.image_id).toBeUndefined();
});

it('preserves edits and refuses to silently rebase an open configuration dialog', async () => {
  const data = cvmWorkspace('stopped');
  const { client, writes } = mount(data, { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /修改配置/ }));
  const dialog = screen.getByRole('dialog', { name: '修改配置' });
  const cpu = within(dialog).getByRole('spinbutton', { name: 'CPU（核）' }) as HTMLInputElement;
  fireEvent.change(cpu, { target: { value: '3' } });
  expect(within(dialog).getByLabelText('未保存的修改')).toBeTruthy();
  const latest = structuredClone(data);
  latest.instances[0].desired.generation += 1;
  latest.instances[0].desired.spec.resources.vcpus = 2;
  await act(async () => client.setQueryData(['cvm'], latest));
  await within(dialog).findByRole('button', { name: '载入最新配置并替换当前输入' });
  expect(cpu.value).toBe('3');
  expect((within(dialog).getByRole('button', { name: '保存变更' }) as HTMLButtonElement).disabled).toBe(true);
  expect(writes).toHaveLength(0);
  fireEvent.click(within(dialog).getByRole('button', { name: '载入最新配置并替换当前输入' }));
  expect(cpu.value).toBe('2');
  expect(within(dialog).queryByLabelText('未保存的修改')).toBeNull();
  fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body.expected_generation).toBe(latest.instances[0].desired.generation);
});

it.each(['restart', 'configure'] as const)(
  'replays the original %s request after uncertainty and a version refresh',
  async action => {
    const data = cvmWorkspace(action === 'configure' ? 'stopped' : 'detail');
    const { client, writes } = mount(data, { p: 'instance', id: 'cvm-alice' });
    if (action === 'configure') {
      fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
      fireEvent.click(screen.getByRole('menuitem', { name: /修改配置/ }));
      fireEvent.change(screen.getByRole('spinbutton', { name: 'CPU（核）' }), { target: { value: '3' } });
    } else fireEvent.click(await screen.findByRole('button', { name: '重启' }));
    const name = action === 'configure' ? '修改配置' : '重启实例';
    let dialog = screen.getByRole('dialog', { name });
    fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
    await within(dialog).findByText(/上次保存结果尚未确认/);
    const latest = structuredClone(data);
    latest.instances[0].desired.generation += 1;
    if (action === 'configure') latest.instances[0].desired.spec.resources.vcpus = 3;
    await act(async () => client.setQueryData(['cvm'], latest));
    expect((within(dialog).getByRole('button', { name: '保存变更' }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1]).toEqual(writes[0]);
    await within(dialog).findByText(/上次保存结果尚未确认/);
    fireEvent.click(within(dialog).getByRole('button', { name: '取消' }));
    const current = client.getQueryData<CvmWorkspace>(['cvm'])!;
    current.instances[0].status = 'applied';
    current.instances[0].revision_id = current.instances[0].published_revision_id;
    await act(async () => client.setQueryData(['cvm'], structuredClone(current)));
    if (action === 'configure') {
      fireEvent.click(screen.getByRole('button', { name: '实例更多操作' }));
      fireEvent.click(screen.getByRole('menuitem', { name: /修改配置/ }));
    } else fireEvent.click(screen.getByRole('button', { name: '重启' }));
    dialog = screen.getByRole('dialog', { name });
    fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
    await waitFor(() => expect(writes).toHaveLength(3));
    expect(writes[2]).toEqual(writes[0]);
  },
);

it('keeps the staged entry visible and locks draft editing after an uncertain save', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  const { listen, target, add } = await openSharedEntryForm();
  fireEvent.change(listen, { target: { value: '30040' } });
  fireEvent.change(target, { target: { value: '30040 → 8080' } });
  fireEvent.click(add);
  const list = screen.getByRole('list', { name: '访问入口列表' });
  expect(await within(list).findByText('203.0.113.20 · TCP 30040；映射 30040 → 8080', { exact: true })).toBeTruthy();
  expect(writes).toEqual([]);
  fireEvent.click(await screen.findByRole('button', { name: '保存网络变更' }));
  await screen.findByText('执行结果尚未确认');
  expect(within(list).getByText('203.0.113.20 · TCP 30040；映射 30040 → 8080', { exact: true })).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>('button', { name: '丢弃当前草稿' }).disabled).toBe(true);
  expect(screen.getByRole('button', { name: '重试原保存' })).toBeTruthy();
  expect(writes[0]).toMatchObject({
    url: '/cvm/cvm-alice/network-edit',
    body: { entry_additions: [{ mode: { tcp: { mappings: [{ target: { start: 8080, end: 8080 } }] } } }] },
  });
});

it.each(['pending', 'applied'] as const)('marks saved work independently of Guest status (%s)', async status => {
  const data = cvmWorkspace('detail');
  data.pending_revision_id = 42;
  data.can_publish = true;
  Object.assign(data.instances[0], {
    revision_id: 42,
    published_revision_id: 41,
    operation_kind: 'configure',
    status,
  });
  mount(data, { p: 'instance', id: 'cvm-alice' });
  expect(await screen.findByRole('img', { name: '运行中' })).toBeTruthy();
  expect(screen.getByText('待发布', { selector: '[role="status"]' }).className).toContain('st-pending');
  expect(screen.getByRole('button', { name: '查看发布计划' })).toBeTruthy();
});

it('explains data loss and confirms a saved reinstall', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /重装系统/ }));
  const dialog = screen.getByRole('dialog', { name: '重装系统' });
  expect(within(dialog).getByText(/重装会清除系统盘中的全部数据/)).toBeTruthy();
  expect(within(dialog).getByText(/保存后在「发布」中统一应用/)).toBeTruthy();
  expect(writes).toHaveLength(0);
  fireEvent.click(within(dialog).getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ action: 'reinstall', image_id: 'debian-13' });
});

it.each([false, true])(
  'confirms full deletion and retries the same request with retain_on_delete=%s',
  async retainOnDelete => {
    const data = cvmWorkspace('detail');
    data.hosts[0].capabilities.retain_on_delete = retainOnDelete;
    const { writes } = mount(data, { p: 'instance', id: 'cvm-alice' });
    fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: /^删除/ }));
    const dialog = screen.getByRole('dialog', { name: '删除容器？' });
    expect(writes).toHaveLength(0);
    const save = within(dialog).getByRole<HTMLButtonElement>('button', { name: '保存删除变更' });
    const name = within(dialog).getByRole('textbox', { name: '输入完整容器名称以确认删除' });
    fireEvent.change(name, { target: { value: 'home-work' } });
    expect(save.disabled).toBe(true);
    fireEvent.change(name, { target: { value: data.instances[0].desired.spec.name } });
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0].url).toBe('/cvm/cvm-alice/operations');
    expect(writes[0].body).toMatchObject({
      action: 'delete',
      delete_data: true,
      expected_generation: data.instances[0].desired.generation,
    });
    fireEvent.click(await within(dialog).findByRole('button', { name: '重试原请求' }));
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1].body).toEqual(writes[0].body);
  },
);

it('the shared shell list drill retains creation and quota actions without user forwarding', async () => {
  mount(cvmWorkspace('list'), { p: 'list' });
  expect(await screen.findByRole('tab', { name: '网络' })).toBeTruthy();
  expect(screen.queryByRole('tab', { name: '额度' })).toBeNull();
  expect(screen.getByRole('button', { name: '创建容器' })).toBeTruthy();
  expect(screen.queryByRole('menuitem', { name: /连接已有机器/ })).toBeNull();
  expect(screen.queryByRole('button', { name: '返回列表' })).toBeNull();
});

it('allows clearing a capacity input without substituting zero or changing another field', async () => {
  mount(cvmWorkspace('new'), { p: 'new' });
  await fillCreate();
  const cpu = screen.getByRole('spinbutton', { name: 'CPU（核）' }) as HTMLInputElement;
  const memory = screen.getByRole('spinbutton', { name: '内存（MiB）' }) as HTMLInputElement;
  fireEvent.change(cpu, { target: { value: '' } });
  expect(cpu.value).toBe('');
  expect(memory.value).toBe('128');
  fireEvent.change(cpu, { target: { value: '3' } });
  expect(cpu.value).toBe('3');
});

it('shows measured Guest capacity and per-NIC rates, not configured memory as usage', async () => {
  const { cvmMonitoring } = await import('../fixtures/cvm/data');
  const data = cvmMonitoring();
  mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' }, who, undefined, data);
  const monitoring = await screen.findByRole('tabpanel', { name: '概览' });
  await within(monitoring).findByText(/持续采样/);
  const memory = within(monitoring).getByRole('button', { name: /内存/ });
  expect(memory.textContent).toContain(
    String(Math.round((data.latest!.memory_used_bytes / data.latest!.memory_total_bytes) * 1000) / 10),
  );
  fireEvent.click(screen.getByRole('button', { name: '监控与诊断' }));
  const diagnostics = within(screen.getByRole('region', { name: '容器 监控' }));
  expect(await diagnostics.findByRole('option', { name: 'enp0s2 · 副网卡' })).toBeTruthy();
  fireEvent.change(diagnostics.getByLabelText('监控网卡'), { target: { value: 'nic-business' } });
  expect((diagnostics.getByLabelText('监控网卡') as HTMLSelectElement).value).toBe('nic-business');
});
it('marks expired samples and never shows their CPU or network rates as live', async () => {
  const { cvmMonitoring } = await import('../fixtures/cvm/data');
  mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' }, who, undefined, cvmMonitoring('stale'));
  const monitoring = await screen.findByRole('tabpanel', { name: '概览' });
  await within(monitoring).findAllByText(/读数已过期/);
  expect(within(monitoring).getByRole('group', { name: '容器读数已停止更新' })).toBeTruthy();
  expect(within(monitoring).queryByText(/持续采样/)).toBeNull();
  const readings = within(within(monitoring).getByRole('group', { name: '容器读数已停止更新' }));
  expect(readings.getByRole('button', { name: /CPU/ }).textContent).toBe('CPU—');
  expect(readings.getByRole('button', { name: /网络/ }).textContent).toBe('网络—');
});

it('waits for independent health instead of treating a lifecycle receipt as a shutdown observation', async () => {
  mount(cvmWorkspace('stopped'), { p: 'instance', id: 'cvm-alice' });
  const monitoring = await screen.findByRole('tabpanel', { name: '概览' });
  await within(monitoring).findByText('等待采样');
  expect(within(monitoring).queryByText('已关机')).toBeNull();
});

it('updates terminal launcher availability without reopening an independent page', async () => {
  const child = { closed: true, focus: vi.fn() } as unknown as Window;
  const open = vi.spyOn(window, 'open').mockReturnValue(child);
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '终端' }));
  expect(open).toHaveBeenCalledTimes(1);
  expect(open.mock.calls[0]).toEqual([expect.stringContaining('webshell=cvm-alice'), '_blank']);
  const stopped = structuredClone(data);
  stopped.instances[0].desired.power = 'stopped';
  stopped.instances[0].report!.power = 'stopped';
  await act(async () => client.setQueryData(['cvm'], stopped));
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(true));
  await act(async () => client.setQueryData(['cvm'], data));
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(false));
  expect(open).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull();
});

it('the independent terminal page tears down when host policy or instance power changes', async () => {
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  render(
    <QueryClientProvider client={client}>
      <TerminalWindow id="cvm-alice" />
    </QueryClientProvider>,
  );
  await screen.findByRole('dialog', { name: '实例终端会话' });
  const stopped = structuredClone(data);
  stopped.instances[0].report!.power = 'stopped';
  stopped.instances[0].desired.power = 'stopped';
  await act(async () => client.setQueryData(['cvm'], stopped));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
  expect(screen.getByRole('alert').textContent).toContain('运行中的实例');
  await act(async () => client.setQueryData(['cvm'], data));
  await screen.findByRole('dialog', { name: '实例终端会话' });
  const revoked = structuredClone(data);
  revoked.hosts[0].terminal_enabled = false;
  await act(async () => client.setQueryData(['cvm'], revoked));
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
  expect(screen.getByRole('alert').textContent).toContain('WebShell 已关闭');
});

it('retains the terminal on a transient workspace refresh failure but tears it down on permission denial', async () => {
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  render(
    <QueryClientProvider client={client}>
      <TerminalWindow id="cvm-alice" />
    </QueryClientProvider>,
  );
  await screen.findByRole('dialog', { name: '实例终端会话' });
  const previousFetch = globalThis.fetch;
  let status = 503;
  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
      String(input) === '/cvm'
        ? Promise.resolve(Response.json({ error: 'workspace refresh fixture' }, { status }))
        : previousFetch(input, init),
    ),
  );
  await act(async () => {
    await client.invalidateQueries({ queryKey: ['cvm'], exact: true });
  });
  expect(screen.getByRole('dialog', { name: '实例终端会话' })).toBeTruthy();
  status = 403;
  await act(async () => {
    await client.invalidateQueries({ queryKey: ['cvm'], exact: true });
  });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
});

it('opens diagnostics while retaining overview state and requests real history ranges', async () => {
  const { cvmMonitoring } = await import('../fixtures/cvm/data');
  const data = cvmMonitoring();
  data.health = {
    id: 'cvm-alice',
    generation: 1,
    observed_at_unix_millis: Date.now(),
    runtime_id: 'cube-1',
    runtime_state: 'running',
    guest_state: 'reachable',
    ssh_state: 'unreachable',
    services: [{ id: 'rule-1', state: 'unreachable', latency_millis: null }],
    error: null,
  };
  data.issues = [{ kind: 'ssh', severity: 'warn', message: '宿主无法连接实例 SSH 端口' }];
  mount(cvmWorkspace('unknown'), { p: 'instance', id: 'cvm-alice' }, who, undefined, data);
  await screen.findByText(/持续采样/);
  expect(screen.queryByText('监控曲线')).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '监控与诊断' }));
  await screen.findByText('监控曲线');
  expect(screen.getByText('宿主 → 实例 TCP 22')).toBeTruthy();
  expect(screen.getByText(/本地端口探测不代表公网链路可达/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '24 小时' }));
  await waitFor(() =>
    expect(fetch).toHaveBeenCalledWith('/cvm/cvm-alice/monitoring?window_secs=86400', expect.anything()),
  );
  fireEvent.keyDown(screen.getByRole('tab', { name: '监控与诊断' }), { key: 'Home' });
  expect(screen.getByRole('tab', { name: '概览' }).getAttribute('aria-selected')).toBe('true');
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  expect(screen.getByRole('region', { name: '路由与命中测试' })).toBeTruthy();
});

it('manages WebShell from the 容器 page and preserves versioned server state', async () => {
  const data = cvmWorkspace('list');
  const { writes } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  const dialog = screen.getByRole('table', { name: '宿主' });
  const toggle = within(dialog).getByRole('switch', { name: `${data.hosts[0].name} WebShell` });
  expect((toggle as HTMLInputElement).checked).toBe(true);
  fireEvent.click(toggle);
  await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(false));
  expect(writes[0]).toEqual({
    url: `/cvm/hosts/${data.hosts[0].node_id}/terminal`,
    body: { enabled: false, expected_revision: 1 },
  });
  fireEvent.click(toggle);
  await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(true));
  expect(writes[1].body).toEqual({ enabled: true, expected_revision: 2 });
});

it.each([true, false])('uses explicitly reported Guest terminal capability (%s)', async capability => {
  const data = cvmWorkspace('detail');
  data.hosts[0].capabilities.guest_terminal = capability;
  mount(data, { p: 'instance', id: data.instances[0].desired.id });
  const button = await screen.findByRole('button', { name: '终端' });
  expect((button as HTMLButtonElement).disabled).toBe(capability === false);
});

it('keeps the policy unchanged and shows a concurrent update error', async () => {
  const data = cvmWorkspace('list');
  mount(data, {}, who, 409);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  const dialog = screen.getByRole('table', { name: '宿主' });
  const toggle = within(dialog).getByRole('switch', { name: `${data.hosts[0].name} WebShell` });
  fireEvent.click(toggle);
  await within(dialog).findByText('WebShell 设置已更新，请刷新后重试');
  expect((toggle as HTMLInputElement).checked).toBe(true);
});

it('hides WebShell management from users without host policy access', async () => {
  const data = cvmWorkspace('list', 'user');
  mount(data, {}, { ...who, role: 'user' });
  await screen.findByRole('region', { name: '容器清单' });
  expect(screen.queryByRole('tab', { name: '宿主' })).toBeNull();
  expect(screen.queryByRole('switch')).toBeNull();
  expect(screen.queryByRole('button', { name: 'WebShell 管理' })).toBeNull();
});

it('disables the terminal launcher when host policy is revoked', async () => {
  const open = vi.spyOn(window, 'open').mockReturnValue({ closed: true, focus: vi.fn() } as unknown as Window);
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '终端' }));
  const disabled = structuredClone(data);
  disabled.hosts[0].terminal_enabled = false;
  await act(async () => client.setQueryData(['cvm'], disabled));
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(true));
  await act(async () => client.setQueryData(['cvm'], data));
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(false));
  expect(open).toHaveBeenCalledTimes(1);
});

it('opens the list terminal independently without navigating, and removes access when revoked', async () => {
  const open = vi.spyOn(window, 'open').mockReturnValue({ closed: true, focus: vi.fn() } as unknown as Window);
  const data = cvmWorkspace('list');
  const { client } = mount(data);
  const list = await screen.findByRole('table', { name: '按宿主分组的容器' });
  fireEvent.click(within(list).getByRole('button', { name: 'home-workspace 更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /^终端/ }));
  expect(open).toHaveBeenCalledTimes(1);
  expect(open.mock.calls[0][0]).toContain('webshell=cvm-alice');
  expect(navigate).not.toHaveBeenCalled();
  const revoked = structuredClone(data);
  revoked.hosts[0].terminal_enabled = false;
  await act(async () => client.setQueryData(['cvm'], revoked));
  await waitFor(() => expect(within(list).queryByRole('button', { name: '终端' })).toBeNull());
  fireEvent.click(within(list).getByRole('button', { name: 'home-workspace 更多操作' }));
  const actions = screen.getByRole('menu', { name: 'home-workspace 更多操作' });
  expect(within(actions).queryByRole('menuitem', { name: /^终端/ })).toBeNull();
  await act(async () => client.setQueryData(['cvm'], { ...data, can_write: false, can_use: false }));
  await waitFor(() => expect(screen.queryByRole('button', { name: '终端' })).toBeNull());
});

it('labels a bare-metal local exit as the existing host network', async () => {
  const data = baremetalWorkspace();
  mount(data);
  fireEvent.click(await screen.findByRole('button', { name: 'baremetal-workspace' }));
  expect(navigate).toHaveBeenCalledWith('cvm', { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  const nics = screen.getByRole('region', { name: '网卡' });
  expect(within(nics).getByText(/宿主原有网络/)).toBeTruthy();
  expect(within(nics).queryByText(/宿主机? NAT/)).toBeNull();
});

it.each(['system-admin', 'user'] as const)(
  'resolves network-only provider names for %s without capability access',
  async role => {
    const data = cvmWorkspace('detail', role);
    expect(data.hosts.some(host => host.node_id === 'remote-ingress')).toBe(false);
    if (role !== 'system-admin') expect(nicCatalog(false).providers).toEqual([]);
    mount(data, { p: 'instance', id: data.instances[0].desired.id }, { ...who, role });
    fireEvent.click(await screen.findByRole('tab', { name: '网络' }));
    const entries = screen.getByRole('list', { name: '访问入口列表' });
    expect((await within(entries).findAllByText(/GreenVPS · 香港网络/)).length).toBeGreaterThan(0);
    expect(entries.textContent).not.toContain('remote-ingress');
  },
);

it('loads fleet readings in one batch and keeps missing or expired metrics unknown', async () => {
  const data = cvmWorkspace('list');
  const { client } = mount(data);
  const row = await screen.findByRole('row', { name: /home-workspace/ });
  const metrics = () => Array.from(row.querySelectorAll('.cvm-list-metric'), el => el.textContent);
  await waitFor(() => expect(metrics()).toEqual(['9%', '47%']));
  const reads = vi.mocked(fetch).mock.calls.map(([url]) => String(url));
  expect(reads.filter(url => url === '/cvm/readings')).toHaveLength(1);
  expect(reads.filter(url => url.includes('/monitoring'))).toHaveLength(0);
  const readings = cvmReadings(data);
  const expired = structuredClone(readings);
  expired.instances[0].latest!.observed_at_unix_millis -= 180_000;
  await act(async () => client.setQueryData(['cvm-readings'], expired));
  await waitFor(() => expect(metrics()).toEqual(['—', '—']));
  await act(async () => client.setQueryData(['cvm-readings'], readings));
  await waitFor(() => expect(metrics()).toEqual(['9%', '47%']));
  await act(async () => client.setQueryData(['cvm-readings'], { ...readings, instances: [] }));
  await waitFor(() => expect(metrics()).toEqual(['—', '—']));
});

it('creates without credentials or a user quota and can leave the instance unassigned', async () => {
  const { writes } = mount(cvmWorkspace('new'), { p: 'new' });
  await fillCreate();
  expect(screen.queryByRole('textbox', { name: 'SSH 公钥' })).toBeNull();
  expect(screen.queryByLabelText('密码')).toBeNull();
  expect((screen.getByRole('combobox', { name: '使用者' }) as HTMLSelectElement).value).toBe('');
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ tenant_id: 'platform', user_id: null });
  expect(writes[0].body).not.toHaveProperty('ssh_public_key');
  expect(writes[0].body).not.toHaveProperty('password');
});

it('revokes use on the instance rather than editing a user quota', async () => {
  const data = cvmWorkspace('detail');
  const { writes } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '使用授权' }));
  const dialog = screen.getByRole('dialog', { name: '容器使用授权' });
  fireEvent.change(within(dialog).getByRole('combobox', { name: '使用用户' }), { target: { value: '' } });
  expect(within(dialog).getByText(/立即失去控制台访问；收回授权不可撤回/)).toBeTruthy();
  fireEvent.click(within(dialog).getByRole('button', { name: '收回授权' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({ url: '/cvm/cvm-alice/operations', body: { action: 'assign', user_id: null } });
});

it('limits ordinary users to use and lifecycle controls', async () => {
  const data = cvmWorkspace('detail', 'user');
  mount(data, { p: 'instance', id: data.instances[0].desired.id }, { ...who, role: 'user' });
  expect(await screen.findByRole('button', { name: '关机' })).toBeTruthy();
  expect(screen.getByRole('button', { name: '终端' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: '删除' })).toBeNull();
  expect(screen.queryByRole('button', { name: '实例更多操作' })).toBeNull();
  expect(screen.queryByRole('button', { name: '使用授权' })).toBeNull();
});

it('permits logical CPU memory and disk above the host baseline without user quota checks', async () => {
  const data = cvmWorkspace('list');
  const caps = data.hosts[0].capabilities;
  caps.allocatable_vcpus = 1;
  caps.allocatable_memory_bytes = 128 * 1024 ** 2;
  caps.allocatable_disk_bytes = 3 * 1024 ** 3;
  const { writes } = mount(data, { p: 'new' });
  await fillCreate();
  fireEvent.change(screen.getByRole('spinbutton', { name: 'CPU（核）' }), { target: { value: '2' } });
  fireEvent.change(screen.getByRole('spinbutton', { name: '内存（MiB）' }), { target: { value: '512' } });
  fireEvent.change(screen.getByRole('spinbutton', { name: '系统盘（GiB）' }), { target: { value: '8' } });
  expect(screen.queryByText(/超售/)).toBeNull();
  const create = screen.getByRole('button', { name: '保存创建变更' });
  expect((create as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(create);
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body.resources).toMatchObject({
    vcpus: 2,
    memory_bytes: 512 * 1024 ** 2,
    root_disk_bytes: 8 * 1024 ** 3,
  });
});

it('keeps host policy management reachable when creation and quota grants are unavailable', async () => {
  const data = cvmWorkspace('empty');
  const host = cvmWorkspace('list').hosts[0];
  host.capabilities.create = false;
  data.hosts = [host];
  mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.getByRole('switch', { name: `${host.name} WebShell` })).toBeTruthy();
});

it('merges concurrent host policy replies without replacing another host or newer instance state', async () => {
  const data = cvmWorkspace('list');
  data.hosts.push({ ...structuredClone(data.hosts[0]), node_id: 'second', name: '宿主 B' });
  const { client } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  const oldFetch = vi.mocked(fetch).getMockImplementation()!;
  const replies = new Map<string, (response: Response) => void>();
  vi.mocked(fetch).mockImplementation((input, init) =>
    init?.method === 'PUT'
      ? new Promise<Response>(resolve => replies.set(String(input), resolve))
      : String(input) === '/cvm'
        ? new Promise<Response>(() => {})
        : oldFetch(input, init),
  );
  const first = screen.getByRole('switch', { name: `${data.hosts[0].name} WebShell` });
  const second = screen.getByRole('switch', { name: '宿主 B WebShell' });
  fireEvent.click(first);
  fireEvent.click(second);
  await waitFor(() => expect(replies.size).toBe(2));
  // Model a newer poll before either write response arrives.
  const current = structuredClone(data);
  current.instances[0].desired.generation = 7;
  current.instances[0].status = 'pending';
  current.hosts.forEach(host => {
    host.terminal_enabled = false;
    host.terminal_revision = 2;
  });
  Object.assign(data, structuredClone(current));
  await act(async () => {
    client.setQueryData(['cvm'], current);
  });
  const staleReply = structuredClone(data);
  staleReply.instances[0].desired.generation = 1;
  staleReply.hosts[0].terminal_enabled = true;
  staleReply.hosts[0].terminal_revision = 1;
  await act(async () => {
    replies.get(`/cvm/hosts/${data.hosts[0].node_id}/terminal`)!(Response.json(data));
  });
  await act(async () => {
    replies.get('/cvm/hosts/second/terminal')!(Response.json(staleReply));
  });
  await waitFor(() => expect((second as HTMLInputElement).disabled).toBe(false));
  const result = client.getQueryData<CvmWorkspace>(['cvm'])!;
  expect(result.hosts.map(host => host.terminal_enabled)).toEqual([false, false]);
  expect(result.instances[0].desired.generation).toBe(7);
});

it('adopts the existing OS without an image, SSH key or editable virtual resources', async () => {
  const data = baremetalWorkspace('baremetal-hosts');
  const { writes } = mount(data);
  fireEvent.click(await screen.findByRole('button', { name: /^纳管裸机$/ }));
  expect(navigate).toHaveBeenLastCalledWith('cvm', { p: 'adopt' });
  const dialog = await screen.findByRole('form', { name: '纳管裸机' });
  expect(within(dialog).queryByLabelText('SSH 公钥')).toBeNull();
  fireEvent.click(within(dialog).getByRole('checkbox'));
  fireEvent.click(within(dialog).getByRole('button', { name: '确认纳管' }));
  await waitFor(() => expect(writes.length).toBe(1));
  expect(writes[0].url).toBe('/cvm/adopt');
  expect(writes[0].body).toMatchObject({
    node_id: data.hosts[0].node_id,
    identity: data.hosts[0].capabilities.host_system!.identity,
  });
  expect(writes[0].body).not.toHaveProperty('image_id');
  expect(writes[0].body).not.toHaveProperty('resources');
});

it.each(['empty', 'list'])('keeps bare-metal adoption discoverable with %s Cube hosts', async scene => {
  const { writes } = mount(cvmWorkspace(scene));
  fireEvent.click(await screen.findByRole('button', { name: /^纳管裸机$/ }));
  const dialog = await screen.findByRole('form', { name: '纳管裸机' });
  expect(within(dialog).getByText('尚无可纳管的裸机')).toBeTruthy();
  expect(within(dialog).queryByRole('button', { name: '确认纳管' })).toBeNull();
  expect(within(dialog).queryByRole('combobox')).toBeNull();
  if (scene === 'list') expect(within(dialog).getByText(/安装或升级组合执行栈/)).toBeTruthy();
  fireEvent.click(within(dialog).getByRole('button', { name: '查看宿主' }));
  expect(screen.getByRole('tab', { name: '宿主' }).getAttribute('aria-selected')).toBe('true');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(screen.getByRole('button', { name: /^纳管裸机$/ })).toBeTruthy();
  expect(writes).toEqual([]);
});

it.each(['stale', 'error', 'occupied'] as const)(
  'explains a %s bare-metal host without submitting adoption',
  async state => {
    const data = baremetalWorkspace('baremetal-hosts');
    if (state === 'stale') data.hosts[0].fresh = false;
    else if (state === 'error') data.hosts[0].error = 'host configuration is unavailable';
    else data.instances = baremetalWorkspace().instances;
    const { writes } = mount(data);
    fireEvent.click(await screen.findByRole('button', { name: /^纳管裸机$/ }));
    const dialog = await screen.findByRole('form', { name: '纳管裸机' });
    expect(within(dialog).queryByRole('button', { name: '确认纳管' })).toBeNull();
    const option = within(dialog).getByRole('option', { name: new RegExp(data.hosts[0].name) });
    expect((option as HTMLOptionElement).disabled).toBe(true);
    expect(option.textContent).toContain(
      state === 'occupied' ? '裸机已纳管' : state === 'stale' ? '等待机器重新上报' : '执行能力不可用',
    );
    expect(writes).toEqual([]);
  },
);

it('offers only native hosts when Cube and bare-metal hosts coexist', async () => {
  const data = baremetalWorkspace('baremetal-hosts');
  const cube = cvmWorkspace('list').hosts[0];
  cube.node_id = 'cube-only';
  cube.name = '仅容器模式';
  data.hosts.push(cube);
  mount(data);
  fireEvent.click(await screen.findByRole('button', { name: /^纳管裸机$/ }));
  const dialog = await screen.findByRole('form', { name: '纳管裸机' });
  expect(within(dialog).queryByRole('option', { name: cube.name })).toBeNull();
  expect((within(dialog).getByRole('combobox', { name: '选择裸机' }) as HTMLSelectElement).value).toBe(
    data.hosts[0].node_id,
  );
});

it('does not offer bare-metal adoption to an ordinary user', async () => {
  mount(baremetalWorkspace('baremetal-hosts', 'user'), {}, { ...who, role: 'user' });
  await screen.findByRole('region', { name: '容器清单' });
  expect(screen.queryByRole('button', { name: '纳管裸机' })).toBeNull();
  expect(routeForViewer({ nav: 'cvm', drill: { p: 'adopt' } }, { ...who, role: 'user' })).toEqual({ nav: 'cvm' });
});
it('releases bare metal using a separate operation and never offers VM power or erase actions', async () => {
  const data = baremetalWorkspace();
  const { writes } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  const release = screen.getByRole('menuitem', { name: /^解除纳管/ });
  expect(screen.queryByRole('button', { name: /^删除$/ })).toBeNull();
  expect(screen.queryByRole('button', { name: /^关机$/ })).toBeNull();
  expect(screen.queryByRole('button', { name: /^重启$/ })).toBeNull();
  fireEvent.click(release);
  expect(writes).toHaveLength(0);
  fireEvent.click(screen.getByRole('button', { name: '保存解除纳管' }));
  await waitFor(() => expect(writes.length).toBe(1));
  expect(writes[0].body.action).toBe('release_host');
  expect(writes[0].body).not.toHaveProperty('delete_data');
});

it('retrieves a password only on explicit request and can hide it again', async () => {
  const data = cvmWorkspace('detail', 'user');
  mount(data, { p: 'instance', id: data.instances[0].desired.id }, { ...who, role: 'user' });
  const reveal = await screen.findByRole('button', { name: '查看登录密码' });
  expect(vi.mocked(fetch).mock.calls.filter(([input]) => String(input).endsWith('/credential'))).toHaveLength(0);
  fireEvent.click(reveal);
  const password = await screen.findByRole('textbox', { name: '容器登录密码' });
  expect((password as HTMLInputElement).value).toBe('F'.repeat(32));
  fireEvent.click(screen.getByRole('button', { name: '隐藏密码' }));
  expect(screen.queryByRole('textbox', { name: '容器登录密码' })).toBeNull();
});

async function openSharedEntryForm() {
  const network = await screen.findByRole('tab', { name: '网络' });
  if (network.getAttribute('aria-selected') !== 'true') fireEvent.click(network);
  fireEvent.click(screen.getByRole('button', { name: '添加入口' }));
  const form = screen.getByRole('form', { name: '添加访问入口' });
  const addresses = await within(form).findByRole('radiogroup', { name: '入口地址' });
  fireEvent.click(await within(addresses).findByRole('radio', { name: /203\.0\.113\.20/ }));
  return {
    form,
    listen: within(form).getByRole('textbox', { name: 'TCP 入口端口' }),
    target: within(form).getByRole('textbox', { name: 'TCP 端口映射（可选）' }),
    add: within(form).getByRole<HTMLButtonElement>('button', { name: '加入变更' }),
  };
}

it('stages one L4 range and rejects a range that contains an occupied middle port', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  const { form, listen, target, add } = await openSharedEntryForm();
  fireEvent.change(listen, { target: { value: '30079-30082' } });
  fireEvent.change(target, { target: { value: '30079-30082 → 40000-40003' } });
  expect(within(form).getByRole('status').textContent).toBe('所选地址或端口已被占用。');
  expect(add.disabled).toBe(true);
  fireEvent.change(listen, { target: { value: '30040-30043' } });
  fireEvent.change(target, { target: { value: '30040-30043 → 40000-40003' } });
  fireEvent.click(add);
  const bar = await screen.findByLabelText('容器草稿与待发布变更');
  expect(within(bar).getByRole('button', { name: '1 处未保存' })).toBeTruthy();
  expect(writes).toEqual([]);
  fireEvent.click(within(bar).getByRole('button', { name: '保存网络变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({
    url: '/cvm/cvm-alice/network-edit',
    body: {
      entry_additions: [
        {
          publication_id: 'entrypub-hk',
          address: '203.0.113.20',
          mode: {
            kind: 'protocols',
            tcp: {
              kind: 'forward',
              ports: { kind: 'ranges', ranges: [{ start: 30040, end: 30043 }] },
              mappings: [{ listen: { start: 30040, end: 30043 }, target: { start: 40000, end: 40003 } }],
            },
            udp: { kind: 'disabled' },
            icmp: false,
            other_ip: false,
          },
        },
      ],
    },
  });
});

it.each([
  ['single port', '30040', '8080'],
  ['port range', '30040-30043', '40000-40003'],
])(
  'replays an accepted %s draft with the original body and digest after its save response is lost',
  async (_, listenText, targetText) => {
    const data = cvmWorkspace('detail');
    mount(data, { p: 'instance', id: 'cvm-alice' });
    const board = entryBoard(data, 'list');
    const prior = globalThis.fetch;
    const requests: { body: string; digest: string | null }[] = [];
    let committed: CvmEntry | null = null;
    const result = { instance: data.instances[0], mounted_interfaces: {}, input_sha256: 'a'.repeat(64) };
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input, init) => {
        if (String(input) === '/cvm/cvm-alice/network-edit' && init?.method === 'PUT') {
          requests.push({
            body: String(init.body),
            digest: new Headers(init.headers).get('x-brocade-cvm-input-sha256'),
          });
          if (committed) return Response.json(result);
          const body = JSON.parse(String(init.body)) as CvmNetworkEditRequest;
          committed = { ...board.entries[0], ...body.entry_additions![0], id: 'entry-committed', state: 'pending' };
          board.entries.push(committed);
          data.pending_revision_id = 42;
          Object.assign(data.instances[0], { revision_id: 42, published_revision_id: 41, operation_kind: 'entries' });
          return Response.json({ error: '保存响应丢失，请重试' }, { status: 503 });
        }
        if (String(input) === '/cvm/cvm-alice/entries') return Response.json(board.entries);
        return prior(input, init);
      }),
    );
    const { listen, target, add } = await openSharedEntryForm();
    fireEvent.change(listen, { target: { value: listenText } });
    fireEvent.change(target, { target: { value: `${listenText} → ${targetText}` } });
    fireEvent.click(add);
    fireEvent.click(await screen.findByRole('button', { name: '保存网络变更' }));
    await screen.findByText('保存响应丢失，请重试');
    const bar = within(screen.getByLabelText('容器草稿与待发布变更'));
    expect(bar.getByRole<HTMLButtonElement>('button', { name: '丢弃当前草稿' }).disabled).toBe(true);
    expect(screen.queryByRole('button', { name: '添加入口' })).toBeNull();
    fireEvent.click(bar.getByRole('button', { name: '重试原保存' }));
    await bar.findByRole('button', { name: '1 个容器待发布' });
    await waitFor(() => expect(bar.queryByRole('button', { name: '1 处未保存' })).toBeNull());
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
    expect(requests[0].digest).toBe(result.input_sha256);
    expect(JSON.parse(requests[0].body).request_id).toBeTruthy();
    expect(board.entries.filter(entry => entry.id === 'entry-committed')).toHaveLength(1);
  },
);

it('rechecks occupancy and permits a changed entry request after an explicit conflict', async () => {
  const data = cvmWorkspace('detail');
  const { client, writes } = mount(data, { p: 'instance', id: 'cvm-alice' });
  const board = entryBoard(data, 'list');
  const prior = globalThis.fetch;
  const checks: CvmNetworkEditRequest[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      if (String(input) === '/cvm/cvm-alice/network-edit?preview=true' && init?.method === 'PUT') {
        checks.push(JSON.parse(String(init.body)));
        if (checks.length === 1) {
          board.publications[0].usage[0].ports.push({
            protocol: 'tcp',
            port_start: 30040,
            port_end: 30043,
            entry_id: null,
          });
          return Response.json({ error: '端口已由其他请求占用' }, { status: 409 });
        }
      }
      if (String(input) === '/cvm/entries') return Response.json(board);
      return prior(input, init);
    }),
  );
  const original = await openSharedEntryForm();
  fireEvent.change(original.listen, { target: { value: '30040-30043' } });
  fireEvent.change(original.target, { target: { value: '30040-30043 → 40000-40003' } });
  fireEvent.click(original.add);
  fireEvent.click(await screen.findByRole('button', { name: '保存网络变更' }));
  await screen.findByText('端口已由其他请求占用');
  expect(writes).toEqual([]);
  expect(screen.queryByRole('button', { name: '重试原保存' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '丢弃当前草稿' }));
  await act(async () => client.invalidateQueries({ queryKey: ['cvm-entries'] }));
  const revised = await openSharedEntryForm();
  fireEvent.change(revised.listen, { target: { value: '30040-30043' } });
  expect(within(revised.form).getByRole('status').textContent).toBe('所选地址或端口已被占用。');
  expect(revised.add.disabled).toBe(true);
  fireEvent.change(revised.listen, { target: { value: '30060-30063' } });
  fireEvent.click(revised.add);
  fireEvent.click(await screen.findByRole('button', { name: '保存网络变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(checks).toHaveLength(2);
  expect(checks[1].entry_additions?.[0].mode).toMatchObject({
    tcp: { ports: { ranges: [{ start: 30060, end: 30063 }] } },
  });
  expect(checks[1].request_id).not.toBe(checks[0].request_id);
  expect(writes[0].body).toEqual(checks[1]);
});
