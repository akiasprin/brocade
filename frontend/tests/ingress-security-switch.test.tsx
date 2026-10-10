import { ingressUpsertBody } from '../src/api';
import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  CertificateTrack,
  Hysteria2Settings,
  SnapshotIngress,
  SnapshotVless,
  TransportKind,
  UpsertIngressBody,
  RealityFallbackLimits,
  XhttpTuning,
  XhttpXmux,
} from '../src/api';
import { draft } from '../src/draft';
import { IngressGuardBlock, IngressPanel, IngressStreamRow, IngressEncryptionRow } from '../src/panes/chains';

function ingress(
  kind: TransportKind,
  flow = '',
  xmux: XhttpXmux | null = null,
  tuning: XhttpTuning | null = null,
): SnapshotIngress {
  const xhttp = { path: '/existing', host: null, xmux, tuning, mode: 'auto' as const };
  let vless: SnapshotVless;
  switch (kind) {
    case 'vless-reality':
      vless = { kind, flow };
      break;
    case 'vless-tls':
      vless = { kind, flow };
      break;
    case 'vless-reality-xhttp':
      vless = { kind, flow, xhttp };
      break;
    case 'vless-tls-xhttp':
      vless = { kind, flow, xhttp };
      break;
  }
  return {
    id: 'ingress-1',
    chain: 'chain-1',
    node: 'node-1',
    bind: '0.0.0.0',
    port: 443,
    projection: {},
    guard: {
      no_private: true,
      no_bittorrent: true,
      no_mail: true,
      no_udp_amplification: true,
      tcp_and_quic_only: false,
    },
    identity: { public_key: 'public-key', short_ids: ['0123456789abcdef'] },
    wires: { vless },
  };
}

function Harness({
  kind,
  flow,
  xmux,
  tuning,
  certificateName,
  certificateTrack,
}: {
  kind: TransportKind;
  flow?: string;
  xmux?: XhttpXmux | null;
  tuning?: XhttpTuning | null;
  certificateName?: string | null;
  certificateTrack?: CertificateTrack | null;
}) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000 },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const value = ingress(kind, flow, xmux, tuning);

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="VLESS" editable>
        <IngressStreamRow
          appId="app-1"
          ingress={value}
          certificateName={certificateName}
          certificateTrack={certificateTrack}
          editable
          section="vless"
        />
      </IngressPanel>
    </QueryClientProvider>
  );
}

describe('REALITY fallback creation defaults', () => {
  it('uses strict when a TLS-only ingress gains REALITY settings', () => {
    expect(ingressUpsertBody(ingress('vless-tls')).reality.fallback_limits).toEqual({ mode: 'strict' });
  });

  it('uses strict when an ingress without VLESS gains REALITY settings', () => {
    const value = ingress('vless-reality');
    value.wires.vless = null;
    expect(ingressUpsertBody(value).reality.fallback_limits).toEqual({ mode: 'strict' });
  });

  it.each<RealityFallbackLimits>([
    { mode: 'off' },
    { mode: 'balanced' },
    { mode: 'strict' },
    {
      mode: 'custom',
      upload: { after_bytes: 10, bytes_per_sec: 20, burst_bytes_per_sec: 30 },
      download: { after_bytes: 40, bytes_per_sec: 50, burst_bytes_per_sec: 60 },
    },
  ])('preserves an explicitly saved $mode policy on unrelated edits', policy => {
    const value = ingress('vless-reality');
    value.wires.vless = { kind: 'vless-reality', fallback_limits: policy };
    expect(ingressUpsertBody(value, { port: 8443 }).reality.fallback_limits).toEqual(policy);
  });

  it('does not reinterpret legacy REALITY snapshots without a stored policy', () => {
    expect(ingressUpsertBody(ingress('vless-reality')).reality.fallback_limits).toEqual({ mode: 'off' });
  });
});

function anytlsIngress(): SnapshotIngress {
  const base = ingress('vless-reality');
  return {
    ...base,
    wires: {
      vless: base.wires.vless,
      anytls: {
        port: 19443,
        security: 'tls',
        padding_scheme: [],
        masquerade: { kind: 'not-found' },
      },
    },
  };
}

function AnyTlsHarness({ editable = true }: { editable?: boolean } = {}) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000 },
      anytls_padding_scheme: ['stop=4', '0=22-29', '1=60-96', '2=95-125,c,185-245', '3=200-460'],
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const value = anytlsIngress();

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="AnyTLS" editable={editable}>
        <IngressStreamRow appId="app-1" ingress={value} editable={editable} section="protocols" />
        <IngressStreamRow appId="app-1" ingress={value} editable={editable} section="anytls" />
      </IngressPanel>
    </QueryClientProvider>
  );
}

function Hy2Harness({
  settings = {},
  editable = true,
}: {
  settings?: Partial<Hysteria2Settings>;
  editable?: boolean;
} = {}) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000 },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const base = ingress('vless-reality');
  const value: SnapshotIngress = {
    ...base,
    wires: {
      ...base.wires,
      hysteria2: {
        port: 18443,
        hop: null,
        bandwidth: {},
        congestion: 'brutal',
        obfs: null,
        masquerade: { kind: 'not-found' },
        ...settings,
      },
    },
  };

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="Hysteria 2" editable={editable}>
        <IngressStreamRow appId="app-1" ingress={value} editable={editable} section="hy2" />
      </IngressPanel>
    </QueryClientProvider>
  );
}

