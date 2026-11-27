import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AdminRole } from '../src/api';
import {
  STACK_COLD_PAIR_CONTRACT,
  STACK_FRESH_INSTALL_CONTRACT,
  type StackRelease,
  type StackReleaseHost,
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
const installed = (now: number) =>
  ({
    node_id: 'fresh',
    bundle_sha256: source,
    cubelet_sha256: cubelet,
    shim_sha256: shim,
    storage_backend: 'xfs',
    observed_at: now - 30,
  }) as const;
const empty = (now: number, backend: 'plain' | 'xfs'): StackReleaseHost => ({
  node_id: `empty-${backend}`,
  observed_at: now - 30,
  installable: backend,
});
const view = (now: number): StackReleaseView => ({
  pairs: [
    { ...coldSource, id: 'upgrade-xfs', storage_backend: 'xfs' },
    { ...coldSource, id: 'upgrade-plain', storage_backend: 'plain' },
    { ...coldSource, id: 'wrong-cubelet', source_cubelet_sha256: '1'.repeat(64), storage_backend: 'xfs' },
    { ...coldSource, id: 'wrong-shim', source_shim_sha256: '2'.repeat(64), storage_backend: 'xfs' },
    { id: 'install-xfs', contract: STACK_FRESH_INSTALL_CONTRACT, target_sha256: target, storage_backend: 'xfs' },
    {
      id: 'install-plain',
      contract: STACK_FRESH_INSTALL_CONTRACT,
      source_sha256: null,
      source_cubelet_sha256: null,
      source_shim_sha256: null,
      target_sha256: target,
      storage_backend: 'plain',
    },
  ],
  hosts: [installed(now), { ...installed(now), node_id: 'stale', observed_at: now - 151 }],
  releases: [],
});
const release = (now: number, overrides: Partial<StackRelease> = {}): StackRelease => ({
  id: 17,
  node_id: 'fresh',
  source_sha256: source,
  target_sha256: target,
  status: 'ready',
  start_deadline_at: now + 18_000,
  ...overrides,
});
const currentTime = () => Math.floor(Date.now() / 1_000);

const ordinaryResponses: Record<string, unknown> = {
  '/deployments?limit=50': { deployments: [] },
  '/revisions?limit=50': { current_revision: 1, revisions: [] },
  '/deployments/verify': { summary: { changed_targets: 0 } },
  '/binary-releases/agent': { current: null },
  '/binary-releases/xray': { current: null },
  '/deployment-settings': {},
};
const nodeEntries = [
  { node_id: 'fresh', name: '宿主甲' },
  { node_id: 'stale', name: '宿主乙' },
];

function mount(
  role: AdminRole,
  stack: (url: string, init?: RequestInit) => Response = () => Response.json(view(currentTime())),
  extraNodes: object[] = [],
) {
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.startsWith(path)) return stack(url, init);
    if (url === '/nodes/agent-state') return Response.json({ nodes: [...nodeEntries, ...extraNodes] });
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
  return {
    stackRequests,
    posts: () => stackRequests().filter(([, init]) => init?.method === 'POST'),
    refetch: () =>
      act(async () => {
        await client.refetchQueries({ queryKey: ['cvm-stack-releases'] });
      }),
  };
}

async function openTab() {
  fireEvent.click(await screen.findByRole('tab', { name: 'Cube' }));
  await screen.findByRole('region', { name: 'Cube 宿主' });
}

const row = (name: string) =>
  within(screen.getByRole('rowheader', { name: new RegExp(name) }).closest('[role="row"]') as HTMLElement);
const button = (name: string) => screen.getByRole<HTMLButtonElement>('button', { name });
const scope = (label: string) => within(screen.getByRole('group', { name: label }));

