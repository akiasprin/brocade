import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ConsoleSnapshot, DeploymentListItem, ExternalOutbound, TunnelProbeView } from '../src/api';
import { draft } from '../src/draft';
import { singleTunnelTenant, TunnelsPane } from '../src/panes/tunnels';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const initial = { node_count: 0, chain_group_count: [] };

const outbound: ExternalOutbound = {
  id: 'custom-1111-1111',
  tenant: 'platform',
  name: '供应商出口',
  address: 'edge.example.com',
  port: 443,
  protocol: { t: 'anytls', v: { credential: '<redacted>' } },
  security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
  bindings: [],
};

const warpOutbound: ExternalOutbound = {
  id: 'warp-8f3a-2d71',
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

function mount(
  data: Win['data'],
  snapshotValue: ConsoleSnapshot | null = snapshot,
  configDeployments: DeploymentListItem[] = [],
  committedSnapshot: ConsoleSnapshot | null = snapshotValue,
  options: { publicView?: boolean; seedDeployments?: boolean } = {},
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  if (snapshotValue) client.setQueryData(['snapshot'], snapshotValue);
  if (committedSnapshot) client.setQueryData(['snapshot', 'committed'], committedSnapshot);
  if (options.seedDeployments !== false) {
    client.setQueryData(['deployments', 'config'], { deployments: configDeployments });
  }
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['users'], { users: [] });
  client.setQueryData(['tunnel-probe-capability'], {
    available: true,
    version: 'test',
    reason: null,
    concurrency: 1,
  });
  for (const tunnel of snapshotValue?.snapshot.external_outbounds ?? []) {
    const probe: TunnelProbeView = {
      item: {
        tenant_id: tunnel.tenant,
        outbound_id: tunnel.id,
        name: tunnel.name,
        protocol: tunnel.protocol.t,
        supported: true,
        unsupported_reason: null,
        health: 'unknown',
        policy: null,
        latest_run: null,
      },
      retention_days: 7,
      summary: {
        window_secs: 86_400,
        total: 0,
        succeeded: 0,
        success_rate: null,
        p50_ms: null,
        p95_ms: null,
        failures: 0,
      },
      points: [],
      recent_runs: [],
    };
    client.setQueryData(['tunnel-probe', tunnel.tenant, tunnel.id, 86_400], probe);
  }
  const view = render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial,
          who: {
            operator_id: options.publicView ? 'public' : 'editor',
            role: options.publicView ? 'readonly' : 'editor',
            tenant_scope: 'platform',
            token_prefix: null,
            masked_assets: options.publicView ?? false,
          },
        }}
      >
        <TunnelsPane win={windowState(data)} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return Object.assign(view, { client });
}

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

it('shows the real list frame without inventing optional groups while the model is loading', () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(() => new Promise<Response>(() => {})),
  );
  const view = mount({ drill: { p: 'list' } }, null);

  expect(view.getByRole('heading', { name: '隧道' })).toBeTruthy();
  expect(view.getByRole('button', { name: /VPN Gate 出口池/ })).toBeTruthy();
  expect(view.queryByRole('heading', { name: 'Cloudflare WARP' })).toBeNull();
  expect(view.queryByRole('heading', { name: '自定义隧道' })).toBeNull();
  expect(view.getAllByRole('status')).toHaveLength(1);
  expect(view.queryByText('还没有自定义隧道')).toBeNull();
  expect((view.getByRole('button', { name: '＋ 新建隧道' }) as HTMLButtonElement).disabled).toBe(true);
  expect(view.client.getQueryState(['tenants'])).toBeUndefined();
});

it('never shows a custom group when the loaded model contains only WARP', async () => {
  let finishSnapshot!: (response: Response) => void;
  const response = new Promise<Response>(resolve => {
    finishSnapshot = resolve;
  });
  vi.stubGlobal(
    'fetch',
    vi.fn(() => response),
  );
  const view = mount({ drill: { p: 'list' } }, null);

  expect(view.queryByRole('heading', { name: '自定义隧道' })).toBeNull();
  finishSnapshot(
    Response.json({ ...snapshot, snapshot: { ...snapshot.snapshot, external_outbounds: [warpOutbound] } }),
  );
  await waitFor(() => expect(view.getByRole('heading', { name: 'Cloudflare WARP' })).toBeTruthy());
  expect(view.queryByRole('heading', { name: '自定义隧道' })).toBeNull();
});

