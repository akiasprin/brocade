// Regression coverage converted from the frontend audit's delayed-response reproductions.
import { useEffect, useState } from 'react';
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
window.matchMedia = ((media: string) => ({
  matches: false,
  media,
  addEventListener() {},
  removeEventListener() {},
})) as unknown as typeof window.matchMedia;
const { draft } = await import('../src/draft');
const { DraftBar } = await import('../src/forge/draft-bar');
const { SessionProvider } = await import('../src/session');
const { fetchSnapshot } = await import('../src/api');
const who = {
  operator_id: 'audit',
  role: 'system-admin' as const,
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};
const initial = { node_count: 0, chain_group_count: [] };
const clients: QueryClient[] = [];
function client() {
  const qc = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 5000, refetchOnWindowFocus: false, refetchOnReconnect: false },
      mutations: { retry: false },
    },
  });
  clients.push(qc);
  return qc;
}
function json(body: unknown) {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => (resolve = r));
  return { promise, resolve };
}
afterEach(() => {
  cleanup();
  clients.forEach(qc => qc.clear());
  clients.length = 0;
  draft.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
it('submission preserves edits added after the request was sent', async () => {
  draft.init('audit-submit');
  draft.clear();
  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'submitted' } });
  const response = deferred<Response>();
  let sent: unknown;
  vi.stubGlobal(
    'fetch',
    vi.fn((_path, init) => {
      sent = JSON.parse(init.body);
      return response.promise;
    }),
  );
  render(
    <QueryClientProvider client={client()}>
      <SessionProvider value={{ who, initial }}>
        <DraftBar current={1} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '提交' }));
  await waitFor(() => expect(sent).toBeDefined());
  act(() => draft.push({ op: 'update_node', node_id: 'B', node: { name: 'never submitted' } }));
  expect(draft.ops()).toHaveLength(2);
  await act(async () => response.resolve(json({ revision_id: 2 })));
  await waitFor(() =>
    expect(draft.ops()).toEqual([{ op: 'update_node', node_id: 'B', node: { name: 'never submitted' } }]),
  );
  expect(JSON.stringify(sent)).not.toContain('never submitted');
});

