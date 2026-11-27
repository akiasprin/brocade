import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AdminRole } from '../src/api';
import {
  STACK_COLD_PAIR_CONTRACT,
  STACK_FRESH_INSTALL_CONTRACT,
  type StackRelease,
  type StackReleaseView,
} from '../src/api/cvm-stack-releases';
import { CVM_ENABLED } from '../src/product';
import { SessionProvider } from '../src/session';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  addEventListener: () => {},
  removeEventListener: () => {},
})) as unknown as typeof window.matchMedia;
const { DeployPane } = await import('../src/panes/deploy');

const path = '/cvm/stack-releases';
const source = 'a'.repeat(64);
const target = 'b'.repeat(64);
const cubelet = 'c'.repeat(64);
const shim = 'd'.repeat(64);
const coldSource = {
  contract: STACK_COLD_PAIR_CONTRACT,
  source_sha256: source,
  source_cubelet_sha256: cubelet,
  source_shim_sha256: shim,
  target_sha256: target,
} as const;
const view = (now: number): StackReleaseView => ({
  pairs: [
    { ...coldSource, id: 'upgrade-xfs', storage_backend: 'xfs', expires_at: now + 7_200 },
    { ...coldSource, id: 'upgrade-plain', storage_backend: 'plain', expires_at: now + 7_200 },
    { ...coldSource, id: 'old-pair', storage_backend: 'xfs', expires_at: now - 1 },
    {
      ...coldSource,
      id: 'wrong-cubelet',
      source_cubelet_sha256: '1'.repeat(64),
      storage_backend: 'xfs',
      expires_at: now + 7_200,
    },
    {
      ...coldSource,
      id: 'wrong-shim',
      source_shim_sha256: '2'.repeat(64),
      storage_backend: 'xfs',
      expires_at: now + 7_200,
    },
    {
      id: 'install-xfs',
      contract: STACK_FRESH_INSTALL_CONTRACT,
      target_sha256: target,
      storage_backend: 'xfs',
      expires_at: now + 7_200,
    },
    {
      id: 'install-plain',
      contract: STACK_FRESH_INSTALL_CONTRACT,
      source_sha256: null,
      source_cubelet_sha256: null,
      source_shim_sha256: null,
      target_sha256: target,
      storage_backend: 'plain',
      expires_at: now + 7_200,
    },
  ],
  hosts: [
    {
      node_id: 'fresh',
      bundle_sha256: source,
      cubelet_sha256: cubelet,
      shim_sha256: shim,
      storage_backend: 'xfs',
      observed_at: now - 30,
    },
    {
      node_id: 'stale',
      bundle_sha256: source,
      cubelet_sha256: cubelet,
      shim_sha256: shim,
      storage_backend: 'xfs',
      observed_at: now - 151,
    },
  ],
  releases: [],
});

const release = (now: number, overrides: Partial<StackRelease> = {}): StackRelease => ({
  id: 17,
  node_id: 'fresh',
  source_sha256: source,
  target_sha256: target,
  status: 'ready',
  expires_at: now + 1_200,
  ...overrides,
});

const ordinaryResponses: Record<string, unknown> = {
  '/deployments?limit=50': { deployments: [] },
  '/revisions?limit=50': { current_revision: 1, revisions: [] },
  '/deployments/verify': { summary: { changed_targets: 0 } },
  '/binary-releases/agent': { current: null },
  '/binary-releases/xray': { current: null },
  '/deployment-settings': {},
  '/nodes/agent-state': {
    nodes: [
      { node_id: 'fresh', name: '宿主甲' },
      { node_id: 'stale', name: '宿主乙' },
    ],
  },
};

function mount(
  role: AdminRole,
  stack: (url: string, init?: RequestInit) => Response = () => Response.json(view(Date.now() / 1_000)),
) {
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.startsWith(path)) return stack(url, init);
    if (url in ordinaryResponses) return Response.json(ordinaryResponses[url]);
    throw new Error(`Unexpected API request ${url}`);
  });
  vi.stubGlobal('fetch', fetch);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial: { node_count: 2, chain_group_count: [] },
          who: { operator_id: 'operator', role, tenant_scope: null, token_prefix: null, masked_assets: false },
        }}
      >
        <DeployPane
          win={{
            id: 1,
            key: 'tab:deploy',
            title: '发布',
            x: 0,
            y: 0,
            w: 800,
            h: 600,
            z: 1,
            min: false,
            home: 'desk',
            data: {},
          }}
        />
      </SessionProvider>
    </QueryClientProvider>,
  );
  const stackRequests = () => fetch.mock.calls.filter(([url]) => url.startsWith(path));
  return { client, stackRequests };
}