it('uses the draft-projected snapshot rather than the committed model for the list', () => {
  const draftTunnel = { ...outbound, id: 'custom-2222-2222', name: '草稿新增出口' };
  const projected: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [outbound, draftTunnel] },
  };
  const view = mount({ drill: { p: 'list' } }, projected, [], snapshot);

  expect(view.getByRole('button', { name: /草稿新增出口/ })).toBeTruthy();
  expect(view.client.getQueryData(['snapshot', 'committed'])).toBe(snapshot);
});

it('keeps the frame visible until a real draft preview supplies the tunnel rows', async () => {
  draft.init(`tunnels-${crypto.randomUUID()}`);
  const draftTunnel = { ...outbound, id: 'custom-3333-3333', name: '草稿新出口' };
  draft.push({
    op: 'upsert_external_outbound',
    outbound: {
      id: draftTunnel.id,
      tenant_id: draftTunnel.tenant,
      name: draftTunnel.name,
      address: draftTunnel.address,
      port: draftTunnel.port,
      protocol: draftTunnel.protocol,
      security: draftTunnel.security,
    },
  });
  let finishPreview!: (response: Response) => void;
  const preview = new Promise<Response>(resolve => {
    finishPreview = resolve;
  });
  const fetch = vi.fn((_path: RequestInfo | URL) => preview);
  vi.stubGlobal('fetch', fetch);
  const view = mount({ drill: { p: 'list' } }, null);

  expect(view.queryByRole('heading', { name: 'Cloudflare WARP' })).toBeNull();
  expect(view.queryByRole('heading', { name: '自定义隧道' })).toBeNull();
  expect(view.getByRole('button', { name: /VPN Gate 出口池/ })).toBeTruthy();
  expect(view.queryByText('还没有自定义隧道')).toBeNull();
  expect(fetch.mock.calls[0]?.[0]).toBe('/model/preview');

  finishPreview(
    Response.json({
      snapshot: {
        ...snapshot,
        snapshot: { ...snapshot.snapshot, external_outbounds: [outbound, draftTunnel] },
      },
      compile: {},
      artifacts: {},
    }),
  );
  await waitFor(() => expect(view.getByRole('button', { name: /草稿新出口/ })).toBeTruthy());
  expect(view.queryByRole('status')).toBeNull();
  expect(view.client.getQueryState(['tenants'])).toBeUndefined();
});

it('owns creation on the tunnel list page', () => {
  const view = mount({ drill: { p: 'list' } });

  expect(view.client.getQueryState(['tenants'])).toBeUndefined();

  fireEvent.click(view.getByRole('button', { name: '＋ 新建隧道' }));

  expect(view.getByRole('dialog', { name: '选择隧道类型' })).toBeTruthy();
  expect(view.queryByRole('combobox', { name: '租户' })).toBeNull();
  expect(view.getByRole('button', { name: /Cloudflare WARP/ })).toBeTruthy();
  expect(view.getByRole('button', { name: /导入或自定义配置/ })).toBeTruthy();
  expect(view.getByText(/VLESS \/ Shadowsocks \/ SOCKS5/)).toBeTruthy();
});

it('derives the only tenant from the current snapshot without assuming a fixed name', () => {
  const emptyWorkspace = { ...snapshot.snapshot, apps: [], external_outbounds: [], users: [{ tenant: 'my-root' }] };
  expect(singleTunnelTenant(emptyWorkspace, null)).toBe('my-root');
  expect(
    singleTunnelTenant({ ...emptyWorkspace, external_outbounds: [{ ...outbound, tenant: 'other' }] }, null),
  ).toBeNull();
  expect(singleTunnelTenant({ ...emptyWorkspace, users: [] }, 'scoped-root')).toBe('scoped-root');
});

