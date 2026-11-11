import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { CvmPane } from '../../src/panes/cvm';
import { SessionProvider } from '../../src/session';
import { routeForViewer } from '../../src/forge/access';
import type { CvmWorkspace, CvmMonitoring, Whoami } from '../../src/api';
import { nicCatalog, nicState } from '../fixtures/cvm/network-data';
import type { Win } from '../../src/wm/store';
import { cvmReadings, cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';

vi.mock('../../src/panes/cvm/monitoring-charts', () => ({ default: () => <div>监控曲线</div> }));
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
  drill: Record<string, unknown> = {},
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
        next.available_hosts = [...(next.available_hosts ?? []).filter(item => item.node_id !== node), ...(!body.enabled ? [host] : [])];
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
      if (url === '/cvm/quota' && init?.method === 'PUT') { writes.push({ url, body: JSON.parse(String(init.body)) }); return new Response(null, { status: 204 }); }
      if (init?.method === 'POST') {
        writes.push({ url, body: JSON.parse(String(init.body)) });
        return Response.json({ error: '执行结果尚未确认' }, { status: failure ?? 503 });
      }
      if (url === '/cvm/readings') return Response.json(cvmReadings(data));
      if (url === '/cvm') return Response.json(data);
      if (url === '/cvm/networks') return Response.json(nicCatalog(data.can_manage_network));
      if (url === '/cvm/entries') return Response.json(entryBoard(data, 'list'));
      if (url.endsWith('/entries')) return Response.json(entryBoard(data, 'list').entries);
      if (url.endsWith('/network')) return Response.json(nicState(data.instances[0],data.redacted));
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
    data: { drill },
  };
  render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who: identity, initial: { node_count: 1, chain_group_count: [] } }}>
        <CvmPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return { writes, client };
}
async function fillCreate() {
  fireEvent.change(await screen.findByRole('textbox', { name: '实例名称' }), { target: { value: 'my-cvm' } });
  fireEvent.change(screen.getByRole('textbox', { name: 'SSH 公钥' }), {
    target: { value: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEiBcpQEzqzjADp8VXvOigHnqLh5L5xP76cwoCD8YMK5' },
  });
}
it('separates instance and host pages without removing creation and terminal management', async () => {
  const data = cvmWorkspace('list');
  mount(data);
  const instances = await screen.findByRole('tab', { name: '组合台' });
  expect(instances.getAttribute('aria-selected')).toBe('true');
  expect(screen.queryByRole('list', { name: '宿主' })).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  const hosts = screen.getByRole('list', { name: '宿主' });
  expect(within(hosts).getByRole('listitem', { name: data.hosts[0].name })).toBeTruthy();
  expect(screen.queryByRole('list', { name: '容器实例' })).toBeNull();
  expect(screen.getByText('1 / 8')).toBeTruthy();
  expect(screen.getByText(/1 个可见实例/)).toBeTruthy();
  expect(screen.queryByText('cube-kvm-v1')).toBeNull();
  expect(screen.getByRole('switch', { name: `${data.hosts[0].name} WebShell` })).toBeTruthy();
  fireEvent.click(instances);
  expect(screen.getByRole('button', { name: '＋ 创建容器' })).toBeTruthy();
});

it('removes only the host identity and lets an administrator explicitly add it back', async () => {
  const data = cvmWorkspace('new');
  const node = data.hosts[0].node_id;
  const name = data.hosts[0].name;
  const { writes, client } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name: '移除宿主' }));
  await screen.findByText('尚无宿主');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(client.getQueryData<CvmWorkspace>(['cvm'])?.hosts).toHaveLength(0);
  expect(writes[0]).toEqual({ url: `/cvm/hosts/${node}`, body: { enabled: false, expected_revision: 0 } });
  fireEvent.click(screen.getByRole('tab', { name: '组合台' }));
  expect((screen.getByRole('button', { name: '＋ 创建容器' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  fireEvent.change(screen.getByRole('combobox', { name: '添加宿主' }), { target: { value: node } });
  await screen.findByRole('listitem', { name });
  expect(writes[1]).toEqual({ url: `/cvm/hosts/${node}`, body: { enabled: true, expected_revision: 1 } });
  expect(screen.queryByRole('combobox', { name: '添加宿主' })).toBeNull();
});

it('keeps an occupied host visible and shows the server cleanup requirement', async () => {
  const data = cvmWorkspace('list');
  mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  fireEvent.click(screen.getByRole('button', { name: '移除宿主' }));
  await screen.findByText('请先删除该宿主上的实例，并等待系统盘和网络资源清理完成');
  expect(screen.getByRole('listitem', { name: data.hosts[0].name })).toBeTruthy();
  expect(screen.queryByRole('combobox', { name: '添加宿主' })).toBeNull();
});

it('does not offer host identity management to an ordinary user', async () => {
  const data = cvmWorkspace('list', 'user');
  mount(data, {}, { ...who, role: 'user' });
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.queryByRole('button', { name: '移除宿主' })).toBeNull();
  expect(screen.queryByRole('combobox', { name: '添加宿主' })).toBeNull();
});

it('moves between page tabs with arrow and boundary keys', async () => {
  mount(cvmWorkspace('list'));
  const instances = await screen.findByRole('tab', { name: '组合台' });
  const hosts = screen.getByRole('tab', { name: '宿主' });
  fireEvent.keyDown(instances, { key: 'End' });
  const networks=screen.getByRole('tab',{name:'额度'});
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
  expect(screen.getByText(/1 个可见实例/)).toBeTruthy();
  expect(screen.queryByRole('button', { name: /查看.*实例/ })).toBeNull();
  expect(screen.queryByText('能力报告新鲜')).toBeNull();
  expect(screen.queryByText('等待新的能力报告')).toBeNull();
  expect(within(screen.getByRole('listitem', { name: data.hosts[0].name })).getByText('上报')).toBeTruthy();
});

it.each(['stale', 'error'] as const)('does not advertise creation from a %s host report', async state => {
  const data = cvmWorkspace('list');
  if (state === 'stale') data.hosts[0].fresh = false;
  else data.hosts[0].error = '执行能力暂不可用';
  mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.getByRole('status').textContent).toBe(state === 'stale' ? '能力已过期' : '执行能力不可用');
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
  expect(await screen.findByText(`${data.hosts.length - 1} 台宿主`)).toBeTruthy();
  expect(screen.queryByText('仅代理机器')).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  const hosts = screen.getByRole('list', { name: '宿主' });
  expect(within(hosts).queryByText('仅代理机器')).toBeNull();
  expect(within(hosts).queryByText('执行能力不可用')).toBeNull();
});

