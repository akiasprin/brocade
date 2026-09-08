import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ExternalOutbound, SnapshotApp } from '../src/api';
import { draft } from '../src/draft';
import { ExternalOutboundEditor, ProxyOutboundDeleteDialog } from '../src/panes/rules';

const encryption = `mlkem768x25519plus.native.1rtt.${'A'.repeat(43)}`;
const outbound: ExternalOutbound = {
  id: 'vendor',
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
it('rejects unsupported AnyTLS certificate bypass instead of silently losing it', () => {
  const view = editor();
  fireEvent.change(view.getByPlaceholderText('anytls://… / vless://… / ss://… / socks5://… / https://…'), {
    target: { value: 'anytls://secret@example.com?insecure=1' },
  });
  expect(view.getByRole('button', { name: '创建并选中' }).hasAttribute('disabled')).toBe(true);
  expect(view.getByText('暂不支持跳过 AnyTLS 证书验证，请使用有效证书')).toBeTruthy();
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
it('offers native VLESS Encryption as a manual choice with no TLS fields', () => {
  const view = editor();
  fireEvent.click(view.getByRole('button', { name: '手动填写' }));
  fireEvent.click(view.getByRole('button', { name: 'VLESS Encryption' }));
  expect(view.getByRole('button', { name: 'VLESS Encryption' }).getAttribute('aria-pressed')).toBe('true');
  expect(view.getByLabelText('VLESS Encryption 参数').getAttribute('aria-invalid')).toBe('true');
  fireEvent.change(view.getByLabelText('VLESS Encryption 参数'), { target: { value: encryption } });
  expect(view.getByLabelText('VLESS Encryption 参数').getAttribute('aria-invalid')).toBe('false');
  expect(view.queryByText('Server Name')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: 'AnyTLS' }));
  expect(view.getByRole('button', { name: 'AnyTLS' }).getAttribute('aria-pressed')).toBe('true');
  expect(view.queryByLabelText('VLESS Encryption 参数')).toBeNull();
});
it('deletes an unused resource through a reversible draft', async () => {
  const closed = vi.fn();
  const view = wrap(<ProxyOutboundDeleteDialog outbound={outbound} apps={[]} onClose={closed} />);
  fireEvent.click(view.getByRole('button', { name: '删除代理出站' }));
  await waitFor(() => expect(closed).toHaveBeenCalled());
  expect(draft.snapshot()[0].op).toEqual({ op: 'delete_external_outbound', tenant_id: 'platform', id: 'vendor' });
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
  const view = wrap(<ProxyOutboundDeleteDialog outbound={outbound} apps={[app]} onClose={() => undefined} />);
  expect(view.getByText('线路 / 链 / hk / 规则 1')).toBeTruthy();
  expect(view.getByText('线路 / 前置组 前置')).toBeTruthy();
  expect(view.getByRole('button', { name: '删除代理出站' }).hasAttribute('disabled')).toBe(true);
});
it('moves deletion after earlier reference-removal operations when replacing an upsert', () => {
  draft.push({ op: 'upsert_external_outbound', outbound: { ...outbound, tenant_id: outbound.tenant } });
  draft.push({
    op: 'upsert_front',
    app_id: 'a',
    front: { id: 'f', tenant_id: 'platform', name: 'front', strategy: 'select', via: [], external_via: [] },
  });
  draft.push({ op: 'delete_external_outbound', tenant_id: outbound.tenant, id: outbound.id });
  expect(draft.snapshot().map(entry => entry.op.op)).toEqual(['upsert_front', 'delete_external_outbound']);
});
