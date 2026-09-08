import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { draft } from '../src/draft';
import type { CertsView, NodeAgentStateItem } from '../src/api';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { CertGroupCard, LogRetentionCard, certificateInstallPath } = await import('../src/panes/nodes');

const certs: CertsView = {
  sealing_available: true,
  domain: null,
  groups: [
    {
      id: 'group-1',
      domain: 'example.com',
      label: 'abcd1234',
      name: '默认组',
      signing_method: 'self-signed',
      note: null,
      status: 'active',
      names: ['*.abcd1234.example.com', 'abcd1234.example.com'],
      nodes: ['n1'],
      certificates: [
        {
          id: 'cert-serving',
          status: 'serving',
          origin: 'bootstrap',
          signing_method: 'self-signed',
          certificate_name: 'primary.example.com',
          runtime_slot: 'a',
          issuer: 'Harbor Edge Root CA',
          issued_at: '2026-09-01T00:00:00Z',
          expires_at: '2126-09-01T00:00:00Z',
          sha256: 'a'.repeat(64),
          attempts: 1,
          last_error: null,
          last_attempt_at: '2026-09-01T00:00:00Z',
        },
        {
          id: 'cert-failed',
          status: 'failed',
          origin: 'spare',
          signing_method: 'public-ca',
          certificate_name: null,
          runtime_slot: null,
          issuer: null,
          issued_at: null,
          expires_at: null,
          sha256: null,
          attempts: 3,
          last_error: 'DNS challenge failed',
          last_attempt_at: '2026-09-02T00:00:00Z',
        },
      ],
    },
  ],
  nodes: [
    {
      node_id: 'n1',
      label_id: 'group-1',
      group_name: '默认组',
      certificate_name: 'primary.example.com',
      on_disk: 'current',
      observed_at: '2026-09-03T00:00:00Z',
    },
  ],
  letsencrypt: 'https://acme',
  letsencrypt_staging: 'https://acme-staging',
};

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('机器证书面板', () => {
  it('换组先保存草稿，立即显示草稿值且不重复显示保存按钮，清除草稿恢复原组', async () => {
    const next = { ...certs.groups[0], id: 'group-2', name: '新组', nodes: [] };
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) !== '/certs') throw new Error(`未预期的请求：${String(input)}`);
      return Response.json({ ...certs, groups: [...certs.groups, next] });
    });
    vi.stubGlobal('fetch', fetcher);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <CertGroupCard node={{ node_id: 'n1' } as NodeAgentStateItem} canEdit />
      </QueryClientProvider>,
    );
    const select = (await view.findByRole('combobox')) as HTMLSelectElement;
    expect(select.value).toBe('group-1');
    expect(view.queryByRole('button', { name: '保存到草稿' })).toBeNull();
    fireEvent.change(select, { target: { value: 'group-2' } });
    expect(draft.isEmpty()).toBe(true);
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(view.queryByRole('button', { name: '保存到草稿' })).toBeNull());
    expect(select.value).toBe('group-2');
    expect(draft.ops()).toEqual([{ op: 'set_node_cert_group', node_id: 'n1', label_id: 'group-2' }]);
    expect(view.queryByText('已安装')).toBeNull();
    fireEvent.change(select, { target: { value: '' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(select.value).toBe(''));
    expect(draft.ops()).toEqual([{ op: 'set_node_cert_group', node_id: 'n1', label_id: null }]);
    draft.clear();
    await waitFor(() => expect(select.value).toBe('group-1'));
    expect(fetcher.mock.calls.every(([url]) => String(url) === '/certs')).toBe(true);
  });

  it('始终展开并逐张输出证书元数据、安装状态和固定运行槽路径', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) !== '/certs') throw new Error(`未预期的请求：${String(input)}`);
        return new Response(JSON.stringify(certs), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <CertGroupCard node={{ node_id: 'n1' } as NodeAgentStateItem} canEdit={false} />
      </QueryClientProvider>,
    );

    await view.findByText('证书组');
    expect(view.container.querySelector('details')).toBeNull();

    expect(await view.findByText('cert-serving')).toBeTruthy();
    expect(view.getByText('cert-failed')).toBeTruthy();
    expect(view.getByText('/var/lib/brocade-agent/tls/self-signed/slot-a.pem')).toBeTruthy();
    expect(view.getByText('Harbor Edge Root CA')).toBeTruthy();
    expect(view.getByText('a'.repeat(64))).toBeTruthy();
    expect(view.getByText('DNS challenge failed')).toBeTruthy();
    expect(view.getAllByText('未安装').length).toBeGreaterThanOrEqual(2);
    expect(view.getByText('已安装')).toBeTruthy();
  });

  it('公有 CA 使用单一当前路径，自签证书使用槽路径', () => {
    expect(certificateInstallPath({ signing_method: 'public-ca', runtime_slot: null, status: 'serving' })).toBe(
      '/var/lib/brocade-agent/tls/public-ca/current.pem',
    );
    expect(certificateInstallPath({ signing_method: 'public-ca', runtime_slot: null, status: 'ready' })).toBeNull();
    expect(certificateInstallPath({ signing_method: 'self-signed', runtime_slot: null, status: 'ready' })).toBeNull();
  });
});

describe('机器日志保留面板', () => {
  it('默认收起，并在标题显示全部继承或本机覆盖数量', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              global: { agent_journal_mib: 100, xray_mib: 100, phantun_mib: 16 },
              nodes: [
                {
                  node_id: 'n1',
                  tenant_id: 'platform',
                  name: '香港',
                  overrides: { agent_journal_mib: null, xray_mib: 64, phantun_mib: null },
                  effective: { agent_journal_mib: 100, xray_mib: 64, phantun_mib: 16 },
                },
              ],
            }),
            { status: 200, headers: { 'content-type': 'application/json' } },
          ),
      ),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <LogRetentionCard node={{ node_id: 'n1' } as NodeAgentStateItem} canEdit={false} />
      </QueryClientProvider>,
    );

    await view.findByText('日志保留');
    const details = view.container.querySelector('details');
    expect(details?.open).toBe(false);
    expect(view.getByText('本机覆盖 1 项')).toBeTruthy();
  });
});
