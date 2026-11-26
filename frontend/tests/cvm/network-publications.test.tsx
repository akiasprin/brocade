import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmNicCatalog } from '../../src/api';
import { NIC_CATALOG } from '../../src/panes/cvm/network-common';
import { EntryPublications } from '../../src/panes/cvm/entry-publications';
import { NetworkPublicationEditor, NetworkPublications } from '../../src/panes/cvm/network-publications';
import { cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import { allocatedNic, nicCatalog } from '../fixtures/cvm/network-data';

let client: QueryClient;
afterEach(() => {
  cleanup();
  client?.clear();
  vi.unstubAllGlobals();
});

function fixture() {
  const workspace = cvmWorkspace('detail');
  const catalog = nicCatalog(true, workspace.instances[0]);
  return { workspace, catalog };
}

function mount(children: ReactNode, catalog: CvmNicCatalog, entries = entryBoard(cvmWorkspace(), 'board')) {
  const writes: { url: string; method: string; body: unknown }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? 'GET';
      if (method !== 'GET') {
        writes.push({ url, method, body: JSON.parse(String(init?.body)) });
        return Response.json({});
      }
      return Response.json(url === '/cvm/networks' ? catalog : entries);
    }),
  );
  client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(<QueryClientProvider client={client}>{children}</QueryClientProvider>);
  return writes;
}

it('lists entry and outbound resources in tables with actual addresses, quotas and edit navigation', async () => {
  const { workspace, catalog } = fixture();
  const entries = entryBoard(workspace, 'board');
  entries.publications[0].spec.addresses.push({
    address: '203.0.113.23',
    kind: 'shared',
    port_start: 443,
    port_end: 443,
  });
  const edit = vi.fn();
  const create = vi.fn();
  mount(
    <>
      <EntryPublications workspace={workspace} />
      <NetworkPublications workspace={workspace} onEdit={edit} onCreate={create} />
    </>,
    catalog,
    entries,
  );
  const entry = within(await screen.findByRole('table', { name: '入站方式' }));
  expect(entry.getByText('30000–30999')).toBeTruthy();
  expect(entry.getByText('端口 443')).toBeTruthy();
  expect(entry.getByText('独占地址')).toBeTruthy();
  const outbound = within(await screen.findByRole('table', { name: '出站方式' }));
  expect(outbound.getByText('100.00 GiB')).toBeTruthy();
  expect(outbound.getByText('1 个旧规格')).toBeTruthy();
  expect(outbound.getByText('alice')).toBeTruthy();
  fireEvent.click(outbound.getByRole('button', { name: '香港 · 公网出口' }));
  expect(edit).toHaveBeenCalledExactlyOnceWith('network-hk');
  fireEvent.click(screen.getByRole('button', { name: '添加出站方式' }));
  expect(create).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('shows grant impact before saving, supports adding an owner and preserves authorization-only saves', async () => {
  const { workspace, catalog } = fixture();
  workspace.owners.push({ tenant_id: 'platform', user_id: 'bob', name: 'bob' });
  catalog.providers[0].fresh = false;
  const back = vi.fn();
  const writes = mount(<NetworkPublicationEditor workspace={workspace} id="network-hk" onBack={back} />, catalog);
  await screen.findByRole('heading', { name: '香港 · 公网出口', level: 1 });
  expect(screen.queryByRole('dialog')).toBeNull();
  const grants = within(screen.getByRole('group', { name: '授权使用者' }));
  expect(grants.getByText('1 个出站方式')).toBeTruthy();
  fireEvent.click(grants.getByRole('button', { name: '撤销 alice 的授权' }));
  expect(grants.getByText(/待撤销 alice 的授权，将阻断其 1 个出站方式/)).toBeTruthy();
  expect(writes).toHaveLength(0);
  const select = grants.getByRole('combobox', { name: '选择授权使用者' });
  const option = grants.getByRole<HTMLOptionElement>('option', { name: 'bob · 平台' });
  fireEvent.change(select, { target: { value: option.value } });
  fireEvent.click(grants.getByRole('button', { name: '＋ 授权' }));
  expect(grants.getByText('0 个出站方式')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '保存为 v3' }));
  await waitFor(() => expect(back).toHaveBeenCalledTimes(1));
  expect(writes).toMatchObject([
    {
      url: '/cvm/networks/network-hk',
      method: 'PUT',
      body: {
        request_id: expect.any(String),
        expected_revision: 2,
        spec: { ...catalog.publications[0].spec, grants: [{ tenant_id: 'platform', user_id: 'bob' }] },
      },
    },
  ]);
});

it('shows all derived allocations and batches only attached old versions into one request per container', async () => {
  const { workspace } = fixture();
  const instance = workspace.instances[0];
  const base = instance.desired.spec.interfaces[1];
  instance.desired.spec.interfaces.push(
    { ...base, id: 'nic-second', allocated: allocatedNic('nic-second') },
    { ...base, id: 'nic-current', allocated: { ...allocatedNic('nic-current'), publication_revision: 2 } },
  );
  const catalog = nicCatalog(true, instance);
  catalog.attachments.push({
    ...catalog.attachments[0],
    allocation_id: 'allocation-old',
    attached: false,
    cleanup_pending: true,
  });
  const writes = mount(<NetworkPublicationEditor workspace={workspace} id="network-hk" onBack={vi.fn()} />, catalog);
  const table = within(await screen.findByRole('table', { name: '已分配出站方式' }));
  expect(table.getAllByRole('row')).toHaveLength(5);
  expect(table.getAllByText('旧规格')).toHaveLength(2);
  expect(table.getByText('最新规格')).toBeTruthy();
  expect(table.getByText('等待清理')).toBeTruthy();
  expect(table.getAllByText('10.60.0.2')).toHaveLength(3);
  expect(screen.getByRole('group', { name: '授权使用者' }).textContent).toContain('3 个出站方式');
  fireEvent.click(screen.getByRole('button', { name: '批量应用 v2 · 2 个' }));
  const bulk = within(await screen.findByRole('dialog', { name: '批量应用出站方式规格' }));
  expect(bulk.getAllByRole<HTMLInputElement>('checkbox').map(option => option.checked)).toEqual([true, true]);
  fireEvent.click(bulk.getByRole('button', { name: '保存变更' }));
  await bulk.findByText(/1 个容器已保存/);
  expect(writes).toMatchObject([
    {
      url: '/cvm/cvm-alice/interface-versions',
      method: 'PUT',
      body: {
        expected_generation: 1,
        request_id: expect.any(String),
        updates: [
          { interface_id: 'nic-business', publication_revision: 2, reallocate: false },
          { interface_id: 'nic-second', publication_revision: 2, reallocate: false },
        ],
      },
    },
  ]);
});

it('does not submit an edited old specification over a distinct revision received during editing', async () => {
  const { workspace, catalog } = fixture();
  const writes = mount(<NetworkPublicationEditor workspace={workspace} id="network-hk" onBack={vi.fn()} />, catalog);
  const name = await screen.findByRole<HTMLInputElement>('textbox', { name: '名称' });
  fireEvent.change(name, { target: { value: '本地草稿' } });
  act(() =>
    client.setQueryData(NIC_CATALOG, { ...catalog, publications: [{ ...catalog.publications[0], revision: 3 }] }),
  );
  await screen.findByText(/网络规格已更新/);
  expect(name.value).toBe('本地草稿');
  expect(screen.getByRole<HTMLButtonElement>('button', { name: '保存为 v3' }).disabled).toBe(true);
  expect(writes).toHaveLength(0);
});