function NewAnyTlsHarness({ anytlsBase, encryptionBase }: { anytlsBase: number; encryptionBase?: number }) {
  const [anytlsVisible, setAnyTlsVisible] = useState(false);
  const [hy2Visible, setHy2Visible] = useState(false);
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: anytlsBase, hy2_base: 18000, vless_encryption_base: encryptionBase },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const value = ingress('vless-reality');

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="AnyTLS" editable>
        <IngressStreamRow
          appId="app-1"
          ingress={value}
          editable
          section="protocols"
          onAnyTlsEnabledChange={enabled => setAnyTlsVisible(enabled ?? false)}
          onHy2EnabledChange={enabled => setHy2Visible(enabled ?? false)}
        />
      </IngressPanel>
      {anytlsVisible && (
        <IngressPanel appId="app-1" ingress={value} title="AnyTLS 配置" editable collapsible initiallyExpanded>
          <IngressStreamRow appId="app-1" ingress={value} editable section="anytls" anytlsEnabled />
        </IngressPanel>
      )}
      {hy2Visible && (
        <IngressPanel appId="app-1" ingress={value} title="Hysteria 2" editable collapsible initiallyExpanded>
          <IngressStreamRow appId="app-1" ingress={value} editable section="hy2" hy2Enabled />
        </IngressPanel>
      )}
    </QueryClientProvider>
  );
}

function NewVlessHarness() {
  const [vlessVisible, setVlessVisible] = useState(false);
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000 },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const withAnyTls = anytlsIngress();
  const value: SnapshotIngress = { ...withAnyTls, wires: { ...withAnyTls.wires, vless: null } };

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="协议" editable>
        <IngressStreamRow
          appId="app-1"
          ingress={value}
          editable
          section="protocols"
          onVlessEnabledChange={enabled => setVlessVisible(enabled ?? false)}
        />
      </IngressPanel>
      {vlessVisible && (
        <IngressPanel appId="app-1" ingress={value} title="VLESS" editable collapsible initiallyExpanded>
          <IngressStreamRow appId="app-1" ingress={value} editable section="vless" vlessEnabled />
        </IngressPanel>
      )}
    </QueryClientProvider>
  );
}

function MtProtoAvailabilityHarness({
  publicIpv4,
  nat,
  enabled = false,
}: {
  publicIpv4: string | null;
  nat: boolean;
  enabled?: boolean;
}) {
  const [client] = useState(() => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    queryClient.setQueryData(['nodes'], {
      nodes: [
        {
          node_id: 'node-1',
          public_ipv4: publicIpv4,
          public_ipv4_nat: nat,
        },
      ],
    });
    queryClient.setQueryData(['revisions'], { current_revision: null });
    queryClient.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000, mtproto_base: 28800 },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    return queryClient;
  });
  const base = ingress('vless-reality');
  const value: SnapshotIngress = {
    ...base,
    wires: {
      ...base.wires,
      mtproto: enabled ? { port: 28800 } : null,
    },
  };

  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="协议" editable>
        <IngressStreamRow appId="app-1" ingress={value} editable section="protocols" />
      </IngressPanel>
    </QueryClientProvider>
  );
}

describe('MTProxy machine eligibility', () => {
  it.each([
    [null, false, '当前机器没有配置公网 IPv4，不能启用 MTProxy'],
    ['198.51.100.10', true, '当前机器的公网 IPv4 未直接配置在网卡上，不能启用 MTProxy'],
  ] as const)('blocks creation without a network-card public IPv4', (publicIpv4, nat, message) => {
    const view = render(<MtProtoAvailabilityHarness publicIpv4={publicIpv4} nat={nat} />);
    const toggle = view.getByRole('checkbox', { name: 'MTProxy（Telegram）' }) as HTMLInputElement;

    expect(toggle.disabled).toBe(true);
    expect(view.getByText(message)).toBeTruthy();
  });

  it('uses the Telegram mark and documents the outbound handshake requirement', () => {
    const view = render(<MtProtoAvailabilityHarness publicIpv4="198.51.100.10" nat={false} />);
    const toggle = view.getByRole('checkbox', { name: 'MTProxy（Telegram）' });
    const card = toggle.closest('.protocol-choice');

    expect(card?.querySelectorAll('.protocol-choice-icon path')).toHaveLength(2);
    expect(card?.querySelector('.protocol-choice-icon circle')).toBeNull();
    expect(view.getByText('Telegram MTProto 代理；出口地址参与中继握手，要求网卡直配公网 IPv4。')).toBeTruthy();
  });

  it('allows creation on a network-card public IPv4 and still lets an invalid legacy instance be disabled', () => {
    const direct = render(<MtProtoAvailabilityHarness publicIpv4="198.51.100.10" nat={false} />);
    expect((direct.getByRole('checkbox', { name: 'MTProxy（Telegram）' }) as HTMLInputElement).disabled).toBe(false);
    direct.unmount();

    const legacy = render(<MtProtoAvailabilityHarness publicIpv4="198.51.100.10" nat enabled />);
    const toggle = legacy.getByRole('checkbox', { name: 'MTProxy（Telegram）' }) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    expect(toggle.disabled).toBe(false);
  });

  it('shows the redacted listener port in a read-only MTProxy panel', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    client.setQueryData(['nodes'], { nodes: [] });
    client.setQueryData(['revisions'], { current_revision: null });
    client.setQueryData(['settings'], {
      ports: { anytls_base: 16000, hy2_base: 18000, mtproto_base: 28800 },
      reality_site: {
        dest: 'www.example.com:443',
        server_names: ['www.example.com'],
        fingerprint: 'chrome',
      },
    });
    const base = ingress('vless-reality');
    const value: SnapshotIngress = {
      ...base,
      wires: { ...base.wires, mtproto: { port: '***' as unknown as number } },
    };
    const view = render(
      <QueryClientProvider client={client}>
        <IngressPanel appId="app-1" ingress={value} title="MTProxy" editable={false}>
          <IngressStreamRow appId="app-1" ingress={value} editable={false} section="mtproto" />
        </IngressPanel>
      </QueryClientProvider>,
    );

    expect((view.getByRole('textbox', { name: 'MTProxy 监听端口' }) as HTMLInputElement).value).toBe('***');
  });
});

