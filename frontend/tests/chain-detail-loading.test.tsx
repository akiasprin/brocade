import { StrictMode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { CompileView, ConsoleSnapshot, NodeAgentStateItem, SnapshotApp, Whoami } from '../src/api';
import { draft } from '../src/draft';
import { ChainsPane } from '../src/panes/chains';
import { SessionProvider } from '../src/session';
import { Loading, LoadingBoundary } from '../src/ui/bits';
import type { Win } from '../src/wm/store';

const initial = { node_count: 0, chain_group_count: [] };

const app: SnapshotApp = {
  id: 'app-1',
  label: '测试项目',
  chains: [{ id: 'chain-1', tenant: 'platform', name: '测试线路' }],
  ingresses: [
    {
      id: 'ingress-1',
      chain: 'chain-1',
      node: 'entry',
      bind: '0.0.0.0',
      port: 443,
      projection: {},
      guard: {
        no_private: true,
        no_bittorrent: true,
        no_mail: true,
        no_udp_amplification: true,
        tcp_and_quic_only: false,
        cn_source_block: 'off',
      },
      identity: { public_key: 'public-key', short_ids: ['0123456789abcdef'] },
      wires: { vless: { kind: 'vless-reality' } },
    },
  ],
  steps: [{ chain: 'chain-1', node: 'entry', accept: null, hop_in: null, rules: [] }],
  fronts: [],
  grants: [],
};
const snapshot: ConsoleSnapshot = {
  snapshot: { revision: 7, apps: [app], nodes: [], external_outbounds: [] },
  node_egress_dns: [],
  redacted: false,
};
const compiled: CompileView = {
  revision: 7,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: {},
  apps: [],
  redacted: false,
};
const settings = {
  ports: { ingress_base: 13443, anytls_base: 15443, hop_base: 22000, hy2_base: 32000 },
  reality_site: {
    dest: 'site.example:443',
    server_names: ['site.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
};
const win: Win = {
  id: 1,
  key: 'tab:chains',
  title: '线路',
  x: 0,
  y: 0,
  w: 1000,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'chain', app: app.id, chain: 'chain-1' } },
};
const responses: Record<string, unknown> = {
  '/model/snapshot': snapshot,
  '/nodes/agent-state': { nodes: [] },
  '/probes/e2e?format=columnar-v1': { chains: [] },
  '/settings': settings,
  '/revisions?limit=50': { current_revision: 7, revisions: [] },
  '/compile/7': compiled,
  '/apps/app-1/chains/chain-1/unlock-probe/capabilities': {
    family_mode: 'split',
    ipv6_available: true,
    ipv6_reason: null,
  },
  '/apps/app-1/chains/chain-1/unlock-probe?family=v4': null,
};
const clients: QueryClient[] = [];

function deferredResponse() {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>(done => {
    resolve = done;
  });
  return { promise, resolve };
}

function setup({
  viewer = 'operator',
  current = 7,
}: {
  viewer?: 'operator' | 'public' | 'user';
  current?: number | null;
} = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  clients.push(client);
  // Entering from the list already has these two responses, but not the detail's editor dependencies.
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['e2e-probes'], { chains: [] });
  const held = new Map<string, ReturnType<typeof deferredResponse>>();
  const fetcher = vi.fn((path: string) => {
    if (held.has(path)) return held.get(path)!.promise;
    if (!(path in responses)) throw new Error(`Unexpected request: ${path}`);
    const body = path === '/revisions?limit=50' ? { current_revision: current, revisions: [] } : responses[path];
    return Promise.resolve(Response.json(body));
  });
  vi.stubGlobal('fetch', fetcher);
  const who: Whoami = {
    operator_id: viewer,
    role: viewer === 'operator' ? 'system-admin' : viewer === 'public' ? 'readonly' : 'user',
    tenant_scope: viewer === 'user' ? 'platform' : null,
    token_prefix: null,
    masked_assets: viewer !== 'operator',
  };
  const mount = () =>
    render(
      <StrictMode>
        <QueryClientProvider client={client}>
          <SessionProvider value={{ who, initial }}>
            <LoadingBoundary variant="chain-detail" fallback={<Loading variant="chain-detail" showSkeleton />}>
              <ChainsPane win={win} />
            </LoadingBoundary>
          </SessionProvider>
        </QueryClientProvider>
      </StrictMode>,
    );
  const hold = (path: string) => {
    const response = deferredResponse();
    held.set(path, response);
    return response;
  };
  const finish = async (path: string, response = Response.json(responses[path])) => {
    await act(async () => {
      held.get(path)!.resolve(response);
      // React Query batches observer notifications onto the next task.
      await new Promise(resolve => setTimeout(resolve, 0));
    });
  };
  return { client, fetcher, mount, hold, finish };
}

afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  draft.clear();
  vi.unstubAllGlobals();
});

describe('客户端配置面板', () => {
  it('保存栏始终占位，校验两个地址族并合并为同一份草稿', async () => {
    draft.init('client-projection-panel-test');
    const { mount } = setup();
    const projection = {
      v4: { host: 'edge.example.com', port: 443 },
      v6: { host: '2001:db8::20', port: 443 },
    };
    const preview = {
      snapshot: {
        ...snapshot,
        snapshot: {
          ...snapshot.snapshot,
          apps: [{ ...app, ingresses: [{ ...app.ingresses[0], projection }] }],
        },
      },
      compile: compiled,
      artifacts: { revision: 7, artifacts: [] },
    };
    const fetch = globalThis.fetch;
    vi.stubGlobal('fetch', (path: string, init?: RequestInit) =>
      path === '/model/preview' ? Promise.resolve(Response.json(preview)) : fetch(path, init),
    );
    const view = mount();
    const title = await view.findByRole('heading', { name: '客户端配置' });
    const panel = within(title.closest('section')!);
    const save = panel.getByRole('button', { name: '保存' }) as HTMLButtonElement;
    const reset = panel.getByRole('button', { name: '还原' }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(reset.disabled).toBe(true);

    for (const toggle of panel.getAllByRole('button', { name: '转换' })) fireEvent.click(toggle);
    expect(save.disabled).toBe(true);
    expect(reset.disabled).toBe(false);
    fireEvent.change(panel.getByRole('textbox', { name: /IPv4 地址或域名$/ }), {
      target: { value: 'edge.example.com' },
    });
    expect(save.disabled).toBe(true);
    fireEvent.change(panel.getByRole('textbox', { name: /IPv6 地址或域名$/ }), {
      target: { value: '2001:db8::20' },
    });
    expect(panel.getByText('2 项更改待保存')).toBeTruthy();
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    expect(draft.ops()[0]).toMatchObject({
      op: 'upsert_ingress',
      app_id: 'app-1',
      ingress: {
        projection,
      },
    });
    await waitFor(() => expect(panel.queryByRole('textbox')).toBeNull());
    expect(panel.getByRole('button', { name: '保存' })).toBe(save);
    await waitFor(() => expect(save.disabled).toBe(true));
    expect(panel.getByText('edge.example.com:443')).toBeTruthy();
    expect(panel.getByText('[2001:db8::20]:443')).toBeTruthy();
  });
});

describe('入站机器迁移', () => {
  const node = (node_id: string, name: string) =>
    ({
      node_id,
      name,
      retired_at: null,
      public_ipv4: '192.0.2.1',
      public_ipv4_nat: false,
      wg_fake_tcp_port: null,
    }) as NodeAgentStateItem;

  const occupiedApp: SnapshotApp = {
    ...app,
    id: 'app-2',
    label: '占用端口的项目',
    chains: [{ id: 'chain-2', tenant: 'platform', name: '已有线路' }],
    ingresses: [
      {
        ...app.ingresses[0],
        id: 'ingress-2',
        chain: 'chain-2',
        node: 'target',
        port: 443,
      },
    ],
    steps: [{ chain: 'chain-2', node: 'target', accept: null, hop_in: null, rules: [] }],
  };

  const renderMigration = async (confirm: boolean) => {
    draft.init(`ingress-migration-${confirm}`);
    const prepared = setup();
    prepared.client.setQueryData(['snapshot'], {
      ...snapshot,
      snapshot: { ...snapshot.snapshot, apps: [app, occupiedApp] },
    });
    prepared.client.setQueryData(['nodes'], { nodes: [node('entry', '当前入口'), node('target', '目标入口')] });
    const confirmation = vi.fn((_message: string) => confirm);
    vi.stubGlobal('confirm', confirmation);
    const view = prepared.mount();
    await view.findByText('链路规则');
    const panel = within(view.getByRole('heading', { name: '入站' }).closest('section')!);
    const selection = panel.getByRole('combobox') as HTMLSelectElement;
    fireEvent.change(selection, { target: { value: 'target' } });
    return { confirmation, selection };
  };

  it('发现冲突后先说明轮转结果，取消时不写入草稿', async () => {
    const { confirmation, selection } = await renderMigration(false);
    expect(confirmation).toHaveBeenCalledOnce();
    expect(confirmation.mock.calls[0][0]).toContain('VLESS · TLS / REALITY：TCP 443');
    expect(confirmation.mock.calls[0][0]).toContain('改用 444');
    expect(draft.ops()).toEqual([]);
    expect(selection.value).toBe('entry');
  });

  it('确认后把机器和轮转端口合并成同一条草稿操作', async () => {
    const { confirmation } = await renderMigration(true);
    expect(confirmation).toHaveBeenCalledOnce();
    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    expect(draft.ops()[0]).toMatchObject({
      op: 'upsert_ingress',
      app_id: 'app-1',
      ingress: { node_id: 'target', port: 444 },
    });
  });
});

describe('链详情统一等待首屏依赖', () => {
  it('复用机器详情连续纸，并按接入、协议、规则顺序直接展开', async () => {
    const { mount } = setup();
    const view = mount();
    await view.findByRole('heading', { name: '入站' });
    const page = view.container.querySelector('.chain-detailpage');
    expect(page?.classList.contains('nd-sheet')).toBe(true);
    expect(page?.querySelector(':scope > .nd-paper > .nd-page-head')).toBeTruthy();
    expect(view.queryByRole('tablist')).toBeNull();
    expect(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBeTruthy();
    expect(view.getByText('链路规则')).toBeTruthy();
    expect(view.getByRole('button', { name: /entry入口/ })).toBeTruthy();
  });

  it.each(['settings-last', 'compile-last'])('协议与规则依赖一起就绪，整段等待不重挂骨架：%s', async order => {
    const { fetcher, mount, hold, finish } = setup();
    hold('/settings');
    hold('/revisions?limit=50');
    hold('/compile/7');
    const view = mount();
    const skeleton = view.getByRole('status');
    const expectWaiting = () => {
      expect(view.getAllByRole('status')).toEqual([skeleton]);
      expect(view.container.querySelector('.chain-detailpage')).toBeNull();
      expect(view.queryByText('链路规则')).toBeNull();
      expect(view.queryByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBeNull();
    };
    expectWaiting();
    expect(fetcher.mock.calls.map(([path]) => path)).toEqual(['/settings', '/revisions?limit=50']);

    await finish('/revisions?limit=50');
    await waitFor(() => expect(fetcher).toHaveBeenCalledWith('/compile/7', expect.anything()));
    expectWaiting();
    await finish(order === 'settings-last' ? '/compile/7' : '/settings');
    expectWaiting();
    await finish(order === 'settings-last' ? '/settings' : '/compile/7');

    await waitFor(() => expect(view.queryByRole('status')).toBeNull());
    expect(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBeTruthy();
    expect(view.getByRole('option', { name: '全局站点 site.example' })).toBeTruthy();
    expect(view.getByText('链路规则')).toBeTruthy();
    expect(view.container.querySelector('.chain-rule-node-head')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: /entry入口/ }));
    expect(view.container.querySelector('.rule-editor')).toBeTruthy();
    expect(view.queryByRole('status')).toBeNull();
    expect(fetcher.mock.calls.map(([path]) => path).sort()).toEqual([
      '/apps/app-1/chains/chain-1/unlock-probe/capabilities',
      '/apps/app-1/chains/chain-1/unlock-probe?family=v4',
      '/compile/7',
      '/revisions?limit=50',
      '/settings',
    ]);
  });

  it('冷启动时并行请求全部首屏依赖，仍等机器列表就绪才显示', async () => {
    const { client, fetcher, mount, hold, finish } = setup();
    client.removeQueries({ queryKey: ['snapshot'] });
    client.removeQueries({ queryKey: ['nodes'] });
    hold('/model/snapshot');
    hold('/nodes/agent-state');
    const view = mount();
    const skeleton = view.getByRole('status');
    await waitFor(() => expect(client.getQueryState(['compile', 7])?.status).toBe('success'));
    expect(fetcher.mock.calls.map(([path]) => path)).toEqual(
      expect.arrayContaining([
        '/model/snapshot',
        '/nodes/agent-state',
        '/settings',
        '/revisions?limit=50',
        '/compile/7',
      ]),
    );
    await finish('/model/snapshot');
    expect(view.getByRole('status')).toBe(skeleton);
    expect(view.container.querySelector('.chain-detailpage')).toBeNull();
    await finish('/nodes/agent-state');
    await waitFor(() => expect(view.queryByRole('status')).toBeNull());
    expect(view.getByText('链路规则')).toBeTruthy();
    expect(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBeTruthy();
  });

  it('首屏连通性请求未结束时不提前显示“还没探过”', async () => {
    const { client, mount, hold, finish } = setup();
    client.removeQueries({ queryKey: ['e2e-probes'] });
    hold('/probes/e2e?format=columnar-v1');
    const view = mount();
    await waitFor(() => expect(client.getQueryState(['compile', 7])?.status).toBe('success'));
    expect(view.getByRole('status')).toBeTruthy();
    expect(view.container.querySelector('.chain-detailpage')).toBeNull();
    await finish('/probes/e2e?format=columnar-v1');
    await waitFor(() => expect(view.queryByRole('status')).toBeNull());
    expect(view.getByText('链路规则')).toBeTruthy();
  });

  it('没有当前修订时不等待未启用的编译查询', async () => {
    const { fetcher, mount } = setup({ current: null });
    const view = mount();
    await waitFor(() => expect(view.queryByRole('status')).toBeNull());
    expect(view.getByText('链路规则')).toBeTruthy();
    expect(fetcher.mock.calls.some(([path]) => path.startsWith('/compile/'))).toBe(false);
  });

  it.each(['public', 'user'] as const)('%s 访客仍等待规则编译完成，但不会请求或等待无权读取的设置', async viewer => {
    const { client, fetcher, mount, hold, finish } = setup({ viewer });
    await client
      .fetchQuery({
        queryKey: ['settings'],
        queryFn: async () => {
          throw new Error('forbidden');
        },
      })
      .catch(() => undefined);
    hold('/compile/7');
    const view = mount();
    await waitFor(() => expect(fetcher).toHaveBeenCalledWith('/compile/7', expect.anything()));
    expect(view.container.querySelector('.chain-detailpage')).toBeNull();
    await finish('/compile/7');
    await waitFor(() => expect(view.queryByRole('status')).toBeNull());
    expect(view.getByText('链路规则')).toBeTruthy();
    expect(view.queryByText('forbidden')).toBeNull();
    expect(fetcher.mock.calls.some(([path]) => path === '/settings')).toBe(false);
  });

  it.each(['/settings', '/revisions?limit=50', '/compile/7'])('依赖失败退出等待并显示错误：%s', async path => {
    const { mount, hold, finish } = setup();
    hold(path);
    const view = mount();
    await finish(path, Response.json({ error: '首屏依赖读取失败' }, { status: 503 }));
    await waitFor(() => expect(view.getByText('首屏依赖读取失败')).toBeTruthy());
    expect(view.queryByRole('status')).toBeNull();
    expect(view.container.querySelector('.chain-detailpage')).toBeNull();
  });

  it('命中缓存直接显示，后台刷新不重挂协议表单或丢失输入', async () => {
    const { client, fetcher, mount, hold, finish } = setup();
    client.setQueryData(['settings'], settings);
    client.setQueryData(['revisions'], responses['/revisions?limit=50']);
    client.setQueryData(['compile', 7], compiled);
    const view = mount();
    expect(view.queryByRole('status')).toBeNull();
    expect(fetcher.mock.calls.map(([path]) => path)).toEqual([
      '/apps/app-1/chains/chain-1/unlock-probe/capabilities',
      '/apps/app-1/chains/chain-1/unlock-probe?family=v4',
    ]);
    fireEvent.click(view.getByRole('button', { name: '测试线路' }));
    const input = view.getByRole('textbox', { name: '链名' });
    fireEvent.change(input, { target: { value: '未保存的名称' } });
    const protocol = view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' });
    hold('/settings');
    act(() => {
      void client.invalidateQueries({ queryKey: ['settings'] });
    });
    expect(view.queryByRole('status')).toBeNull();
    expect(view.getByRole('textbox', { name: '链名' })).toBe(input);
    await finish('/settings');
    expect(view.queryByRole('status')).toBeNull();
    expect(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBe(protocol);
    expect(view.getByRole('textbox', { name: '链名' })).toBe(input);
    expect((input as HTMLInputElement).value).toBe('未保存的名称');
  });

  it.each(['success', 'error'] as const)('提交切换修订时保留页面、展开状态和输入：%s', async result => {
    const { client, fetcher, mount, hold, finish } = setup();
    const view = mount();
    await view.findByText('链路规则');
    fireEvent.click(view.getByRole('button', { name: '测试线路' }));
    const input = view.getByRole('textbox', { name: '链名' }) as HTMLInputElement;
    fireEvent.change(input, { target: { value: '继续编辑的名称' } });
    fireEvent.click(view.getByRole('button', { name: /entry入口/ }));
    const page = view.container.querySelector('.chain-detailpage');
    const editor = view.container.querySelector('.rule-editor')!;
    fireEvent.click(within(editor as HTMLElement).getByRole('button', { name: '＋ 加一条' }));
    const saveRules = view.getByRole('button', { name: '保存到草稿' }) as HTMLButtonElement;
    await waitFor(() => expect(saveRules.disabled).toBe(false));
    const protocol = view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' });
    const port = view.getByRole('textbox', { name: 'VLESS · TLS / REALITY 监听端口', hidden: true });
    const assertPreserved = () => {
      expect(view.container.querySelector('.chain-detailpage')).toBe(page);
      expect(view.container.querySelector('.rule-editor')).toBe(editor);
      expect(view.getByRole('textbox', { name: '链名' })).toBe(input);
      expect(input.value).toBe('继续编辑的名称');
      expect(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' })).toBe(protocol);
      expect(view.getByRole('textbox', { name: 'VLESS · TLS / REALITY 监听端口', hidden: true })).toBe(port);
      expect(view.getByRole('button', { name: /entry入口/ }).getAttribute('aria-expanded')).toBe('true');
    };
    hold('/compile/8');
    act(() => client.setQueryData(['revisions'], { current_revision: 8, revisions: [] }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledWith('/compile/8', expect.anything()));
    assertPreserved();
    expect(protocol.matches(':disabled')).toBe(true);
    expect(port.matches(':disabled')).toBe(true);
    expect(editor.closest('fieldset[disabled]')).toBeTruthy();
    expect(saveRules.disabled).toBe(true);
    fireEvent.click(saveRules);
    expect(draft.ops()).toEqual([]);

    // The snapshot can arrive before compilation. Show the new stored value in place,
    // while retaining unrelated edits and keeping port-dependent operations blocked.
    act(() =>
      client.setQueryData(['snapshot'], {
        ...snapshot,
        snapshot: {
          ...snapshot.snapshot,
          revision: 8,
          apps: [{ ...app, ingresses: [{ ...app.ingresses[0], port: 8443 }] }],
        },
      }),
    );
    await waitFor(() => expect((port as HTMLInputElement).value).toBe('8443'));
    assertPreserved();
    expect(saveRules.disabled).toBe(true);

    await finish(
      '/compile/8',
      result === 'success'
        ? Response.json({ ...compiled, revision: 8 })
        : Response.json({ error: '新修订读取失败' }, { status: 503 }),
    );
    assertPreserved();
    if (result === 'error') {
      await waitFor(() => expect(view.queryAllByText('新修订读取失败').length).toBeGreaterThan(0));
      expect(protocol.matches(':disabled')).toBe(true);
      act(() => client.setQueryData(['compile', 8], { ...compiled, revision: 8 }));
      await waitFor(() => expect(protocol.matches(':disabled')).toBe(false));
      assertPreserved();
    } else {
      await waitFor(() => expect(protocol.matches(':disabled')).toBe(false));
    }
    await waitFor(() => {
      expect(port.matches(':disabled')).toBe(false);
      expect(saveRules.disabled).toBe(false);
    });
  });
});
