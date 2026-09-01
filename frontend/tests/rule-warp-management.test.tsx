import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { ConsoleSnapshot, ExternalOutbound, NodeAgentStateItem, Rule } from '../src/api';
import { draft } from '../src/draft';
import { RuleEditor } from '../src/panes/rules';

const warp: ExternalOutbound = {
  id: 'warp',
  tenant: 'platform',
  name: 'Cloudflare WARP',
  address: 'engage.cloudflareclient.com',
  port: 2408,
  protocol: {
    t: 'warp',
    v: {
      mtu: 1280,
      keep_alive: 25,
      allowed_ips: ['0.0.0.0/0', '::/0'],
      no_kernel_tun: false,
      domain_strategy: 'ForceIP',
      workers: 0,
    },
  },
  security: { t: 'none' },
  bindings: [],
};

const vendor: ExternalOutbound = {
  id: 'vendor',
  tenant: 'platform',
  name: '供应商出口',
  address: 'edge.example.com',
  port: 443,
  protocol: {
    t: 'vless',
    v: { credential: '<redacted>', encryption: 'none', flow: null, transport: { t: 'raw' } },
  },
  security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
  bindings: [],
};

afterEach(() => {
  cleanup();
  draft.clear();
});

function renderEditor(initial: Rule[]) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  const snapshot: ConsoleSnapshot = {
    snapshot: {
      revision: 1,
      apps: [
        {
          id: 'video',
          label: '视频',
          chains: [{ id: 'stream', tenant: 'platform', name: '流媒体' }],
          steps: [],
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
      external_outbounds: [warp, vendor],
    },
    node_egress_dns: [],
    redacted: false,
  };
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'hk',
        tenant_id: 'platform',
        name: '香港落地',
        public_ipv4: 'hk.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        egress_allowed: true,
      } as NodeAgentStateItem,
    ],
  });

  return render(
    <QueryClientProvider client={client}>
      <RuleEditor
        appId="video"
        chainId="stream"
        nodeId="hk"
        initial={initial}
        accept={null}
        peers={[]}
        isForwardTarget={false}
        fallback={{ rules: [], pending: false }}
      />
    </QueryClientProvider>,
  );
}

describe('WARP management inside forwarding rules', () => {
  it('keeps WARP manageable from the target menu even when another outbound is selected', async () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: vendor.id } }]);

    fireEvent.click(view.getByRole('button', { name: /供应商出口/ }));
    fireEvent.click(view.getByRole('button', { name: '管理 Cloudflare WARP' }));

    await waitFor(() => expect(view.getByRole('dialog', { name: '管理当前机器的 WARP 出口' })).toBeTruthy());
    expect(view.getByText('香港落地 · 当前机器')).toBeTruthy();
    expect(view.getByRole('button', { name: '注册并绑定当前机器' })).toBeTruthy();
  });

  it('opens machine lifecycle settings instead of the generic protocol editor for selected WARP', async () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }]);

    fireEvent.click(view.getByRole('button', { name: '机器设置' }));

    await waitFor(() => expect(view.getByRole('dialog', { name: '管理当前机器的 WARP 出口' })).toBeTruthy());
    expect(view.queryByRole('dialog', { name: '配置外部出站' })).toBeNull();
    expect(view.getByText('默认参数属于租户 WARP 资源；当前机器有覆盖时，以机器参数为准。')).toBeTruthy();
  });
});
