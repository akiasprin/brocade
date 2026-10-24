import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ExternalOutbound, SnapshotApp } from '../src/api';
import { draft } from '../src/draft';
import { ExternalOutboundEditor, TunnelDeleteDialog } from '../src/panes/rules';

const encryption = `mlkem768x25519plus.native.1rtt.${'A'.repeat(43)}`;
const outbound: ExternalOutbound = {
  id: 'custom-1111-1111',
  tenant: 'platform',
  name: '供应商',
  address: 'example.com',
  port: 443,
  protocol: { t: 'anytls', v: { credential: '<redacted>' } },
  security: { t: 'tls', v: { server_name: 'example.com', fingerprint: 'chrome' } },
  bindings: [],
};
afterEach(() => {
  cleanup();
  draft.clear();
});
function wrap(child: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{child}</QueryClientProvider>);
}
function editor() {
  const saved = vi.fn();
  const view = wrap(
    <ExternalOutboundEditor tenantId="platform" existing={null} onClose={() => undefined} onSaved={saved} />,
  );
  return { ...view, saved };
}
it('keeps the generated tunnel id out of the editor', async () => {
  const view = editor();
  expect(view.queryByText('出站 ID')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  expect(view.queryByText('资源 ID')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: '粘贴分享链接' }));
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: 'anytls://secret@example.com' },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0].id).toMatch(/^custom-[0-9a-f]{4}-[0-9a-f]{4}$/);
});
it('orders secure tunnel protocols before generic proxies', () => {
  const view = editor();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  const protocolNames = new Set([
    'VLESS Encryption',
    'VLESS',
    'AnyTLS',
    'Shadowsocks',
    'WireGuard',
    'SOCKS5',
    'HTTP CONNECT',
  ]);
  expect(
    view
      .getAllByRole('button')
      .map(button => button.textContent)
      .filter((name): name is string => name !== null && protocolNames.has(name)),
  ).toEqual(['VLESS Encryption', 'VLESS', 'AnyTLS', 'Shadowsocks', 'WireGuard', 'SOCKS5', 'HTTP CONNECT']);
});
it('keeps existing tunnels on remote resolution without creating a dirty draft', () => {
  const view = wrap(
    <ExternalOutboundEditor tenantId="platform" existing={outbound} onClose={() => undefined} onSaved={vi.fn()} />,
  );
  expect((view.getByRole('combobox', { name: '目标域名解析' }) as HTMLSelectElement).value).toBe('as_is');
  expect(view.getByRole('button', { name: '保存并选中' }).hasAttribute('disabled')).toBe(true);
  expect(draft.snapshot()).toEqual([]);
});
it.each(['use_ipv4v6', 'use_ipv6v4', 'use_ip', 'use_ipv4', 'use_ipv6'])(
  'saves %s at the tunnel level and retains the redacted credential',
  async strategy => {
    const saved = vi.fn();
    const view = wrap(
      <ExternalOutboundEditor tenantId="platform" existing={outbound} onClose={() => undefined} onSaved={saved} />,
    );
    fireEvent.change(view.getByRole('combobox', { name: '目标域名解析' }), { target: { value: strategy } });
    fireEvent.click(view.getByRole('button', { name: '保存并选中' }));
    await waitFor(() => expect(saved).toHaveBeenCalled());
    expect(saved.mock.calls[0][0]).toMatchObject({
      target_strategy: strategy,
      protocol: { t: 'anytls', v: { credential: '<redacted>' } },
    });
    expect(draft.snapshot()[0].op).toMatchObject({
      op: 'upsert_external_outbound',
      outbound: { target_strategy: strategy },
    });
  },
);
it('loads the saved strategy and can return to remote resolution', async () => {
  const saved = vi.fn();
  const view = wrap(
    <ExternalOutboundEditor
      tenantId="platform"
      existing={{ ...outbound, target_strategy: 'use_ipv4v6' }}
      onClose={() => undefined}
      onSaved={saved}
    />,
  );
  expect((view.getByRole('combobox', { name: '目标域名解析' }) as HTMLSelectElement).value).toBe('use_ipv4v6');
  expect(view.getByRole('button', { name: '保存并选中' }).hasAttribute('disabled')).toBe(true);
  fireEvent.change(view.getByRole('combobox', { name: '目标域名解析' }), { target: { value: 'as_is' } });
  fireEvent.click(view.getByRole('button', { name: '保存并选中' }));
  await waitFor(() => expect(saved).toHaveBeenCalled());
  expect(saved.mock.calls[0][0].target_strategy).toBeUndefined();
  expect(
    (draft.snapshot()[0].op as { outbound: { target_strategy?: string } }).outbound.target_strategy,
  ).toBeUndefined();
});
it('preserves the selected strategy when switching between import and manual entry', async () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: 'anytls://secret@example.com' },
  });
  fireEvent.change(view.getByRole('combobox', { name: '目标域名解析' }), { target: { value: 'use_ipv4v6' } });
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  expect((view.getByRole('combobox', { name: '目标域名解析' }) as HTMLSelectElement).value).toBe('use_ipv4v6');
  fireEvent.click(view.getByRole('button', { name: '粘贴分享链接' }));
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0].target_strategy).toBe('use_ipv4v6');
});
it('imports AnyTLS with encoded password, IPv6, SNI and default port into the draft', async () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: 'anytls://pass%3Aword%40x@[2001:db8::1]/?sni=edge.example.com#Any%20TLS' },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    name: 'Any TLS',
    address: '2001:db8::1',
    port: 443,
    protocol: { t: 'anytls', v: { credential: 'pass:word@x' } },
    security: { t: 'tls', v: { server_name: 'edge.example.com' } },
  });
  expect(draft.snapshot()[0].op.op).toBe('upsert_external_outbound');
});
it.each(['import', 'manual'])('imports AnyTLS peer, fingerprint, insecure and UDP aliases via %s mode', async mode => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: {
      value:
        'anytls://test-password@192.0.2.25:771' +
        '?peer=tls.vendor.example&insecure=1&udp=1&fingerprint=chrome#PH-test',
    },
  });
  expect(view.getByText(/TLS · insecure/)).toBeTruthy();
  if (mode === 'manual') {
    fireEvent.click(view.getByRole('button', { name: '手动填写' }));
    expect((view.getByRole('checkbox', { name: '跳过证书验证（insecure）' }) as HTMLInputElement).checked).toBe(true);
  }
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    name: 'PH-test',
    address: '192.0.2.25',
    port: 771,
    protocol: { t: 'anytls', v: { credential: 'test-password' } },
    security: {
      t: 'tls',
      v: { server_name: 'tls.vendor.example', fingerprint: 'chrome', allow_insecure: true },
    },
  });
});
it.each(['', '?insecure=0', '?insecure=false', '?allowInsecure=false'])(
  'keeps verification on for AnyTLS %s',
  query => {
    const view = editor();
    fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
      target: { value: `anytls://test-password@example.com${query}` },
    });
    fireEvent.click(view.getByRole('button', { name: '手动填写' }));
    expect((view.getByRole('checkbox', { name: '跳过证书验证（insecure）' }) as HTMLInputElement).checked).toBe(false);
  },
);
it.each([
  ['insecure=yes', 'insecure / allowInsecure 必须为 1、0、true 或 false'],
  ['insecure=0&allowInsecure=1', 'insecure / allowInsecure 参数互相冲突'],
  ['sni=one.example&peer=two.example', 'sni / peer 参数互相冲突'],
  ['fp=chrome&fingerprint=firefox', 'fp / fingerprint 参数互相冲突'],
  ['udp=0', '当前 AnyTLS 出站不能单独关闭 UDP，udp 参数必须为 1 或 true'],
])('rejects invalid or conflicting AnyTLS parameters: %s', (query, error) => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: `anytls://test-password@example.com?${query}` },
  });
  expect(view.getByText(error)).toBeTruthy();
  expect(view.getByRole('button', { name: '创建并选中' }).hasAttribute('disabled')).toBe(true);
});
it('preserves explicit insecure on edit and lets the operator re-enable verification', async () => {
  const saved = vi.fn();
  const view = wrap(
    <ExternalOutboundEditor
      tenantId="platform"
      existing={{
        ...outbound,
        security: {
          t: 'tls',
          v: { server_name: 'example.com', fingerprint: 'chrome', allow_insecure: true },
        },
      }}
      onClose={() => undefined}
      onSaved={saved}
    />,
  );
  const checkbox = view.getByRole('checkbox', { name: '跳过证书验证（insecure）' }) as HTMLInputElement;
  expect(checkbox.checked).toBe(true);
  fireEvent.click(checkbox);
  fireEvent.click(view.getByRole('button', { name: '保存并选中' }));
  await waitFor(() => expect(saved).toHaveBeenCalled());
  expect(saved.mock.calls[0][0].security.v.allow_insecure ?? false).toBe(false);
});
it('imports VLESS Encryption without requiring TLS', async () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: `vless://uuid@example.com:8443?encryption=${encryption}&security=none&type=tcp#Encrypted` },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    protocol: { t: 'vless', v: { encryption } },
    security: { t: 'none' },
  });
});
it('imports legacy base64 VLESS WebSocket links with TLS aliases', async () => {
  const view = editor();
  const uuid = '11111111-2222-4333-8444-555555555555';
  const authority = window
    .btoa(`none:${uuid}@192.0.2.44:443`)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
  const link =
    `vless://${authority}?path=%2Fvideo%2Fexample&remarks=%E6%96%B0%E5%8A%A0%E5%9D%A1` +
    '&obfsParam=ws.example.net&obfs=websocket&tls=1&peer=tls.example.net&udp=1&fingerprint=chrome';

  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: link },
  });
  expect(view.getByText('✓ 已识别，可以生成 Xray outbound')).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    name: '新加坡',
    address: '192.0.2.44',
    port: 443,
    protocol: {
      t: 'vless',
      v: {
        credential: uuid,
        encryption: 'none',
        flow: null,
        transport: { t: 'websocket', v: { path: '/video/example', host: 'ws.example.net' } },
      },
    },
    security: { t: 'tls', v: { server_name: 'tls.example.net', fingerprint: 'chrome' } },
  });
});
it('preserves the complete XHTTP, Finalmask and TLS share-link payload', async () => {
  const view = editor();
  const pins = '11'.repeat(32);
  const extra = {
    noSSEHeader: true,
    xmux: { maxConcurrency: 7 },
    downloadSettings: {
      address: 'download.example.com',
      port: 8443,
      network: 'xhttp',
      security: 'tls',
      tlsSettings: {
        serverName: 'download-sni.example.com',
        fingerprint: 'firefox',
        allowInsecure: true,
        alpn: ['h2'],
        echConfigList: 'download-ech',
        pinnedPeerCertSha256: pins,
        verifyPeerCertByName: 'download-cert.example.com',
      },
      xhttpSettings: {
        path: '/download',
        host: 'download-host.example.com',
        mode: 'packet-up',
        xmux: { maxConcurrency: 3 },
      },
    },
  };
  const finalMask = { tcp: [], udp: [] };
  const params = new URLSearchParams({
    type: 'xhttp',
    security: 'tls',
    path: '/upload',
    host: 'upload-host.example.com',
    mode: 'stream-up',
    extra: JSON.stringify(extra),
    fm: JSON.stringify(finalMask),
    sni: 'upload-sni.example.com',
    fp: 'chrome',
    alpn: 'h2,http/1.1',
    ech: 'upload-ech',
    pcs: pins,
    vcn: 'upload-cert.example.com',
  });
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: `vless://11111111-2222-4333-8444-555555555555@upload.example.com:443?${params}#Full` },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    name: 'Full',
    protocol: {
      t: 'vless',
      v: {
        final_mask: finalMask,
        transport: {
          t: 'xhttp',
          v: {
            path: '/upload',
            host: 'upload-host.example.com',
            mode: 'stream-up',
            mux: 7,
            extra,
            download: {
              address: 'download.example.com',
              port: 8443,
              path: '/download',
              host: 'download-host.example.com',
              mode: 'packet-up',
              mux: 3,
              security: {
                t: 'tls',
                v: {
                  server_name: 'download-sni.example.com',
                  fingerprint: 'firefox',
                  allow_insecure: true,
                  alpn: ['h2'],
                  ech_config_list: 'download-ech',
                  pinned_peer_cert_sha256: pins,
                  verify_peer_cert_by_name: 'download-cert.example.com',
                },
              },
            },
          },
        },
      },
    },
    security: {
      t: 'tls',
      v: {
        server_name: 'upload-sni.example.com',
        fingerprint: 'chrome',
        alpn: ['h2', 'http/1.1'],
        ech_config_list: 'upload-ech',
        pinned_peer_cert_sha256: pins,
        verify_peer_cert_by_name: 'upload-cert.example.com',
      },
    },
  });
});
it.each(['insecure=1', 'allowInsecure=true'])('preserves VLESS TLS certificate bypass: %s', async query => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: {
      value: `vless://11111111-2222-4333-8444-555555555555@edge.example.com:443?security=tls&${query}`,
    },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0].security.v.allow_insecure).toBe(true);
});
it('imports an explicit HTTPS certificate bypass', async () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: 'https://proxy.example.com:8443?insecure=1' },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    protocol: { t: 'http_connect' },
    security: { t: 'tls', v: { allow_insecure: true } },
  });
});
it('makes raw XHTTP xmux and downloadSettings visibly authoritative', () => {
  const existing: ExternalOutbound = {
    ...outbound,
    protocol: {
      t: 'vless',
      v: {
        credential: '<redacted>',
        encryption: 'none',
        flow: null,
        final_mask: null,
        transport: {
          t: 'xhttp',
          v: {
            path: '/upload',
            host: null,
            mux: 7,
            mode: 'stream-up',
            download: null,
            extra: {
              xmux: { maxConcurrency: 7, maxConnections: 2 },
              downloadSettings: { network: 'xhttp', extraFutureField: true },
            },
          },
        },
      },
    },
  };
  const view = wrap(
    <ExternalOutboundEditor tenantId="platform" existing={existing} onClose={() => undefined} onSaved={vi.fn()} />,
  );

  const mux = view.getByLabelText('XHTTP 上传 XMUX') as HTMLInputElement;
  const download = view.getByRole('checkbox', { name: /下载使用另一组服务器/ }) as HTMLInputElement;
  expect(mux.disabled).toBe(true);
  expect(download.disabled).toBe(true);
  expect(view.getByText('当前值由 Extra JSON 的 xmux 完整定义。')).toBeTruthy();

  fireEvent.change(view.getByLabelText('XHTTP Extra JSON'), {
    target: { value: '{"noSSEHeader":true}' },
  });
  expect(mux.disabled).toBe(false);
  expect(mux.value).toBe('');
  expect(download.disabled).toBe(false);
  expect(download.checked).toBe(false);
});
it.each([
  ['mKCP', 'type=kcp&mtu=1200&tti=50', { t: 'kcp', v: { mtu: 1200, tti: 50 } }],
  [
    'HTTP/2',
    'type=http&path=%2Fh2&host=h2-host.example.com',
    { t: 'http', v: { path: '/h2', host: 'h2-host.example.com' } },
  ],
  [
    'gRPC',
    'type=grpc&serviceName=Brocade.Edge&mode=multi&authority=authority.example.com',
    { t: 'grpc', v: { service_name: 'Brocade.Edge', mode: 'multi', authority: 'authority.example.com' } },
  ],
  [
    'HTTPUpgrade',
    'type=httpupgrade&path=%2Fupgrade&host=upgrade-host.example.com',
    { t: 'http_upgrade', v: { path: '/upgrade', host: 'upgrade-host.example.com' } },
  ],
])('imports the %s VLESS transport without dropping its fields', async (_name, transportQuery, transport) => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: {
      value:
        `vless://11111111-2222-4333-8444-555555555555@edge.example.com:443?${transportQuery}` +
        '&security=tls&sni=edge.example.com&fp=chrome#Transport',
    },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    protocol: { t: 'vless', v: { transport } },
  });
});
it('rejects recognized VLESS fields when they would be ignored or silently downgraded', () => {
  const view = editor();
  const input = view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…');
  fireEvent.change(input, {
    target: {
      value:
        'vless://11111111-2222-4333-8444-555555555555@edge.example.com:443?type=tcp&path=%2Fignored' +
        '&security=tls&sni=edge.example.com',
    },
  });
  expect(view.getByText(/path；不会静默丢弃/)).toBeTruthy();
  fireEvent.change(input, {
    target: {
      value:
        'vless://11111111-2222-4333-8444-555555555555@edge.example.com:443?type=grpc' +
        '&serviceName=edge&mode=guna&security=tls&sni=edge.example.com',
    },
  });
  expect(view.getByText('当前内置 Xray 已移除 gRPC guna，不能静默替换成 gun')).toBeTruthy();
  expect(view.getByRole('button', { name: '创建并选中' }).hasAttribute('disabled')).toBe(true);
});
it('rejects transport security layered over VLESS Encryption', () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: `vless://uuid@example.com:8443?encryption=${encryption}&security=tls&type=tcp#Encrypted` },
  });
  expect(view.getByText('VLESS Encryption 不叠加 TLS 或 REALITY 传输安全')).toBeTruthy();
  expect(view.getByRole('button', { name: '创建并选中' }).hasAttribute('disabled')).toBe(true);
});
it('imports classic Shadowsocks AES-256-GCM with an ordinary password', async () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: {
      value: 'ss://YWVzLTI1Ni1nY206Y2xhc3NpYy1zZWNyZXQ=@ss.example.com:8388#Classic%20SS',
    },
  });
  fireEvent.click(view.getByRole('button', { name: '创建并选中' }));
  await waitFor(() => expect(view.saved).toHaveBeenCalled());
  expect(view.saved.mock.calls[0][0]).toMatchObject({
    name: 'Classic SS',
    address: 'ss.example.com',
    port: 8388,
    protocol: {
      t: 'shadowsocks2022',
      v: { method: 'aes-256-gcm', credential: 'classic-secret' },
    },
    security: { t: 'none' },
  });
});
it('offers classic and 2022 methods under the Shadowsocks manual protocol', () => {
  const view = editor();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  fireEvent.click(view.getByRole('button', { name: 'Shadowsocks' }));
  for (const method of [
    'aes-128-gcm',
    'aes-256-gcm',
    'chacha20-ietf-poly1305',
    'xchacha20-ietf-poly1305',
    '2022-blake3-aes-128-gcm',
    '2022-blake3-aes-256-gcm',
    '2022-blake3-chacha20-poly1305',
  ]) {
    expect(view.getByRole('option', { name: method })).toBeTruthy();
  }
  expect(view.queryByText('安全层')).toBeNull();
  expect(view.queryByText('传输安全')).toBeNull();
  expect(view.queryByRole('button', { name: 'Shadowsocks 2022' })).toBeNull();
});
it('offers native VLESS Encryption as a manual choice with no TLS fields', () => {
  const view = editor();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  fireEvent.click(view.getByRole('button', { name: 'VLESS Encryption' }));
  expect(view.getByRole('button', { name: 'VLESS Encryption' }).getAttribute('aria-pressed')).toBe('true');
  expect(view.getByLabelText('VLESS Encryption 参数').getAttribute('aria-invalid')).toBe('true');
  fireEvent.change(view.getByLabelText('VLESS Encryption 参数'), { target: { value: encryption } });
  expect(view.getByLabelText('VLESS Encryption 参数').getAttribute('aria-invalid')).toBe('false');
  expect(view.queryByText('传输安全')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: 'VLESS' }));
  expect(view.queryByLabelText('VLESS Encryption 参数')).toBeNull();
  expect(view.getByText('传输安全')).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: 'AnyTLS' }));
  expect(view.getByRole('button', { name: 'AnyTLS' }).getAttribute('aria-pressed')).toBe('true');
  expect(view.queryByLabelText('VLESS Encryption 参数')).toBeNull();
});
it('offers WebSocket as a manual VLESS transport and disables Vision flow', () => {
  const view = editor();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  fireEvent.click(view.getByRole('button', { name: 'WebSocket' }));
  expect(view.getByRole('button', { name: 'WebSocket' }).getAttribute('aria-pressed')).toBe('true');
  expect(view.getByLabelText('WebSocket Path')).toBeTruthy();
  expect(view.getByLabelText('WebSocket Host')).toBeTruthy();
  expect(view.getByRole('combobox', { name: 'Flow' }).hasAttribute('disabled')).toBe(true);
  expect(view.getByText('WebSocket 与 Vision Flow 不兼容。')).toBeTruthy();
});
it('deletes an unused resource through a reversible draft', async () => {
  const closed = vi.fn();
  const deleted = vi.fn();
  const view = wrap(<TunnelDeleteDialog outbound={outbound} apps={[]} onClose={closed} onDeleted={deleted} />);
  fireEvent.click(view.getByRole('button', { name: '删除隧道' }));
  await waitFor(() => expect(closed).toHaveBeenCalled());
  expect(deleted).toHaveBeenCalledOnce();
  expect(draft.snapshot()[0].op).toEqual({
    op: 'delete_external_outbound',
    tenant_id: 'platform',
    id: 'custom-1111-1111',
  });
});
it('shows rule and front references and blocks deletion', () => {
  const app: SnapshotApp = {
    id: 'a',
    label: '线路',
    chains: [{ id: 'c', name: '链', tenant: 'platform' }],
    ingresses: [],
    grants: [],
    steps: [
      {
        chain: 'c',
        node: 'hk',
        accept: null,
        hop_in: null,
        rules: [{ m: { t: 'any' }, a: { t: 'proxy', outbound: outbound.id } }],
      },
    ],
    fronts: [{ id: 'f', name: '前置', tenant: 'platform', strategy: 'select', via: [], external_via: [outbound.id] }],
  };
  const view = wrap(<TunnelDeleteDialog outbound={outbound} apps={[app]} onClose={() => undefined} />);
  expect(view.getByText('线路 / 链 / hk / 规则 1')).toBeTruthy();
  expect(view.getByText('线路 / 前置组 前置')).toBeTruthy();
  expect(view.getByRole('button', { name: '删除隧道' }).hasAttribute('disabled')).toBe(true);
});