function SnapshotReader() {
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        void qc.invalidateQueries({ queryKey: ['snapshot'] });
      }),
    [qc],
  );
  const q = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  return <div>{q.data ? JSON.stringify(q.data) : 'waiting'}</div>;
}
it('draft invalidation during initial fetch returns the latest preview', async () => {
  draft.init('audit-inflight');
  draft.clear();
  const response = deferred<Response>();
  const fetch = vi.fn((path: string) =>
    path === '/model/snapshot' ? response.promise : Promise.resolve(json({ snapshot: { tag: 'draft' } })),
  );
  vi.stubGlobal('fetch', fetch);
  const qc = client();
  render(
    <QueryClientProvider client={qc}>
      <SnapshotReader />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  act(() => draft.push({ op: 'update_node', node_id: 'A', node: { name: 'new draft' } }));
  await act(async () => response.resolve(json({ tag: 'old committed' })));
  await waitFor(() => expect(screen.getByText(/draft/)).toBeDefined());
  expect(screen.queryByText(/old committed/)).toBeNull();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(draft.ops()).toHaveLength(1);
});

const committedSettings = () => ({
  reality_client: { min_client_ver: null, max_client_ver: null, max_time_diff_ms: null },
  reality_site: {
    dest: 'www.committed.example:443',
    server_names: ['www.committed.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
  overlay: { keepalive_secs: 25, mtu: 1420, disabled_links: [] },
  ports: { ingress_base: 13443, anytls_base: 14443, hop_base: 20000, hy2_base: 30000 },
  probe: { endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace', timeout_secs: 10, interval_secs: 60 },
  geodata: { cron: 'CRON_TZ=Asia/Shanghai 30 6 * * *', geoip_url: 'https://geoip', geosite_url: 'https://geosite' },
  connection: {
    conn_idle_secs: 300,
    uplink_only_secs: 2,
    downlink_only_secs: 5,
    buffer_size_kb: null,
    handshake_secs: 60,
  },
  relay_mux: {
    concurrency: 1,
    prewarm_workers: 0,
    reuse_threshold: 2,
    max_probing_workers: 1,
    probe_interval_ms: 5000,
    probe_timeout_ms: 2000,
    idle_ttl_ms: 24000,
    max_requests_per_worker: 128,
  },
  anytls_padding_scheme: ['stop=4'],
  stats_user_online: false,
});

const ROUTES: Record<string, () => unknown> = {
  '/settings': committedSettings,
  '/branding': () => ({ site_name: 'brocade', icon: null, accent: null }),
  '/auth/state': () => ({ public_open: false }),
  '/certs': () => ({ groups: [], nodes: [], letsencrypt: 'https://acme', letsencrypt_staging: 'https://acme-staging' }),
  '/distribution': () => ({
    stored: { agent_public_url: 'https://example', xray_version: '26.7.28' },
    effective: { agent_public_url: 'https://example', xray_version: '26.7.28' },
  }),
  '/agent-log-policy': () => ({
    global: { agent_journal_mib: 100, xray_mib: 100, phantun_mib: 100 },
    nodes: [],
  }),
  '/ping-probe/settings': () => ({ targets: [], interval_secs: 60, timeout_ms: 420 }),
  '/tunnel-probes': () => ({
    origin: 'console',
    endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace',
    retention_days: 7,
    items: [],
  }),
  '/tunnel-probes/capability': () => ({ available: true, version: '26.4.25', reason: null, concurrency: 30 }),
  '/vpngate': () => ({
    manual_pools_supported: true,
    status: {},
    countries: [],
    admission_policy: {
      minimum_successful_sources: 1,
      country_policy: 'any_match',
      risk_decision_policy: 'all_available_pass',
      provider_rules: [
        { provider: 'proxycheck', maximum_score: 80 },
        { provider: 'ffraud', maximum_score: 80 },
        { provider: 'iplogs', maximum_score: 80 },
      ],
    },
  }),
  '/links/mtu': () => ({ default_mtu: 1420, nodes: [], links: [] }),
  '/revisions?limit=50': () => ({ current_revision: 7, revisions: [{ id: 7 }] }),
};

const { SettingsPane } = await import('../src/panes/settings');
const { AgentReleaseTab, useAgentRelease } = await import('../src/panes/agent-release');
/** 发布页把编辑状态放在页头；这里直接进入编辑，只测 Agent 页签的升级范围。 */
function AgentHarness() {
  const agent = useAgentRelease();
  const [editing, setEditing] = useState(true);
  return <AgentReleaseTab agent={agent} editable editing={editing} onEditingChange={setEditing} />;
}
it('branding save preserves further typing while the request is pending', async () => {
  draft.init('audit-branding');
  draft.clear();
  const qc = client();
  const keys: Record<string, string> = {
    '/settings': 'settings',
    '/branding': 'branding',
    '/auth/state': 'auth-state',
    '/certs': 'certs',
    '/distribution': 'distribution',
    '/agent-log-policy': 'agent-log-policy',
    '/ping-probe/settings': 'ping-probe-settings',
    '/tunnel-probes': 'tunnel-probes',
    '/tunnel-probes/capability': 'tunnel-probe-capability',
    '/vpngate': 'vpngate',
  };
  for (const [path, key] of Object.entries(keys)) qc.setQueryData([key], ROUTES[path]());
  qc.setQueryData(['nodes'], { nodes: [] });
  qc.setQueryData(['link-mtu'], { nodes: [], links: [], default_mtu: 1420 });
  const response = deferred<Response>();
  const fetch = vi.fn(() => response.promise);
  vi.stubGlobal('fetch', fetch);
  const view = render(
    <QueryClientProvider client={qc}>
      <SessionProvider value={{ who, initial }}>
        <SettingsPane />
      </SessionProvider>
    </QueryClientProvider>,
  );
  const field = view.container.querySelector('#set-branding input') as HTMLInputElement;
  fireEvent.change(field, { target: { value: 'submitted title' } });
  const button = Array.from(view.container.querySelectorAll('#set-branding button')).find(b =>
    b.textContent?.includes('保存'),
  )!;
  fireEvent.click(button);
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  fireEvent.change(field, { target: { value: 'later unsaved title' } });
  expect(field.value).toBe('later unsaved title');
  await act(async () => response.resolve(json({ site_name: 'submitted title', icon_data_url: null })));
  await waitFor(() =>
    expect(qc.getQueryData(['branding'])).toEqual({ site_name: 'submitted title', icon_data_url: null }),
  );
  expect(field.value).toBe('later unsaved title');
});
it('agent selected-node set cannot expand to all nodes when only counts match', async () => {
  const qc = client();
  qc.setQueryData(['nodes'], {
    nodes: [
      { node_id: 'A', agent_version: null },
      { node_id: 'B', agent_version: null },
    ],
  });
  qc.setQueryData(['agent-release'], {
    agent_version: '1',
    available_release_id: 'a'.repeat(64),
    available_agents: [],
    released: { scope: 'nodes', nodes: ['A', 'removed-C'], release_id: 'a'.repeat(64) },
  });
  let sent: { scope: string; nodes: string[] } | undefined;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (_path, init) => {
      sent = JSON.parse(init.body);
      return json({ ...qc.getQueryData<object>(['agent-release']), released: sent });
    }),
  );
  render(
    <QueryClientProvider client={qc}>
      <AgentHarness />
    </QueryClientProvider>,
  );
  expect(screen.getByRole('button', { name: '选中的机器' }).getAttribute('aria-pressed')).toBe('true');
  expect((screen.getByLabelText('升级 A') as HTMLInputElement).checked).toBe(true);
  expect((screen.getByLabelText('升级 B') as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: '批准' }));
  await waitFor(() => expect(sent).toBeDefined());
  expect(sent!.scope).toBe('nodes');
  expect(sent!.nodes).toEqual(['A']);
  expect(sent!.nodes).not.toContain('B');
});

it('committing settings draft keeps the new baseline and refreshes settings', async () => {
  draft.init('audit-settings-commit');
  draft.clear();
  const qc = client();
  const keys: Record<string, string> = {
    '/settings': 'settings',
    '/branding': 'branding',
    '/auth/state': 'auth-state',
    '/certs': 'certs',
    '/distribution': 'distribution',
    '/agent-log-policy': 'agent-log-policy',
    '/ping-probe/settings': 'ping-probe-settings',
    '/tunnel-probes': 'tunnel-probes',
    '/tunnel-probes/capability': 'tunnel-probe-capability',
    '/vpngate': 'vpngate',
  };
  for (const [path, key] of Object.entries(keys)) qc.setQueryData([key], ROUTES[path]());
  qc.setQueryData(['nodes'], { nodes: [] });
  qc.setQueryData(['link-mtu'], { nodes: [], links: [], default_mtu: 1420 });
  const pending = committedSettings();
  pending.overlay.mtu = 1380;
  draft.push({ op: 'update_settings', settings: pending as unknown as import('../src/api').ModelSettings });
  let committed = false;
  const fetch = vi.fn(async (path: string) => {
    if (path === '/model/apply') {
      committed = true;
      return json({ revision_id: 8 });
    }
    if (path === '/settings') return json(pending);
    if (path === '/nodes/agent-state') return json({ nodes: [] });
    if (ROUTES[path]) return json(ROUTES[path]());
    throw new Error(path);
  });
  vi.stubGlobal('fetch', fetch);
  const view = render(
    <QueryClientProvider client={qc}>
      <SessionProvider value={{ who, initial }}>
        <DraftBar current={7} />
        <SettingsPane />
      </SessionProvider>
    </QueryClientProvider>,
  );
  const field = view.container.querySelectorAll('#set-wg input')[1] as HTMLInputElement;
  expect(field.value).toBe('1380');
  fireEvent.click(screen.getByRole('button', { name: '提交' }));
  await waitFor(() => expect(committed).toBe(true));
  await waitFor(() => expect(draft.isEmpty()).toBe(true));
  await waitFor(() => expect(field.value).toBe('1380'));
  await waitFor(() => expect(fetch.mock.calls.some(([path]) => path === '/settings')).toBe(true));
});

function settingsClient() {
  const qc = client();
  const keys: Record<string, string> = {
    '/settings': 'settings',
    '/branding': 'branding',
    '/auth/state': 'auth-state',
    '/certs': 'certs',
    '/distribution': 'distribution',
    '/agent-log-policy': 'agent-log-policy',
    '/ping-probe/settings': 'ping-probe-settings',
    '/tunnel-probes': 'tunnel-probes',
    '/tunnel-probes/capability': 'tunnel-probe-capability',
    '/vpngate': 'vpngate',
  };
  for (const [path, key] of Object.entries(keys)) qc.setQueryData([key], ROUTES[path]());
  qc.setQueryData(['nodes'], { nodes: [] });
  qc.setQueryData(['link-mtu'], { nodes: [], links: [], default_mtu: 1420 });
  return qc;
}
function mountSettings(qc: QueryClient) {
  return render(
    <QueryClientProvider client={qc}>
      <SessionProvider value={{ who, initial }}>
        <SettingsPane />
      </SessionProvider>
    </QueryClientProvider>,
  );
}
function saveSection(id: string) {
  const button = Array.from(document.querySelectorAll(`#${id} button`)).find(item =>
    item.textContent?.startsWith('保存'),
  );
  if (!button) throw new Error(`missing save button: ${id}`);
  fireEvent.click(button);
}

it.each(['distribution', 'ping', 'certificate'] as const)('%s saves preserve input made while waiting', async kind => {
  draft.init(`pending-${kind}`);
  draft.clear();
  const qc = settingsClient();
  const certs = {
    groups: [],
    nodes: [],
    sealing_available: true,
    letsencrypt: 'https://acme',
    letsencrypt_staging: 'https://staging',
    domain: {
      id: 'domain',
      domain: 'example.com',
      signing_method: 'public-ca',
      acme_directory: 'https://acme',
      acme_contact: null,
      renew_before_days: 30,
      has_credential: true,
      has_account: true,
      dns_provider: 'cloudflare',
    },
  };
  qc.setQueryData(['certs'], certs);
  const response = deferred<Response>();
  const fetch = vi.fn(() => response.promise);
  vi.stubGlobal('fetch', fetch);
  const view = mountSettings(qc);
  const id = kind === 'distribution' ? 'set-dist' : kind === 'ping' ? 'set-ping-probe' : 'set-cert';
  const input = (
    kind === 'ping'
      ? view.getByLabelText('探测间隔')
      : kind === 'certificate'
        ? view.getByPlaceholderText('example.net')
        : view.container.querySelector('#set-dist input')
  ) as HTMLInputElement;
  const submitted =
    kind === 'ping' ? '120' : kind === 'certificate' ? 'saved.example.com' : 'https://saved.example.com';
  const later = kind === 'ping' ? '180' : kind === 'certificate' ? 'later.example.com' : 'https://later.example.com';
  fireEvent.change(input, { target: { value: submitted } });
  saveSection(id);
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  fireEvent.change(input, { target: { value: later } });
  const result =
    kind === 'distribution'
      ? { stored: { agent_public_url: submitted }, effective: { agent_public_url: submitted } }
      : kind === 'ping'
        ? { interval_secs: 120, timeout_ms: 420, targets: [] }
        : { ...certs, domain: { ...certs.domain, domain: submitted } };
  await act(async () => response.resolve(json(result)));
  const key = kind === 'distribution' ? 'distribution' : kind === 'ping' ? 'ping-probe-settings' : 'certs';
  await waitFor(() => expect(qc.getQueryData([key])).toEqual(result));
  expect(input.value).toBe(later);
  expect(document.querySelector(`#${id}`)?.textContent).toContain('有未保存');
});

it('ping targets take one optional address per family and save an empty family as null', async () => {
  draft.init('ping-dual-stack');
  draft.clear();
  const qc = settingsClient();
  qc.setQueryData(['ping-probe-settings'], {
    targets: [{ name: 'CF', kind: 'icmp', ipv4: '1.1.1.1', ipv6: null }],
    interval_secs: 60,
    timeout_ms: 420,
  });
  let sent: unknown;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (_path: string, init: RequestInit) => {
      sent = JSON.parse(String(init.body));
      return json(sent);
    }),
  );
  const view = mountSettings(qc);
  const ipv4 = view.getByLabelText('目标 1 IPv4 地址') as HTMLInputElement;
  const ipv6 = view.getByLabelText('目标 1 IPv6 地址') as HTMLInputElement;
  expect(ipv6.placeholder).toBe('不探测');

  fireEvent.change(ipv6, { target: { value: ' [2606:4700:4700::1111] ' } });
  fireEvent.blur(ipv6);
  expect(ipv6.value).toBe('2606:4700:4700::1111');
  fireEvent.change(ipv4, { target: { value: '' } });
  expect(ipv4.placeholder).toBe('不探测');

  fireEvent.change(ipv6, { target: { value: '' } });
  const section = document.querySelector('#set-ping-probe')!;
  expect(section.textContent).toContain('CF：至少填写一个地址');
  expect(ipv4.getAttribute('aria-invalid')).toBe('true');
  expect(ipv6.getAttribute('aria-invalid')).toBe('true');

  fireEvent.change(ipv6, { target: { value: '2606:4700:4700::1111' } });
  expect(ipv4.getAttribute('aria-invalid')).toBeNull();
  saveSection('set-ping-probe');
  await waitFor(() =>
    expect(sent).toEqual({
      targets: [{ name: 'CF', kind: 'icmp', ipv4: null, ipv6: '2606:4700:4700::1111' }],
      interval_secs: 60,
      timeout_ms: 420,
    }),
  );
});

