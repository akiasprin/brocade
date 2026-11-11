import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const route = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock('../src/forge/route', () => route);

import { OnboardingGuide, openOnboardingGuide } from '../src/panes/onboarding';
import { ProvisionInstall, type Drill } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import { draft } from '../src/draft';
import { forge } from '../src/forge/state';
import { wm } from '../src/wm/store';
import type { NodeInitialization, ProvisionNodeResult } from '../src/api';

const who = {
  operator_id: 'root',
  role: 'system-admin' as const,
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};

const response = (body: unknown) => ({ ok: true, status: 200, statusText: 'OK', json: async () => body }) as Response;

function state(status: 'active' | 'complete' | 'not-applicable') {
  return {
    status,
    root_tenant_id: status === 'not-applicable' ? null : 'platform',
    started_at: status === 'not-applicable' ? null : '2026-10-19T00:00:00Z',
    node_id: status === 'complete' ? 'hk-01' : null,
    app_id: status === 'complete' ? 'app-main' : null,
    chain_id: status === 'complete' ? 'chain-main' : null,
    ingress_id: status === 'complete' ? 'ingress-main' : null,
    user_tenant_id: status === 'complete' ? 'platform' : null,
    user_id: status === 'complete' ? 'alice' : null,
    target_revision_id: status === 'complete' ? 3 : null,
    deployment_id: status === 'complete' ? 1 : null,
    completed_at: status === 'complete' ? '2026-10-19T00:10:00Z' : null,
  };
}

/** 模型已提交（修订 3）的进度：链、入口、首个用户都已记入检查点。 */
const committed = (extra: Record<string, unknown> = {}) => ({
  ...state('active'),
  node_id: 'hk-01',
  app_id: 'app-main',
  chain_id: 'chain-main',
  ingress_id: 'ingress-main',
  user_tenant_id: 'platform',
  user_id: 'alice',
  target_revision_id: 3,
  ...extra,
});

const queryClient = () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  // 线路一行的结果取自外壳共享的快照缓存。
  client.setQueryData(['snapshot'], {
    snapshot: { apps: [{ id: 'app-main', label: '默认分组', chains: [{ id: 'chain-main', name: '香港直出' }] }] },
  });
  return client;
};

function renderGuide(
  status: 'active' | 'complete' | 'not-applicable',
  client = queryClient(),
  result?: ProvisionNodeResult,
) {
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: 0, chain_group_count: [], onboarding: status } }}>
        <main className="fg-view" aria-label="机器页面">
          真实业务页面
        </main>
        {result && <ProvisionInstall node="hk-01" result={result} go={vi.fn()} />}
        <OnboardingGuide />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

const checklist = (view: ReturnType<typeof render>) => view.queryByRole('region', { name: '首次上线' });
const pill = (view: ReturnType<typeof render>) =>
  view.getAllByRole('button').find(button => button.className === 'onb-pill');

let setupId = 0;

function installation(client: QueryClient) {
  const result = {
    revision_id: 2,
    enrollment: { token: 'test-onboarding-token', install_command: 'test-install-command', token_prefix: 'test' },
  } as ProvisionNodeResult;
  const drill: Drill = { p: 'install', node: 'hk-01', step: 2, result };
  const win = wm.open('tab:nodes', '机器');
  wm.patchData(win.id, { drill });
  client.setQueryData(['nodes'], {
    nodes: [
      { node_id: 'hk-01', tenant_id: 'platform', name: '首台机器', token_last_used_at: null, last_poll_at: null },
    ],
  });
  client.setQueryData(['revisions'], { current_revision: 2, revisions: [] });
  client.setQueryData(['certs'], { domain: null, groups: [], nodes: [] });
  return drill;
}

function onlineMachine(client: QueryClient, initialization: NodeInitialization) {
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'hk-01',
        tenant_id: 'platform',
        name: '首台机器',
        token_last_used_at: new Date().toISOString(),
        last_poll_at: new Date().toISOString(),
        initialization,
      },
    ],
  });
}

const initialized: NodeInitialization = { state: 'succeeded', revision_id: 2, deployment_id: 4, reason: null };

beforeEach(() => {
  route.navigate.mockReset();
  wm.init(`onboarding-test-${++setupId}`);
  draft.init(`onboarding-test-${setupId}`);
  forge.setNav('nodes');
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});

