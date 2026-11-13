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
    template: 'Koipy 测速',
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
const MO = '\u{1F1F2}\u{1F1F4}';
const hoppingHy2Text = [
  `hysteria2://u@192.0.2.44:50000-50009/?sni=hy2.example#${named(`${MO}澳门示例节点 | QUIC`)}`,
  `hysteria2://u@[2001:db8:44::1]:50000-50009/?sni=hy2.example#${named(`${MO}澳门示例节点 | QUIC | v6`)}`,
  '',
].join('\n');
const mtproxyText = [
  `# @brocade-name=${named(`${JP}东京 IIJ | MTProxy`)}`,
  'tg://proxy?server=tyo.example&port=15443&secret=dd00112233445566778899aabbccddeeff',
  '',
].join('\n');

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

// 「节点」页签并入网络拨测，挂载时读取本机拨测能力；拨测本身见 grant-probe.test.tsx，这里关掉。
const probeOff = () => jsonResponse({ available: false, version: null, reason: '测试中不发起拨测', concurrency: 30 });

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

// 订阅卡上的协议与地址族是共用菜单：点栏位打开，再点选项。菜单在弹窗之外的图层里，从整页查找。
const pick = async (view: ReturnType<typeof renderViewer>, field: string, option: string) => {
  fireEvent.click(await view.findByRole('combobox', { name: field }));
  fireEvent.click(await view.findByRole('option', { name: option }));
};

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

  it('parses Hysteria 2 port-hopping endpoints without WHATWG URL rejecting the port range', () => {
    const listing = parseNodeListing(hoppingHy2Text);
    expect(listing.links.map(link => [link.family, link.endpoint, link.stack])).toEqual([
      ['ipv4', '192.0.2.44:50000-50009', 'QUIC'],
      ['ipv6', '[2001:db8:44::1]:50000-50009', 'QUIC'],
    ]);

    const [group] = groupNodeLinks(listing.links);
    expect(group).toMatchObject({ name: '澳门示例节点', region: 'MO' });
    expect(group.rows).toHaveLength(1);
    expect(group.rows[0].links).toMatchObject({ ipv4: listing.links[0], ipv6: listing.links[1] });
  });

  it('parses Telegram deep links and keeps the out-of-band Brocade display name', () => {
    const listing = parseNodeListing(mtproxyText);
    expect(listing.notes).toEqual([]);
    expect(listing.links).toHaveLength(1);
    expect(listing.links[0]).toMatchObject({
      protocol: 'mtproto',
      family: 'ipv4',
      endpoint: 'tyo.example:15443',
      stack: 'Padded intermediate',
      region: 'JP',
      name: '东京 IIJ | MTProxy',
      base: '东京 IIJ',
    });
  });
});