it('keeps an empty host tab available without claiming an instance exists', async () => {
  mount(cvmWorkspace('empty'));
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.getByText('尚无宿主')).toBeTruthy();
  expect(screen.queryByRole('list', { name: '容器实例' })).toBeNull();
});













it('keeps a stable creation identity across an uncertain response and refuses a changed payload', async () => {
  const { writes } = mount(cvmWorkspace('new'), { p: 'new' });
  await fillCreate();
  fireEvent.click(screen.getByRole('button', { name: '创建实例' }));
  await screen.findByText('执行结果尚未确认');
  fireEvent.click(screen.getByRole('button', { name: '创建实例' }));
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[0].body.request_id).toBe(writes[1].body.request_id);
  fireEvent.change(screen.getByRole('textbox', { name: '实例名称' }), { target: { value: 'different' } });
  fireEvent.click(screen.getByRole('button', { name: '创建实例' }));
  await screen.findByText(/上次创建结果尚未确认/);
  expect(writes).toHaveLength(2);
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
  expect(((await screen.findByRole('button', { name: '删除' })) as HTMLButtonElement).disabled).toBe(false);
  expect((screen.getByRole('button', { name: '核查并重试原任务' }) as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(screen.getByRole('tab', { name: '网络' }));
  expect((screen.getByRole('button', { name: '＋ 挂载网卡' }) as HTMLButtonElement).disabled).toBe(true);
});
it.each(['reader', 'public'])('keeps read-only %s identities outside creation while allowing detail', operatorId => {
  const viewer: Whoami = { ...who, operator_id: operatorId, role: 'readonly', masked_assets: true };
  expect(routeForViewer({ nav: 'cvm', drill: { p: 'new' } }, viewer)).toEqual({ nav: 'cvm' });
  const location = { nav: 'cvm' as const, drill: { p: 'instance', id: 'cvm-alice' } };
  expect(routeForViewer(location, viewer)).toEqual(location);
});

it('lets guests inspect hosts without creation or management', async () => {
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
  await screen.findByRole('tab', { name: '组合台' });
  expect(screen.queryByRole('button', { name: '＋ 创建容器' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'WebShell 管理' })).toBeNull();
  fireEvent.click(screen.getByRole('tab', { name: '宿主' }));
  expect(screen.getByRole('list', { name: '宿主' })).toBeTruthy();
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
  for (const name of ['删除', '＋ 挂载网卡', '终端']) expect(screen.queryByRole('button', { name })).toBeNull();
  expect(screen.getByText('连接地址已脱敏。')).toBeTruthy();
  expect(screen.queryByText('192.0.2.21/32')).toBeNull();
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
  fireEvent.click(screen.getByRole('button', { name: '创建实例' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body.resources).toEqual({ vcpus: 3, memory_bytes: 768 * 1024 ** 2, root_disk_bytes: 3 * 1024 ** 3 });
  expect(writes[0].body.profile_id).toBeUndefined();
});

it('shows actual running state and requires confirmation before a durable restart', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  await screen.findByText('运行中');
  expect(screen.queryByText(/期望运行/)).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '重启' }));
  const dialog = screen.getByRole('dialog', { name: '重启实例' });
  expect(writes).toHaveLength(0);
  fireEvent.click(within(dialog).getByRole('button', { name: '确认重启实例' }));
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
  fireEvent.click(within(dialog).getByRole('button', { name: '确认修改配置' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ action: 'configure', vcpus: 3, memory_bytes: 768 * 1024 ** 2 });
  expect(writes[0].body.image_id).toBeUndefined();
});

