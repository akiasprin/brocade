import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmCreate, Whoami } from '../../src/api';
import { CreateForm } from '../../src/panes/cvm/create';
import { SessionProvider } from '../../src/session';
import { cvmInstance, cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import { nicCatalog } from '../fixtures/cvm/network-data';

const who: Whoami = {
  operator_id: 'admin',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
});

function mount({
  initialCopy,
  initialHost,
  failOnce = false,
}: { initialCopy?: string; initialHost?: string; failOnce?: boolean } = {}) {
  const workspace = cvmWorkspace();
  const [source] = workspace.instances;
  const GiB = 1024 ** 3;
  source.desired.spec.resources = { vcpus: 2, memory_bytes: GiB, root_disk_bytes: 6 * GiB };
  const wrongTenant = { ...cvmInstance('foreign'), tenant_id: 'another' };
  wrongTenant.desired.spec.name = 'another-tenant-container';
  const deleted = cvmInstance('deleted');
  deleted.desired.power = 'deleted';
  deleted.desired.spec.name = 'deleted-container';
  deleted.reserved = false;
  workspace.instances.push(wrongTenant, deleted);
  workspace.tenants?.push({ tenant_id: 'another', name: '另一个租户' });
  const host = workspace.hosts[0];
  host.capabilities.primary_ip_delivery = true;
  const incompatible = structuredClone(host);
  incompatible.node_id = 'other-host';
  incompatible.name = '不同镜像的宿主';
  incompatible.capabilities.images[0].sha256 = 'b'.repeat(64);
  const stale = { ...host, node_id: 'stale-host', name: '过期宿主', fresh: false };
  workspace.hosts.push(incompatible, stale);
  const catalog = nicCatalog(true, source);
  catalog.providers.push({
    ...catalog.providers[0],
    node_id: host.node_id,
    name: host.name,
    capabilities: { ...catalog.providers[0].capabilities, consume: true },
  });
  const requests: CvmCreate[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const path = String(input);
      if (path === '/cvm/networks') return Response.json(catalog);
      if (path === '/cvm/entries') return Response.json(entryBoard(workspace, 'list'));
      if (path === '/cvm' && init?.method === 'POST') {
        requests.push(JSON.parse(String(init.body)) as CvmCreate);
        if (failOnce && requests.length === 1) return Response.json({ error: '暂未确认结果' }, { status: 503 });
        return Response.json(cvmInstance('new-id'));
      }
      throw new Error(`Unexpected request ${path}`);
    }),
  );
  const onCreated = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: 1, chain_group_count: [] } }}>
        <CreateForm workspace={workspace} initialCopy={initialCopy} initialHost={initialHost} onCreated={onCreated} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  return { requests, onCreated, source };
}

