import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { ManagedNetworking, ContainerAccessPanel } from '../../src/panes/cvm/networking';
import { TransportDialog } from '../../src/panes/cvm/network-dialogs';
import { useNetworkDraft } from '../../src/panes/cvm/network-draft';
import { cvmInstance, cvmWorkspace } from '../fixtures/cvm/data';
import { nicCatalog, nicState } from '../fixtures/cvm/network-data';
import { chainCatalog } from '../fixtures/cvm/chain-data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import type { CvmEntry } from '../../src/api';
import { networkProviderName } from '../../src/panes/cvm/network-names';

it('uses scoped display names without capabilities and preserves old Console fallbacks', () => {
  const catalog = nicCatalog(false);
  expect(catalog.providers).toEqual([]);
  expect(networkProviderName(catalog, 'remote-ingress')).toBe('GreenVPS · 香港网络');
  const legacy = nicCatalog();
  delete legacy.provider_names;
  expect(networkProviderName(legacy, 'remote-ingress')).toBe('GreenVPS · 香港网络');
  expect(networkProviderName(legacy, 'hk-hkix', cvmWorkspace('list').hosts)).toBe('香港 HKIX');
  expect(networkProviderName(catalog, 'toString')).toBe('toString');
});

let entries: CvmEntry[] = [];
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(c => c.clear());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
function mount(children: ReactNode, status = 503) {
  const writes: { url: string; body: Record<string, unknown>; inputSha: string | null }[] = [];
  const instance = cvmInstance();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const url = String(input);
      if (url.startsWith('/cvm/chain-egress')) return Response.json(chainCatalog(cvmWorkspace(), instance.desired.id));
      if (init?.method && init.method !== 'GET') {
        writes.push({
          url,
          body: JSON.parse(String(init.body)),
          inputSha: new Headers(init.headers).get('x-brocade-cvm-input-sha256'),
        });
        return Response.json({ error: '服务端设置已变化，请核查后重试' }, { status });
      }
      if (url === '/cvm/networks') return Response.json(nicCatalog());
      if (url.endsWith('/network')) return Response.json(nicState(instance));
      if (url === '/cvm/entries') return Response.json(entryBoard(cvmWorkspace(), 'list'));
      if (url.endsWith('/entries')) return Response.json(entries);
      throw Error('unexpected ' + url);
    }),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const view = render(<QueryClientProvider client={client}>{children}</QueryClientProvider>);
  return { writes, client, ...view };
}
function Network({ workspace }: { workspace: ReturnType<typeof cvmWorkspace> }) {
  const instance = workspace.instances[0];
  const draft = useNetworkDraft(instance, workspace.can_write);
  return <ManagedNetworking instance={instance} workspace={workspace} draft={draft} />;
}

it('stages unmounting only after routes and priorities are cleared, without writing to the server', async () => {
  const { writes } = mount(<Network workspace={cvmWorkspace('detail')} />);
  await screen.findByTitle('两端已连接');
  const editor = within(screen.getByRole('region', { name: '路由' }));
  const actions = screen.getByRole('button', { name: /出站方式操作$/ });
  fireEvent.click(actions);
  expect(screen.getByRole<HTMLButtonElement>('menuitem', { name: /^移除出站方式/ }).disabled).toBe(true);
  fireEvent.keyDown(screen.getByRole('menu'), { key: 'Escape' });
  await waitFor(() => expect(actions.getAttribute('aria-expanded')).toBe('false'));
  expect(editor.getAllByRole('textbox', { name: '目标 IP／网段' })).toHaveLength(1);
  fireEvent.click(editor.getByRole('button', { name: /香港 · 公网出口 · IPv4/ }));
  fireEvent.click(editor.getByRole('button', { name: '移除此出口的路由与优先级' }));
  await waitFor(() =>
    expect(within(editor.getByRole('table', { name: '目标路由' })).getAllByRole('row')).toHaveLength(3),
  );
  expect(editor.queryAllByRole('spinbutton', { name: /优先级/ })).toHaveLength(0);
  fireEvent.click(screen.getByRole('button', { name: /出站方式操作$/ }));
  const detach = await screen.findByRole<HTMLButtonElement>('menuitem', { name: /^移除出站方式/ });
  expect(detach.disabled).toBe(false);
  fireEvent.click(detach);
  expect(await screen.findByText('待移除')).toBeTruthy();
  expect(writes).toHaveLength(0);
});
it('changes transport without asking for another IP or route allocation', async () => {
  const instance = cvmInstance();
  const { writes } = mount(
    <TransportDialog
      instance={instance}
      nic={instance.desired.spec.interfaces[1]}
      state={nicState(instance)}
      onClose={() => {}}
    />,
  );
  fireEvent.change(screen.getByRole('combobox', { name: '连接方式' }), { target: { value: 'xray' } });
  fireEvent.click(screen.getByRole('button', { name: '保存变更' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ transport: 'xray', expected_generation: 1 });
  expect(writes[0].body).not.toHaveProperty('ipv4');
});
it('recommends only active entries to SSH and keeps the management address as an explicit choice', async () => {
  const instance = cvmInstance();
  entries = entryBoard(cvmWorkspace('detail'), 'detail').entries;
  // A pending SSH entry is not offered; the UDP entry never is.
  entries.push({
    ...entries[0],
    id: 'entry-pending',
    state: 'pending',
    mode: {
      kind: 'l4_range',
      protocol: 'tcp',
      listen_start: 30023,
      listen_end: 30023,
      target_start: 22,
      target_end: 22,
    },
  });
  mount(<ContainerAccessPanel instance={instance} redacted={false} />);
  const panel = await screen.findByRole('region', { name: 'SSH 登录' });
  await waitFor(() => expect(panel.textContent).toContain('ssh -l cube -p 30022 -- 203.0.113.20'));
  const options = within(panel)
    .getAllByRole('option')
    .map(option => option.textContent);
  expect(options).toEqual([
    'TCP 入站 · 203.0.113.20:30022',
    '整 IP 入站 · 198.51.100.30:22',
    '宿主内管理地址 · 192.168.0.21:22',
  ]);
  expect(panel.textContent).not.toContain('30023');
  entries = [];
});
it('redacts addresses and removes network writes for a read-only viewer', async () => {
  const workspace = cvmWorkspace('detail', 'readonly');
  mount(<Network workspace={workspace} />);
  expect(screen.getByText('当前权限不显示目标网段、出口和 DNS 等路由详情。')).toBeTruthy();
  expect(screen.queryByRole('button', { name: '更换连接方式' })).toBeNull();
  expect(screen.queryByText('203.0.113.20')).toBeNull();
});

it('bulk compatible updates keep one stable request per container across an uncertain response', async () => {
  const { BulkNicVersionDialog } = await import('../../src/panes/cvm/network-bulk');
  const instance = cvmInstance();
  const catalog = nicCatalog(true, instance);
  const { writes } = mount(
    <BulkNicVersionDialog publication={catalog.publications[0]} attachments={catalog.attachments} onClose={() => {}} />,
  );
  fireEvent.click(screen.getByRole('checkbox'));
  const save = screen.getByRole('button', { name: '保存变更' });
  fireEvent.click(save);
  await screen.findByText(/服务端设置已变化/);
  fireEvent.click(save);
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[0]).toEqual(writes[1]);
  expect(writes[0].url).toBe('/cvm/cvm-alice/interface-versions');
  expect(writes[0].body).toMatchObject({
    expected_generation: 1,
    updates: [{ interface_id: 'nic-business', publication_revision: 2, reallocate: false }],
  });
});
