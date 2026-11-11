import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Session } from '../src/session';

vi.mock('../src/forge/shell', () => ({
  ForgeShell: ({ session, onLogout }: { session: Session; onLogout: () => void }) => (
    <div>
      <span>{`viewer:${session.who.operator_id}`}</span>
      <button onClick={onLogout}>打开登录页</button>
    </div>
  ),
}));
vi.mock('../src/panes/preload', () => ({
  initialPaneFromHash: () => 'usage',
  initialNodeDetailFromHash: () => null,
  preloadPaneForHash: async () => undefined,
}));

const { App } = await import('../src/app');
const { suppressAutoPublic } = await import('../src/session');
const visitor: Session & { features: { cvm: boolean } } = {
  features: { cvm: true },
  who: {
    operator_id: 'public',
    role: 'readonly',
    tenant_scope: 'platform',
    token_prefix: null,
    masked_assets: true,
  },
  initial: { node_count: 0, chain_group_count: [] },
};
const mount = () =>
  render(
    <QueryClientProvider
      client={new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } })}
    >
      <App />
    </QueryClientProvider>,
  );
const stubRequests = (bootstrap: () => Response) => {
  const fetchMock = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const path = String(input);
    if (path === '/branding') return Response.json({ site_name: 'Test console', icon_data_url: null });
    if (path === '/auth/state') return Response.json({ initialized: true, public_open: true });
    if (path === '/bootstrap') return bootstrap();
    if (path === '/auth/logout') return Response.json({ revoked: false });
    throw new Error(`Unexpected request: ${path}`);
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
};

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
  window.history.replaceState(null, '', '/#/usage');
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
});

describe('无状态访客入口', () => {
  it('直接读取访客 bootstrap，首次访问不发起登录或退出请求', async () => {
    const requests = stubRequests(() => Response.json(visitor));
    const view = mount();
    await view.findByText('viewer:public');
    expect(requests.mock.calls.map(([input]) => String(input)).sort()).toEqual([
      '/auth/state',
      '/bootstrap',
      '/branding',
    ]);
  });

  it('打开登录页后刷新仍显示表单，点击访客模式才返回访客视角', async () => {
    const requests = stubRequests(() => Response.json(visitor));
    const first = mount();
    fireEvent.click(await first.findByRole('button', { name: '打开登录页' }));
    await first.findByRole('button', { name: '登录' });
    first.unmount();
    const refreshed = mount();
    await refreshed.findByRole('button', { name: '登录' });
    expect(refreshed.queryByText('viewer:public')).toBeNull();
    fireEvent.click(refreshed.getByRole('button', { name: '访客模式' }));
    await refreshed.findByText('viewer:public');
    expect(requests.mock.calls.some(([input]) => String(input) === '/auth/login')).toBe(false);
  });

  it('失效账号返回表单，显式进入访客时先清除旧账号 Cookie', async () => {
    let cleared = false;
    const requests = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
      const path = String(input);
      if (path === '/branding') return Response.json({ site_name: 'Test console', icon_data_url: null });
      if (path === '/auth/state') return Response.json({ initialized: true, public_open: true });
      if (path === '/bootstrap')
        return cleared ? Response.json(visitor) : Response.json({ error: 'unauthorized' }, { status: 401 });
      if (path === '/auth/logout') {
        cleared = true;
        return Response.json({ revoked: false });
      }
      throw new Error(`Unexpected request: ${path}`);
    });
    vi.stubGlobal('fetch', requests);
    const view = mount();
    await view.findByRole('button', { name: '登录' });
    expect(requests.mock.calls.filter(([input]) => String(input) === '/bootstrap')).toHaveLength(1);
    fireEvent.click(view.getByRole('button', { name: '访客模式' }));
    await view.findByText('viewer:public');
    expect(requests.mock.calls.filter(([, init]) => init?.method === 'POST').map(([input]) => String(input))).toEqual([
      '/auth/logout',
    ]);
    expect(requests.mock.calls.find(([input]) => String(input) === '/auth/logout')?.[1]?.method).toBe('POST');
  });

  it('访客自动进入被抑制时仍恢复有效的管理员账号', async () => {
    suppressAutoPublic();
    const requests = stubRequests(() =>
      Response.json({
        ...visitor,
        who: { ...visitor.who, operator_id: 'root', role: 'system-admin', tenant_scope: null, masked_assets: false },
      }),
    );
    const view = mount();
    await waitFor(() => expect(view.getByText('viewer:root')).toBeTruthy());
    expect(requests.mock.calls.some(([input]) => String(input) === '/auth/login')).toBe(false);
  });
});
