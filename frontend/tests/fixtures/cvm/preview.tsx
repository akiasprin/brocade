// The real application and CVM pane, using a synthetic API transport.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../../src/app';
import { theme } from '../../../src/forge/theme';
import { installBackend } from '../console-backend';
import { baremetalWorkspace, cvmReadings, cvmWorkspace } from './data';
import { allocatedNic, nicCatalog, nicState } from './network-data';
import { networkLogs } from './log-data';
import { entryBoard } from './entry-data';
import { artifactHistory, artifactReceipt } from './artifact-data';
import { chainCatalog } from './chain-data';
import type { CvmChange, CvmCreate, CvmNetworkQuotaLimits, DeploymentPlan } from '../../../src/api';
import type { CvmNetworkEditRequest } from '../../../src/api/cvm-network-edit';
import '../../../src/styles.css';

const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'list';
const role = params.get('role') ?? 'system-admin';
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
installBackend();
const fallback = globalThis.fetch;
const workspace = scene.startsWith('baremetal-') ? baremetalWorkspace(scene, role) : cvmWorkspace(scene, role);
if (scene === 'staged' || scene === 'staged-entry') {
  workspace.pending_revision_id = 42;
  Object.assign(workspace.instances[0], {
    revision_id: 42,
    published_revision_id: 41,
    operation_kind: scene === 'staged-entry' ? 'entries' : 'configure',
    status: scene === 'staged-entry' ? 'applied' : 'pending',
  });
}
if (scene === 'oversold') {
  const caps = workspace.hosts[0].capabilities;
  caps.allocatable_vcpus = 1;
  caps.allocatable_memory_bytes = 128 * 1024 ** 2;
  caps.allocatable_disk_bytes = 1024 ** 3;
  workspace.instances[0].desired.spec.resources.vcpus = 3;
}