describe('接入协议面板折叠', () => {
  it('已有协议默认折叠，刚添加的协议默认展开', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const value = ingress('vless-reality');
    const fields = (
      <>
        <dt>监听端口</dt>
        <dd>443</dd>
      </>
    );
    const view = render(
      <QueryClientProvider client={client}>
        <IngressPanel
          appId="app-1"
          ingress={value}
          title="已有 VLESS"
          editable
          collapsible
          summary="TCP 443 · REALITY · TCP"
        >
          {fields}
        </IngressPanel>
        <IngressPanel appId="app-1" ingress={value} title="新 AnyTLS" editable collapsible initiallyExpanded>
          {fields}
        </IngressPanel>
      </QueryClientProvider>,
    );

    const existing = view.getByRole('heading', { name: '已有 VLESS', level: 4 }).closest('details');
    const fresh = view.getByRole('heading', { name: '新 AnyTLS', level: 4 }).closest('details');
    if (!(existing instanceof HTMLDetailsElement) || !(fresh instanceof HTMLDetailsElement)) {
      throw new Error('协议配置没有使用可折叠面板');
    }
    expect(existing.open).toBe(false);
    expect(fresh.open).toBe(true);
    expect(existing.classList.contains('config-disclosure')).toBe(true);
    expect(existing.querySelector('summary')?.textContent).toContain('TCP 443 · REALITY · TCP');
    expect(existing.querySelector('.config-disclosure-toggle')?.textContent).toBe('展开');
    expect(fresh.querySelector('.config-disclosure-toggle')?.textContent).toBe('收起');

    const summary = existing.querySelector('summary');
    if (!(summary instanceof HTMLElement)) throw new Error('协议面板没有折叠标题');
    fireEvent.click(summary);
    expect(existing.open).toBe(true);
    await waitFor(() => expect(existing.querySelector('.config-disclosure-toggle')?.textContent).toBe('收起'));
  });

  it('安全策略默认收起并使用相同的摘要和开关布局', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <IngressGuardBlock appId="app-1" ingress={ingress('vless-reality')} editable />
      </QueryClientProvider>,
    );

    const panel = view.getByRole('heading', { name: '安全策略', level: 4 }).closest('details');
    if (!(panel instanceof HTMLDetailsElement)) throw new Error('安全策略没有使用可折叠面板');
    expect(panel.open).toBe(false);
    expect(panel.classList.contains('config-disclosure')).toBe(true);
    expect(panel.querySelector('.config-disclosure-summary')?.textContent).toBe('已启用 4 / 5 项');
    expect(panel.querySelector('.config-disclosure-toggle')?.textContent).toBe('展开');

    const summary = panel.querySelector('summary');
    if (!(summary instanceof HTMLElement)) throw new Error('安全策略没有折叠标题');
    fireEvent.click(summary);
    expect(panel.open).toBe(true);
    await waitFor(() => expect(panel.querySelector('.config-disclosure-toggle')?.textContent).toBe('收起'));
  });
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

function securitySelect(container: HTMLElement): HTMLSelectElement {
  const select = Array.from(container.querySelectorAll('select')).find(item =>
    Array.from(item.options).some(option => option.value === 'reality'),
  );
  if (!select) throw new Error('security selector not found');
  return select;
}

function networkSelect(container: HTMLElement): HTMLSelectElement {
  const select = Array.from(container.querySelectorAll('select')).find(item =>
    Array.from(item.options).some(option => option.value === 'xhttp'),
  );
  if (!select) throw new Error('network selector not found');
  return select;
}

function allowSnapshotRefresh() {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        ({
          ok: true,
          status: 200,
          statusText: 'OK',
          json: async () => ({
            snapshot: {
              snapshot: { revision: 1, apps: [], nodes: [] },
              node_egress_dns: [],
              redacted: false,
            },
            compile: {},
            artifacts: { revision: 1, artifacts: [] },
          }),
        }) as Response,
    ),
  );
}

async function saveDraft(view: ReturnType<typeof render>) {
  allowSnapshotRefresh();
  fireEvent.click(view.getByRole('button', { name: '保存' }));
  await waitFor(() => expect(draft.ops()).toHaveLength(1));
  const operation = draft.ops()[0];
  if (operation?.op !== 'upsert_ingress') throw new Error('expected an ingress draft operation');
  return operation.ingress;
}

function savedXhttp(body: UpsertIngressBody) {
  const transport = body.wires.vless;
  if (!transport || !('xhttp' in transport)) throw new Error('expected an XHTTP transport');
  return transport.xhttp;
}

