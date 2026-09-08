import { cleanup, fireEvent, render } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { InitializeAdmin, PasswordLogin, Login } from '../src/ui/login';

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
  vi.unstubAllGlobals();
});

describe('系统初始化默认身份', () => {
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
    expect(view.container.querySelector('.fw-kind')?.textContent).toBe('加载中');
    complete(jsonResponse({ initialized: false, public_open: false }));
    await vi.waitFor(() => expect(view.container.querySelector('.fw-kind')?.textContent).toBe('初始化'));
  });

  it('普通登录默认填写 root', () => {
    const view = render(<PasswordLogin onLogin={vi.fn()} publicOpen={false} />, { wrapper: wrapper() });
    expect((view.getByLabelText('用户名') as HTMLInputElement).value).toBe('root');
  });

  it('以 root 初始化后直接进入控制台，zero 不设置登录密码', async () => {
    const onInitialized = vi.fn();
    let initBody: Record<string, unknown> | null = null;
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/auth/init') {
        initBody = JSON.parse(String(init?.body));
        return jsonResponse(
          {
            admin: { operator_id: 'root', role: 'system-admin', tenant_scope: null, token_prefix: null },
            session_expires_at: '2030-01-01T00:00:00Z',
          },
          201,
        );
      }
      throw new Error(`unexpected request ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    const view = render(<InitializeAdmin onInitialized={onInitialized} />, { wrapper: wrapper() });
    expect((view.getByLabelText('用户名') as HTMLInputElement).value).toBe('root');
    expect(view.getByText('面板支持多用户统计；初始化时会预置使用者 zero，默认不开放登录。')).toBeTruthy();
    fireEvent.change(view.getByLabelText('密码'), { target: { value: 'root-password' } });
    fireEvent.click(view.getByRole('button', { name: '创建管理员' }));

    await vi.waitFor(() => expect(onInitialized).toHaveBeenCalled());
    expect(initBody).toEqual({
      operator_id: 'root',
      display_name: 'root',
      password: 'root-password',
      root_tenant: 'platform',
    });
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/visitor-access')).toBe(false);
    expect(view.queryByText(/zero.*密码/)).toBeNull();
    expect(view.queryByText(/租户/)).toBeNull();
    expect(onInitialized).toHaveBeenCalledWith({
      who: { operator_id: 'root', role: 'system-admin', tenant_scope: null, token_prefix: null },
    });
  });
});