describe('subscription and node dialog', () => {
  it('keeps Koipy on a second card, revokes in two steps and generates only on a separate action', async () => {
    const replacementUrl = haitunUrl.replace('f98b74ba', 'a98b74ba');
    let generations = 0;
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') {
        return jsonResponse({
          template: 'Koipy 测速',
          status: 'active',
          urls: urls(++generations === 1 ? haitunUrl : replacementUrl),
          created_at: '2026-08-28 12:00:00+00',
          revoked_at: null,
        });
      }
      if (init?.method === 'DELETE') {
        return jsonResponse({ ...subscriptionInfo().haitun, status: 'revoked', revoked_at: '2026-10-09T00:00:00Z' });
      }
      return jsonResponse(subscriptionInfo());
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('clash');

    // 测速订阅是后面那张卡：只露出卡头的名称与状态，点一下换到前面。
    const peek = await view.findByRole('button', { name: '切换到 Koipy 测速订阅（尚未生成）' });
    expect(view.getByTitle('模板 SubBoost 标准版')).toBeTruthy();
    fireEvent.click(peek);
    expect(view.getByRole('region', { name: 'Koipy 测速订阅' })).toBeTruthy();
    expect(view.queryByRole('region', { name: 'Clash 订阅' })).toBeNull();
    expect(view.getByText('尚未生成')).toBeTruthy();
    expect(view.getByText('暂无地址')).toBeTruthy();
    // 风险说明收在 (!) 里，展开后才显示。
    const note = view.getByText(/测速订阅包含用户 UUID/);
    expect(note.hidden).toBe(true);
    fireEvent.click(view.getByRole('button', { name: '测速订阅说明' }));
    expect(note.hidden).toBe(false);
    expect(note.textContent).toMatch(/更换用户 UUID 并发布授权/);

    fireEvent.click(view.getByRole('button', { name: '生成测速地址' }));
    await view.findByRole('button', { name: '撤销测速地址' });
    expect(view.getByText('可用')).toBeTruthy();
    // 生成时间是 PostgreSQL 的 timestamptz::text，按 UTC+8 只写日期。
    expect(view.getByText('生成于')).toBeTruthy();
    expect(view.getByText(/^(?:2026\/)?8\/28$/)).toBeTruthy();
    expect(view.getByRole('img', { name: 'Koipy 测速地址二维码' })).toBeTruthy();
    expect(view.queryByText(haitunUrl)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '显示 Koipy 测速地址' }));
    expect(view.getByText(haitunUrl)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledWith(
      '/users/platform.acme/alice/clash-subscription/haitun',
      expect.objectContaining({ method: 'POST' }),
    );

    // 撤销就地两步：第一次只变成「确认撤销」，不发请求。
    expect(view.queryByRole('button', { name: '重新生成测速地址' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '撤销测速地址' }));
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === 'DELETE')).toBe(false);
    expect(view.getByText(haitunUrl)).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '确认撤销测速地址' }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/users/platform.acme/alice/clash-subscription/haitun',
        expect.objectContaining({ method: 'DELETE' }),
      ),
    );
    await waitFor(() => expect(view.queryByText(haitunUrl)).toBeNull());
    await view.findByText('已撤销');
    expect(view.getByText('撤销于')).toBeTruthy();
    expect(view.queryByRole('button', { name: '显示 Koipy 测速地址' })).toBeNull();
    expect(generations).toBe(1);
    fireEvent.click(view.getByRole('button', { name: '重新生成测速地址' }));
    await view.findByRole('button', { name: '撤销测速地址' });
    expect(view.queryByText(replacementUrl)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '显示 Koipy 测速地址' }));
    expect(view.getByText(replacementUrl)).toBeTruthy();
    // 换回订阅卡：订阅地址没有被测速订阅的生成与撤销改动。
    fireEvent.click(view.getByRole('button', { name: '切换到 Clash 订阅' }));
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));
    expect(view.getByText(standardUrl)).toBeTruthy();
    expect(generations).toBe(2);
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith('/regenerate'))).toBe(false);
  });

  it('disarms the revoke confirmation when focus leaves the button', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === 'DELETE') throw new Error('should not revoke');
        return jsonResponse({
          ...subscriptionInfo(),
          haitun: { ...subscriptionInfo().haitun, status: 'active', urls: urls(haitunUrl) },
        });
      }),
    );
    const view = renderViewer('clash');
    fireEvent.click(await view.findByRole('button', { name: '切换到 Koipy 测速订阅（可用）' }));
    fireEvent.click(view.getByRole('button', { name: '撤销测速地址' }));
    const armed = view.getByRole('button', { name: '确认撤销测速地址' });
    expect(armed.textContent).toBe('确认撤销');
    fireEvent.blur(armed);
    expect(view.getByRole('button', { name: '撤销测速地址' }).textContent).toBe('撤销');
  });

  it.each([false, true])('hides Koipy when the server denies access (self-service: %s)', async selfService => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse({ ...subscriptionInfo(), haitun: null })),
    );
    const view = renderViewer('clash', selfService);
    await view.findByRole('region', { name: 'Clash 订阅' });
    expect(view.queryByText('Koipy 测速订阅')).toBeNull();
    expect(view.queryByRole('button', { name: /Koipy/ })).toBeNull();
  });

  it('does not expose cached administrator Koipy data in the self-service dialog', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash', true);
    await view.findByRole('region', { name: 'Clash 订阅' });
    expect(view.queryByText('Koipy 测速订阅')).toBeNull();
  });

  it('keeps the previous address and permits retry if revocation fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === 'DELETE')
          return { ok: false, status: 500, json: async () => ({ error: 'revocation failed' }) } as Response;
        return jsonResponse({
          ...subscriptionInfo(),
          haitun: { ...subscriptionInfo().haitun, status: 'active', urls: urls(haitunUrl) },
        });
      }),
    );
    const view = renderViewer('clash');
    fireEvent.click(await view.findByRole('button', { name: '切换到 Koipy 测速订阅（可用）' }));
    fireEvent.click(view.getByRole('button', { name: '显示 Koipy 测速地址' }));
    fireEvent.click(view.getByRole('button', { name: '撤销测速地址' }));
    fireEvent.click(view.getByRole('button', { name: '确认撤销测速地址' }));
    await view.findByText(/revocation failed/);
    expect(view.getByText(haitunUrl)).toBeTruthy();
    expect((view.getByRole('button', { name: '撤销测速地址' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('blocks duplicate revocation and prevents an older in-flight read from restoring the old URL', async () => {
    const info = {
      ...subscriptionInfo(),
      haitun: { ...subscriptionInfo().haitun, status: 'active', urls: urls(haitunUrl) },
    };
    let completeRead!: (response: Response) => void;
    let completeRevocation!: (response: Response) => void;
    const oldRead = new Promise<Response>(resolve => {
      completeRead = resolve;
    });
    const revocation = new Promise<Response>(resolve => {
      completeRevocation = resolve;
    });
    let reads = 0;
    let readSignal: AbortSignal | null | undefined;
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (String(_input).includes('/subscription-connectors/'))
        return jsonResponse({ writable: true, version: 0, config: null, inactive: false, active: null, latest: null });
      if (init?.method === 'DELETE') return revocation;
      if (++reads === 1) return jsonResponse(info);
      readSignal = init?.signal;
      return oldRead;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('clash');
    fireEvent.click(await view.findByRole('button', { name: '切换到 Koipy 测速订阅（可用）' }));
    const revoke = await view.findByRole('button', { name: '撤销测速地址' });
    const refreshing = view.client.refetchQueries({ queryKey: ['clash-subscription'] });
    await waitFor(() => expect(reads).toBe(2));
    fireEvent.click(revoke);
    fireEvent.click(revoke);
    await waitFor(() => expect((revoke as HTMLButtonElement).disabled).toBe(true));
    fireEvent.click(revoke);
    fireEvent.click(revoke);
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === 'DELETE')).toHaveLength(1);
    completeRevocation(
      jsonResponse({ ...info.haitun, status: 'revoked', urls: null, revoked_at: '2026-10-09T00:00:00Z' }),
    );
    await waitFor(() => expect(readSignal?.aborted).toBe(true));
    completeRead(jsonResponse(info));
    await refreshing;
    await view.findByText('已撤销');
    expect(view.queryByRole('button', { name: '显示 Koipy 测速地址' })).toBeNull();
    expect(view.queryByText(haitunUrl)).toBeNull();
  });

  it('combines protocol and address-family choices in the public URL', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash');

    await pick(view, '协议', 'Hysteria 2');
    await pick(view, '地址族', 'IPv4');
    expect(view.getByRole('combobox', { name: '协议' }).textContent).toContain('Hysteria 2');
    expect(view.getByRole('combobox', { name: '地址族' }).textContent).toContain('IPv4');
    // 一行里只露主机名与 Token 末四位：路径收成「…/」，筛选参数保持可读。
    expect(view.baseElement.querySelector('.sub-url-mask')?.textContent).toBe('••••');
    expect(view.baseElement.querySelector('.sub-pass-address code')?.textContent).toBe(
      'sub.example/…/••••db5c/clash.yaml?family=v4&protocol=hysteria2',
    );
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));

    expect(view.getByText(`${standardUrl}?family=v4&protocol=hysteria2`)).toBeTruthy();
    expect(view.getByTitle('按 UTC+8 计').textContent).toBe('9/1 00:00');
  });

  it('reads remaining traffic as the card figure and adds the expiry only when the service has one', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash', true);
    const card = await view.findByRole('region', { name: 'Clash 订阅' });
    expect(card.querySelector('.sub-pass-primary b')?.textContent).toBe('1KiB');
    expect(view.queryByText('到期')).toBeNull();
    // 凭据提醒收在卡头的 (!) 里。
    const note = view.getByText(/订阅地址与二维码包含访问凭据/);
    expect(note.hidden).toBe(true);
    fireEvent.click(view.getByRole('button', { name: '订阅凭据说明' }));
    expect(note.hidden).toBe(false);
    cleanup();

    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        jsonResponse({ ...subscriptionInfo(), remaining_bytes: 0, expires_at: '2099-01-31T16:00:00Z' }),
      ),
    );
    const exhausted = renderViewer('clash', true);
    const figure = (await exhausted.findByRole('region', { name: 'Clash 订阅' })).querySelector('.sub-pass-primary b');
    expect(figure?.className).toBe('bad');
    expect(exhausted.getByText('到期')).toBeTruthy();
    expect(exhausted.getByText('2099/2/1')).toBeTruthy();
    cleanup();

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse({ ...subscriptionInfo(), remaining_bytes: null })),
    );
    const unlimited = renderViewer('clash', true);
    const unlimitedCard = await unlimited.findByRole('region', { name: 'Clash 订阅' });
    expect(unlimitedCard.querySelector('.sub-pass-primary b')?.textContent).toBe('不限量');
  });

  it.each([false, true])(
    'shows the QR code by default without revealing the text token (self-service: %s)',
    async selfService => {
      const fetchMock = vi.fn(async () => jsonResponse(subscriptionInfo()));
      vi.stubGlobal('fetch', fetchMock);
      const view = renderViewer('clash', selfService);
      expect(await view.findByRole('img', { name: 'Clash 订阅地址二维码' })).toBeTruthy();
      expect(view.queryByRole('button', { name: /(?:显示|收起).*二维码/ })).toBeNull();
      expect(view.queryByRole('button', { name: /标准配色|跟随主题/ })).toBeNull();
      expect(view.queryByRole('button', { name: 'MTProxy' })).toBeNull();
      expect(view.baseElement.querySelector('.sub-url-mask')).not.toBeNull();
      expect(view.queryByText(standardUrl)).toBeNull();
      expect(view.getByRole('button', { name: '下载二维码' })).toBeTruthy();
      expect(fetchMock).toHaveBeenCalledTimes(2);
    },
  );

  it('keeps the QR code visible and masks the address again when either filter changes', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(subscriptionInfo())),
    );
    const view = renderViewer('clash');
    const qr = await view.findByRole('img', { name: 'Clash 订阅地址二维码' });
    for (const [field, option] of [
      ['协议', 'Hysteria 2'],
      ['地址族', 'IPv6'],
    ]) {
      fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));
      expect(view.baseElement.querySelector('.sub-url-mask')).toBeNull();
      await pick(view, field, option);
      expect(view.getByRole('img', { name: 'Clash 订阅地址二维码' })).toBe(qr);
      expect(view.baseElement.querySelector('.sub-url-mask')).not.toBeNull();
    }
  });

  it('keeps MTProxy in node filters and clears it when switching to the subscription', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (path === '/grant-probes/capability') return probeOff();
        if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
        return artifact(mtproxyText);
      }),
    );
    const view = renderViewer('uri');
    fireEvent.click(await view.findByRole('button', { name: 'MTProxy' }));
    fireEvent.click(view.getByRole('tab', { name: '订阅' }));
    await view.findByRole('img', { name: 'Clash 订阅地址二维码' });
    expect(view.queryByRole('button', { name: 'MTProxy' })).toBeNull();
    expect(view.getByRole('combobox', { name: '协议' }).textContent).toContain('全部');
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));
    expect(view.getByText(standardUrl)).toBeTruthy();
  });

  it('uses dropdowns for node filters and a bottom sheet for the card fields on narrow screens', async () => {
    stubViewport(true);
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (path === '/grant-probes/capability') return probeOff();
        if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
        return artifact(uriText);
      }),
    );
    const view = renderViewer('uri');

    await view.findByRole('region', { name: '东京 IIJ' });
    expect(view.queryByRole('button', { name: 'Hysteria 2' })).toBeNull();
    fireEvent.change(view.getByRole('combobox', { name: '协议' }), { target: { value: 'hysteria2' } });
    fireEvent.change(view.getByRole('combobox', { name: '地址族' }), { target: { value: 'v4' } });
    expect(view.baseElement.querySelector('.sub-pick-value')?.textContent).toBe('Hysteria 2');

    fireEvent.click(view.getByRole('tab', { name: '订阅' }));
    const protocol = await view.findByRole('combobox', { name: '协议' });
    expect(protocol.textContent).toContain('Hysteria 2');
    fireEvent.click(protocol);
    expect(document.body.querySelector('.choice-layer.drawer')).not.toBeNull();
    fireEvent.click(view.getByRole('option', { name: 'AnyTLS' }));
    fireEvent.click(view.getByRole('button', { name: '显示 Clash 订阅地址' }));

    expect(view.getByText(`${standardUrl}?family=v4&protocol=anytls`)).toBeTruthy();
  });

  it('moves between the two tabs with the arrow keys like the node detail tabs', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (path === '/grant-probes/capability') return probeOff();
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
      if (path === '/grant-probes/capability') return probeOff();
      if (path.endsWith('/clash-subscription')) return jsonResponse(subscriptionInfo());
      return artifact(uriText);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderViewer('clash');

    await pick(view, '协议', 'Hysteria 2');
    await pick(view, '地址族', 'IPv4');
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
        if (path === '/grant-probes/capability') return probeOff();
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
    expect(await view.findByRole('region', { name: 'Clash 订阅' })).toBeTruthy();
  });

  it('shows Hysteria 2 port-hopping endpoints in both address-family columns', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
        if (String(input) === '/grant-probes/capability') return probeOff();
        return artifact(hoppingHy2Text);
      }),
    );
    const view = renderViewer('uri');

    const group = await view.findByRole('region', { name: '澳门示例节点' });
    expect(group.querySelectorAll('.node-row')).toHaveLength(1);
    expect(view.getByRole('button', { name: '复制 澳门示例节点 | QUIC（IPv4）' }).getAttribute('title')).toBe(
      '192.0.2.44:50000-50009 · 复制分享链接',
    );
    expect(view.getByRole('button', { name: '复制 澳门示例节点 | QUIC | v6（IPv6）' }).getAttribute('title')).toBe(
      '[2001:db8:44::1]:50000-50009 · 复制分享链接',
    );
    expect([...group.querySelectorAll('.node-copy-port')].map(node => node.textContent)).toEqual([
      ':50000-50009',
      ':50000-50009',
    ]);
  });

  it('lists self-signed URI entries by default and lets the operator stop listing them', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 9, revisions: [] });
      if (path === '/grant-probes/capability') return probeOff();
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