it('hides chain proxy management from the tunnel page', () => {
  const view = mount({ drill: { p: 'list' } });
  const panels = view.container.querySelectorAll('.tunnel-cardpage > .panel');
  const header = panels[0]?.querySelector(':scope > header');

  expect(panels).toHaveLength(1);
  expect(within(panels[0] as HTMLElement).getByRole('heading', { name: '隧道' })).toBeTruthy();
  expect(header?.textContent).not.toMatch(/自定义.*WARP.*VPN Gate/);
  expect(header?.querySelector('.hint')).toBeNull();
  expect(header?.querySelector('.rd')).toBeNull();
  expect(view.queryByRole('heading', { name: '链式代理' })).toBeNull();
  expect(view.queryByRole('button', { name: /优选入口/ })).toBeNull();
});

it('aligns VPN Gate with managed tunnel rows and hides the tenant from WARP', () => {
  const warpSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [warpOutbound] },
  };
  const view = mount({ drill: { p: 'list' } }, warpSnapshot);

  const vpngate = view.getByRole('button', { name: /VPN Gate 出口池/ });
  expect(vpngate.classList.contains('tunnel-row')).toBe(true);
  expect(vpngate.closest('.tunnel-list')).not.toBeNull();
  expect(vpngate.querySelector('.tunnel-proto.large')).toBeNull();

  const warp = view.getByRole('button', { name: /Cloudflare WARP/ });
  expect(within(warp).queryByText('平台')).toBeNull();
  expect(view.getByText('0 个机器 · 0 条链路')).toBeTruthy();
  const warpHeader = view.getByRole('heading', { name: 'Cloudflare WARP' }).closest('header');
  expect(warpHeader?.querySelector('.tunnel-group-agg b')).toBeNull();
});

it('keeps the VPN Gate list row limited to configured metadata', () => {
  const configuredRegions = [
    ['vpngate-1111-1111', 'JP'],
    ['vpngate-2222-2222', 'KR'],
    ['vpngate-3333-3333', 'US'],
    ['vpngate-4444-4444', 'SG'],
    ['vpngate-5555-5555', 'DE'],
  ] as const;
  const vpngateOutbounds: ExternalOutbound[] = configuredRegions.map(([id, countryCode]) => ({
    ...outbound,
    id,
    name: `${countryCode} 出口池`,
    protocol: {
      t: 'vpngate',
      v: { country_code: countryCode, max_connect_ms: 15_000, min_download_bps: 1_000_000, max_candidates: 10 },
    },
    security: { t: 'none' },
  }));
  const vpngateSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: {
      ...snapshot.snapshot,
      apps: [
        {
          ...snapshot.snapshot.apps[0],
          chains: Array.from({ length: 6 }, (_, index) => ({
            id: `chain-${index + 1}`,
            tenant: 'platform',
            name: `线路 ${index + 1}`,
          })),
          steps: Array.from({ length: 6 }, (_, index) => ({
            chain: `chain-${index + 1}`,
            node: `node-${index + 1}`,
            accept: null,
            hop_in: null,
            rules: [
              {
                m: { t: 'any' as const },
                a: { t: 'proxy' as const, outbound: vpngateOutbounds[index % vpngateOutbounds.length].id },
              },
            ],
          })),
        },
      ],
      external_outbounds: [...vpngateOutbounds, warpOutbound],
    },
  };
  const view = mount({ drill: { p: 'list' } }, vpngateSnapshot);

  const provider = view.getByRole('button', { name: /VPN Gate 出口池/ });
  expect(within(provider).getByText('5 个地区')).toBeTruthy();
  expect(view.getByText('5 个地区 · 6 条链路')).toBeTruthy();
  expect(view.queryByText(/个出站/)).toBeNull();
  expect(view.queryByText(/已配置出口/)).toBeNull();
  expect(within(provider).getAllByRole('img')).toHaveLength(5);
  for (const [, countryCode] of configuredRegions) {
    expect(within(provider).getByRole('img', { name: `${countryCode} 地区旗` })).toBeTruthy();
  }
  expect(view.queryByText(/有实测|目录节点|正在读取目录/)).toBeNull();
});