it('a rejected save retains subsequent input and presents the error', async () => {
  const qc = settingsClient();
  const response = deferred<Response>();
  const fetch = vi.fn(() => response.promise);
  vi.stubGlobal('fetch', fetch);
  const view = mountSettings(qc);
  const input = view.container.querySelector('#set-branding input') as HTMLInputElement;
  fireEvent.change(input, { target: { value: 'submitted' } });
  saveSection('set-branding');
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  fireEvent.change(input, { target: { value: 'later' } });
  await act(async () => response.resolve(new Response(JSON.stringify({ error: 'save failed' }), { status: 500 })));
  expect(await screen.findByText('save failed')).toBeTruthy();
  expect(input.value).toBe('later');
});

it.each([false, true])('discard while preview is pending ignores the old response (failure=%s)', async fail => {
  draft.init(`preview-discard-${fail}`);
  draft.clear();
  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'preview' } });
  const old = deferred<Response>();
  vi.stubGlobal(
    'fetch',
    vi.fn((path: string) =>
      path === '/model/preview' ? old.promise : Promise.resolve(json({ tag: 'committed latest' })),
    ),
  );
  const read = fetchSnapshot();
  draft.clear();
  old.resolve(
    fail
      ? new Response(JSON.stringify({ error: 'old draft invalid' }), { status: 400 })
      : json({ snapshot: { tag: 'old draft' } }),
  );
  expect(await read).toEqual({ tag: 'committed latest' });
});

