import { cleanup, fireEvent, render } from '@testing-library/react';
import { StrictMode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  DirectLogin,
  InitializeAdmin,
  PasswordLogin,
  Login,
  directLoginUrl,
  parseDirectLoginHash,
  parseInitializationHash,
} from '../src/ui/login';

const wrapper = () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
};

const jsonResponse = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '/');
});

describe('系统初始化默认身份', () => {
  it('严格解析安装器链接并保留编码后的凭据内容', () => {
    const token = 'bootstrap-test-token-with-at-least-32-bytes/+#%';
    expect(parseInitializationHash(`#/initialize/${encodeURIComponent(token)}`)).toBe(token);
    for (const hash of ['#/initialize/short', '#/initialize/%E0%A4%A', '#/initialize/a/b', '#/login/a/b']) {
      expect(parseInitializationHash(hash)).toBeNull();
    }
    expect(parseInitializationHash(`#/initialize/${'a'.repeat(513)}`)).toBeNull();
  });

  it('完整初始化链接自动携带凭据，在 StrictMode 下清除地址后仍可创建管理员', async () => {
    const token = 'bootstrap-test-token-with-at-least-32-bytes/+#%';
    window.history.replaceState(null, '', `/?from=install#/initialize/${encodeURIComponent(token)}`);
    const onLogin = vi.fn();
    const localWrite = vi.spyOn(Storage.prototype, 'setItem');
    let authorization: string | null = null;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      expect(path).not.toContain(token);
      if (path === '/auth/state') return jsonResponse({ initialized: false, public_open: false });
      if (path === '/auth/init') {
        expect(window.location.hash).toBe('');
        authorization = new Headers(init?.headers).get('authorization');
        return jsonResponse({ admin: { operator_id: 'root', role: 'system-admin' } }, 201);
      }
      if (path === '/bootstrap') {
        return jsonResponse({ features: { cvm: true }, who: { operator_id: 'root', role: 'system-admin' }, initial: {} });
      }
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(
      <StrictMode>
        <Login branding={{ site_name: '我的站点', icon_data_url: null }} onLogin={onLogin} />
      </StrictMode>,
      { wrapper: wrapper() },
    );
    await view.findByRole('button', { name: '创建管理员' });
    expect(window.location.hash).toBe('');
    expect(window.location.search).toBe('?from=install');
    expect(view.queryByLabelText('初始化凭据')).toBeNull();
    expect(view.container.innerHTML).not.toContain(token);
    fireEvent.change(view.getByLabelText('密码'), { target: { value: 'root-password' } });
    const submit = view.getByRole('button', { name: '创建管理员' }) as HTMLButtonElement;
    expect(submit.disabled).toBe(false);
    fireEvent.click(submit);
    await vi.waitFor(() => expect(onLogin).toHaveBeenCalledOnce());
    expect(authorization).toBe(`Bearer ${token}`);
    expect(localWrite).not.toHaveBeenCalled();
    localWrite.mockRestore();
  });

  it('已初始化站点打开旧链接时也清除地址中的一次性凭据', async () => {
    window.history.replaceState(null, '', '/#/initialize/bootstrap-test-token-with-at-least-32-bytes');
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse({ initialized: true, public_open: false })),
    );
    const view = render(<Login branding={{ site_name: '我的站点', icon_data_url: null }} onLogin={vi.fn()} />, {
      wrapper: wrapper(),
    });
    await view.findByRole('button', { name: '登录' });
    expect(window.location.hash).toBe('');
    expect(view.queryByLabelText('初始化凭据')).toBeNull();
  });

  it('读取状态前显示加载中，初始化站点不会先显示登录标题', async () => {
    let complete!: (response: Response) => void;
    const auth = new Promise<Response>(resolve => {
      complete = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn((path: string) => (path === '/auth/state' ? auth : new Promise(() => {}))),
    );
    const view = render(<Login branding={{ site_name: '我的站点', icon_data_url: null }} onLogin={vi.fn()} />, {
      wrapper: wrapper(),
    });
    expect(view.container.querySelector('.login-title')?.textContent).toBe('加载中');
    complete(jsonResponse({ initialized: false, public_open: false }));
    await vi.waitFor(() => expect(view.container.querySelector('.login-title')?.textContent).toBe('初始化'));
  });

  it('普通登录默认填写 root', () => {
    const view = render(<PasswordLogin onLogin={vi.fn()} publicOpen={false} />, { wrapper: wrapper() });
    const username = view.getByLabelText('用户名') as HTMLInputElement;
    const password = view.getByLabelText('密码') as HTMLInputElement;
    expect(username.value).toBe('root');
    expect(username.autocomplete).toBe('username');
    expect(password.type).toBe('password');
    expect(password.autocomplete).toBe('current-password');
  });

  it('以 root 初始化后进入入门向导，不预置网络用户', async () => {
    const onInitialized = vi.fn();
    let initBody: Record<string, unknown> | null = null;
    let initAuthorization: string | null = null;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/auth/init') {
        initBody = JSON.parse(String(init?.body));
        initAuthorization = new Headers(init?.headers).get('authorization');
        return jsonResponse(
          {
            admin: { operator_id: 'root', role: 'system-admin', tenant_scope: null, token_prefix: null },
            session_expires_at: '2030-01-01T00:00:00Z',
          },
          201,
        );
      }
      if (path === '/bootstrap') {
        return jsonResponse({
          features: { cvm: true },
          who: { operator_id: 'root', role: 'system-admin', tenant_scope: null, token_prefix: null },
          initial: { node_count: 0, chain_group_count: [], onboarding: 'active' },
        });
      }
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    const view = render(<InitializeAdmin onInitialized={onInitialized} />, { wrapper: wrapper() });
    expect((view.getByLabelText('用户名') as HTMLInputElement).value).toBe('root');
    const bootstrapToken = view.getByLabelText('初始化凭据') as HTMLInputElement;
    const password = view.getByLabelText('密码') as HTMLInputElement;
    expect(bootstrapToken.type).toBe('text');
    expect(bootstrapToken.autocomplete).toBe('off');
    expect(bootstrapToken.classList.contains('config-secret-input')).toBe(true);
    expect(password.type).toBe('password');
    expect(password.autocomplete).toBe('new-password');
    expect(view.getByText('先创建系统管理员；登录后由入门向导纳管首台机器并创建第一个用户。')).toBeTruthy();
    const submit = view.getByRole('button', { name: '创建管理员' }) as HTMLButtonElement;
    fireEvent.change(view.getByLabelText('密码'), { target: { value: 'root-password' } });
    expect(submit.disabled).toBe(true);
    fireEvent.change(view.getByLabelText('初始化凭据'), {
      target: { value: 'bootstrap-test-token-with-at-least-32-bytes' },
    });
    expect(submit.disabled).toBe(false);
    fireEvent.click(submit);

    await vi.waitFor(() => expect(onInitialized).toHaveBeenCalled());
    expect(initBody).toEqual({
      operator_id: 'root',
      display_name: 'root',
      password: 'root-password',
      root_tenant: 'platform',
    });
    expect(initAuthorization).toBe('Bearer bootstrap-test-token-with-at-least-32-bytes');
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/visitor-access')).toBe(false);
    expect(view.queryByText(/zero.*密码/)).toBeNull();
    expect(view.queryByText(/租户/)).toBeNull();
    expect(onInitialized).toHaveBeenCalledWith({
      features: { cvm: true },
      who: { operator_id: 'root', role: 'system-admin', tenant_scope: null, token_prefix: null },
      initial: { node_count: 0, chain_group_count: [], onboarding: 'active' },
    });
  });

  it('明确提示错误的一次性初始化凭据', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse({ error: 'unauthorized' }, 401)),
    );
    const view = render(<InitializeAdmin onInitialized={vi.fn()} />, { wrapper: wrapper() });

    fireEvent.change(view.getByLabelText('初始化凭据'), { target: { value: 'wrong-token' } });
    fireEvent.change(view.getByLabelText('密码'), { target: { value: 'root-password' } });
    fireEvent.click(view.getByRole('button', { name: '创建管理员' }));

    await vi.waitFor(() => expect(view.getByText('初始化凭据不正确')).toBeTruthy());
  });
});