it('requires both the exact name and data-loss acknowledgement before reinstall', async () => {
  const { writes } = mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '实例更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /重装系统/ }));
  const dialog = screen.getByRole('dialog', { name: '重装系统' });
  fireEvent.click(within(dialog).getByRole('button', { name: '确认重装系统' }));
  expect(writes).toHaveLength(0);
  fireEvent.change(within(dialog).getByRole('textbox', { name: '输入实例名称以确认' }), {
    target: { value: 'home-workspace' },
  });
  fireEvent.click(within(dialog).getByRole('checkbox', { name: '确认清除系统盘数据' }));
  fireEvent.click(within(dialog).getByRole('button', { name: '确认重装系统' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ action: 'reinstall', image_id: 'debian-13' });
});

it.each([false, true])('deletes the entire instance directly with retain_on_delete=%s', async retainOnDelete => {
  const data = cvmWorkspace('detail');
  data.hosts[0].capabilities.retain_on_delete = retainOnDelete;
  const { writes } = mount(data, { p: 'instance', id: 'cvm-alice' });
  fireEvent.click(await screen.findByRole('button', { name: '删除' }));
  expect(screen.queryByRole('dialog')).toBeNull();
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].url).toBe('/cvm/cvm-alice/operations');
  expect(writes[0].body).toMatchObject({
    action: 'delete', delete_data: true, expected_generation: data.instances[0].desired.generation,
  });
  await screen.findByText('执行结果尚未确认');
  fireEvent.click(screen.getByRole('button', { name: '删除' }));
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[1].body).toEqual(writes[0].body);
});



it('the shared shell list drill retains creation and quota actions without user forwarding', async () => {
  mount(cvmWorkspace('list'), { p: 'list' });
  expect(await screen.findByRole('tab', { name: '额度' })).toBeTruthy();
  expect(screen.getByRole('button', { name: '＋ 创建容器' })).toBeTruthy();
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
  const monitoring = await screen.findByRole('region', { name: '容器 监控' });
  await within(monitoring).findByText('持续采样');
  expect(within(monitoring).getByText('/ 475.0 MiB 可见内存')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '查看监控与诊断' }));
  expect(within(monitoring).getByRole('option', { name: 'enp0s2 · 副网卡' })).toBeTruthy();
  fireEvent.change(within(monitoring).getByLabelText('监控网卡'), { target: { value: 'nic-business' } });
  expect((within(monitoring).getByLabelText('监控网卡') as HTMLSelectElement).value).toBe('nic-business');
});
it('marks expired samples and never shows their CPU or network rates as live', async () => {
  const { cvmMonitoring } = await import('../fixtures/cvm/data');
  mount(cvmWorkspace('detail'), { p: 'instance', id: 'cvm-alice' }, who, undefined, cvmMonitoring('stale'));
  const monitoring = await screen.findByRole('region', { name: '容器 监控' });
  await within(monitoring).findByText('读数已过期');
  expect(within(monitoring).getByLabelText('最后实例读数')).toBeTruthy();
  expect(within(monitoring).queryByText('持续采样')).toBeNull();
  expect(within(monitoring).getByLabelText('最后实例读数').textContent).toContain('— / —');
});