it('copies tenant-eligible specs and publication intent without copying assignment, credentials or addresses', async () => {
  const { requests, onCreated, source } = mount({ initialHost: 'other-host' });
  const copy = screen.getByLabelText('复制现有容器');
  expect(within(copy).queryByRole('option', { name: 'another-tenant-container' })).toBeNull();
  expect(within(copy).queryByRole('option', { name: 'deleted-container' })).toBeNull();
  fireEvent.change(copy, { target: { value: source.desired.id } });
  expect((screen.getByLabelText('容器名称') as HTMLInputElement).value).toBe('home-workspace-copy');
  expect((screen.getByLabelText('使用者') as HTMLSelectElement).value).toBe('');
  expect((screen.getByRole('button', { name: /不同镜像的宿主/ }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole('button', { name: /过期宿主/ }) as HTMLButtonElement).disabled).toBe(true);
  const allocation = screen.getByRole('region', { name: '创建后的宿主分配' });
  expect(within(allocation).getByText('5 核 / 8 核')).toBeTruthy();
  expect(within(allocation).getByText('2.5 GiB / 8 GiB')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  expect(requests).toHaveLength(0);
  expect(screen.getByRole('alert').textContent).toMatch(/选择获授权的使用者/);

  fireEvent.change(screen.getByLabelText('使用者'), { target: { value: 'alice' } });
  const entry = screen.getByLabelText('访问入口地址');
  const shared = await within(entry).findByRole('option', { name: /香港共享入口/ });
  expect(within(entry).queryByRole('option', { name: /198\.51\.100\.30/ })).toBeNull();
  expect(within(entry).getByRole('option', { name: /198\.51\.100\.31/ })).toBeTruthy();
  expect((entry as HTMLSelectElement).value).toBe('');
  fireEvent.change(entry, { target: { value: (shared as HTMLOptionElement).value } });
  const listen = screen.getByLabelText('TCP 入口端口');
  expect((listen as HTMLInputElement).value).toBe('');
  fireEvent.change(listen, { target: { value: '30022' } });
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  expect(requests).toHaveLength(0);
  expect(screen.getByRole('alert').textContent).toMatch(/端口已占用/);

  fireEvent.change(listen, { target: { value: '30054-30055' } });
  fireEvent.change(screen.getByLabelText('TCP 端口映射（可选）'), { target: { value: '30054-30055 → 8080-8081' } });
  fireEvent.click(screen.getByRole('checkbox', { name: 'UDP' }));
  fireEvent.change(screen.getByLabelText('UDP 入口端口'), { target: { value: '30054-30055' } });
  fireEvent.change(screen.getByLabelText('UDP 端口映射（可选）'), { target: { value: '30054-30055 → 8080-8081' } });
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1));
  expect(requests).toEqual([
    {
      request_id: expect.any(String),
      tenant_id: 'platform',
      user_id: 'alice',
      node_id: 'hk-hkix',
      name: 'home-workspace-copy',
      image_id: source.desired.spec.image?.id,
      resources: source.desired.spec.resources,
      interfaces: [{ source_id: 'public', purpose: 'contact', default_ipv4: false, default_ipv6: false }],
    },
  ]);
  expect(onCreated).toHaveBeenCalledWith('new-id', {
    nics: [{ publication_id: 'network-hk', transport: 'wireguard' }],
    entry: {
      publication_id: 'entrypub-hk',
      address: '203.0.113.20',
      mode: {
        kind: 'protocols',
        tcp: {
          kind: 'forward',
          ports: { kind: 'ranges', ranges: [{ start: 30054, end: 30055 }] },
          mappings: [{ listen: { start: 30054, end: 30055 }, target: { start: 8080, end: 8081 } }],
        },
        udp: {
          kind: 'forward',
          ports: { kind: 'ranges', ranges: [{ start: 30054, end: 30055 }] },
          mappings: [{ listen: { start: 30054, end: 30055 }, target: { start: 8080, end: 8081 } }],
        },
        icmp: false,
        other_ip: false,
      },
    },
  });
});

it('preserves the exact create request and network handoff on an uncertain retry', async () => {
  const { requests, onCreated } = mount({ initialCopy: 'cvm-alice', failOnce: true });
  fireEvent.change(screen.getByLabelText('使用者'), { target: { value: 'alice' } });
  await screen.findByRole('option', { name: /香港共享入口/ });
  await screen.findByRole('option', { name: /公网出口 · WireGuard/ });
  fireEvent.click(screen.getByRole('button', { name: '保存创建变更' }));
  const retry = await screen.findByRole('button', { name: '重试保存创建变更' });
  expect((screen.getByLabelText('容器名称') as HTMLInputElement).matches(':disabled')).toBe(true);
  expect(onCreated).not.toHaveBeenCalled();
  fireEvent.click(retry);
  await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1));
  expect(requests).toHaveLength(2);
  expect(requests[0]).toEqual(requests[1]);
  expect(onCreated).toHaveBeenCalledWith('new-id', {
    nics: [{ publication_id: 'network-hk', transport: 'wireguard' }],
  });
});