async function openTab() {
  fireEvent.click(await screen.findByRole('tab', { name: 'Cube 运行栈' }));
  await screen.findByRole('region', { name: '运行栈发布记录' });
}

function deadlineInput(seconds: number) {
  const date = new Date(seconds * 1_000);
  date.setMinutes(date.getMinutes() - date.getTimezoneOffset());
  return date.toISOString().slice(0, 16);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('Cube 运行栈发布契约', () => {
  it.each(['system-admin', 'tenant-admin'] as const)(
    '按构建变体和 %s 权限显示入口，打开前不读取 Cube 接口',
    async role => {
      const { stackRequests } = mount(role);
      const config = await screen.findByRole('tab', { name: '配置' });
      expect(screen.queryByRole('tab', { name: 'Cube 运行栈' }) !== null).toBe(CVM_ENABLED && role === 'system-admin');
      expect(stackRequests()).toEqual([]);
      fireEvent.keyDown(config, { key: 'End' });
      if (CVM_ENABLED && role === 'system-admin') {
        await screen.findByRole('region', { name: '准备冷窗口' });
        expect(stackRequests()).toEqual([[path, expect.objectContaining({ credentials: 'same-origin' })]]);
      } else {
        expect(screen.getByRole('tab', { name: 'Xray' }).getAttribute('aria-selected')).toBe('true');
        expect(stackRequests()).toEqual([]);
      }
    },
  );

  it.runIf(CVM_ENABLED)('仅提交匹配的单宿主和有效运行包对，窗口截止使用 Unix 秒', async () => {
    const now = Math.floor(Date.now() / 1_000);
    const data = view(now);
    const { stackRequests } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const request = JSON.parse(String(init.body));
      const created = release(now, { status: 'preparing', expires_at: request.expires_at });
      data.releases = [created];
      return Response.json(created);
    });
    await openTab();
    const host = screen.getByLabelText('目标宿主');
    const pair = screen.getByLabelText('兼容运行包对') as HTMLSelectElement;
    const create = screen.getByRole<HTMLButtonElement>('button', { name: '创建准备单' });
    fireEvent.change(host, { target: { value: 'stale' } });
    expect(screen.getByText('证据已过期，当前状态未知')).toBeTruthy();
    expect(pair.disabled).toBe(true);
    expect(create.disabled).toBe(true);
    fireEvent.change(host, { target: { value: 'fresh' } });
    expect(screen.getByText('宿主安装证据有效')).toBeTruthy();
    expect(Array.from(pair.options, option => option.value)).toEqual(['', 'upgrade-xfs', 'old-pair']);
    expect(within(pair).getByRole<HTMLOptionElement>('option', { name: /old-pair/ }).disabled).toBe(true);
    fireEvent.change(pair, { target: { value: 'upgrade-xfs' } });
    const deadline = screen.getByLabelText('窗口截止（本地时间）');
    fireEvent.change(deadline, { target: { value: deadlineInput(now + 8_000) } });
    expect(create.disabled).toBe(true);
    const validDeadline = deadlineInput(now + 1_200);
    fireEvent.change(deadline, { target: { value: validDeadline } });
    fireEvent.click(create);
    await screen.findByText('准备中');
    expect(stackRequests().filter(([, init]) => init?.method === 'POST')).toEqual([
      [
        path,
        expect.objectContaining({
          method: 'POST',
          credentials: 'same-origin',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            node_id: 'fresh',
            pair_id: 'upgrade-xfs',
            expires_at: Date.parse(validDeadline) / 1_000,
          }),
        }),
      ],
    ]);
  });

  it.runIf(CVM_ENABLED)('首次安装仅使用匹配存储的安装包，明确批准并核对目标包观测', async () => {
    const now = Math.floor(Date.now() / 1_000);
    const data = view(now);
    const plainHost = { node_id: 'empty-plain', observed_at: now - 30, installable: 'plain' } as const;
    data.hosts.push(plainHost, { node_id: 'empty-xfs', observed_at: now - 30, installable: 'xfs' });
    const installation = release(now, { node_id: plainHost.node_id, source_sha256: null });
    const { client, stackRequests } = mount('system-admin', (url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const updated = {
        ...installation,
        status: url === path ? 'preparing' : 'approved',
        expires_at: url === path ? JSON.parse(String(init.body)).expires_at : data.releases[0].expires_at,
      } as const;
      data.releases = [updated];
      return Response.json(updated);
    });

    await openTab();
    const history = within(screen.getByRole('region', { name: '运行栈发布记录' }));
    const host = screen.getByLabelText('目标宿主');
    fireEvent.change(host, { target: { value: 'empty-xfs' } });
    const form = within(screen.getByRole('region', { name: '准备首次安装' }));
    const pair = form.getByLabelText<HTMLSelectElement>('首次安装包');
    expect(Array.from(pair.options, option => option.value)).toEqual(['', 'install-xfs']);
    fireEvent.change(pair, { target: { value: 'install-xfs' } });
    fireEvent.change(host, { target: { value: plainHost.node_id } });
    expect(pair.value).toBe('');
    expect(Array.from(pair.options, option => option.value)).toEqual(['', 'install-plain']);
    expect(form.getByText('已确认宿主符合首次安装条件')).toBeTruthy();
    expect(form.queryByText('已安装 Shim SHA-256')).toBeNull();
    expect(screen.queryByRole('button', { name: '前往容器电源操作' })).toBeNull();

    fireEvent.change(pair, { target: { value: 'install-plain' } });
    const validDeadline = deadlineInput(now + 1_200);
    fireEvent.change(form.getByLabelText('窗口截止（本地时间）'), { target: { value: validDeadline } });
    fireEvent.click(form.getByRole('button', { name: '创建首次安装准备单' }));
    await history.findByText('准备中');
    expect(history.getByText(/#17.*首次安装/)).toBeTruthy();
    expect(history.queryByText('来源包 SHA-256')).toBeNull();

    data.releases = [{ ...data.releases[0], status: 'ready' }];
    data.hosts = data.hosts.map(item =>
      item.node_id === plainHost.node_id ? { ...plainHost, observed_at: now - 151 } : item,
    );
    const refetch = () => client.refetchQueries({ queryKey: ['cvm-stack-releases'] });
    await refetch();
    const approve = await history.findByRole<HTMLButtonElement>('button', { name: '批准首次安装' });
    expect(approve.disabled).toBe(true);
    data.hosts = data.hosts.map(item => (item.node_id === plainHost.node_id ? plainHost : item));
    await refetch();
    await waitFor(() => expect(approve.disabled).toBe(false));
    fireEvent.click(approve);
    const dialog = within(screen.getByRole('dialog', { name: '批准首次安装 #17' }));
    const confirm = dialog.getByRole<HTMLButtonElement>('button', { name: '确认批准' });
    expect(dialog.getByText(target)).toBeTruthy();
    expect(dialog.queryByText(/停机|已停止/)).toBeNull();
    expect(confirm.disabled).toBe(true);
    fireEvent.click(dialog.getByRole('checkbox', { name: /我已核对目标宿主、安装包和窗口/ }));
    fireEvent.click(confirm);
    await history.findByText('已批准');
    expect(stackRequests().filter(([, init]) => init?.method === 'POST')).toEqual([
      [
        path,
        expect.objectContaining({
          body: JSON.stringify({
            node_id: plainHost.node_id,
            pair_id: 'install-plain',
            expires_at: Date.parse(validDeadline) / 1_000,
          }),
        }),
      ],
      [`${path}/17/approve`, { method: 'POST', credentials: 'same-origin', headers: {} }],
    ]);

    data.releases = [{ ...data.releases[0], status: 'applying' }];
    await refetch();
    await history.findByText('正在首次安装');
    expect(history.queryByRole('button', { name: '取消发布' })).toBeNull();
    data.releases = [{ ...data.releases[0], status: 'succeeded' }];
    await refetch();
    await history.findByText('宿主运行栈首次安装成功');
    expect(history.getByText('当前观测尚未确认安装包摘要与目标包一致。')).toBeTruthy();

    data.hosts = data.hosts.map(item =>
      item.node_id !== plainHost.node_id
        ? item
        : {
            node_id: plainHost.node_id,
            observed_at: now,
            bundle_sha256: target,
            cubelet_sha256: cubelet,
            shim_sha256: shim,
            storage_backend: 'plain',
          },
    );
    await refetch();
    await history.findByText('当前安装包摘要与目标包一致。');
    expect(history.getByText('已安装 Shim SHA-256')).toBeTruthy();
    expect(history.queryByText('已确认宿主符合首次安装条件')).toBeNull();
    expect(history.queryByText('VM Shim 仍待冷启动验证。')).toBeNull();
    expect(screen.getByRole('region', { name: '准备冷窗口' })).toBeTruthy();
  });

  it.runIf(CVM_ENABLED)('必须明确批准冷切换，开始切换后不再提供取消，成功仅指宿主', async () => {
    const now = Math.floor(Date.now() / 1_000);
    const data = view(now);
    data.releases = [release(now, { roots_sha256: 'e'.repeat(64) })];
    const { client, stackRequests } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const updated = { ...data.releases[0], status: 'approved' } as const;
      data.releases = [updated];
      return Response.json(updated);
    });
    await openTab();
    fireEvent.click(screen.getByRole('button', { name: '批准冷切换' }));
    const dialog = within(screen.getByRole('dialog', { name: '批准冷切换 #17' }));
    const confirm = dialog.getByRole<HTMLButtonElement>('button', { name: '确认批准' });
    expect(dialog.getByText(/批准后 Agent 可立即开始切换宿主运行栈/)).toBeTruthy();
    expect(confirm.disabled).toBe(true);
    expect(stackRequests().some(([, init]) => init?.method === 'POST')).toBe(false);
    fireEvent.click(dialog.getByRole('checkbox'));
    expect(confirm.disabled).toBe(false);
    fireEvent.click(confirm);
    await screen.findByText('已批准');
    expect(stackRequests().filter(([, init]) => init?.method === 'POST')).toEqual([
      [`${path}/17/approve`, { method: 'POST', credentials: 'same-origin', headers: {} }],
    ]);
    data.releases = [release(now, { status: 'applying' })];
    await client.refetchQueries({ queryKey: ['cvm-stack-releases'] });
    await screen.findByText('正在切换');
    expect(screen.queryByRole('button', { name: '取消发布' })).toBeNull();
    data.releases = [release(now, { status: 'succeeded' })];
    await client.refetchQueries({ queryKey: ['cvm-stack-releases'] });
    expect(await screen.findByText('宿主运行栈切换成功')).toBeTruthy();
    expect(screen.getByText('VM Shim 仍待冷启动验证。')).toBeTruthy();
    expect(screen.getByText(/Guest OS 不会升级/)).toBeTruthy();
    expect(screen.getByText(/原先已停止的虚拟机保持停止/)).toBeTruthy();
    expect(screen.getByRole('button', { name: '前往容器电源操作' })).toBeTruthy();
  });

  it.runIf(CVM_ENABLED)('取消未执行的发布，缺少宿主证据时不批准', async () => {
    const now = Math.floor(Date.now() / 1_000);
    const data = view(now);
    data.releases = [release(now, { node_id: 'no-evidence' })];
    const { stackRequests } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const cancelled = { ...data.releases[0], status: 'cancelled' } as const;
      data.releases = [cancelled];
      return Response.json(cancelled);
    });
    await openTab();
    expect(screen.getByText('宿主安装状态未知')).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>('button', { name: '批准冷切换' }).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: '取消发布' }));
    fireEvent.click(
      within(screen.getByRole('dialog', { name: '取消发布 #17' })).getByRole('button', { name: '确认取消发布' }),
    );
    await screen.findByText('已取消');
    expect(stackRequests().filter(([, init]) => init?.method === 'POST')).toEqual([
      [`${path}/17/cancel`, { method: 'POST', credentials: 'same-origin', headers: {} }],
    ]);
  });

  it.runIf(CVM_ENABLED)('服务端拒绝后隐藏缓存的发布证据和写操作', async () => {
    const now = Math.floor(Date.now() / 1_000);
    const data = view(now);
    data.releases = [release(now)];
    let allowed = true;
    const { client } = mount('system-admin', () =>
      allowed ? Response.json(data) : Response.json({ error: 'forbidden' }, { status: 403 }),
    );
    await openTab();
    expect(screen.getByText('待批准')).toBeTruthy();
    allowed = false;
    await client.refetchQueries({ queryKey: ['cvm-stack-releases'] });
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('当前账号不能查看这项内容'));
    expect(screen.queryByText('待批准')).toBeNull();
    expect(screen.queryByRole('button', { name: '批准冷切换' })).toBeNull();
  });
});
