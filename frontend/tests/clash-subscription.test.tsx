import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { groupNodeLinks, parseNodeListing, SubscriptionViewer } from '../src/panes/subscription';

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

const JP = '\u{1F1EF}\u{1F1F5}';
const HK = '\u{1F1ED}\u{1F1F0}';
const named = (name: string) => encodeURIComponent(name);

// uri.txt 的形状与 core/format/uri.rs 的输出一致：一条链的 VLESS 双栈、VLESS Encryption 与 AnyTLS，
// 一段前置代理跳过说明，一段自签证书隐藏说明。
const uriText = [
  `vless://u@tyo.example:443?encryption=none&type=tcp&security=reality&sni=www.microsoft.com&fp=chrome&pbk=K&sid=1#${named(`${JP}东京 IIJ`)}`,
  `vless://u@[2a0f::21]:443?encryption=none&type=tcp&security=reality&sni=www.microsoft.com&fp=chrome&pbk=K&sid=1#${named(`${JP}东京 IIJ | v6`)}`,
  `vless://u@tyo.example:8444?encryption=mlkem768x25519plus.native.0rtt.K&type=xhttp&security=none&path=%2Fe#${named(`${JP}东京 IIJ | VLESS Encryption`)}`,
  `anytls://u@tyo.example:8443?sni=tyo.example#${named(`${JP}东京 IIJ | AnyTLS`)}`,
  '',
  '# 以下条目带前置代理，URI 列表表达不了，已跳过：',
  `# ${HK}香港 CMI`,
  '# 换 Clash 目标即可。',
  '',
  '# 以下自签证书地址默认隐藏；在连接地址窗口明确允许 insecure 后才会显示：',
  `# ${JP}东京 NTT | QUIC`,
  '',
].join('\n');

const selfSignedLink = `hysteria2://u@tyo-n.example:443?sni=tyo-n.example&insecure=1#${named(`${JP}东京 NTT | QUIC`)}`;

const jsonResponse = (body: unknown) =>
  ({
    ok: true,
    status: 200,
    statusText: 'OK',
    json: async () => body,
  }) as Response;

const artifact = (content: string) =>
  jsonResponse({
    revision: 9,
    target_kind: 'user',
    target_id: 'platform.acme:alice',
    artifact_kind: 'uri',
    state: 'present',
    sha256: 'abc',
    byte_len: content.length,
    content,
    redacted: false,
  });

const renderViewer = (kind: 'uri' | 'clash', selfService = false) => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      <SubscriptionViewer
        tenant="platform.acme"
        user="alice"
        kind={kind}
        selfService={selfService}
        onClose={() => undefined}
      />
    </QueryClientProvider>,
  );
  return { ...view, client };
};

// 弹窗按 useNarrow 选择筛选控件：默认桌面宽度（两组分段控件），窄屏用例单独改写。
const stubViewport = (narrow: boolean) =>
  vi.stubGlobal('matchMedia', () => ({ matches: narrow, addEventListener() {}, removeEventListener() {} }));

beforeEach(() => stubViewport(false));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('node link listing', () => {
  it('parses the served uri.txt into rows per wire and keeps every entry', () => {
    const listing = parseNodeListing(uriText);
    expect(listing.links.map(link => [link.protocol, link.family, link.stack, link.endpoint])).toEqual([
      ['vless', 'ipv4', 'REALITY · TCP', 'tyo.example:443'],
      ['vless', 'ipv6', 'REALITY · TCP', '[2a0f::21]:443'],
      ['vless-encryption', 'ipv4', 'Encryption · XHTTP', 'tyo.example:8444'],
      ['anytls', 'ipv4', 'TLS', 'tyo.example:8443'],
    ]);
    expect(listing.links[0]).toMatchObject({ region: 'JP', name: '东京 IIJ', base: '东京 IIJ' });
    expect(listing.links[1]).toMatchObject({ name: '东京 IIJ | v6', base: '东京 IIJ' });
    expect(listing.notes).toEqual([
      '以下条目带前置代理，URI 列表表达不了，已跳过：香港 CMI 换 Clash 目标即可。',
      '以下自签证书地址默认隐藏；在连接地址窗口明确允许 insecure 后才会显示：东京 NTT | QUIC',
    ]);

    const groups = groupNodeLinks(listing.links);
    expect(groups).toHaveLength(1);
    expect(groups[0]).toMatchObject({ name: '东京 IIJ', region: 'JP' });
    // IPv6 条目并入同一协议的 IPv4 行；Encryption 与 AnyTLS 各自一行。
    expect(groups[0].rows.map(row => [row.protocol, Object.keys(row.links).sort().join('+')])).toEqual([
      ['vless', 'ipv4+ipv6'],
      ['vless-encryption', 'ipv4'],
      ['anytls', 'ipv4'],
    ]);
  });

  it('treats the empty-listing placeholder as an empty list rather than a note', () => {
    expect(parseNodeListing('# （这个用户没有能用纯 URI 表达的接入面）\n')).toEqual({ links: [], notes: [] });
  });
});