describe('VLESS security draft', () => {
  it.each([
    ['public-ca', '本机 CA 证书'],
    ['self-signed', '本机自签证书'],
  ] as const)('labels the node-certificate option for the %s track', (certificateTrack, label) => {
    const view = render(
      <Harness kind="vless-reality" certificateName="edge.example.com" certificateTrack={certificateTrack} />,
    );

    expect(view.getByRole('option', { name: `${label} edge.example.com` })).toBeTruthy();
  });

  it('keeps one stable save action while the security draft appears and disappears', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-tls" />);
    const save = view.getByRole('button', { name: '保存' }) as HTMLButtonElement;
    const restore = view.getByRole('button', { name: '还原' });

    expect(save.disabled).toBe(true);
    expect(save.title).toBe('没有未保存的修改');
    expect(save.closest('header')).toBeNull();
    expect(restore.closest('header')).toBeNull();
    expect(save.closest('footer')).not.toBeNull();
    expect(restore.closest('footer')).toBe(save.closest('footer'));

    fireEvent.change(securitySelect(view.container), { target: { value: 'reality' } });
    await waitFor(() => expect(save.disabled).toBe(false));
    expect(view.getByRole('button', { name: '保存' })).toBe(save);

    fireEvent.change(securitySelect(view.container), { target: { value: 'tls' } });
    await waitFor(() => expect(save.disabled).toBe(true));
    expect(view.getByRole('button', { name: '保存' })).toBe(save);
  });

  it('expands REALITY settings immediately and permits saving TLS/TCP → REALITY/TCP', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-tls" />);

    fireEvent.change(securitySelect(view.container), { target: { value: 'reality' } });

    await waitFor(() => expect(view.getByText('REALITY 目标站点')).toBeTruthy());
    expect(view.getByText('Fallback 域名')).toBeTruthy();
    expect(view.getByText('Fallback 限速')).toBeTruthy();
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(false);

    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toEqual({ kind: 'vless-reality' });
  });

  it('collapses REALITY settings immediately and permits saving REALITY/TCP → TLS/TCP', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-reality" />);
    expect(view.getByText('REALITY 目标站点')).toBeTruthy();

    fireEvent.change(securitySelect(view.container), { target: { value: 'tls' } });

    await waitFor(() => expect(view.queryByText('REALITY 目标站点')).toBeNull());
    expect(view.queryByText('Fallback 域名')).toBeNull();
    expect(view.queryByText('Fallback 限速')).toBeNull();
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(false);

    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toEqual({ kind: 'vless-tls' });
  });

  it('preserves XHTTP settings and permits saving TLS/XHTTP → REALITY/XHTTP', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-tls-xhttp" />);

    fireEvent.change(securitySelect(view.container), { target: { value: 'reality' } });

    await waitFor(() => expect(view.getByText('REALITY 目标站点')).toBeTruthy());
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(false);

    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toEqual({
      kind: 'vless-reality-xhttp',
      xhttp: { path: '/existing', host: null, xmux: null, tuning: null, mode: 'auto', download: null },
    });
  });

  it('does not expose or resubmit TLS client fingerprint and HTTP version', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-tls-xhttp" />);

    expect(view.queryByRole('combobox', { name: 'XHTTP HTTP 版本' })).toBeNull();
    expect(view.queryByRole('combobox', { name: 'TLS 客户端指纹' })).toBeNull();
    fireEvent.change(view.getByDisplayValue('/existing'), { target: { value: '/changed' } });
    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toMatchObject({
      kind: 'vless-tls-xhttp',
      xhttp: { path: '/changed', mode: 'auto' },
    });
    expect(saved.reality.fingerprint).toBeUndefined();
  });

  it('writes the XHTTP transport and clears Vision in the same draft operation', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-reality" flow="xtls-rprx-vision" />);

    fireEvent.change(networkSelect(view.container), { target: { value: 'xhttp' } });

    await waitFor(() => expect(networkSelect(view.container).value).toBe('xhttp'));
    expect(view.getByText('已将下方流控重置为关闭，流控选项与 XHTTP 互斥。')).toBeTruthy();
    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toMatchObject({
      kind: 'vless-reality-xhttp',
      xhttp: { path: expect.stringMatching(/^\/[a-z0-9]{8}$/), host: null, xmux: null, mode: 'auto' },
    });
    expect(savedXhttp(saved)).not.toHaveProperty('mux');
    expect(saved.reality.flow).toBe('');
  });

  it('uses the QUIC-style empty form and fills safe XMUX defaults around one custom value', async () => {
    draft.clear();
    const view = render(<Harness kind="vless-reality-xhttp" />);

    expect(view.getByText('Padding 与 XMUX 调优（留空 = 使用默认值）')).toBeTruthy();
    const concurrency = view.getByRole('spinbutton', { name: 'XMUX 最大并发流' }) as HTMLInputElement;
    const requestFrom = view.getByRole('spinbutton', { name: 'XMUX 请求轮换下限' }) as HTMLInputElement;
    const requestTo = view.getByRole('spinbutton', { name: 'XMUX 请求轮换上限' }) as HTMLInputElement;
    const reusableFrom = view.getByRole('spinbutton', { name: 'XMUX 复用时长下限' }) as HTMLInputElement;
    const reusableTo = view.getByRole('spinbutton', { name: 'XMUX 复用时长上限' }) as HTMLInputElement;
    expect([concurrency, requestFrom, requestTo, reusableFrom, reusableTo].map(input => input.value)).toEqual([
      '',
      '',
      '',
      '',
      '',
    ]);
    expect([concurrency, requestFrom, requestTo, reusableFrom, reusableTo].map(input => input.placeholder)).toEqual([
      '1',
      '600',
      '900',
      '1800',
      '3000',
    ]);

    fireEvent.change(concurrency, { target: { value: '8' } });
    expect([requestFrom.value, requestTo.value, reusableFrom.value, reusableTo.value]).toEqual(['', '', '', '']);

    const saved = await saveDraft(view);
    expect(savedXhttp(saved).xmux).toEqual({
      max_concurrency: 8,
      max_connections: null,
      h_max_request_times: { from: 600, to: 900 },
      h_max_reusable_secs: { from: 1800, to: 3000 },
      h_keep_alive_period_secs: null,
    });
  });

  it('clearing every visible XMUX override removes the whole object', async () => {
    draft.clear();
    const view = render(
      <Harness
        kind="vless-reality-xhttp"
        xmux={{
          max_concurrency: 8,
          h_max_request_times: { from: 600, to: 900 },
          h_max_reusable_secs: { from: 1800, to: 3000 },
        }}
      />,
    );
    const concurrency = view.getByRole('spinbutton', { name: 'XMUX 最大并发流' }) as HTMLInputElement;
    expect(concurrency.value).toBe('8');
    expect((view.getByRole('spinbutton', { name: 'XMUX 请求轮换下限' }) as HTMLInputElement).value).toBe('');
    expect((view.getByRole('spinbutton', { name: 'XMUX 请求轮换上限' }) as HTMLInputElement).value).toBe('');
    expect((view.getByRole('spinbutton', { name: 'XMUX 复用时长下限' }) as HTMLInputElement).value).toBe('');
    expect((view.getByRole('spinbutton', { name: 'XMUX 复用时长上限' }) as HTMLInputElement).value).toBe('');

    fireEvent.change(concurrency, { target: { value: '' } });

    const saved = await saveDraft(view);
    expect(savedXhttp(saved).xmux).toBeNull();
  });

  it('saves only Padding with XMUX', async () => {
    draft.clear();
    const view = render(
      <Harness
        kind="vless-reality-xhttp"
        tuning={{
          x_padding_bytes: { from: 100, to: 1000 },
        }}
      />,
    );
    const set = (name: string, value: string) =>
      fireEvent.change(view.getByRole('spinbutton', { name }), { target: { value } });

    expect(view.queryByRole('spinbutton', { name: 'XHTTP 每次 POST 字节下限' })).toBeNull();
    expect(view.queryByRole('spinbutton', { name: 'XHTTP POST 间隔下限' })).toBeNull();
    expect(view.queryByRole('spinbutton', { name: 'XHTTP 服务端缓存 POST 数' })).toBeNull();
    expect(view.queryByRole('spinbutton', { name: 'XHTTP 上传分块下限' })).toBeNull();
    set('XMUX 最大连接数', '4');
    set('XMUX 空闲保活间隔', '15');
    set('XHTTP Padding 下限', '200');
    set('XHTTP Padding 上限', '600');

    const saved = await saveDraft(view);
    expect(savedXhttp(saved).xmux).toEqual({
      max_concurrency: null,
      max_connections: 4,
      h_max_request_times: { from: 600, to: 900 },
      h_max_reusable_secs: { from: 1800, to: 3000 },
      h_keep_alive_period_secs: 15,
    });
    expect(savedXhttp(saved).tuning).toEqual({
      x_padding_bytes: { from: 200, to: 600 },
    });
  });
});