it('waits for independent health instead of treating a lifecycle receipt as a shutdown observation', async () => {
  mount(cvmWorkspace('stopped'), { p: 'instance', id: 'cvm-alice' });
  const monitoring = await screen.findByRole('region', { name: '容器 监控' });
  await within(monitoring).findByText('等待采样');
  expect(within(monitoring).queryByText('已关机')).toBeNull();
});

it('closes a terminal after an instance change and never opens it again after start without a click', async () => {
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '终端' }));
  await screen.findByRole('dialog', { name: '实例终端会话' });
  const stopped = structuredClone(data);
  stopped.instances[0].desired.generation += 1;
  stopped.instances[0].desired.power = 'stopped';
  stopped.instances[0].report!.power = 'stopped';
  await act(async () => {
    client.setQueryData(['cvm'], stopped);
  });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
  const started = structuredClone(stopped);
  started.instances[0].desired.generation += 1;
  started.instances[0].desired.power = 'running';
  started.instances[0].report!.power = 'running';
  await act(async () => {
    client.setQueryData(['cvm'], started);
  });
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '终端' }));
  await screen.findByRole('dialog', { name: '实例终端会话' });
  await act(async () => {
    client.setQueryData(['cvm'], { ...started, can_write: false });
  });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
  await act(async () => {
    client.setQueryData(['cvm'], started);
  });
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull();
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
  await screen.findByText('持续采样');
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
  expect(screen.getByRole('button', { name: '路由表' })).toBeTruthy();
});

it('manages WebShell from the 容器 page and preserves versioned server state', async () => {
  const data = cvmWorkspace('list');
  const { writes } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  const dialog = screen.getByRole('list', { name: '宿主' });
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
  const dialog = screen.getByRole('list', { name: '宿主' });
  const toggle = within(dialog).getByRole('switch', { name: `${data.hosts[0].name} WebShell` });
  fireEvent.click(toggle);
  await within(dialog).findByText('WebShell 设置已更新，请刷新后重试');
  expect((toggle as HTMLInputElement).checked).toBe(true);
});

it('hides WebShell management from users without host policy access', async () => {
  const data = cvmWorkspace('list', 'user');
  mount(data, {}, { ...who, role: 'user' });
  fireEvent.click(await screen.findByRole('tab', { name: '宿主' }));
  expect(screen.queryByRole('switch')).toBeNull();
  expect(screen.queryByRole('button', { name: 'WebShell 管理' })).toBeNull();
});

it('closes the terminal when host policy is disabled and does not reopen after enabling', async () => {
  const data = cvmWorkspace('detail');
  const { client } = mount(data, { p: 'instance', id: data.instances[0].desired.id });
  fireEvent.click(await screen.findByRole('button', { name: '终端' }));
  await screen.findByRole('dialog', { name: '实例终端会话' });
  const disabled = structuredClone(data);
  disabled.hosts[0].terminal_enabled = false;
  await act(async () => {
    client.setQueryData(['cvm'], disabled);
  });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull());
  expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => {
    client.setQueryData(['cvm'], data);
  });
  await waitFor(() => expect((screen.getByRole('button', { name: '终端' }) as HTMLButtonElement).disabled).toBe(false));
  expect(screen.queryByRole('dialog', { name: '实例终端会话' })).toBeNull();
});