it('owns editing and deletion without exposing hidden chain proxy management', async () => {
  const view = mount({ drill: { p: 'custom', id: outbound.id } });

  expect(view.container.querySelector('.tunnel-strip')?.children).toHaveLength(3);
  expect(view.getByRole('heading', { name: outbound.name })).toBeTruthy();
  expect(view.queryByText(/租户/)).toBeNull();
  expect(view.getByText('传输安全')).toBeTruthy();
  expect(view.queryByText('安全层')).toBeNull();
  expect(view.queryByText('链式代理引用')).toBeNull();
  expect(view.queryByRole('button', { name: /优选入口/ })).toBeNull();
  expect(view.queryByRole('checkbox')).toBeNull();

  fireEvent.click(view.getByRole('button', { name: '编辑' }));
  await waitFor(() => expect(view.getByRole('dialog', { name: '编辑隧道' })).toBeTruthy());
  fireEvent.click(view.getByRole('button', { name: '取消' }));

  fireEvent.click(view.getByRole('button', { name: '删除' }));
  await waitFor(() => expect(view.getByRole('dialog', { name: '删除隧道' })).toBeTruthy());
});

it('identifies VLESS Encryption without a separate transport-security fact', () => {
  const encrypted: ExternalOutbound = {
    ...outbound,
    id: 'encrypted',
    name: '原生加密出口',
    protocol: {
      t: 'vless',
      v: {
        credential: '<redacted>',
        encryption: `mlkem768x25519plus.native.1rtt.${'A'.repeat(43)}`,
        flow: null,
        transport: { t: 'raw' },
      },
    },
    security: { t: 'none' },
  };
  const encryptedSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [encrypted] },
  };
  const view = mount({ drill: { p: 'custom', id: encrypted.id } }, encryptedSnapshot);

  expect(view.container.querySelector('.tunnel-strip')?.children).toHaveLength(2);
  expect(view.getAllByText('VLESS Encryption').length).toBeGreaterThan(0);
  expect(view.queryByText('传输安全')).toBeNull();
  expect(view.queryByText('安全层')).toBeNull();
});

it('shows WARP status in the page body without a duplicate progress strip', () => {
  const warpSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [warpOutbound] },
  };
  const view = mount({ drill: { p: 'warp', id: warpOutbound.id } }, warpSnapshot);
  expect(view.container.querySelector('.tunnel-strip')).toBeNull();
  expect(view.container.querySelector('.wp-page .cg-body')).not.toBeNull();
  expect(view.container.querySelector('.wp-page .cg-aside')).not.toBeNull();
  const sidePanels = Array.from(view.container.querySelectorAll('.wp-page .cg-aside > .panel'));
  expect(sidePanels.length).toBeGreaterThan(0);
  expect(sidePanels.every(panel => panel.classList.contains('config-panel'))).toBe(true);
  expect(view.queryByRole('list', { name: 'WARP 启用进度' })).toBeNull();
  expect(view.container.querySelector('.wp-page .nd-ident-meta')).toBeNull();
  expect(view.container.querySelector('.wp-title-row > .wp-title-icon + .nd-id')).not.toBeNull();
  expect(view.container.querySelector('.wp-title-icon .detail-title-glyph svg')?.getAttribute('width')).toBe('18');
  expect(view.container.querySelector('.wp-title-icon .cg-lamp')).toBeNull();
  const machineHeading = view.getByRole('heading', { name: '机器身份' });
  const machinePanel = machineHeading.closest('section') as HTMLElement;
  const machineHeader = machinePanel.querySelector(':scope > header') as HTMLElement;
  expect(within(machineHeader).queryByRole('button', { name: '注册机器' })).toBeNull();
  expect(within(machinePanel).getByText('新增身份')).toBeTruthy();
  expect(within(machinePanel).getByRole('combobox', { name: '待注册机器' })).toBeTruthy();
  expect(within(machinePanel).getByRole('button', { name: '注册机器' }).closest('.wp-register-bar')).not.toBeNull();
  expect(machinePanel.querySelector('.cgr-notice')).toBeNull();
  expect(within(machinePanel).queryByText('没有已注册的机器')).toBeNull();
  expect(view.getByRole('heading', { name: '规则引用' })).toBeTruthy();
  expect(view.queryByRole('heading', { name: '线路拨测' })).toBeNull();
  expect(view.getAllByRole('button', { name: '前往线路' })).toHaveLength(2);
});

