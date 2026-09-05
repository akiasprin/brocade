import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SubscriptionViewer } from '../src/panes/subscription';

const standardUrl = 'https://sub.example/sub/v1/2d2304da-f114-4574-8d44-625afdb1db5c/clash.yaml';
const haitunUrl = 'https://sub.example/sub/v1/haitun/f98b74ba-58f1-41d0-aaad-8fa5724c6d2d/clash.yaml';

const urls = (url: string) => ({
  both: url,
  v4: `${url}?family=v4`,
  v6: `${url}?family=v6`,
});

const subscriptionInfo = () => ({
  url: standardUrl,
  urls: urls(standardUrl),
  template: 'SubBoost 标准版',
  haitun: {
    template: 'koipy 测速',
    status: 'not-created',
    urls: null,
    created_at: null,
    revoked_at: null,
  },
  remaining_bytes: 1024,
  reset_at: '2026-09-01T00:00:00+08:00',
  usage_has_gap: false,
});

const jsonResponse = (body: unknown) =>
  ({
    ok: true,
    status: 200,
    statusText: 'OK',
    json: async () => body,
  }) as Response;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('Clash subscription templates', () => {
  it('generates and revokes an independent koipy URL from the native template select', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') {
        return jsonResponse({
          template: 'koipy 测速',
          status: 'active',
          urls: urls(haitunUrl),
          created_at: '2026-08-28 12:00:00+00',
          revoked_at: null,
        });
      }
      if (init?.method === 'DELETE') {
        return jsonResponse({
          template: 'koipy 测速',
          status: 'revoked',
          urls: null,
          created_at: '2026-08-28 12:00:00+00',
          revoked_at: '2026-08-28 12:05:00+00',
        });
      }
      return jsonResponse(subscriptionInfo());
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <SubscriptionViewer tenant="platform.acme" user="alice" kind="clash" onClose={() => undefined} />
      </QueryClientProvider>,
    );

    const template = await view.findByRole('combobox', { name: '订阅模板' });
    expect(template).toBeInstanceOf(HTMLSelectElement);
    fireEvent.change(template, { target: { value: 'haitun' } });
    expect(view.getByText('尚未生成')).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: '生成 koipy 测速地址' }));
    await view.findByRole('button', { name: '撤销 koipy 测速地址' });
    expect(view.getByText('● 可用')).toBeTruthy();
    expect(view.queryByText(haitunUrl)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '显示' }));
    expect(view.getByText(haitunUrl)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      '/users/platform.acme/alice/clash-subscription/haitun',
      expect.objectContaining({ method: 'POST' }),
    );

    fireEvent.click(view.getByRole('button', { name: '撤销 koipy 测速地址' }));
    await view.findByRole('button', { name: '重新生成' });
    expect(view.getByText('已撤销')).toBeTruthy();
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/users/platform.acme/alice/clash-subscription/haitun',
        expect.objectContaining({ method: 'DELETE' }),
      ),
    );
  });

  it('combines protocol and address-family choices in the public URL', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <SubscriptionViewer tenant="platform.acme" user="alice" kind="clash" onClose={() => undefined} />
      </QueryClientProvider>,
    );

    fireEvent.click(await view.findByRole('tab', { name: 'Hysteria 2' }));
    fireEvent.click(view.getByRole('tab', { name: '仅 IPv4' }));
    fireEvent.click(view.getByRole('button', { name: '显示' }));

    expect(view.getByText(`${standardUrl}?family=v4&protocol=hysteria2`)).toBeTruthy();
  });

  it('requests a protocol-filtered URI artifact instead of filtering text in the browser', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/revisions?limit=50') {
        return jsonResponse({ current_revision: 9, revisions: [] });
      }
      return jsonResponse({
        revision: 9,
        target_kind: 'user',
        target_id: 'platform.acme:alice',
        artifact_kind: 'uri',
        state: 'present',
        sha256: 'abc',
        byte_len: 16,
        content: path.includes('protocol=hysteria2') ? 'hysteria2://selected' : 'vless://selected',
        redacted: false,
      });
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <SubscriptionViewer tenant="platform.acme" user="alice" kind="uri" onClose={() => undefined} />
      </QueryClientProvider>,
    );

    await view.findByText('vless://selected');
    fireEvent.click(view.getByRole('tab', { name: 'Hysteria 2' }));
    await view.findByText('hysteria2://selected');
    fireEvent.click(view.getByRole('tab', { name: '仅 IPv4' }));

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([input]) =>
          String(input).endsWith(
            '/artifacts/content/user/platform.acme%3Aalice/uri?family=v4&protocol=hysteria2&serving=true',
          ),
        ),
      ).toBe(true),
    );
  });

  it('requests self-signed URI entries only after an explicit one-time insecure choice', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/revisions?limit=50') {
        return jsonResponse({ current_revision: 9, revisions: [] });
      }
      return jsonResponse({
        revision: 9,
        target_kind: 'user',
        target_id: 'platform.acme:alice',
        artifact_kind: 'uri',
        state: 'present',
        sha256: 'abc',
        byte_len: 16,
        content: path.includes('insecure=true')
          ? 'anytls://secret@203.0.113.7:19443?sni=private.example&insecure=1'
          : '# 自签证书地址默认隐藏',
        redacted: false,
      });
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <SubscriptionViewer tenant="platform.acme" user="alice" kind="uri" onClose={() => undefined} />
      </QueryClientProvider>,
    );

    await view.findByText('# 自签证书地址默认隐藏');
    expect(view.queryByText(/anytls:\/\//)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '允许 insecure' }));
    await view.findByText(/anytls:\/\/secret/);
    expect(
      fetchMock.mock.calls.some(([input]) =>
        String(input).endsWith('/artifacts/content/user/platform.acme%3Aalice/uri?serving=true&insecure=true'),
      ),
    ).toBe(true);
  });
});