function expectPost(posts: ReturnType<ReturnType<typeof mount>['posts']>, node: string, pair: string) {
  expect(posts).toEqual([
    [
      path,
      expect.objectContaining({
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ node_id: node, pair_id: pair }),
      }),
    ],
  ]);
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
      expect(screen.queryByRole('tab', { name: 'Cube' }) !== null).toBe(CVM_ENABLED && role === 'system-admin');
      expect(stackRequests()).toEqual([]);
      fireEvent.keyDown(config, { key: 'End' });
      if (CVM_ENABLED && role === 'system-admin') {
        await screen.findByRole('region', { name: 'Cube 宿主' });
        expect(stackRequests()).toEqual([[path, expect.objectContaining({ credentials: 'same-origin' })]]);
      } else {
        expect(screen.getByRole('tab', { name: 'Xray' }).getAttribute('aria-selected')).toBe('true');
        expect(stackRequests()).toEqual([]);
      }
    },
  );

  it.runIf(CVM_ENABLED)('无宿主上报时保留配置提示，禁用原因可聚焦，历史按需加载', async () => {
    const { stackRequests } = mount('system-admin', url =>
      Response.json(url === path ? { pairs: [], hosts: [], releases: [] } : []),
    );
    await openTab();
    expect(screen.getByText(/暂无有效的宿主运行栈上报/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: '前往容器电源操作' })).toBeNull();
    for (const name of ['安装 Cube', '升级 Cube']) {
      const entry = button(name);
      expect(entry.getAttribute('aria-disabled')).toBe('true');
      expect(entry.disabled).toBe(false);
      expect(document.getElementById(entry.getAttribute('aria-describedby')!)?.textContent).toMatch(/暂无可.*的宿主/);
      entry.focus();
      expect(document.activeElement).toBe(entry);
      fireEvent.click(entry);
    }
    expect(screen.queryByRole('radio')).toBeNull();
    expect(stackRequests().map(([url]) => url)).toEqual([path]);
    fireEvent.click(button('发布历史'));
    expect(await screen.findByText('暂无运行栈发布记录。')).toBeTruthy();
    expect(stackRequests().map(([url]) => url)).toEqual([path, `${path}/history`]);
  });

  it.runIf(CVM_ENABLED)('活跃 CVM 节点缺少观测时仍显示为未知且不可选', async () => {
    const data = view(currentTime());
    mount('system-admin', () => Response.json(data), [
      { node_id: 'missing', name: '无观测宿主', lifecycle_phase: 'active', runtime_versions: { agent_flavor: 'cvm' } },
      { node_id: 'base', name: '基础版节点', lifecycle_phase: 'active', runtime_versions: { agent_flavor: 'base' } },
      { node_id: 'retired', name: '退役节点', lifecycle_phase: 'retired', runtime_versions: { agent_flavor: 'cvm' } },
    ]);
    await openTab();
    expect(row('无观测宿主').getByText('状态未知')).toBeTruthy();
    expect(screen.queryByRole('rowheader', { name: /基础版节点|退役节点/ })).toBeNull();
    fireEvent.click(button('升级 Cube'));
    expect(screen.getAllByRole('radio').map(radio => radio.getAttribute('aria-label'))).toEqual(['升级 宿主甲']);
  });

  it.runIf(CVM_ENABLED)('有宿主上报但没有签名包时提示检查发布目录', async () => {
    mount('system-admin', () => Response.json({ ...view(currentTime()), pairs: [] }));
    await openTab();
    expect(screen.getByText(/暂无可用的签名运行包/)).toBeTruthy();
    expect(row('宿主甲').getByText('不可升级')).toBeTruthy();
    expect(button('升级 Cube').getAttribute('aria-disabled')).toBe('true');
  });

  it.runIf(CVM_ENABLED)('混合宿主分开选择，匹配存储和三项来源摘要，提交后显示服务端截止', async () => {
    const now = currentTime();
    const data = view(now);
    data.hosts.push(empty(now, 'xfs'), { ...installed(now), node_id: 'future', observed_at: now + 300 });
    data.pairs.push({ ...coldSource, id: 'second-xfs', target_sha256: 'e'.repeat(64), storage_backend: 'xfs' });
    const deadline = now + 18_180;
    const { posts } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const created = release(now, { status: 'preparing', start_deadline_at: deadline });
      data.releases = [created];
      return Response.json(created);
    });
    await openTab();
    expect(button('升级 Cube').classList.contains('primary')).toBe(true);
    expect(button('安装 Cube').classList.contains('primary')).toBe(false);
    expect(row('宿主乙').getByText('证据已过期，当前状态未知')).toBeTruthy();
    expect(row('future').getByText('状态未知')).toBeTruthy();
    fireEvent.click(button('升级 Cube'));
    const toolbar = scope('升级范围');
    expect(toolbar.getByText(/停止该宿主上正在运行的虚拟机并等待回执/)).toBeTruthy();
    expect(toolbar.getByText(/维护窗口 5 小时，预计/)).toBeTruthy();
    expect(toolbar.getByRole<HTMLButtonElement>('button', { name: '升级' }).disabled).toBe(true);
    expect(screen.getAllByRole('radio').map(radio => radio.getAttribute('aria-label'))).toEqual(['升级 宿主甲']);
    fireEvent.click(screen.getByRole('radio', { name: '升级 宿主甲' }));
    const pair = toolbar.getByLabelText<HTMLSelectElement>('兼容运行包对');
    expect(Array.from(pair.options, option => option.value)).toEqual(['', 'upgrade-xfs', 'second-xfs']);
    fireEvent.change(pair, { target: { value: 'upgrade-xfs' } });
    fireEvent.click(toolbar.getByRole('button', { name: '升级' }));
    await screen.findByText('准备中');
    const submitted = screen.getByRole('status');
    expect(submitted.textContent).toContain('维护窗口 5 小时');
    expect(submitted.textContent).not.toContain('预计');
    expect(submitted.querySelector('time')?.dateTime).toBe(new Date(deadline * 1_000).toISOString());
    expectPost(posts(), 'fresh', 'upgrade-xfs');
  });

  it.runIf(CVM_ENABLED)('全未安装时主入口为安装，只有兼容空机可选，首次安装自动执行', async () => {
    const now = currentTime();
    const data = view(now);
    data.hosts = [empty(now, 'plain'), empty(now, 'xfs')];
    const { posts, refetch } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const created = release(now, { node_id: 'empty-plain', source_sha256: null, status: 'preparing' });
      data.releases = [created];
      return Response.json(created);
    });
    await openTab();
    expect(screen.getByRole('region', { name: '已安装' }).textContent).toContain('0 / 2 台');
    expect(button('安装 Cube').classList.contains('primary')).toBe(true);
    expect(button('升级 Cube').getAttribute('aria-disabled')).toBe('true');
    fireEvent.click(button('安装 Cube'));
    fireEvent.click(screen.getByRole('radio', { name: '安装 empty-xfs' }));
    fireEvent.click(screen.getByRole('radio', { name: '安装 empty-plain' }));
    expect(screen.getByRole<HTMLInputElement>('radio', { name: '安装 empty-xfs' }).checked).toBe(false);
    const toolbar = scope('安装范围');
    expect(toolbar.queryByRole('combobox')).toBeNull();
    expect(toolbar.queryByText(/停机|停止/)).toBeNull();
    expect(toolbar.queryByRole('button', { name: '前往容器电源操作' })).toBeNull();
    fireEvent.click(toolbar.getByRole('button', { name: '安装' }));
    await screen.findByText('准备中');
    const update = async (status: StackRelease['status']) => {
      data.releases = [{ ...data.releases[0], status }];
      await refetch();
    };
    await update('ready');
    expect(row('empty-plain').getByText('准备中')).toBeTruthy();
    expect(row('empty-plain').getByRole('button', { name: '取消发布 #17' })).toBeTruthy();
    await update('applying');
    await screen.findByText('安装中');
    expect(screen.queryByRole('button', { name: /取消发布/ })).toBeNull();
    await update('succeeded');
    await screen.findByText('当前观测尚未确认安装包摘要与目标包一致。');
    data.hosts = [
      empty(now, 'xfs'),
      { ...installed(now), node_id: 'empty-plain', bundle_sha256: target, storage_backend: 'plain' },
    ];
    await refetch();
    await waitFor(() => expect(row('empty-plain').getByText('已一致')).toBeTruthy());
    expect(screen.queryByRole('button', { name: '前往容器电源操作' })).toBeNull();
    expectPost(posts(), 'empty-plain', 'install-plain');
  });

  it.runIf(CVM_ENABLED)('失败后可重试，拒绝提交时保留宿主和清单选择，阻断须恢复', async () => {
    const now = currentTime();
    const data = view(now);
    data.hosts.push({ ...installed(now), node_id: 'blocked' });
    const alternate = { ...coldSource, id: 'alternate', storage_backend: 'xfs' } as const;
    data.pairs.push(alternate);
    data.releases = [
      release(now, { status: 'recovered', error: '已恢复来源包' }),
      release(now, { id: 16, node_id: 'blocked', status: 'blocked' }),
    ];
    const { posts } = mount('system-admin', (_url, init) =>
      init?.method === 'POST' ? Response.json({ error: '虚拟机尚未停止' }, { status: 409 }) : Response.json(data),
    );
    await openTab();
    expect(row('blocked').getByText('已阻断 · 待恢复')).toBeTruthy();
    expect(row('宿主甲').getByText('已恢复来源包')).toBeTruthy();
    fireEvent.click(button('升级 Cube'));
    expect(screen.queryByRole('radio', { name: '升级 blocked' })).toBeNull();
    const radio = screen.getByRole<HTMLInputElement>('radio', { name: '升级 宿主甲' });
    fireEvent.click(radio);
    const toolbar = scope('升级范围');
    const pair = toolbar.getByLabelText<HTMLSelectElement>('兼容运行包对');
    fireEvent.change(pair, { target: { value: alternate.id } });
    const upgrade = toolbar.getByRole<HTMLButtonElement>('button', { name: '升级' });
    fireEvent.click(upgrade);
    expect((await toolbar.findByRole('alert')).textContent).toBe('虚拟机尚未停止');
    await waitFor(() => expect(upgrade.disabled).toBe(false));
    expect(radio.checked).toBe(true);
    expect(pair.value).toBe(alternate.id);
    expectPost(posts(), 'fresh', alternate.id);
  });

  it.each(['preparing', 'ready'] as const)('从 %s 行直接取消，之后按最新观测恢复入口', async status => {
    if (!CVM_ENABLED) return;
    const now = currentTime();
    const data = view(now);
    data.releases = [release(now, { status })];
    const { posts } = mount('system-admin', (_url, init) => {
      if (init?.method !== 'POST') return Response.json(data);
      const cancelled = { ...data.releases[0], status: 'cancelled' } as const;
      data.releases = [cancelled];
      return Response.json(cancelled);
    });
    await openTab();
    expect(row('宿主甲').getByText('准备中')).toBeTruthy();
    expect(button('升级 Cube').getAttribute('aria-disabled')).toBe('true');
    fireEvent.click(button('取消发布 #17'));
    await screen.findByText('#17 已取消');
    expect(button('升级 Cube').getAttribute('aria-disabled')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(posts()).toEqual([[`${path}/17/cancel`, { method: 'POST', credentials: 'same-origin', headers: {} }]]);
  });

  it.runIf(CVM_ENABLED)('运行中无取消入口，冷切换后须观测确认目标才能提示恢复电源', async () => {
    const now = currentTime();
    const data = view(now);
    data.releases = [
      release(now, { status: 'applying' }),
      release(now, { id: 16, node_id: 'no-evidence', status: 'preparing' }),
    ];
    const { refetch } = mount('system-admin', () => Response.json(data));
    await openTab();
    expect(row('宿主甲').getByText('切换中')).toBeTruthy();
    expect(row('宿主甲').queryByRole('button', { name: /取消发布/ })).toBeNull();
    expect(row('no-evidence').getByText('宿主安装状态未知')).toBeTruthy();
    data.releases[0] = release(now, { status: 'succeeded' });
    await refetch();
    await screen.findByText('当前观测尚未确认安装包摘要与目标包一致。');
    expect(screen.queryByRole('button', { name: '前往容器电源操作' })).toBeNull();
    data.hosts[0] = { ...installed(now), bundle_sha256: target };
    await refetch();
    await screen.findByText('仅启动维护前运行的虚拟机，逐台验证 VM Shim 冷启动。');
    expect(row('宿主甲').getByText('已一致')).toBeTruthy();
    expect(button('前往容器电源操作')).toBeTruthy();
  });

  it.runIf(CVM_ENABLED)('历史展开后才读取终态摘要，保留顺序且不随后台概览读取', async () => {
    const now = currentTime();
    const data = view(now);
    data.releases = [release(now)];
    const records = [
      release(now, { id: 16, status: 'recovered', roots_sha256: shim, error: '卷根校验失败' }),
      release(now, { id: 15, status: 'succeeded', source_sha256: null, node_id: 'empty-xfs' }),
    ];
    const { refetch, stackRequests } = mount('system-admin', url => Response.json(url === path ? data : records));
    await openTab();
    expect(stackRequests().map(([url]) => url)).toEqual([path]);
    fireEvent.click(button('发布历史'));
    const history = within(screen.getByRole('region', { name: '运行栈发布记录' }));
    await history.findByRole('button', { name: /#16/ });
    expect(history.getAllByRole('listitem').map(item => item.textContent?.match(/#\d+/)?.[0])).toEqual(['#16', '#15']);
    fireEvent.click(history.getByRole('button', { name: /#16/ }));
    expect(history.getByText('目标包 SHA-256')).toBeTruthy();
    expect(history.getByText(target)).toBeTruthy();
    expect(history.getByText(shim)).toBeTruthy();
    expect(history.getByText('卷根校验失败')).toBeTruthy();
    expect(history.queryByRole('button', { name: /取消发布/ })).toBeNull();
    await refetch();
    expect(stackRequests().map(([url]) => url)).toEqual([path, `${path}/history`, path]);
    fireEvent.click(button('收起发布历史'));
    expect(history.queryByRole('button', { name: /#16/ })).toBeNull();
  });

  it.runIf(CVM_ENABLED)('服务端拒绝后隐藏缓存的发布证据、历史和写入口', async () => {
    const now = currentTime();
    const data = view(now);
    data.releases = [release(now)];
    let allowed = true;
    const { refetch } = mount('system-admin', () =>
      allowed ? Response.json(data) : Response.json({ error: 'forbidden' }, { status: 403 }),
    );
    await openTab();
    expect(screen.getByText('准备中')).toBeTruthy();
    allowed = false;
    await refetch();
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('当前账号不能查看这项内容'));
    expect(screen.queryByText('准备中')).toBeNull();
    expect(screen.queryByRole('button', { name: '发布历史' })).toBeNull();
    expect(button('升级 Cube').getAttribute('aria-disabled')).toBe('true');
  });
});