describe('AnyTLS ingress draft', () => {
  it('allocates a newly enabled listener from the global AnyTLS port base', async () => {
    draft.clear();
    allowSnapshotRefresh();
    const view = render(<NewAnyTlsHarness anytlsBase={16123} />);

    fireEvent.click(view.getByRole('checkbox', { name: 'AnyTLS（TCP）' }));
    expect((view.getByRole('textbox', { name: 'AnyTLS 监听端口' }) as HTMLInputElement).value).toBe('16123');

    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    const operation = draft.ops()[0];
    if (operation?.op !== 'upsert_ingress') throw new Error('expected an ingress draft operation');
    const saved = operation.ingress as UpsertIngressBody;
    expect(saved.wires?.anytls?.port).toBe(16123);
  });

  it('reveals a newly enabled Hysteria 2 panel in the click frame', () => {
    draft.clear();
    allowSnapshotRefresh();
    const view = render(<NewAnyTlsHarness anytlsBase={16000} />);

    fireEvent.click(view.getByRole('checkbox', { name: 'Hysteria 2（QUIC over UDP）' }));

    expect(view.getByRole('heading', { name: 'Hysteria 2', level: 4 })).toBeTruthy();
  });

  it('reveals a newly enabled VLESS panel in the click frame', () => {
    draft.clear();
    allowSnapshotRefresh();
    const view = render(<NewVlessHarness />);

    fireEvent.click(view.getByRole('checkbox', { name: 'VLESS · TLS / REALITY（TCP / XHTTP）' }));

    expect(view.getByRole('heading', { name: 'VLESS', level: 4 })).toBeTruthy();
    expect(view.getByText('传输安全')).toBeTruthy();
  });

  it('exposes the listener, padding presets, session fields, and masquerade presets', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);

    expect((view.getByRole('checkbox', { name: 'AnyTLS（TCP）' }) as HTMLInputElement).checked).toBe(true);
    const listener = view.getByRole('textbox', { name: 'AnyTLS 监听端口' }) as HTMLInputElement;
    expect(listener.value).toBe('19443');
    expect(listener.parentElement?.textContent).toBe('');
    const paddingPreset = view.getByRole('combobox', {
      name: 'AnyTLS Padding 预设',
    }) as HTMLSelectElement;
    expect(paddingPreset.value).toBe('global');
    expect(view.queryByRole('textbox', { name: 'AnyTLS Padding' })).toBeNull();
    expect(view.getByRole('option', { name: '原生精简 2 段' })).toBeTruthy();
    expect(view.getByRole('option', { name: '原生精简 4 段' })).toBeTruthy();
    expect(view.getByRole('option', { name: '原生完整 8 段' })).toBeTruthy();
    expect(Array.from(paddingPreset.options).map(option => option.textContent)).toContain('自定义');
    expect(view.queryByText('预设')).toBeNull();
    expect(view.getByText('伪装')).toBeTruthy();
    const masquerade = view.getByRole('combobox', { name: 'AnyTLS Masquerade 类型' }) as HTMLSelectElement;
    expect(masquerade.value).toBe('404');
    expect(Array.from(masquerade.options).map(option => option.textContent)).toEqual(['404', '自定义']);
    expect(view.queryByRole('spinbutton', { name: 'AnyTLS Masquerade 状态码' })).toBeNull();
    expect(view.queryByRole('textbox', { name: 'AnyTLS Masquerade Headers' })).toBeNull();
    expect(view.getByText('高级参数')).toBeTruthy();
    expect(view.queryByText('Session')).toBeNull();
    const session = view.getByText('连接复用（留空 = 使用默认值）');
    const masqueradeGroup = masquerade.closest('.anytls-masquerade');
    expect(paddingPreset.closest('.anytls-parameter-row')?.nextElementSibling).toBe(masqueradeGroup);
    expect(masqueradeGroup?.nextElementSibling).toBe(session.closest('details'));
    fireEvent.click(session);
    expect((view.getByRole('spinbutton', { name: 'AnyTLS Session 检查间隔' }) as HTMLInputElement).placeholder).toBe(
      '30',
    );
    expect(view.queryByText(/Xray/)).toBeNull();

    fireEvent.change(paddingPreset, {
      target: { value: 'two-stage' },
    });
    expect(view.queryByRole('textbox', { name: 'AnyTLS Padding' })).toBeNull();
    fireEvent.change(paddingPreset, {
      target: { value: 'custom' },
    });
    expect((view.getByRole('textbox', { name: 'AnyTLS Padding' }) as HTMLTextAreaElement).value).toBe(
      'stop=2\n0=30-30\n1=100-400',
    );

    fireEvent.change(listener, {
      target: { value: '20443' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Padding' }), {
      target: { value: 'stop=2\n0=30-30\n1=70000-70000' },
    });
    fireEvent.change(view.getByRole('spinbutton', { name: 'AnyTLS Session 检查间隔' }), {
      target: { value: '11' },
    });
    fireEvent.change(view.getByRole('spinbutton', { name: 'AnyTLS Session 空闲超时' }), {
      target: { value: '22' },
    });
    fireEvent.change(view.getByRole('spinbutton', { name: 'AnyTLS Session 最少保留数量' }), {
      target: { value: '3' },
    });
    expect(view.queryByRole('spinbutton', { name: 'AnyTLS Masquerade 状态码' })).toBeNull();
    expect(view.queryByRole('textbox', { name: 'AnyTLS Masquerade 正文' })).toBeNull();
    expect(view.queryByRole('textbox', { name: 'AnyTLS Masquerade Headers' })).toBeNull();

    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toEqual({ kind: 'vless-reality' });
    expect(saved.wires?.anytls).toEqual({
      port: 20443,
      security: 'tls',
      padding_scheme: ['stop=2', '0=30-30', '1=70000-70000'],
      idle_session_check_interval_secs: 11,
      idle_session_timeout_secs: 22,
      min_idle_session: 3,
      masquerade: {
        kind: 'not-found',
        headers: {},
      },
    });
  });

  it('keeps VLESS and AnyTLS enabled as separate TCP wires', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);

    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS 监听端口' }), {
      target: { value: '20443' },
    });
    const saved = await saveDraft(view);

    expect(saved.wires?.vless).toEqual({ kind: 'vless-reality' });
    expect(saved.wires?.anytls).toMatchObject({ port: 20443 });
    expect(saved.wires?.hysteria2).toBeNull();
  });

  it('can switch the AnyTLS stream security to REALITY', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);
    const security = view.getByRole('combobox', { name: 'AnyTLS 传输安全' }) as HTMLSelectElement;
    expect(security.value).toBe('tls');

    fireEvent.change(security, { target: { value: 'reality' } });
    expect(view.getByText('REALITY 目标站点')).toBeTruthy();
    expect(view.queryByText('伪装')).toBeNull();
    expect(view.queryByRole('combobox', { name: 'AnyTLS Masquerade 类型' })).toBeNull();
    expect(view.queryByText('与 VLESS 共用同一组 REALITY 目标站点和密钥。')).toBeNull();
    expect(view.queryByRole('option', { name: /本机证书/ })).toBeNull();
    expect(view.getByRole('option', { name: '全局站点 www.example.com' })).toBeTruthy();
    expect(view.getByRole('option', { name: '自定义站点' })).toBeTruthy();

    const saved = await saveDraft(view);
    expect(saved.wires?.anytls?.security).toBe('reality');
    expect(saved.wires?.anytls?.reality).toMatchObject({
      fallback_mode: 'global-site',
      fallback_limits: { mode: 'strict' },
      dest: '',
      server_names: [],
    });

    fireEvent.change(security, { target: { value: 'tls' } });
    expect(view.getByText('伪装')).toBeTruthy();
    expect(view.getByRole('combobox', { name: 'AnyTLS Masquerade 类型' })).toBeTruthy();
  });

  it('keeps arbitrary Masquerade responses available through the custom preset', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);

    fireEvent.change(view.getByRole('combobox', { name: 'AnyTLS Masquerade 类型' }), {
      target: { value: 'custom' },
    });
    fireEvent.change(view.getByRole('spinbutton', { name: 'AnyTLS Masquerade 状态码' }), {
      target: { value: '418' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Masquerade 正文' }), {
      target: { value: 'Nothing here' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Masquerade Headers' }), {
      target: { value: 'Cache-Control: no-store' },
    });

    const saved = await saveDraft(view);
    expect(saved.wires?.anytls?.masquerade).toEqual({
      kind: 'string',
      content: 'Nothing here',
      headers: { 'Cache-Control': 'no-store' },
      status_code: 418,
    });
  });

  it('hides padding configuration from readonly viewers', () => {
    const view = render(<AnyTlsHarness editable={false} />);

    expect(view.queryByText('Padding')).toBeNull();
    expect(view.queryByRole('combobox', { name: 'AnyTLS Padding 预设' })).toBeNull();
    expect(view.queryByRole('textbox', { name: 'AnyTLS Padding' })).toBeNull();
  });

  it('blocks malformed padding before it can be saved', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);
    fireEvent.change(view.getByRole('combobox', { name: 'AnyTLS Padding 预设' }), {
      target: { value: 'custom' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Padding' }), {
      target: { value: '0=30-30\n0=40-40' },
    });

    await waitFor(() => expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true));
    expect(view.getByText(/Padding 格式无效/)).toBeTruthy();
  });

  it('blocks session values outside the backend u32 range', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);
    fireEvent.click(view.getByText('连接复用（留空 = 使用默认值）'));
    const timeout = view.getByRole('spinbutton', { name: 'AnyTLS Session 空闲超时' }) as HTMLInputElement;
    expect(timeout.max).toBe('4294967295');

    fireEvent.change(timeout, { target: { value: '4294967296' } });

    await waitFor(() => expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true));
    expect(view.getByText(/0 到 4294967295/)).toBeTruthy();
  });
});