it('loads fleet readings in one batch and keeps missing or expired metrics unknown', async () => {
  const data = cvmWorkspace('list');
  const { client } = mount(data);
  // The board lists containers as pills; the inspector shows the selected one's readings.
  fireEvent.click(await screen.findByRole('button', { name: /home-workspace/ }));
  const inspector = await screen.findByRole('complementary', { name: 'home-workspace' });
  const resources = inspector.querySelector('.node-card-resources')!;
  await waitFor(() => expect(resources.textContent).toContain('CPU9%'));
  expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).includes('/monitoring'))).toHaveLength(0);
  const readings = cvmReadings(data);
  readings.instances[0].state = 'stale';
  await act(async () => { client.setQueryData(['cvm-readings'], readings); });
  await waitFor(() => expect(resources.textContent).toBe('CPU—内存—磁盘—'));
  const expired = structuredClone(readings);
  expired.instances[0].state = 'live';
  expired.instances[0].latest!.observed_at_unix_millis -= 180_000;
  await act(async () => { client.setQueryData(['cvm-readings'], expired); });
  await waitFor(() => expect(resources.textContent).toBe('CPU—内存—磁盘—'));
});

it('uses durable quota usage including storage that outlives compute reservations', async () => {
  const data = cvmWorkspace('new');
  data.usage![0].instances = 0;
  data.usage![0].disk_bytes = data.quotas[0].max_disk_bytes - 1024 ** 3;
  const { writes } = mount(data, { p: 'new' });
  await fillCreate();
  const summary = screen.getByLabelText('创建汇总');
  expect(summary.textContent).toContain('22 GiB / 20 GiB');
  expect((screen.getByRole('button', { name: '创建实例' }) as HTMLButtonElement).disabled).toBe(true);
  expect(writes).toEqual([]);
});

it('edits quota from its tab and submits the selected owner and limits', async () => {
  const data = cvmWorkspace('list');
  const { writes } = mount(data);
  fireEvent.click(await screen.findByRole('tab', { name: '额度' }));
  fireEvent.click(screen.getByRole('button', { name: '编辑额度' }));
  const dialog = screen.getByRole('dialog', { name: '容器 用户额度' });
  fireEvent.change(within(dialog).getByRole('spinbutton', { name: 'vCPU 总额' }), { target: { value: '6' } });
  fireEvent.click(within(dialog).getByRole('button', { name: '保存额度' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toMatchObject({ url: '/cvm/quota', body: { tenant_id: 'platform', user_id: 'alice', max_vcpus: 6 } });
});

it('keeps host policy management reachable when creation and quota grants are unavailable', async () => {
  const data = cvmWorkspace('empty');
  const host = cvmWorkspace('list').hosts[0];
  host.capabilities.create = false;
  data.hosts = [host];
  data.quotas = [];
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
  vi.mocked(fetch).mockImplementation((input, init) => init?.method === 'PUT'
    ? new Promise<Response>(resolve => replies.set(String(input), resolve))
    : String(input) === '/cvm' ? new Promise<Response>(() => {}) : oldFetch(input, init));
  const first = screen.getByRole('switch', { name: `${data.hosts[0].name} WebShell` });
  const second = screen.getByRole('switch', { name: '宿主 B WebShell' });
  fireEvent.click(first);
  fireEvent.click(second);
  await waitFor(() => expect(replies.size).toBe(2));
  // Model a newer poll before either write response arrives.
  const current = structuredClone(data);
  current.instances[0].desired.generation = 7;
  current.instances[0].status = 'pending';
  current.hosts.forEach(host => { host.terminal_enabled = false; host.terminal_revision = 2; });
  Object.assign(data, structuredClone(current));
  await act(async () => { client.setQueryData(['cvm'], current); });
  const staleReply = structuredClone(data);
  staleReply.instances[0].desired.generation = 1;
  staleReply.hosts[0].terminal_enabled = true;
  staleReply.hosts[0].terminal_revision = 1;
  await act(async () => { replies.get(`/cvm/hosts/${data.hosts[0].node_id}/terminal`)!(Response.json(data)); });
  await act(async () => { replies.get('/cvm/hosts/second/terminal')!(Response.json(staleReply)); });
  await waitFor(() => expect((second as HTMLInputElement).disabled).toBe(false));
  const result = client.getQueryData<CvmWorkspace>(['cvm'])!;
  expect(result.hosts.map(host => host.terminal_enabled)).toEqual([false, false]);
  expect(result.instances[0].desired.generation).toBe(7);
});
