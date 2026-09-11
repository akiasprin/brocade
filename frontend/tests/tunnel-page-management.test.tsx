import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ConsoleSnapshot, ExternalOutbound } from '../src/api';
import { TunnelsPane } from '../src/panes/tunnels';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const outbound: ExternalOutbound = {
  id: 'vendor',
  tenant: 'platform',
  name: '供应商出口',
  address: 'edge.example.com',
  port: 443,
  protocol: { t: 'anytls', v: { credential: '<redacted>' } },
  security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
  bindings: [],
};

const snapshot: ConsoleSnapshot = {
  snapshot: {
    revision: 1,
    apps: [
      {
        id: 'video',
        label: '视频',
        chains: [],
        steps: [],
        ingresses: [],
        fronts: [
          {
            id: 'preferred',
            tenant: 'platform',
            name: '优选入口',
            strategy: 'select',
            via: [],
            external_via: [],
          },
        ],
        grants: [],
      },
    ],
    external_outbounds: [outbound],
  },
  node_egress_dns: [],
  redacted: false,
};

const windowState = (data: Win['data']): Win => ({
  id: 9001,
  key: 'tab:tunnels',
  title: '隧道',
  x: 0,
  y: 0,
  w: 880,
  h: 600,
  z: 1,
  min: false,
  home: 'desk',
  data,
});

function mount(data: Win['data']) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['tenants'], {
    tenants: [{ id: 'platform', name: '平台', node_count: 0, user_count: 0, operator_count: 1 }],
  });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'editor',
            role: 'editor',
            tenant_scope: 'platform',
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <TunnelsPane win={windowState(data)} />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

it('owns creation on the tunnel list page', () => {
  const view = mount({ drill: { p: 'list' } });

  fireEvent.click(view.getByRole('button', { name: '＋ 新建隧道' }));

  expect(view.getByRole('dialog', { name: '选择隧道类型' })).toBeTruthy();
  expect(view.getByRole('button', { name: /Cloudflare WARP/ })).toBeTruthy();
  expect(view.getByRole('button', { name: /导入或自定义配置/ })).toBeTruthy();
  expect(view.getByText(/VLESS \/ Shadowsocks \/ SOCKS5/)).toBeTruthy();
});

it('owns editing, deletion and subscription-front selection on the tunnel detail page', () => {
  const view = mount({ drill: { p: 'tunnel', tenant: 'platform', id: outbound.id } });

  expect(view.getByRole('heading', { name: outbound.name })).toBeTruthy();
  expect(view.getByText('订阅前置')).toBeTruthy();
  expect((view.getByRole('checkbox') as HTMLInputElement).checked).toBe(false);

  fireEvent.click(view.getByRole('button', { name: '编辑' }));
  expect(view.getByRole('dialog', { name: '编辑隧道' })).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: '取消' }));

  fireEvent.click(view.getByRole('button', { name: '删除' }));
  expect(view.getByRole('dialog', { name: '删除隧道' })).toBeTruthy();
});