if (scene === 'hosts' || scene === 'new-copy') {
  const host = workspace.hosts[0];
  if (scene === 'new-copy') {
    host.capabilities.primary_ip_delivery = true;
    workspace.instances[0].desired.spec.resources = {
      vcpus: 2,
      memory_bytes: 1024 ** 3,
      root_disk_bytes: 6 * 1024 ** 3,
    };
  }
  host.observed_at = '2026-11-04T10:28:00Z';
  workspace.hosts.push(
    {
      ...structuredClone(host),
      node_id: 'tokyo',
      name: '东京 · 开发宿主',
      capabilities: {
        ...host.capabilities,
        allocatable_vcpus: 16,
        allocatable_memory_bytes: 32 * 1024 ** 3,
        allocatable_disk_bytes: 240 * 1024 ** 3,
      },
    },
    {
      ...structuredClone(host),
      node_id: 'stale-host',
      name: '很长的宿主名称 · 华东家庭网络与双栈业务开发环境',
      observed_at: '2026-11-04T10:10:00Z',
      fresh: false,
    },
    {
      ...structuredClone(host),
      node_id: 'failed-host',
      name: '备用宿主',
      error: '运行时无法连接，等待节点恢复',
    },
  );
  if (scene === 'hosts' && role === 'system-admin') host.terminal_sessions = { used: 2, limit: 3 };
}
if (scene === 'terminal-management') {
  const host = workspace.hosts[0];
  workspace.hosts.push(
    {
      ...structuredClone(host),
      node_id: 'compute-b',
      name: '宿主 B · 双栈',
      terminal_enabled: false,
      capabilities: { ...host.capabilities, guest_terminal: false },
    },
    { ...structuredClone(host), node_id: 'home', name: '家庭 · IPv4 专用来源', can_manage_terminal: false },
  );
}
if (scene === 'fleet') {
  const base = workspace.instances[0];
  const cases = [
    ['home-workspace', 'applied', 'running'],
    ['build-runner', 'applied', 'running'],
    ['database-replica', 'applied', 'running'],
    ['lab-sandbox', 'applied', 'stopped'],
    ['media-encoder', 'failed', 'stopped'],
    ['legacy-api', 'unknown', 'running'],
    ['ci-cache', 'pending', 'running'],
  ] as const;
  workspace.hosts.push({ ...structuredClone(workspace.hosts[0]), node_id: 'tokyo', name: '东京 · 开发宿主' });
  workspace.instances = cases.map(([name, status, power], index) => {
    const item = structuredClone(base);
    item.desired.id = `cvm-fleet-${index}`;
    if ([2, 3, 5].includes(index)) item.node_id = 'tokyo';
    item.desired.spec.name = name;
    item.status = status;
    item.desired.power = power;
    item.report!.power = power;
    item.report!.error =
      status === 'failed'
        ? '系统盘分配失败：可用空间不足。'
        : status === 'unknown'
          ? '运行时联络中断，系统盘与地址分配保留。'
          : null;
    return item;
  });
}
if (scene === 'board' || scene === 'chain-board') {
  const host = workspace.hosts[0];
  workspace.hosts.push({ ...structuredClone(host), node_id: 'tokyo', name: '东京 · 开发宿主' });
  const base = workspace.instances[0];
  const cases = [
    ['home-workspace', 'hk-hkix', 'applied', 'running'],
    ['build-runner', 'hk-hkix', 'applied', 'running'],
    ['database-replica', 'tokyo', 'applied', 'running'],
    ['lab-sandbox', 'tokyo', 'applied', 'stopped'],
  ] as const;
  workspace.instances = cases.map(([name, node, status, power], index) => {
    const item = structuredClone(base);
    item.desired.id = index === 0 ? base.desired.id : `cvm-board-${index}`;
    item.desired.spec.name = name;
    item.node_id = node;
    item.status = status;
    item.desired.power = power;
    item.report!.power = power;
    if (scene === 'chain-board' && index === 1) item.user_id = 'bob';
    if (index === 1) {
      // Port-selected egress: SSH via the HK NIC, everything else stays local.
      item.desired.spec.routing.routes = [
        {
          destination: '0.0.0.0/0',
          interface_id: 'nic-business',
          service: { protocol: 'tcp', port_start: 22, port_end: 22 },
        },
      ];
    }
    if (index > 1) {
      item.desired.spec.interfaces = item.desired.spec.interfaces.filter(nic => nic.purpose === 'contact');
      item.desired.spec.routing = { revision: 1, priorities: [], routes: [], dns: [] };
    }
    return item;
  });
}
if (scene.startsWith('artifacts')) {
  workspace.instances[0].desired.generation = 2;
  artifactReceipt(workspace.instances[0], scene === 'artifacts-mismatch');
  if (scene === 'artifacts-pending') {
    const instance = workspace.instances[0];
    instance.report!.artifacts!.files[0].effective_sha256 = 'e'.repeat(64);
    instance.desired.generation += 1;
    instance.desired.operation_id = 'pending-restart';
    instance.desired.restart_epoch += 1;
    instance.status = 'pending';
  }
}
if (scene.startsWith('chain-')) {
  workspace.pending_revision_id = 25;
  for (const host of workspace.hosts) host.capabilities.chain_egress = true;
  const instance = workspace.instances[0];
  instance.desired.spec.routing.priorities.push({ chain_id: 'chain-us', ipv4: 200, ipv6: 200 });
  instance.desired.spec.routing.routes.push({ destination: '203.0.113.0/24', chain_id: 'chain-us' });
}
const initialInstance = structuredClone(workspace.instances[0]);
const mutations: unknown[] = [];
const entries = entryBoard(workspace, scene);
if (params.get('policy') === '1') {
  const offer = entries.publications.flatMap(p => p.spec.addresses).find(o => o.address === '10.8.0.31');
  if (offer) offer.reserved_ports = { tcp: [{ start: 22, end: 22 }], udp: [] };
}
if (params.get('range') === '1' && entries.entries.length) {
  entries.entries[0].mode = {
    kind: 'l4_range',
    protocol: 'tcp',
    listen_start: 30040,
    listen_end: 30049,
    target_start: 20,
    target_end: 29,
  };
  for (const publication of entries.publications)
    for (const usage of publication.usage)
      for (const port of usage.ports)
        if (port.entry_id === entries.entries[0].id) {
          port.port_start = 30040;
          port.port_end = 30049;
        }
}