function mountAgent(scope: 'off' | 'nodes' | 'all', ids: string[], nodeIds = ['A', 'B']) {
  const qc = client();
  qc.setQueryData(['nodes'], { nodes: nodeIds.map(node_id => ({ node_id, agent_version: null })) });
  const release = {
    agent_version: '1',
    available_release_id: 'a'.repeat(64),
    available_agents: [],
    released: { scope, nodes: ids, release_id: 'a'.repeat(64) },
  };
  qc.setQueryData(['agent-release'], release);
  const fetch = vi.fn(async (_path: string, init?: RequestInit) =>
    json({ ...release, released: JSON.parse(String(init?.body)) }),
  );
  vi.stubGlobal('fetch', fetch);
  const view = render(
    <QueryClientProvider client={qc}>
      <AgentHarness />
    </QueryClientProvider>,
  );
  return { qc, fetch, view };
}
it('selecting every individual machine remains a fixed selection when a machine is added', async () => {
  const { qc, fetch } = mountAgent('off', []);
  fireEvent.click(screen.getByRole('button', { name: '选中的机器' }));
  fireEvent.click(screen.getByLabelText('升级 A'));
  fireEvent.click(screen.getByLabelText('升级 B'));
  act(() => qc.setQueryData(['nodes'], { nodes: ['A', 'B', 'C'].map(node_id => ({ node_id, agent_version: null })) }));
  await waitFor(() => expect(screen.getByLabelText('升级 C')).toBeTruthy());
  expect((screen.getByLabelText('升级 C') as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: '批准' }));
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toMatchObject({ scope: 'nodes', nodes: ['A', 'B'] });
});
it('an explicitly selected all-machines scope includes newly added machines', async () => {
  const { qc, fetch } = mountAgent('off', []);
  fireEvent.click(screen.getByRole('button', { name: '全部机器' }));
  act(() => qc.setQueryData(['nodes'], { nodes: ['A', 'B', 'C'].map(node_id => ({ node_id, agent_version: null })) }));
  await waitFor(() => expect((screen.getByLabelText('升级 C') as HTMLInputElement).checked).toBe(true));
  fireEvent.click(screen.getByRole('button', { name: '批准' }));
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toMatchObject({ scope: 'all', nodes: [] });
});
it('saved all scope does not become off when the fleet is empty', () => {
  mountAgent('all', [], []);
  expect((screen.getByRole('button', { name: '批准' }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByRole('button', { name: '全部机器' }).getAttribute('aria-pressed')).toBe('true');
});
it('reordering the fleet does not make a fixed set dirty', () => {
  mountAgent('nodes', ['B', 'A']);
  expect((screen.getByRole('button', { name: '批准' }) as HTMLButtonElement).disabled).toBe(true);
});
it('a machine selection cannot be approved empty; stopping upgrades is the explicit off scope', async () => {
  const { fetch } = mountAgent('all', []);
  fireEvent.click(screen.getByRole('button', { name: '选中的机器' }));
  expect((screen.getByRole('button', { name: '批准' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: '不升级' }));
  expect(screen.getByText('所有机器保持当前版本，不会自行升级。')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '批准' }));
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toMatchObject({ scope: 'off', nodes: [] });
});
it('available build refresh does not reset unsaved machine selection', async () => {
  const { qc } = mountAgent('off', []);
  fireEvent.click(screen.getByRole('button', { name: '选中的机器' }));
  fireEvent.click(screen.getByLabelText('升级 A'));
  act(() =>
    qc.setQueryData(['agent-release'], {
      ...qc.getQueryData<object>(['agent-release']),
      available_release_id: 'b'.repeat(64),
    }),
  );
  await waitFor(() => expect((screen.getByLabelText('升级 A') as HTMLInputElement).checked).toBe(true));
  expect((screen.getByLabelText('升级 B') as HTMLInputElement).checked).toBe(false);
});

it('a refresh started during a save cannot overwrite its successful response', async () => {
  const qc = settingsClient();
  const put = deferred<Response>();
  const get = deferred<Response>();
  const fetch = vi.fn((_path: string, init?: RequestInit) => (init?.method === 'PUT' ? put.promise : get.promise));
  vi.stubGlobal('fetch', fetch);
  const view = mountSettings(qc);
  const input = view.container.querySelector('#set-branding input') as HTMLInputElement;
  fireEvent.change(input, { target: { value: 'saved title' } });
  saveSection('set-branding');
  await waitFor(() => expect(fetch).toHaveBeenCalled());
  const { fetchBranding } = await import('../src/api');
  const refresh = qc.fetchQuery({ queryKey: ['branding'], queryFn: fetchBranding, staleTime: 0 }).catch(() => null);
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
  await act(async () => put.resolve(json({ site_name: 'saved title', icon_data_url: null })));
  await waitFor(() => expect(qc.getQueryData(['branding'])).toEqual({ site_name: 'saved title', icon_data_url: null }));
  await act(async () => get.resolve(json({ site_name: 'old title', icon_data_url: null })));
  await refresh;
  expect(input.value).toBe('saved title');
  expect(qc.getQueryData(['branding'])).toEqual({ site_name: 'saved title', icon_data_url: null });
});