describe('subscription and node dialog', () => {
  it('generates and regenerates an independent koipy URL with an explicit credential warning', async () => {
    const replacementUrl = haitunUrl.replace('f98b74ba', 'a98b74ba');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') {
        return jsonResponse({
          template: 'koipy 测速',
          status: 'active',
          urls: urls(String(input).endsWith('/regenerate') ? replacementUrl : haitunUrl),
          created_at: '2026-08-28 12:00:00+00',
          revoked_at: null,
        });
      }
      return jsonResponse(subscriptionInfo());
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('clash');

    expect(await view.findByText('尚未生成')).toBeTruthy();
    expect(view.getByText(/风险提示：测速订阅包含用户 UUID/)).toBeTruthy();
    expect(view.getByText(/更换用户 UUID 并发布授权/)).toBeTruthy();
    expect(view.getByText('模板 SubBoost 标准版 · 适用 Mihomo / Clash Meta')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '生成 koipy 测速地址' }));
    await view.findByRole('button', { name: '重新生成 koipy 测速地址' });
    expect(view.getByText('可用')).toBeTruthy();
    expect(view.queryByText(haitunUrl)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '显示 koipy 测速地址' }));
    expect(view.getByText(haitunUrl)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      '/users/platform.acme/alice/clash-subscription/haitun',
      expect.objectContaining({ method: 'POST' }),
    );

    fireEvent.click(view.getByRole('button', { name: '重新生成 koipy 测速地址' }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining('已下载的 UUID 和节点配置仍然有效'));
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith('/regenerate'))).toBe(false);
    expect(view.getByText(haitunUrl)).toBeTruthy();
    confirm.mockReturnValue(true);
    fireEvent.click(view.getByRole('button', { name: '重新生成 koipy 测速地址' }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/users/platform.acme/alice/clash-subscription/haitun/regenerate',
        expect.objectContaining({ method: 'POST' }),
      ),
    );
    await waitFor(() => expect(view.queryByText(haitunUrl)).toBeNull());
    expect(view.queryByText(replacementUrl)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '显示 koipy 测速地址' }));
    expect(view.getByText(replacementUrl)).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));
    expect(view.getByText(standardUrl)).toBeTruthy();
    expect(view.queryByRole('button', { name: /撤销/ })).toBeNull();
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === 'DELETE')).toBe(false);
  });

  it.each([false, true])('hides koipy when the server denies access (self-service: %s)', async selfService => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse({ ...subscriptionInfo(), haitun: null })),
    );
    const view = renderViewer('clash', selfService);
    await view.findByText('Clash 订阅');
    expect(view.queryByText('koipy 测速订阅')).toBeNull();
    expect(view.queryByRole('button', { name: /koipy/ })).toBeNull();
  });

  it('does not expose cached administrator koipy data in the self-service dialog', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash', true);
    await view.findByText('Clash 订阅');
    expect(view.queryByText('koipy 测速订阅')).toBeNull();
  });

  it('keeps the previous address and permits retry if regeneration fails', async () => {
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === 'POST')
          return { ok: false, status: 500, json: async () => ({ error: 'regeneration failed' }) } as Response;
        return jsonResponse({
          ...subscriptionInfo(),
          haitun: { ...subscriptionInfo().haitun, status: 'active', urls: urls(haitunUrl) },
        });
      }),
    );
    const view = renderViewer('clash');
    fireEvent.click(await view.findByRole('button', { name: '显示 koipy 测速地址' }));
    fireEvent.click(view.getByRole('button', { name: '重新生成 koipy 测速地址' }));
    await view.findByText(/regeneration failed/);
    expect(view.getByText(haitunUrl)).toBeTruthy();
    expect((view.getByRole('button', { name: '重新生成 koipy 测速地址' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('blocks duplicate regeneration and prevents an older in-flight read from restoring the old URL', async () => {
    const replacementUrl = haitunUrl.replace('f98b74ba', 'a98b74ba');
    const info = {
      ...subscriptionInfo(),
      haitun: { ...subscriptionInfo().haitun, status: 'active', urls: urls(haitunUrl) },
    };
    let completeRead!: (response: Response) => void;
    let completeRotation!: (response: Response) => void;
    const oldRead = new Promise<Response>(resolve => {
      completeRead = resolve;
    });
    const rotation = new Promise<Response>(resolve => {
      completeRotation = resolve;
    });
    let reads = 0;
    let readSignal: AbortSignal | null | undefined;
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') return rotation;
      if (++reads === 1) return jsonResponse(info);
      readSignal = init?.signal;
      return oldRead;
    });
    vi.stubGlobal('fetch', fetchMock);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    const view = renderViewer('clash');
    const regenerate = await view.findByRole('button', { name: '重新生成 koipy 测速地址' });
    const refreshing = view.client.refetchQueries({ queryKey: ['clash-subscription'] });
    await waitFor(() => expect(reads).toBe(2));
    fireEvent.click(regenerate);
    await waitFor(() => expect((regenerate as HTMLButtonElement).disabled).toBe(true));
    fireEvent.click(regenerate);
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
    completeRotation(jsonResponse({ ...info.haitun, urls: urls(replacementUrl) }));
    await waitFor(() => expect(readSignal?.aborted).toBe(true));
    completeRead(jsonResponse(info));
    await refreshing;
    fireEvent.click(await view.findByRole('button', { name: '显示 koipy 测速地址' }));
    expect(view.getByText(replacementUrl)).toBeTruthy();
    expect(view.queryByText(haitunUrl)).toBeNull();
  });

  it('combines protocol and address-family choices in the public URL', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash');

    fireEvent.click(await view.findByRole('button', { name: 'Hysteria 2' }));
    fireEvent.click(view.getByRole('button', { name: 'IPv4' }));
    // 遮罩只遮 Token，主机、路径与筛选参数保持可读。
    expect(view.baseElement.querySelector('.sub-url-mask')?.textContent).toBe('••••••••-••••-••••-••••-••••••••');
    expect(view.baseElement.querySelector('.sub-url code')?.textContent).toBe(
      'https://sub.example/sub/v1/••••••••-••••-••••-••••-••••••••db5c/clash.yaml?family=v4&protocol=hysteria2',
    );
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));

    expect(view.getByText(`${standardUrl}?family=v4&protocol=hysteria2`)).toBeTruthy();
    expect(view.getByTitle('按 UTC+8 计').textContent).toBe('9/1 00:00');
  });

  it('switches the filters to two dropdowns on narrow screens', async () => {
    stubViewport(true);
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash');

    await view.findByText('Clash 订阅');
    expect(view.queryByRole('button', { name: 'Hysteria 2' })).toBeNull();
    fireEvent.change(view.getByRole('combobox', { name: '协议' }), { target: { value: 'hysteria2' } });
    fireEvent.change(view.getByRole('combobox', { name: '地址族' }), { target: { value: 'v4' } });
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));

    expect(view.getByText(`${standardUrl}?family=v4&protocol=hysteria2`)).toBeTruthy();
    expect(view.baseElement.querySelector('.sub-pick-value')?.textContent).toBe('Hysteria 2');
  });

  it('moves between the two tabs with the arrow keys like the node detail tabs', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
        return artifact(uriText);
      }),
    );
    const view = renderViewer('clash');

    const subscriptionTab = await view.findByRole('tab', { name: '订阅' });
    expect(subscriptionTab.getAttribute('aria-selected')).toBe('true');
    expect(view.getByRole('tabpanel').getAttribute('aria-labelledby')).toBe(subscriptionTab.id);
    fireEvent.keyDown(subscriptionTab, { key: 'ArrowRight' });

    const nodeTab = view.getByRole('tab', { name: '节点' });
    expect(nodeTab.getAttribute('aria-selected')).toBe('true');
    expect(nodeTab.tabIndex).toBe(0);
    expect(subscriptionTab.tabIndex).toBe(-1);
    await view.findByRole('region', { name: '东京 IIJ' });
    expect(view.getByRole('tabpanel').getAttribute('aria-labelledby')).toBe(nodeTab.id);
  });

  it('shares the filters with the node tab, which requests a server-filtered URI artifact', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
      if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
      return artifact(uriText);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('clash');

    fireEvent.click(await view.findByRole('button', { name: 'Hysteria 2' }));
    fireEvent.click(view.getByRole('button', { name: 'IPv4' }));
    fireEvent.click(view.getByRole('tab', { name: '节点' }));

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([input]) =>
          String(input).endsWith(
            '/artifacts/content/user/platform.acme%3Aalice/uri?family=v4&protocol=hysteria2&serving=true&insecure=true',
          ),
        ),
      ).toBe(true),
    );
    expect((view.getByRole('button', { name: 'Hysteria 2' }) as HTMLButtonElement).getAttribute('aria-pressed')).toBe(
      'true',
    );
  });

  it('lists nodes per wire with one copy target per family and explains skipped entries first', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
        return artifact(uriText);
      }),
    );
    const view = renderViewer('uri');

    const group = await view.findByRole('region', { name: '东京 IIJ' });
    expect(group.querySelectorAll('.node-row')).toHaveLength(3);
    expect(group.querySelector('.geo-flag')?.getAttribute('aria-label')).toBe('JP 地区旗');
    expect(view.getByText('4')).toBeTruthy();
    expect(view.getByText(/已跳过：香港 CMI 换 Clash 目标即可/)).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: '复制 东京 IIJ | v6（IPv6）' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(parseNodeListing(uriText).links[1].uri));

    fireEvent.click(view.getByRole('button', { name: '原文' }));
    // 弹窗经 portal 挂在 document.body 下，不在 view.container 之内。
    expect(view.baseElement.querySelector('.node-raw .cd')?.textContent).toContain('# 以下条目带前置代理');

    fireEvent.click(view.getByRole('button', { name: '改用订阅' }));
    expect(await view.findByText('Clash 订阅')).toBeTruthy();
  });

  it('lists self-signed URI entries by default and lets the operator stop listing them', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
      return artifact(path.includes('insecure=true') ? `${selfSignedLink}\n` : uriText);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('uri');

    await view.findByRole('region', { name: '东京 NTT' });
    expect(view.getByText('insecure')).toBeTruthy();
    expect(view.getByText('已列出 1 个自签证书节点（标 insecure），客户端不验证证书')).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(([input]) =>
        String(input).endsWith('/artifacts/content/user/platform.acme%3Aalice/uri?serving=true&insecure=true'),
      ),
    ).toBe(true);

    fireEvent.click(view.getByRole('button', { name: '不再列出' }));
    await view.findByRole('region', { name: '东京 IIJ' });
    expect(view.queryByText('insecure')).toBeNull();
    // 服务端原句写「默认隐藏」，与弹窗的默认相反；改写为当前状态。
    expect(view.getByText('未列出 1 个自签证书节点：东京 NTT | QUIC')).toBeTruthy();
    expect(view.queryByText(/默认隐藏/)).toBeNull();
    expect(
      fetchMock.mock.calls.some(([input]) =>
        String(input).endsWith('/artifacts/content/user/platform.acme%3Aalice/uri?serving=true'),
      ),
    ).toBe(true);

    fireEvent.click(view.getByRole('button', { name: '允许 insecure' }));
    await view.findByRole('region', { name: '东京 NTT' });

    // 切到 VLESS 不收回：VLESS 自签入口在服务端一律跳过，请求仍带 insecure。
    fireEvent.click(view.getByRole('button', { name: 'VLESS' }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([input]) =>
          String(input).endsWith(
            '/artifacts/content/user/platform.acme%3Aalice/uri?protocol=vless&serving=true&insecure=true',
          ),
        ),
      ).toBe(true),
    );
  });
});
