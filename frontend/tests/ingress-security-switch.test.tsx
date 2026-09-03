import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  SnapshotIngress,
  TransportKind,
  UpsertIngressBody,
  XhttpTuning,
  XhttpXmux,
} from '../src/api';
import { draft } from '../src/draft';
import { IngressPanel, IngressStreamRow } from '../src/panes/chains';

function ingress(
  kind: TransportKind,
  flow = '',
  xmux: XhttpXmux | null = null,
  tuning: XhttpTuning | null = null,
): SnapshotIngress {
  const vless = kind.endsWith('-xhttp')
    ? {
        kind,
        flow,
        xhttp: { path: '/existing', host: null, xmux, tuning, mode: 'auto' as const },
      }
    : { kind, flow };
  return {
    id: 'ingress-1',
    chain: 'chain-1',
    node: 'node-1',
    bind: '0.0.0.0',
    port: 443,
    projection: null,
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
}: {
  kind: TransportKind;
  flow?: string;
  xmux?: XhttpXmux | null;
  tuning?: XhttpTuning | null;
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
      ports: { hy2_base: 18000 },
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
        <IngressStreamRow appId="app-1" ingress={value} editable section="vless" />
      </IngressPanel>
    </QueryClientProvider>
  );
}

function anytlsIngress(): SnapshotIngress {
  const base = ingress('vless-reality');
  return {
    ...base,
    wires: {
      vless: base.wires.vless,
      anytls: {
        port: 19443,
        padding_scheme: [],
        masquerade: { kind: 'not-found' },
      },
    },
  };
}

function AnyTlsHarness() {
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
      ports: { hy2_base: 18000 },
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
      <IngressPanel appId="app-1" ingress={value} title="AnyTLS" editable>
        <IngressStreamRow appId="app-1" ingress={value} editable section="protocols" />
        <IngressStreamRow appId="app-1" ingress={value} editable section="anytls" />
      </IngressPanel>
    </QueryClientProvider>
  );
}

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
  // ModelOp keeps the historical minimum ingress shape, while upsertIngress stages the complete
  // UpsertIngressBody at runtime. Assert the actual staged payload, including wires and guard.
  return operation.ingress as UpsertIngressBody;
}

function savedXhttp(body: UpsertIngressBody) {
  const transport = body.wires?.vless;
  if (!transport || !('xhttp' in transport)) throw new Error('expected an XHTTP transport');
  return transport.xhttp;
}

describe('VLESS security draft', () => {
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

    expect(view.getByText('Padding 与 XMUX 调优（留空 = 用 Xray 默认）')).toBeTruthy();
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
  it('exposes the independent TCP port, padding scheme, and full masquerade response', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);

    expect((view.getByRole('checkbox', { name: 'AnyTLS（TCP）' }) as HTMLInputElement).checked).toBe(true);
    expect((view.getByRole('textbox', { name: 'AnyTLS 监听端口' }) as HTMLInputElement).value).toBe('19443');
    expect((view.getByRole('combobox', { name: 'AnyTLS Masquerade 类型' }) as HTMLSelectElement).value).toBe('not-found');
    expect(view.queryByRole('spinbutton', { name: 'AnyTLS Masquerade 状态码' })).toBeNull();

    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS 监听端口' }), {
      target: { value: '20443' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Padding Scheme' }), {
      target: { value: 'stop=2\n0=30-30\n1=70000-70000' },
    });
    fireEvent.change(view.getByRole('combobox', { name: 'AnyTLS Masquerade 类型' }), {
      target: { value: 'string' },
    });
    fireEvent.change(view.getByRole('spinbutton', { name: 'AnyTLS Masquerade 状态码' }), {
      target: { value: '403' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Masquerade 正文' }), {
      target: { value: 'Forbidden' },
    });
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Masquerade Headers' }), {
      target: { value: 'Content-Type: text/plain\nCache-Control: no-store' },
    });

    const saved = await saveDraft(view);
    expect(saved.wires?.vless).toEqual({ kind: 'vless-reality' });
    expect(saved.wires?.anytls).toEqual({
      port: 20443,
      padding_scheme: ['stop=2', '0=30-30', '1=70000-70000'],
      masquerade: {
        kind: 'string',
        content: 'Forbidden',
        status_code: 403,
        headers: { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' },
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

  it('blocks malformed padding before it can be saved', async () => {
    draft.clear();
    const view = render(<AnyTlsHarness />);
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS Padding Scheme' }), {
      target: { value: '0=30-30\n0=40-40' },
    });

    await waitFor(() => expect((view.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true));
    expect(view.getByText(/Padding Scheme 格式无效/)).toBeTruthy();
  });
});