describe('Hysteria 2 ingress form', () => {
  it('keeps the Salamander key out of password-manager heuristics beside bandwidth fields', () => {
    const view = render(
      <Hy2Harness
        settings={{
          bandwidth: { up: '750 mbps', down: '750 mbps' },
          obfs: { kind: 'salamander', password: 'obfuscation-secret' },
        }}
      />,
    );
    const secret = view.getByRole('textbox', { name: 'Hysteria 2 混淆密钥' }) as HTMLInputElement;

    expect(view.getAllByDisplayValue('750 mbps')).toHaveLength(2);
    expect(secret.type).toBe('text');
    expect(secret.autocomplete).toBe('off');
    expect(secret.getAttribute('spellcheck')).toBe('false');
    expect(secret.classList.contains('hy2-obfs-secret')).toBe(true);
    expect(view.container.querySelector('input[type="password"]')).toBeNull();
  });

  it('keeps the listener editable without a protocol suffix or reallocate action', () => {
    const view = render(<Hy2Harness />);
    const listener = view.getByDisplayValue('18443');

    expect(listener.parentElement?.textContent).toBe('');
    expect(view.queryByRole('button', { name: '重新分配' })).toBeNull();
  });

  it.each([
    ['bbr', true],
    ['brutal', true],
    ['reno', false],
    ['force-brutal', false],
  ] as const)('always shows the saved BBR profile for %s; editable=%s', (congestion, enabled) => {
    const view = render(
      <Hy2Harness
        settings={{ congestion, bandwidth: { up: '20 mbps', down: '40 mbps' }, bbr_profile: 'conservative' }}
      />,
    );
    const profile = view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' }) as HTMLSelectElement;

    expect(profile.value).toBe('conservative');
    expect(profile.disabled).toBe(!enabled);
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    if (congestion === 'brutal') expect(view.getByText(/回退 BBR 时生效；本端发送带宽或对端接收带宽/)).toBeTruthy();
    if (!enabled) expect(view.getByText('当前拥塞模式不使用 BBR；保留策略，切回后生效。')).toBeTruthy();
  });

  it('keeps the default profile editable when Brutal bandwidth is filled or cleared', () => {
    const view = render(<Hy2Harness />);
    const profile = view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' }) as HTMLSelectElement;
    const bandwidth = view.getAllByPlaceholderText('留空用 BBR');

    expect(profile.value).toBe('standard');
    expect(profile.disabled).toBe(false);
    for (const input of bandwidth) fireEvent.change(input, { target: { value: '20 mbps' } });
    expect(view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' })).toBe(profile);
    expect(profile.disabled).toBe(false);
    for (const input of bandwidth) fireEvent.change(input, { target: { value: '' } });
    expect(profile.value).toBe('standard');
    expect(profile.disabled).toBe(false);
  });

  it.each(['bbr', 'brutal', 'reno', 'force-brutal'] as const)(
    'shows but never enables the BBR profile for read-only %s',
    congestion => {
      const view = render(<Hy2Harness editable={false} settings={{ congestion, bbr_profile: 'aggressive' }} />);
      const profile = view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' }) as HTMLSelectElement;
      expect(profile.value).toBe('aggressive');
      expect(profile.disabled).toBe(true);
    },
  );

  it('preserves the profile through mode switches and saves it even when the mode disables it', async () => {
    const view = render(<Hy2Harness settings={{ bandwidth: { up: '20 mbps', down: '40 mbps' } }} />);
    const profile = view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' }) as HTMLSelectElement;
    const congestion = view.getByRole('combobox', { name: 'Hysteria 2 拥塞控制' });
    fireEvent.change(profile, { target: { value: 'aggressive' } });

    for (const mode of ['reno', 'force-brutal', 'bbr', 'brutal', 'reno']) {
      fireEvent.change(congestion, { target: { value: mode } });
      expect(profile.value).toBe('aggressive');
      expect(profile.disabled).toBe(mode === 'reno' || mode === 'force-brutal');
    }

    const saved = await saveDraft(view);
    expect(saved.wires?.hysteria2).toMatchObject({
      congestion: 'reno',
      bbr_profile: 'aggressive',
      bandwidth: { up: '20 mbps', down: '40 mbps' },
    });
  });

  it('saves a BBR fallback profile for Brutal with explicit bandwidth', async () => {
    const view = render(<Hy2Harness settings={{ bandwidth: { up: '20 mbps', down: '40 mbps' } }} />);
    fireEvent.change(view.getByRole('combobox', { name: 'Hysteria 2 BBR 策略' }), {
      target: { value: 'conservative' },
    });
    const saved = await saveDraft(view);
    expect(saved.wires?.hysteria2).toMatchObject({ congestion: 'brutal', bbr_profile: 'conservative' });
  });
});

describe('VLESS Encryption ingress draft', () => {
  it.each([
    [undefined, 13800],
    [49000, 49000],
    [443, 444],
  ])('从设置 %s 分配独立端口，避开普通 VLESS 端口', async (base, expected) => {
    draft.clear();
    allowSnapshotRefresh();
    const view = render(<NewAnyTlsHarness anytlsBase={16000} encryptionBase={base} />);
    fireEvent.click(view.getByRole('checkbox', { name: 'VLESS · Encryption（TCP）' }));
    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    const operation = draft.ops()[0];
    if (operation?.op !== 'upsert_ingress') throw new Error('expected ingress operation');
    expect((operation.ingress as UpsertIngressBody).wires).toMatchObject({
      vless: { kind: 'vless-reality' },
      vless_encryption: { port: expected },
    });
  });
});

function EncryptionHarness({ editable = true }: { editable?: boolean } = {}) {
  const [client] = useState(() => {
    const query = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    query.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
    query.setQueryData(['nodes'], { nodes: [] });
    return query;
  });
  const value: SnapshotIngress = {
    ...ingress('vless-reality'),
    wires: { vless_encryption: { port: 48000, public_key: 'example-public-key' } },
  };
  return (
    <QueryClientProvider client={client}>
      <IngressPanel appId="app-1" ingress={value} title="VLESS Encryption" editable={editable}>
        <IngressEncryptionRow ingress={value} editable={editable} />
      </IngressPanel>
    </QueryClientProvider>
  );
}

describe('VLESS Encryption options', () => {
  it('defaults appearance to random and saves fixed 0rtt, ticket range and both padding directions', async () => {
    const view = render(<EncryptionHarness />);
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    const appearance = view.getByLabelText('Encryption 流量外观') as HTMLSelectElement;
    expect(Array.from(appearance.options).map(option => option.textContent)).toEqual(['native', 'xorpub', 'random']);
    expect(appearance.value).toBe('random');
    expect(view.queryByLabelText('Encryption 客户端握手')).toBeNull();
    expect(view.queryByText('会话恢复')).toBeNull();
    expect(view.queryByText('0rtt')).toBeNull();
    fireEvent.click(view.getByText('票据与双向 Padding'));
    fireEvent.change(view.getByLabelText('Encryption 票据有效期'), { target: { value: '100-500s' } });
    fireEvent.change(view.getByLabelText('Encryption 服务端 Padding'), {
      target: { value: '100-35-100.50-0-10.50-0-200' },
    });
    fireEvent.change(view.getByLabelText('Encryption 客户端 Padding'), { target: { value: '100-40-80' } });
    expect(view.getByText('票据有效时间随机取 100–500 秒。')).toBeTruthy();
    const body = await saveDraft(view);
    expect(body.wires?.vless_encryption).toEqual({
      port: 48000,
      options: {
        appearance: 'random',
        client_mode: '0rtt',
        ticket_lifetime: '100-500s',
        server_padding: '100-35-100.50-0-10.50-0-200',
        client_padding: '100-40-80',
      },
    });
    expect(JSON.stringify(body)).not.toContain('example-public-key');
  });

  it('blocks bad ticket ranges and padding before staging', () => {
    const view = render(<EncryptionHarness />);
    fireEvent.click(view.getByText('票据与双向 Padding'));
    fireEvent.change(view.getByLabelText('Encryption 票据有效期'), { target: { value: '500-100s' } });
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(view.getByLabelText('Encryption 票据有效期'), { target: { value: '0s' } });
    expect(view.getByText('服务端不签发会话恢复票据，客户端每次执行完整握手。')).toBeTruthy();
    fireEvent.change(view.getByLabelText('Encryption 客户端 Padding'), { target: { value: '100-1-10' } });
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(view.getByLabelText('Encryption 客户端 Padding'), { target: { value: '100-35-100' } });
    expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(false);
    expect(draft.ops()).toHaveLength(0);
  });

  it('retains options while saving unrelated ingress fields', () => {
    const value: SnapshotIngress = {
      ...ingress('vless-reality'),
      wires: {
        vless_encryption: {
          port: 48000,
          public_key: 'example-public-key',
          options: {
            appearance: 'xorpub',
            client_mode: '1rtt',
            ticket_lifetime: '300-600s',
            server_padding: '',
            client_padding: '100-35-40',
          },
        },
      },
    };
    const body = ingressUpsertBody(value);
    expect(body.wires?.vless_encryption?.options).toEqual(value.wires.vless_encryption?.options);
    expect(JSON.stringify(body)).not.toContain('example-public-key');
  });
});