it('does not request deployment history from a public WARP detail', async () => {
  const request = vi.fn(async (input: RequestInfo | URL) => {
    throw new Error(`访客不应请求 ${String(input)}`);
  });
  vi.stubGlobal('fetch', request);
  const warpSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [warpOutbound] },
  };

  const view = mount({ drill: { p: 'warp', id: warpOutbound.id } }, warpSnapshot, [], warpSnapshot, {
    publicView: true,
    seedDeployments: false,
  });

  await waitFor(() => expect(view.getByRole('heading', { name: warpOutbound.name })).toBeTruthy());
  expect(request).not.toHaveBeenCalled();
  expect(view.client.getQueryState(['deployments', 'config'])?.fetchStatus).toBe('idle');
  expect(view.client.getQueryData(['deployments', 'config'])).toBeUndefined();
});

it('hides tenant metadata from WARP details while keeping operational summary fields', () => {
  const warpSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: { ...snapshot.snapshot, external_outbounds: [warpOutbound] },
  };
  const view = mount({ drill: { p: 'warp', id: warpOutbound.id } }, warpSnapshot);
  const summary = view.getByRole('heading', { name: '隧道' }).closest('section') as HTMLElement;

  expect(view.queryByText('租户')).toBeNull();
  expect(view.queryByText(warpOutbound.tenant, { exact: true })).toBeNull();
  for (const label of ['协议', '修订', '发布', '机器身份']) {
    expect(within(summary).getByText(label, { exact: true })).toBeTruthy();
  }
  expect(within(summary).getByText('Cloudflare WARP')).toBeTruthy();
  expect(within(summary).getByText('r1')).toBeTruthy();
});

it('shows a converged WARP release in the identity and summary without a progress strip', () => {
  const readyWarp: ExternalOutbound = {
    ...warpOutbound,
    bindings: [
      {
        node: 'hk',
        device_id: 'device-hk',
        account_id: 'account-hk',
        registered_at: '2026-09-12T00:00:00Z',
        peer_public_key: 'peer-key',
        local_addresses: ['172.16.0.2/32'],
        reserved: [1, 2, 3],
      },
    ],
  };
  const releasedSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: {
      ...snapshot.snapshot,
      revision: 7,
      apps: [
        {
          ...snapshot.snapshot.apps[0],
          chains: [{ id: 'stream', tenant: 'platform', name: '流媒体' }],
          steps: [
            {
              chain: 'stream',
              node: 'hk',
              accept: null,
              hop_in: null,
              rules: [{ m: { t: 'any' }, a: { t: 'proxy', outbound: readyWarp.id } }],
            },
          ],
        },
      ],
      external_outbounds: [readyWarp],
    },
  };
  const deployment: DeploymentListItem = {
    id: 23,
    revision_id: 7,
    status: 'succeeded',
    activation_status: 'activated',
    settlement_status: 'converged',
    activated_at: '2026-09-12T00:01:00Z',
    active: false,
    actor: 'editor',
    kind: 'config',
    note: null,
    base_revision_id: 6,
    rollback_of_deployment_id: null,
    sync_of_deployment_id: null,
    created_at: '2026-09-12T00:00:30Z',
    started_at: '2026-09-12T00:00:40Z',
    finished_at: '2026-09-12T00:01:00Z',
    total_targets: 1,
    changed_targets: 1,
    skipped_targets: 0,
    failed_targets: 0,
    debt_targets: 0,
    disruptive_targets: 1,
    max_wave: 1,
    awaiting_confirmation: false,
  };
  const view = mount({ drill: { p: 'warp', id: readyWarp.id } }, releasedSnapshot, [deployment]);

  expect(view.container.querySelector('.nd-ident-row .st')?.textContent).toBe('已发布');
  expect(view.getByText('#23 已收敛')).toBeTruthy();
  expect(view.queryByRole('list', { name: 'WARP 启用进度' })).toBeNull();
  expect(view.queryByRole('button', { name: '查看发布记录' })).toBeNull();
});