const catalog = nicCatalog(role === 'system-admin', workspace.instances[0]);
if (scene === 'new-copy' && catalog.providers.length) {
  catalog.providers.push({
    ...catalog.providers[0],
    node_id: workspace.hosts[0].node_id,
    name: workspace.hosts[0].name,
    capabilities: { ...catalog.providers[0].capabilities, consume: true },
  });
}
let networkState = workspace.instances[0] ? nicState(workspace.instances[0], workspace.redacted) : null;
Object.defineProperty(window, '__cvmMutations', { value: mutations });
globalThis.fetch = async (input, init) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const method = init?.method ?? 'GET';
  if (scene.startsWith('staged') && url.pathname === '/revisions') {
    const value = await (await fallback(input, init)).json();
    return Response.json({ ...value, current_revision: 42 });
  }
  if (scene.startsWith('staged') && url.pathname === '/artifacts/index')
    return Response.json({ revision: Number(url.searchParams.get('revision') ?? 42), artifacts: [] });
  if (scene.startsWith('staged') && url.pathname === '/deployments/verify')
    return Response.json({ summary: { changed_targets: 1 } });
  if (url.pathname === '/deployments/plan' && method === 'POST') {
    const revision = Number(JSON.parse(String(init?.body)).revision_id);
    const plan: DeploymentPlan = {
      revision,
      base_revision_id: revision - 1,
      targets: [
        { node_id: workspace.hosts[0].node_id, status: 'pending', wave: 1, disruptive: false, actions: ['sync-cvm'] },
      ],
      summary: {
        total_targets: 1,
        changed_targets: 1,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 1,
      },
      warnings: [],
    };
    return Response.json(plan);
  }
  if (url.pathname === '/cvm/chain-egress' && method === 'GET') {
    const value = chainCatalog(workspace, url.searchParams.get('instance_id'));
    if (params.get('chain') === 'unavailable')
      return Response.json({
        ...value,
        supported: false,
        reason: '此宿主尚不支持线路出口，请管理员检查并更新宿主组件。',
      });
    return Response.json(
      scene.startsWith('chain-')
        ? value
        : {
            ...value,
            supported: false,
            reason: '宿主尚未提供线路出口能力',
            chains: [],
            hidden_unauthorized: 0,
            traffic: [],
          },
    );
  }
  if (url.pathname.endsWith('/artifacts/preview') && method === 'POST')
    return Response.json({ instance: workspace.instances[0], artifacts: artifactHistory(), no_op: false });

  if (url.pathname.endsWith('/artifacts'))
    return Response.json(
      scene === 'artifacts-legacy'
        ? { generation: 1, current: null, previous: null }
        : { ...artifactHistory(), generation: workspace.instances[0].desired.generation },
    );
  if (url.pathname === '/cvm' && method === 'GET') return Response.json(workspace);
  if (url.pathname === '/cvm/network-logs' && method === 'GET')
    return Response.json(networkLogs(url.searchParams.get('nodes')?.split(',')[0]));
  if (url.pathname === '/cvm/entries' && method === 'GET') return Response.json(entries);
  if (url.pathname.startsWith('/cvm/') && url.pathname.endsWith('/entries') && method === 'GET') {
    const id = decodeURIComponent(url.pathname.split('/')[2]);
    return Response.json(entries.entries.filter(entry => entry.instance_id === id));
  }
  if (url.pathname.startsWith('/cvm/entries')) {
    const body = JSON.parse(String(init?.body));
    mutations.push({ method, path: url.pathname, ...body });
    if (method === 'POST') {
      const entry = {
        id: `entry-new-${entries.entries.length}`,
        revision: 1,
        publication_id: body.publication_id,
        provider_node: entries.publications.find(p => p.id === body.publication_id)?.spec.provider_node ?? '',
        instance_id: body.instance_id,
        address: body.address,
        visible_address: body.address,
        mode: body.mode,
        remote: true,
        state: 'pending' as const,
        error: null,
      };
      entries.entries.push(entry);
      return Response.json(entry, { status: 201 });
    }
    const entry = entries.entries.find(item => url.pathname.split('/')[3] === item.id);
    if (!entry) return Response.json({ error: '入站方式不存在' }, { status: 404 });
    entry.revision += 1;
    if (method === 'PUT') entry.instance_id = body.instance_id;
    if (method === 'DELETE') entry.state = 'releasing';
    return Response.json(entry);
  }
  if (url.pathname === '/cvm/readings') return Response.json(cvmReadings(workspace, scene));
  if (url.pathname === '/cvm/networks' && method === 'GET') return Response.json(catalog);
  if (url.pathname.startsWith('/cvm/networks') && method !== 'GET') {
    const body = JSON.parse(String(init?.body));
    mutations.push(body);
    if (body.spec) {
      catalog.publications[0] = { id: 'network-hk', revision: 3, admitting: true, retired: false, spec: body.spec };
      return Response.json(catalog.publications[0]);
    }
    return Response.json(catalog.publications[0]);
  }
  if (url.pathname.endsWith('/network') && method === 'GET') return Response.json(networkState);
  if (url.pathname.endsWith('/network-edit') && method === 'PUT') {
    const id = decodeURIComponent(url.pathname.split('/')[2]);
    const current = workspace.instances.find(item => item.desired.id === id);
    const body = JSON.parse(String(init?.body)) as CvmNetworkEditRequest;
    const preview = url.searchParams.has('preview');
    if (!current) return Response.json({ error: '容器不存在' }, { status: 404 });
    if (body.expected_generation !== current.desired.generation)
      return Response.json({ error: '容器已变化，请刷新后操作' }, { status: 409 });
    const item = structuredClone(current);
    const mounted = Object.fromEntries(
      (body.mounts ?? []).map((mount, index) => [mount.draft_id, `nic-added-${index}`]),
    );
    for (const mount of body.mounts ?? []) {
      const id = mounted[mount.draft_id];
      const allocated = allocatedNic(id);
      allocated.publication_id = mount.publication_id;
      allocated.transport = mount.transport;
      item.desired.spec.interfaces.push({
        id,
        source_id: mount.publication_id,
        purpose: 'business',
        default_ipv4: false,
        default_ipv6: false,
        allocated,
      });
    }
    item.desired.spec.interfaces = item.desired.spec.interfaces.filter(
      nic => !body.detach_interface_ids?.includes(nic.id),
    );
    if (body.routes) {
      const applyMounted = <T extends { interface_id?: string }>(value: T): T =>
        value.interface_id && mounted[value.interface_id]
          ? { ...value, interface_id: mounted[value.interface_id] }
          : value;
      item.desired.spec.routing = {
        ...body.routes.routes,
        revision: body.routes.expected_revision + 1,
        routes: body.routes.routes.routes.map(applyMounted),
        priorities: body.routes.routes.priorities.map(applyMounted),
      };
    }
    const changesDevice = !!(body.mounts?.length || body.detach_interface_ids?.length || body.routes);
    if (changesDevice) {
      item.desired.generation++;
      item.status = 'pending';
    }
    item.revision_id = 42;
    item.published_revision_id ??= 41;
    item.operation_kind = changesDevice ? 'network_edit' : 'entries';
    if (!preview) {
      mutations.push({
        method,
        path: url.pathname,
        ...body,
        ...(body.routes
          ? {
              expected_revision: body.routes.expected_revision,
              routes: body.routes.routes,
            }
          : {}),
      });
      for (const entry of body.entry_additions ?? []) {
        const publication = entries.publications.find(p => p.id === entry.publication_id);
        entries.entries.push({
          ...entry,
          id: `entry-new-${entries.entries.length}`,
          revision: 1,
          provider_node: publication?.spec.provider_node ?? '',
          instance_id: id,
          visible_address:
            publication?.spec.addresses.find(o => o.address === entry.address)?.public_address ?? entry.address,
          state: 'pending',
          error: null,
          remote: true,
        });
      }
      for (const release of body.entry_releases ?? []) {
        const entry = entries.entries.find(entry => entry.id === release.id);
        if (entry) entry.state = 'releasing';
      }
      Object.assign(current, item);
      workspace.pending_revision_id = 42;
      networkState = nicState(item);
    }
    return Response.json({ instance: item, mounted_interfaces: mounted, input_sha256: 'a'.repeat(64) });
  }
  if (url.pathname.endsWith('/pending') && method === 'GET') {
    const item = workspace.instances.find(item => url.pathname === `/cvm/${item.desired.id}/pending`);
    if (!item?.revision_id) return Response.json({ error: '容器没有待发布变更' }, { status: 409 });
    return Response.json({
      revision: item.revision_id,
      author: 'admin',
      saved_at: item.updated_at,
      fields: item.operation_kind === 'network_edit' ? ['interfaces', 'routes'] : ['resources'],
      entries_added: Number(item.operation_kind === 'entries'),
      entries_removed: 0,
      nics_added: 0,
      nics_removed: 0,
    });
  }
  if (url.pathname.endsWith('/routes') && method === 'PUT') {
    const body = JSON.parse(String(init?.body));
    mutations.push(body);
    const item = workspace.instances[0];
    item.desired.spec.routing = { ...body.routes, revision: body.expected_revision + 1 };
    item.desired.generation++;
    return Response.json(item);
  }
  if (url.pathname.endsWith('/interfaces') && method === 'POST') {
    const body = JSON.parse(String(init?.body));
    mutations.push(body);
    const item = workspace.instances[0];
    const allocated = allocatedNic('nic-added');
    allocated.ipv4 = '10.60.0.3';
    allocated.ipv6 = 'fd60::3';
    allocated.mac = '02:00:00:00:00:03';
    item.desired.spec.interfaces.push({
      id: 'nic-added',
      source_id: body.publication_id,
      purpose: 'business',
      default_ipv4: false,
      default_ipv6: false,
      allocated,
    });
    item.desired.generation++;
    item.status = 'pending';
    item.revision_id = 42;
    item.published_revision_id = 41;
    workspace.pending_revision_id = 42;
    networkState = nicState(item);
    return Response.json(item, { status: 202 });
  }
  if (url.pathname.includes('/interfaces/') && method === 'PUT') {
    const body = JSON.parse(String(init?.body));
    mutations.push(body);
    return Response.json(workspace.instances[0]);
  }
  if (url.pathname.startsWith('/cvm/hosts/') && url.pathname.endsWith('/terminal') && method === 'PUT') {
    const request = JSON.parse(String(init?.body));
    mutations.push(request);
    const host = workspace.hosts.find(host => host.node_id === decodeURIComponent(url.pathname.split('/')[3]));
    if (!host?.can_manage_terminal) return Response.json({ error: '无权管理 WebShell' }, { status: 403 });
    if (request.expected_revision !== host.terminal_revision)
      return Response.json({ error: '设置已更新' }, { status: 409 });
    host.terminal_enabled = request.enabled;
    host.terminal_revision = host.terminal_revision + 1;
    return Response.json(workspace);
  }
  if (/^\/cvm\/hosts\/[^/]+$/.test(url.pathname) && method === 'PUT') {
    const request = JSON.parse(String(init?.body));
    mutations.push(request);
    const node = decodeURIComponent(url.pathname.split('/')[3]);
    const host = [...workspace.hosts, ...(workspace.available_hosts ?? [])].find(item => item.node_id === node);
    if (!host?.can_manage_host) return Response.json({ error: '无权管理宿主' }, { status: 403 });
    if (request.expected_revision !== host.host_revision)
      return Response.json({ error: '宿主身份已更新' }, { status: 409 });
    if (!request.enabled && workspace.instances.some(item => item.node_id === node && item.reserved))
      return Response.json({ error: '请先删除该宿主上的实例，并等待系统盘和网络资源清理完成' }, { status: 409 });
    host.host_revision += 1;
    workspace.hosts = [...workspace.hosts.filter(item => item.node_id !== node), ...(request.enabled ? [host] : [])];
    workspace.available_hosts = [
      ...(workspace.available_hosts ?? []).filter(item => item.node_id !== node),
      ...(!request.enabled ? [host] : []),
    ];
    return Response.json(workspace);
  }
  if (url.pathname.endsWith('/terminal') && method === 'POST') {
    mutations.push(JSON.parse(String(init?.body ?? '{}')));
    return Response.json({
      session_id: 'a'.repeat(64),
      ticket: 'fixture-once',
      expires_in_seconds: 30,
      max_sessions_per_node: 3,
      history_bytes: 4 * 1024 ** 2,
      latency_supported: params.get('latency') === '1',
      webrtc: { supported: params.get('rtc') === '1', stun_servers: ['stun:stun.cloudflare.com:3478'] },
    });
  }
  if (url.pathname.endsWith('/credential'))
    return Response.json({ instance_id: workspace.instances[0].desired.id, revision: 1, password: 'F'.repeat(32) });
  if (url.pathname.endsWith('/tcp-pings')) return Response.json([]);
  if (url.pathname.endsWith('/monitoring')) {
    const { cvmMonitoring } = await import('./data');
    return Response.json(cvmMonitoring(scene));
  }
  if (url.pathname === '/cvm' && method === 'POST') {
    const body = JSON.parse(String(init?.body)) as CvmCreate;
    mutations.push(body);
    // Server acceptance is pending; the fixture never fabricates a runtime report.
    const { cvmInstance } = await import('./data');
    const item = cvmInstance('cvm-created');
    item.tenant_id = body.tenant_id;
    item.user_id = body.user_id;
    item.desired.spec.name = body.name;
    item.desired.spec.resources = body.resources;
    item.desired.spec.interfaces = body.interfaces.map((nic, i) => ({ ...nic, id: `nic-new-${i}` }));
    item.status = 'pending';
    item.revision_id = 42;
    item.published_revision_id = null;
    item.operation_kind = 'create';
    workspace.pending_revision_id = 42;
    item.report = null;
    workspace.instances.push(item);
    return Response.json(item, { status: 202 });
  }
  if (url.pathname.startsWith('/cvm/') && method === 'POST') {
    const body = JSON.parse(String(init?.body)) as CvmChange;
    mutations.push(body);
    const item = workspace.instances[0];
    item.status = 'pending';
    item.revision_id = (item.revision_id ?? 2) + 1;
    item.published_revision_id ??= 1;
    item.operation_kind = body.action;
    item.desired.generation += 1;
    if (body.action === 'undo_pending') {
      item.desired = { ...structuredClone(initialInstance.desired), generation: item.desired.generation };
    }
    if (body.action === 'delete') {
      item.desired.power = 'deleted';
      item.desired.delete_data = body.delete_data;
      item.desired.spec.routing.routes = [];
      item.desired.spec.routing.priorities = [];
      item.desired.spec.routing.revision += 1;
    }
    if (body.action === 'detach')
      item.desired.spec.interfaces = item.desired.spec.interfaces.filter(nic => nic.id !== body.interface_id);
    return Response.json(item, { status: 202 });
  }

  const response = await fallback(input, init);
  if (url.pathname === '/bootstrap') {
    const value = await response.json();
    value.who.role = role === 'public' ? 'readonly' : role;
    if (role === 'public') value.who.operator_id = 'public';
    value.who.masked_assets = role === 'readonly' || role === 'public';
    return Response.json(value);
  }
  return response;
};
location.hash =
  scene === 'new' || scene === 'new-copy' || scene === 'experimental'
    ? '#/cvm/new'
    : [
          'detail',
          'staged-entry',
          'chain-detail',
          'artifacts',
          'artifacts-mismatch',
          'artifacts-legacy',
          'artifacts-pending',
          'baremetal-detail',
          'pending',
          'unknown',
          'stale',
          'stopped',
          'connections',
          'ssh-addresses',
          'network-unconfigured',
          'shared-l4',
          'cross-host',
        ].includes(scene)
      ? '#/cvm/instance/cvm-alice'
      : '#/cvm';
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <App />
  </QueryClientProvider>,
);
