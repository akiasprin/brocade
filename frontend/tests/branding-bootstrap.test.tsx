import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener() {},
  removeEventListener() {},
  addListener() {},
  removeListener() {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;
const { App } = await import('../src/app');
const { initialBranding } = await import('../src/api');
const { syncFavicon } = await import('../src/ui/branding');

afterEach(() => {
  cleanup();
  document.getElementById('brocade-branding')?.remove();
  document.querySelector<HTMLLinkElement>('link#brocade-favicon')?.remove();
  vi.unstubAllGlobals();
});

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>,
  );
}

describe('站点标题初始化', () => {
  it('读取首页配置，接口返回前不回退 Brocade，之后仍接受新标题', async () => {
    const script = document.createElement('script');
    script.id = 'brocade-branding';
    script.type = 'application/json';
    script.textContent = JSON.stringify({ site_name: '我的站点', icon_data_url: null });
    document.head.append(script);
    document.title = '我的站点 | 跨境网络小管家';
    let finish!: (response: Response) => void;
    const branding = new Promise<Response>(resolve => {
      finish = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn((path: string) => (path === '/branding' ? branding : new Promise(() => {}))),
    );
    mount();
    expect(initialBranding()?.site_name).toBe('我的站点');
    expect(document.title).toBe('我的站点 | 跨境网络小管家');
    finish(Response.json({ site_name: '更新后的站点', icon_data_url: null }));
    await waitFor(() => expect(document.title).toBe('更新后的站点 | 跨境网络小管家'));
  });

  it('把站点图标同步为 favicon，并接受接口返回的新图标', async () => {
    const favicon = document.createElement('link');
    favicon.id = 'brocade-favicon';
    favicon.rel = 'icon';
    favicon.href = '/favicon.svg';
    favicon.dataset.defaultHref = '/favicon.svg';
    document.head.append(favicon);
    const script = document.createElement('script');
    script.id = 'brocade-branding';
    script.type = 'application/json';
    script.textContent = JSON.stringify({ site_name: '我的站点', icon_data_url: 'data:image/png;base64,aW5pdGlhbA==' });
    document.head.append(script);
    let finish!: (response: Response) => void;
    const branding = new Promise<Response>(resolve => {
      finish = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn((path: string) => (path === '/branding' ? branding : new Promise(() => {}))),
    );

    mount();
    expect(favicon.getAttribute('href')).toBe('data:image/png;base64,aW5pdGlhbA==');
    finish(Response.json({ site_name: '我的站点', icon_data_url: 'data:image/webp;base64,dXBkYXRlZA==' }));
    await waitFor(() => expect(favicon.getAttribute('href')).toBe('data:image/webp;base64,dXBkYXRlZA=='));
    syncFavicon(null);
    expect(favicon.getAttribute('href')).toBe('/favicon.svg');
  });

  it('没有首页配置时保留中性标题，不用默认名称覆盖它', () => {
    document.title = '跨境网络小管家';
    vi.stubGlobal(
      'fetch',
      vi.fn(() => new Promise(() => {})),
    );
    mount();
    expect(document.title).toBe('跨境网络小管家');
  });
  it('站点外观接口失败时使用中性名称，不把失败误报成默认站点', async () => {
    document.title = '跨境网络小管家';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string) => {
        if (path === '/branding') throw new Error('offline');
        if (path === '/whoami') return Response.json({ error: 'unauthorized' }, { status: 401 });
        if (path === '/auth/state') return Response.json({ initialized: true, public_open: false });
        throw new Error(`Unexpected request: ${path}`);
      }),
    );
    const view = mount();
    expect(await view.findByText('控制台 | 跨境网络小管家')).toBeTruthy();
    expect(view.queryByText(/Brocade/)).toBeNull();
    expect(document.title).toBe('跨境网络小管家');
  });
});
