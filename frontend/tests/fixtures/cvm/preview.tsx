// The real application and CVM pane, using a synthetic API transport.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../../src/app';
import { theme } from '../../../src/forge/theme';
import { installBackend } from '../console-backend';
import { cvmReadings, cvmWorkspace } from './data';
import { allocatedNic, nicCatalog, nicState } from './network-data';
import { entryBoard } from './entry-data';
import type { CvmChange, CvmCreate, CvmNetworkQuotaLimits } from '../../../src/api';
import '../../../src/styles.css';

const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'list';
const role = params.get('role') ?? 'system-admin';
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
installBackend();
const fallback = globalThis.fetch;
const workspace = cvmWorkspace(scene, role);
if (scene.startsWith('quota-network')) {
  workspace.hosts[0].capabilities.network_quota = {
    physical_memory_bytes: 958 * 1024 ** 2,
    host_reserved_memory_bytes: 448 * 1024 ** 2,
    contract: 'cubevs-user-quota-v1',
    total_states: 131072,
    system_reserved_states: 4096,
    memory_budget_bytes: 254 * 1024 ** 2,
    fixed_memory_bytes: 28 * 1024 ** 2,
    bytes_per_state: 1536,
    model: 'network-state-budget-v1',
  };
  workspace.network_quotas = [
    {
      tenant_id: 'platform',
      user_id: 'alice',
      node_id: 'hk-hkix',
      id: '1122334455667788',
      revision: 2,
      limits: { max_states: 80000, new_per_second: 1000, burst: 2000 },
      held_states: 80000,
      fresh: true,
      report: {
        id: '1122334455667788',
        revision: 2,
        applied: true,
        states: 2500,
        denied_capacity: 0,
        denied_rate: 0,
        allocation_failures: 0,
        error: null,
      },
    },
  ];
  if (scene === 'quota-network-stale') workspace.hosts[0].fresh = false;
}
if (scene === 'hosts') {
  const host = workspace.hosts[0];
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
  workspace.instances = cases.map(([name, status, power], index) => {
    const item = structuredClone(base);
    item.desired.id = `cvm-fleet-${index}`;
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
if (scene === 'board') {
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
    if (index === 1) {
      // Port-selected egress: SSH via the HK NIC, everything else stays local.
      item.desired.spec.routing.routes = [
        { destination: '0.0.0.0/0', interface_id: 'nic-business', service: { protocol: 'tcp', port_start: 22, port_end: 22 } },
      ];
    }
    if (index > 1) {
      item.desired.spec.interfaces = item.desired.spec.interfaces.filter(nic => nic.purpose === 'contact');
      item.desired.spec.routing = { revision: 1, priorities: [], routes: [], dns: [] };
    }
    return item;
  });
}
const mutations: unknown[] = [];
const entries = entryBoard(workspace, scene);
const catalog = nicCatalog(role === 'system-admin', workspace.instances[0]);
let networkState = workspace.instances[0] ? nicState(workspace.instances[0], workspace.redacted) : null;
Object.defineProperty(window, '__cvmMutations', { value: mutations });
globalThis.fetch = async (input, init) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const method = init?.method ?? 'GET';
  if (url.pathname === '/cvm/quota/network/preview' && method === 'POST') {
    const body = JSON.parse(String(init?.body)) as { limits: CvmNetworkQuotaLimits };
    const own = workspace.network_quotas?.[0];
    return Response.json({
      node_id: 'hk-hkix',
      revision: own?.revision ?? 0,
      budget_revision: 7,
      capacity: workspace.hosts[0].capabilities.network_quota,
      held_states: own?.held_states ?? 0,
      other_allocated: 20000,
      maximum: 106976,
      requested: body.limits,
      additional_states: Math.max(0, body.limits.max_states - (own?.held_states ?? 0)),
      excess_states: Math.max(0, body.limits.max_states - 106976),
      fits: body.limits.max_states <= 106976,
    });
  }
  if (url.pathname === '/cvm' && method === 'GET') return Response.json(workspace);
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
    if (!entry) return Response.json({ error: '访问入口不存在' }, { status: 404 });
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
    workspace.available_hosts = [...(workspace.available_hosts ?? []).filter(item => item.node_id !== node), ...(!request.enabled ? [host] : [])];
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
    });
  }
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
    item.desired.spec.name = body.name;
    item.desired.spec.resources = body.resources;
    item.desired.spec.interfaces = body.interfaces.map((nic, i) => ({ ...nic, id: `nic-new-${i}` }));
    item.status = 'pending';
    item.report = null;
    workspace.instances.push(item);
    return Response.json(item, { status: 202 });
  }
  if (url.pathname.startsWith('/cvm/') && method === 'POST') {
    const body = JSON.parse(String(init?.body)) as CvmChange;
    mutations.push(body);
    const item = workspace.instances[0];
    item.status = 'pending';
    item.desired.generation += 1;
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
  if (url.pathname === '/cvm/quota') {
    mutations.push(JSON.parse(String(init?.body)));
    return Response.json({ ok: true });
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
  scene === 'new' || scene === 'experimental'
    ? '#/cvm/new'
    : [
          'detail',
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