afterEach(() => {
  cleanup();
  draft.clear();
  localStorage.removeItem('brocade.first-run.model-receipt.root');
  vi.unstubAllGlobals();
});

describe('first-run onboarding checklist', () => {
  it('records the new machine and stays a pill over its installation command', async () => {
    const client = queryClient();
    const drill = installation(client);
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/onboarding' && init?.method === 'PUT') {
        expect(JSON.parse(String(init.body))).toEqual({ step: 'node', node_id: 'hk-01' });
        return response({ ...state('active'), node_id: 'hk-01' });
      }
      if (String(input) === '/onboarding') return response(state('active'));
      throw new Error(`unexpected request ${String(input)}`);
    });
    vi.stubGlobal('fetch', fetcher);
    const view = renderGuide('active', client, drill.result);
    expect(view.getByLabelText('安装命令').textContent).toBe('test-install-command');
    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ node_id: 'hk-01' }));
    expect(view.queryByRole('dialog')).toBeNull();
    expect(checklist(view)).toBeNull();
    expect(pill(view)?.textContent).toContain('在机器上执行本页的命令');
    expect(view.getByLabelText('安装命令').textContent).toBe('test-install-command');
    expect(route.navigate).not.toHaveBeenCalled();
    expect(wm.snapshot().wins.find(win => win.key === 'tab:nodes')?.data.drill).toBe(drill);
  });

  it('does not cover the registration form when the guide is restored', async () => {
    const client = queryClient();
    const win = wm.open('tab:nodes', '机器');
    wm.patchData(win.id, { drill: { p: 'provision', step: 1 } });
    client.setQueryData(['nodes'], { nodes: [] });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => response(state('active'))),
    );
    const view = renderGuide('active', client);
    await vi.waitFor(() => expect(pill(view)).toBeTruthy());
    expect(checklist(view)).toBeNull();
    expect(pill(view)?.textContent).toContain('在本页填写机器 ID 并纳管');
  });

  it('expands on request over the current installation without navigating or losing its token', async () => {
    const client = queryClient();
    const drill = installation(client);
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => response({ ...state('active'), node_id: 'hk-01' })),
    );
    const view = renderGuide('active', client, drill.result);
    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ node_id: 'hk-01' }));
    act(() => openOnboardingGuide());
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('在机器上执行安装命令')).toBeTruthy();
    expect(within(list).queryByRole('button', { name: '查看安装命令' })).toBeNull();
    fireEvent.click(within(list).getByRole('button', { name: '收起首次上线清单' }));
    expect(checklist(view)).toBeNull();
    expect(route.navigate).not.toHaveBeenCalled();
    expect(view.getByLabelText('安装命令').textContent).toBe('test-install-command');
    expect(wm.snapshot().wins.find(win => win.key === 'tab:nodes')?.data.drill).toBe(drill);
  });

  it('reuses the existing installation receipt when returning from another page', async () => {
    const client = queryClient();
    const drill = installation(client);
    forge.setNav('users');
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => response({ ...state('active'), node_id: 'hk-01' })),
    );
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: '查看安装命令' }));
    expect(route.navigate).toHaveBeenCalledWith('nodes', drill);
  });

  it('continues on liveness for legacy registrations without an initialization task', async () => {
    const client = queryClient();
    installation(client);
    client.setQueryData(['nodes'], {
      nodes: [
        {
          node_id: 'hk-01',
          tenant_id: 'platform',
          token_last_used_at: new Date().toISOString(),
          last_poll_at: new Date().toISOString(),
        },
      ],
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => response({ ...state('active'), node_id: 'hk-01' })),
    );
    const view = renderGuide('active', client);
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('创建第一条线路')).toBeTruthy();
    expect(within(list).getByText('hk-01 · 在线')).toBeTruthy();
  });

  it.each(['waiting-agent', 'queued', 'running', 'manual', 'failed'] as const)(
    'keeps the installation surface uninterrupted while initialization is %s',
    async initializationState => {
      const client = queryClient();
      const drill = installation(client);
      onlineMachine(client, {
        state: initializationState,
        revision_id: 2,
        deployment_id: initializationState === 'running' || initializationState === 'failed' ? 4 : null,
        reason: null,
      });
      client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
      vi.stubGlobal('fetch', vi.fn());
      const view = renderGuide('active', client, drill.result);
      expect(checklist(view)).toBeNull();
      expect(pill(view)).toBeTruthy();
      expect(view.queryByRole('button', { name: '创建线路' })).toBeNull();
      expect(wm.snapshot().wins.find(win => win.key === 'tab:nodes')?.data.drill).toBe(drill);
      expect(route.navigate).not.toHaveBeenCalled();
    },
  );

  it('prompts for the first chain only after the server reports initialization success', async () => {
    const client = queryClient();
    installation(client);
    onlineMachine(client, { state: 'running', revision_id: 2, deployment_id: 4, reason: null });
    client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
    vi.stubGlobal('fetch', vi.fn());
    const view = renderGuide('active', client);
    expect(checklist(view)).toBeNull();
    expect(pill(view)?.textContent).toContain('Agent 已上线，初始化中');

    act(() => {
      onlineMachine(client, initialized);
    });
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('创建第一条线路')).toBeTruthy();
    fireEvent.click(within(list).getByRole('button', { name: '创建线路' }));
    expect(route.navigate).toHaveBeenCalledWith('nodes', { p: 'chain', id: 'hk-01' });
  });

  it('returns to the installation receipt to follow initialization from another page', async () => {
    const client = queryClient();
    const drill = installation(client);
    onlineMachine(client, { state: 'running', revision_id: 2, deployment_id: 4, reason: null });
    client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
    forge.setNav('users');
    vi.stubGlobal('fetch', vi.fn());
    const view = renderGuide('active', client);
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('Agent 已上线，初始化中')).toBeTruthy();
    fireEvent.click(within(list).getByRole('button', { name: '查看初始化进度' }));
    expect(route.navigate).toHaveBeenCalledWith('nodes', drill);
  });

  it.each([
    ['manual', '需要人工发布', '已有其他机器的配置待发布，请人工处理'],
    ['failed', '初始化需要处理', '初始化发布未完成，请查看发布单并重试失败目标'],
  ] as const)(
    'shows the server reason for %s initialization and preserves the installation receipt',
    async (s, title, reason) => {
      const client = queryClient();
      const drill = installation(client);
      onlineMachine(client, { state: s, revision_id: 2, deployment_id: s === 'failed' ? 4 : null, reason });
      client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
      vi.stubGlobal('fetch', vi.fn());
      const view = renderGuide('active', client);
      act(() => openOnboardingGuide());
      const list = await view.findByRole('region', { name: '首次上线' });
      expect(within(list).getByText(title)).toBeTruthy();
      expect(within(list).getByText(reason)).toBeTruthy();
      expect(within(list).queryByRole('button', { name: '查看初始化进度' })).toBeNull();
      expect(route.navigate).not.toHaveBeenCalled();
      expect(wm.snapshot().wins.find(win => win.key === 'tab:nodes')?.data.drill).toBe(drill);
    },
  );

  it('offers the release plan for review when its model was already committed during manual initialization', async () => {
    const client = queryClient();
    installation(client);
    onlineMachine(client, { state: 'manual', revision_id: 2, deployment_id: null, reason: '请人工发布' });
    client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01', target_revision_id: 3 });
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    forge.setNav('users');
    vi.stubGlobal('fetch', vi.fn());
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: '核对计划' }));
    expect(route.navigate).toHaveBeenCalledWith('deploy', { p: 'plan', revision: 3 });
  });

  it('publishes the committed revision in place and records the change order it created', async () => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], committed());
    forge.setNav('users');
    let created = false;
    const requests: { path: string; method: string; body: unknown }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        const method = init?.method ?? 'GET';
        requests.push({ path, method, body: init?.body ? JSON.parse(String(init.body)) : null });
        if (path === '/deployments' && method === 'POST') {
          created = true;
          return response({ deployment_id: 7, status: 'planned', reused: false });
        }
        if (path.startsWith('/deployments?'))
          return response({
            deployments: created
              ? [
                  {
                    id: 7,
                    revision_id: 3,
                    status: 'running',
                    activation_status: 'waiting',
                    awaiting_confirmation: false,
                  },
                ]
              : [],
          });
        if (path === '/onboarding' && method === 'PUT') return response(committed({ deployment_id: 7 }));
        if (path === '/deployments/7')
          return response({
            id: 7,
            revision_id: 3,
            status: 'running',
            activation_status: 'waiting',
            targets: [{ node_id: 'hk-01', status: 'converging', error: null, dispatched_at: '2026-10-19T00:08:00Z' }],
          });
        throw new Error(`unexpected request ${method} ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('修订 R3 · 发布到 首台机器')).toBeTruthy();
    fireEvent.click(within(list).getByRole('button', { name: '发布' }));

    await within(list).findByText('配置上线中');
    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ deployment_id: 7 }));
    const post = requests.find(request => request.path === '/deployments' && request.method === 'POST');
    expect(post?.body).toMatchObject({ revision_id: 3, idempotency_key: expect.any(String) });
    expect(requests).toContainEqual({
      path: '/onboarding',
      method: 'PUT',
      body: { step: 'deployment', deployment_id: 7 },
    });
    expect(await within(list).findByText('变更单 #7 · 应用中')).toBeTruthy();
    expect(route.navigate).not.toHaveBeenCalled();
  });

  it('creates the first user and its grant in place together with the staged chain', async () => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
    draft.push({
      op: 'upsert_chain',
      app_id: 'app-main',
      chain: { id: 'chain-main', tenant_id: 'platform', name: '香港直出' },
    });
    draft.push({
      op: 'create_ingress',
      app_id: 'app-main',
      ingress: { id: 'ingress-main', chain_id: 'chain-main', node_id: 'hk-01' },
    } as never);
    let applied: { ops: { op: string }[]; note: string } | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === '/model/apply') {
          applied = JSON.parse(String(init?.body));
          return response({ revision_id: 3, changed: 4, client_config: { status: 'unchanged' } });
        }
        if (path === '/onboarding' && init?.method === 'PUT') {
          expect(JSON.parse(String(init.body))).toMatchObject({
            step: 'model',
            app_id: 'app-main',
            chain_id: 'chain-main',
            ingress_id: 'ingress-main',
            user_id: 'alice',
            target_revision_id: 3,
          });
          return response(committed());
        }
        throw new Error(`unexpected request ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByText('香港直出')).toBeTruthy();
    fireEvent.change(within(list).getByLabelText('用户 ID'), { target: { value: 'alice' } });
    fireEvent.click(within(list).getByRole('button', { name: '创建并提交' }));

    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ target_revision_id: 3 }));
    expect(applied!.ops.map(op => op.op)).toEqual(['upsert_chain', 'create_ingress', 'create_user', 'upsert_grant']);
    expect(route.navigate).not.toHaveBeenCalled();
  });

  it.each([null, 6])('shows a fast-failing order and tracks its retry with prior checkpoint %s', async previousId => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], committed({ deployment_id: previousId }));
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    if (previousId != null)
      client.setQueryData(['deployment', previousId], {
        id: previousId,
        status: 'failed',
        activation_status: 'waiting',
        targets: [],
      });
    const keys: string[] = [];
    const checkpoints: number[] = [];
    let order = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === '/deployments' && init?.method === 'POST') {
          keys.push(JSON.parse(String(init.body)).idempotency_key);
          order = keys.length === 1 ? 7 : 8;
          return response({ deployment_id: order, status: 'planned', reused: false });
        }
        if (path.startsWith('/deployments?'))
          return response({
            deployments: order ? [{ id: order, revision_id: 3, status: order === 7 ? 'failed' : 'running' }] : [],
          });
        if (path === '/deployments/7' || path === '/deployments/8')
          return response({
            id: order,
            revision_id: 3,
            status: order === 7 ? 'failed' : 'running',
            activation_status: 'waiting',
            targets: [
              {
                node_id: 'hk-01',
                status: order === 7 ? 'failed' : 'converging',
                error: order === 7 ? 'xray 配置校验失败' : null,
              },
            ],
          });
        if (path === '/onboarding' && init?.method === 'PUT') {
          const id = JSON.parse(String(init.body)).deployment_id;
          checkpoints.push(id);
          return response(committed({ deployment_id: id }));
        }
        throw new Error(`unexpected request ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: previousId == null ? '发布' : '重新发布' }));
    await view.findByText(/变更单 #7 已停止.*xray 配置校验失败/);
    expect(view.getByText('发布没有完成', { exact: true })).toBeTruthy();
    expect(checkpoints).toEqual([]);
    fireEvent.click(view.getByRole('button', { name: '查看变更单' }));
    expect(route.navigate).toHaveBeenCalledWith('deploy', { p: 'detail', id: 7 });

    fireEvent.click(view.getByRole('button', { name: '重新发布' }));
    await view.findByText('配置上线中', { exact: true });
    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ deployment_id: 8 }));
    expect(checkpoints).toEqual([8]);
    expect(keys).toHaveLength(2);
    expect(keys[1]).not.toBe(keys[0]);
  });

  it('keeps a created order visible and retries its checkpoint before starting verification', async () => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], committed());
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    let checkpoints = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === '/deployments' && init?.method === 'POST')
          return response({ deployment_id: 7, status: 'planned', reused: false });
        if (path.startsWith('/deployments?')) return response({ deployments: [] });
        if (path === '/deployments/7')
          return response({ id: 7, revision_id: 3, status: 'succeeded', activation_status: 'activated', targets: [] });
        if (path === '/onboarding' && init?.method === 'PUT') {
          checkpoints += 1;
          return checkpoints === 1
            ? Response.json({ error: '检查点暂时不可用' }, { status: 503 })
            : response(committed({ deployment_id: 7 }));
        }
        if (path === '/grant-probes/capability') return response({ available: false, reason: '测试环境无拨测组件' });
        throw new Error(`unexpected request ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: '发布' }));
    await view.findByText('检查点暂时不可用', { exact: true });
    expect(view.getByText('变更单 #7 已生效，正在记录首次上线进度')).toBeTruthy();
    expect(view.queryByText('拨测不可用', { exact: true })).toBeNull();
    expect(view.getByRole('button', { name: '查看变更单' })).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '重试关联变更单' }));
    await view.findByText('拨测不可用', { exact: true });
    expect(checkpoints).toBe(2);
    expect(client.getQueryData(['onboarding'])).toMatchObject({ deployment_id: 7 });
  });

  it('can recover the checkpoint of an already committed model during manual initialization', async () => {
    const client = queryClient();
    installation(client);
    onlineMachine(client, { state: 'manual', revision_id: 2, deployment_id: null, reason: '请人工发布' });
    client.setQueryData(['onboarding'], { ...state('active'), node_id: 'hk-01' });
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    localStorage.setItem(
      'brocade.first-run.model-receipt.root',
      JSON.stringify({
        appId: 'app-main',
        chainId: 'chain-main',
        ingressId: 'ingress-main',
        tenantId: 'platform',
        userId: 'alice',
        revisionId: 3,
      }),
    );
    forge.setNav('users');
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        expect(String(input)).toBe('/onboarding');
        expect(init?.method).toBe('PUT');
        expect(JSON.parse(String(init?.body))).toMatchObject({ step: 'model', target_revision_id: 3 });
        return response({ ...state('active'), node_id: 'hk-01', target_revision_id: 3 });
      }),
    );
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: '重试确认' }));
    await vi.waitFor(() => expect(client.getQueryData(['onboarding'])).toMatchObject({ target_revision_id: 3 }));
  });

  it('probes only the onboarding entry in place and completes with its passed item', async () => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], committed({ deployment_id: 7 }));
    client.setQueryData(['deployment', 7], {
      id: 7,
      revision_id: 3,
      status: 'succeeded',
      activation_status: 'activated',
      targets: [],
    });
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    forge.setNav('users');
    const item = (id: string, ingress: string) => ({
      id,
      name: id,
      app_id: 'app-main',
      app_name: '默认分组',
      chain_id: ingress === 'ingress-main' ? 'chain-main' : 'chain-other',
      ingress_id: ingress,
      family: 'ipv4',
      protocol: 'vless',
    });
    const requests: { path: string; method: string; body: unknown }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        const method = init?.method ?? 'GET';
        requests.push({ path, method, body: init?.body ? JSON.parse(String(init.body)) : null });
        if (path === '/grant-probes/capability')
          return response({ available: true, version: '26.7.28', reason: null, concurrency: 4 });
        if (path === '/users/platform/alice/grant-probes' && method === 'GET')
          return response({
            serving_revision: 3,
            serving_generation: 9,
            items: [item('entry-v4', 'ingress-main'), item('other-v4', 'ingress-other')],
          });
        if (path === '/users/platform/alice/grant-probes' && method === 'POST')
          return response({
            reused: false,
            job: {
              id: 'probe-1',
              kind: 'grant',
              tenant_id: 'platform',
              user_id: 'alice',
              serving_revision: 3,
              serving_generation: 9,
              status: 'completed',
              message: null,
              created_at_unix_secs: 1,
              finished_at_unix_secs: 2,
              items: [{ ...item('entry-v4', 'ingress-main'), status: 'passed', ttfb_ms: 180, detail: null }],
            },
          });
        if (path === '/onboarding/complete') return response({ ...state('complete') });
        throw new Error(`unexpected request ${method} ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    const list = await view.findByRole('region', { name: '首次上线' });
    fireEvent.click(await within(list).findByRole('button', { name: '拨测' }));

    await within(list).findByText('第一条线路已经可用');
    expect(requests).toContainEqual({
      path: '/users/platform/alice/grant-probes',
      method: 'POST',
      body: { item_ids: ['entry-v4'] },
    });
    expect(requests).toContainEqual({
      path: '/onboarding/complete',
      method: 'POST',
      body: { probe_id: 'probe-1', item_id: 'entry-v4' },
    });
    expect(list.querySelectorAll('.onb-mark.done')).toHaveLength(5);
  });

  it('shows the checklist beside the real machine page and guides the first navigation', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/onboarding') return response(state('active'));
        if (String(input) === '/nodes/agent-state') return response({ nodes: [] });
        throw new Error(`unexpected request ${String(input)}`);
      }),
    );
    const view = renderGuide('active');

    const list = await view.findByRole('region', { name: '首次上线' });
    expect(view.queryByRole('dialog')).toBeNull();
    expect(view.getByRole('main', { name: '机器页面' })).toBeTruthy();
    expect(within(list).getByRole('list', { name: '首次上线步骤' }).children).toHaveLength(5);
    fireEvent.click(await within(list).findByRole('button', { name: '纳管机器' }));
    expect(route.navigate).toHaveBeenCalledWith('nodes', { p: 'provision', step: 1 });
  });

  it('keeps the running probe subscribed and records success while the checklist is collapsed', async () => {
    const client = queryClient();
    onlineMachine(client, initialized);
    client.setQueryData(['onboarding'], committed({ deployment_id: 7 }));
    client.setQueryData(['deployment', 7], { id: 7, status: 'succeeded', activation_status: 'activated', targets: [] });
    client.setQueryData(['deployments', 'config'], { deployments: [] });
    const item = {
      id: 'entry-v4',
      app_id: 'app-main',
      chain_id: 'chain-main',
      ingress_id: 'ingress-main',
      family: 'ipv4',
      protocol: 'vless',
    };
    const plan = { serving_revision: 3, serving_generation: 7, items: [item] };
    client.setQueryData(['grant-probe-capability'], { available: true });
    client.setQueryData(['grant-probe-plan', 'platform', 'alice'], plan);
    const running = {
      id: 'probe-1',
      kind: 'grant',
      tenant_id: 'platform',
      user_id: 'alice',
      status: 'running',
      items: [{ ...item, status: 'running' }],
    };
    const close = vi.fn();
    let snapshot: EventListener | undefined;
    vi.stubGlobal(
      'EventSource',
      class {
        addEventListener(name: string, listener: EventListener) {
          if (name === 'snapshot') snapshot = listener;
        }
        close = close;
      },
    );
    const completions: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === '/users/platform/alice/grant-probes')
          return response(init?.method === 'POST' ? { reused: false, job: running } : plan);
        if (path === '/onboarding/complete') {
          completions.push(JSON.parse(String(init?.body)));
          return response(state('complete'));
        }
        throw new Error(`unexpected request ${path}`);
      }),
    );
    const view = renderGuide('active', client);
    fireEvent.click(await view.findByRole('button', { name: '拨测' }));
    await view.findByText('拨测中', { exact: true });
    await vi.waitFor(() => expect(snapshot).toBeTypeOf('function'));
    fireEvent.click(view.getByRole('button', { name: '收起首次上线清单' }));
    expect(checklist(view)).toBeNull();
    expect(close).not.toHaveBeenCalled();
    act(() =>
      snapshot!(
        new MessageEvent('snapshot', {
          data: JSON.stringify({ ...running, status: 'completed', items: [{ ...item, status: 'passed' }] }),
        }),
      ),
    );
    await view.findByText('第一条线路已经可用', { exact: true });
    expect(completions).toEqual([{ probe_id: 'probe-1', item_id: 'entry-v4' }]);
  });

  it('collapses into a pill and expands again', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/onboarding') return response(state('active'));
        if (String(input) === '/nodes/agent-state') return response({ nodes: [] });
        throw new Error(`unexpected request ${String(input)}`);
      }),
    );
    const view = renderGuide('active');
    const list = await view.findByRole('region', { name: '首次上线' });
    fireEvent.click(within(list).getByRole('button', { name: '收起首次上线清单' }));
    expect(checklist(view)).toBeNull();
    expect(pill(view)?.getAttribute('aria-expanded')).toBe('false');
    expect(pill(view)?.textContent).toContain('纳管第一台机器');
    fireEvent.click(pill(view)!);
    expect(checklist(view)).toBeTruthy();
  });

  it('keeps clear of a lazily mounted footer after its entrance animation and releases it on removal', async () => {
    const client = queryClient();
    client.setQueryData(['onboarding'], state('active'));
    client.setQueryData(['nodes'], { nodes: [] });
    vi.stubGlobal('fetch', vi.fn());
    const view = renderGuide('active', client);
    const dock = view.container.querySelector<HTMLElement>('.onb-dock')!;
    const paper = document.createElement('div');
    paper.className = 'fg-sheet';
    const footer = document.createElement('footer');
    footer.className = 'pv-foot';
    let top = window.innerHeight - 52;
    footer.getBoundingClientRect = () =>
      ({ top, bottom: top + 58, height: 58, left: 0, right: 1440, width: 1440 }) as DOMRect;
    paper.append(footer);
    view.getByRole('main', { name: '机器页面' }).append(paper);
    await vi.waitFor(() => expect(dock.style.getPropertyValue('--onb-lift')).toBe('40px'));

    // 与真实纸面的入场一致：transform 消失只改变位置，不改变尺寸。
    top -= 6;
    fireEvent.animationEnd(paper, { animationName: 'motion-surface-mount' });
    await vi.waitFor(() => expect(dock.style.getPropertyValue('--onb-lift')).toBe('46px'));

    paper.remove();
    await vi.waitFor(() => expect(dock.style.getPropertyValue('--onb-lift')).toBe('0px'));
  });

  it('keeps a completed guide quiet on login and preserves its result when explicitly opened', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/onboarding') return response(state('complete'));
        throw new Error(`unexpected request ${String(input)}`);
      }),
    );
    const view = renderGuide('complete');
    await vi.waitFor(() => expect(view.queryByRole('button')).toBeNull());
    expect(checklist(view)).toBeNull();

    act(() => openOnboardingGuide());
    const list = await view.findByRole('region', { name: '首次上线' });
    expect(within(list).getByRole('heading', { name: '第一条线路已经可用' })).toBeTruthy();
    expect(list.querySelectorAll('.onb-mark.done')).toHaveLength(5);
    fireEvent.click(within(list).getByRole('button', { name: '查看订阅' }));
    expect(route.navigate).toHaveBeenCalledWith('users', { p: 'user', id: 'alice' });
    fireEvent.click(within(list).getByRole('button', { name: '完成' }));
    expect(checklist(view)).toBeNull();
    expect(view.queryByRole('button')).toBeNull();
  });

  it('does not mount an onboarding surface for established installations', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/onboarding') return response(state('not-applicable'));
        throw new Error(`unexpected request ${String(input)}`);
      }),
    );
    const view = renderGuide('not-applicable');
    await vi.waitFor(() => expect(checklist(view)).toBeNull());
    expect(view.queryByRole('button')).toBeNull();
  });
});