describe('用户直达登录', () => {
  it('把 UUID 和 TOKEN 放在不会发给服务端的 fragment 中', () => {
    const url = directLoginUrl(
      '2d2304da-f114-4574-8d44-625afdb1db5c',
      'broc_login_secret/value',
      'https://console.example.net/console?from=admin#/users',
    );
    const parsed = new URL(url);

    expect(`${parsed.origin}${parsed.pathname}${parsed.search}`).toBe('https://console.example.net/console?from=admin');
    expect(parseDirectLoginHash(parsed.hash)).toEqual({
      uuid: '2d2304da-f114-4574-8d44-625afdb1db5c',
      token: 'broc_login_secret/value',
    });
    expect(parseDirectLoginHash('#/login/only-one-part')).toBeNull();
    expect(parseDirectLoginHash('#/login/%E0%A4%A/token')).toBeNull();
  });

  it('用 UUID + TOKEN 换取 cookie 后读取同一份控制台 bootstrap', async () => {
    const onLogin = vi.fn();
    const requests: { path: string; body: unknown }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        requests.push({ path, body: init?.body ? JSON.parse(String(init.body)) : null });
        if (path === '/auth/direct-login') {
          return jsonResponse({
            admin: { operator_id: 'platform/alice', role: 'user', tenant_scope: 'platform' },
            session_expires_at: '2030-01-01T00:00:00Z',
          });
        }
        if (path === '/bootstrap') {
          return jsonResponse({
            features: { cvm: true },
            who: {
              operator_id: 'platform/alice',
              role: 'user',
              tenant_scope: 'platform',
              token_prefix: null,
              self_user: { tenant_id: 'platform', user_id: 'alice' },
            },
            initial: { node_count: 0, chain_group_count: [] },
          });
        }
        throw new Error(`unexpected request ${path}`);
      }),
    );

    render(
      <DirectLogin
        branding={{ site_name: '我的站点', icon_data_url: null }}
        credentials={{ uuid: 'user-uuid', token: 'broc_login_token' }}
        onLogin={onLogin}
        onUsePassword={vi.fn()}
      />,
      { wrapper: wrapper() },
    );

    await vi.waitFor(() => expect(onLogin).toHaveBeenCalledOnce());
    expect(requests).toEqual([
      { path: '/auth/direct-login', body: { uuid: 'user-uuid', token: 'broc_login_token' } },
      { path: '/bootstrap', body: null },
    ]);
  });
});
