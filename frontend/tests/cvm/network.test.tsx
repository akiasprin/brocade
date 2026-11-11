import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { ManagedNetworking, ContainerAccessPanel } from '../../src/panes/cvm/networking';
import { MountNicDialog, RoutesDialog, TransportDialog } from '../../src/panes/cvm/network-dialogs';
import { cvmInstance, cvmWorkspace } from '../fixtures/cvm/data';
import { nicCatalog, nicState } from '../fixtures/cvm/network-data';
import { entryBoard } from '../fixtures/cvm/entry-data';
import type { CvmEntry } from '../../src/api';

let entries: CvmEntry[] = [];
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(c => c.clear());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
function mount(children: ReactNode, status = 503) {
  const writes: { url: string; body: Record<string, unknown> }[] = [];
  const instance = cvmInstance();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init) => {
      const url = String(input);
      if (init?.method && init.method !== 'GET') {
        writes.push({ url, body: JSON.parse(String(init.body)) });
        return Response.json({ error: '服务端设置已变化，请核查后重试' }, { status });
      }
      if (url === '/cvm/networks') return Response.json(nicCatalog());
      if (url.endsWith('/network')) return Response.json(nicState(instance));
      if (url.endsWith('/entries')) return Response.json(entries);
      throw Error('unexpected ' + url);
    }),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const view = render(<QueryClientProvider client={client}>{children}</QueryClientProvider>);
  return { writes, client, ...view };
}
it('mounts a reusable publication using the real intersection and retains request identity after uncertainty', async () => {
  const instance = cvmInstance();
  const { writes } = mount(
    <MountNicDialog instance={instance} catalog={nicCatalog()} state={nicState(instance)} onClose={() => {}} />,
  );
  expect(screen.getByRole('combobox', { name: 'IP 承载' }).querySelectorAll('option')).toHaveLength(2);
  const dns = screen.getByRole('checkbox');
  expect((dns as HTMLInputElement).checked).toBe(false);
  fireEvent.click(dns);
  const save = screen.getByRole('button', { name: '挂载并分配地址' });
  fireEvent.click(save);
  await screen.findByText(/服务端设置已变化/);
  fireEvent.click(save);
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[0]).toEqual(writes[1]);
  expect(writes[0].url).toBe('/cvm/cvm-alice/interfaces');
  expect(writes[0].body).toMatchObject({
    publication_id: 'network-hk',
    expected_generation: 1,
    use_dns_suggestions: true,
  });
});
it('does not invent a transport when the machines have no common implementation', () => {
  const instance = cvmInstance();
  const state = nicState(instance);
  state.mount_options['network-hk'] = { transports: [], error: '双方没有共同承载' };
  mount(<MountNicDialog instance={instance} catalog={nicCatalog()} state={state} onClose={() => {}} />);
  expect((screen.getByRole('button', { name: '挂载并分配地址' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(screen.getByRole('combobox', { name: '网络' }), { target: { value: 'network-hk' } });
  expect(screen.getByText('双方没有共同承载')).toBeTruthy();
});
it('keeps edited routes after a conflict and saves the routing object independently of NICs', async () => {
  const instance = cvmInstance();
  const { writes } = mount(<RoutesDialog instance={instance} catalog={nicCatalog()} onClose={() => {}} />, 409);
  const priority = screen.getByRole('spinbutton', { name: 'IPv4 优先级' });
  fireEvent.change(priority, { target: { value: '7' } });
  fireEvent.click(screen.getByRole('button', { name: '保存' }));
  await screen.findByText(/服务端设置已变化/);
  expect((priority as HTMLInputElement).value).toBe('7');
  expect(writes[0].body).toMatchObject({
    expected_generation: 1,
    expected_revision: 1,
    routes: { priorities: [{ interface_id: 'nic-business', ipv4: 7 }] },
  });
  expect(writes[0].body).not.toHaveProperty('interfaces');
});
it('requires all route and priority references to be removed before unmounting', async () => {
  const workspace = cvmWorkspace('detail');
  const detach = vi.fn();
  const instance = workspace.instances[0];
  mount(<ManagedNetworking instance={instance} workspace={workspace} writable onDetach={detach} />);
  await screen.findByText('两端已连接');
  const button = screen.getByRole('button', { name: '卸载' });
  expect((button as HTMLButtonElement).disabled).toBe(true);
  expect(button.title).toContain('路由表');
  expect(detach).not.toHaveBeenCalled();
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
  fireEvent.change(screen.getByRole('combobox', { name: '承载类型' }), { target: { value: 'xray' } });
  fireEvent.click(screen.getByRole('button', { name: '更换承载' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].body).toMatchObject({ transport: 'xray', expected_generation: 1 });
  expect(writes[0].body).not.toHaveProperty('ipv4');
});
it('recommends only active entries to SSH and keeps the management address as an explicit choice', async () => {
  const instance = cvmInstance();
  entries = entryBoard(cvmWorkspace('detail'), 'detail').entries;
  // A pending SSH entry is not offered; the UDP entry never is.
  entries.push({ ...entries[0], id: 'entry-pending', state: 'pending', mode: { kind: 'l4', protocol: 'tcp', listen_port: 30023, target_port: 22 } });
  mount(<ContainerAccessPanel instance={instance} redacted={false} />);
  const panel = await screen.findByRole('region', { name: 'SSH 登录' });
  await waitFor(() => expect(panel.textContent).toContain('ssh -l cube -p 30022 -- 203.0.113.20'));
  const options = within(panel).getAllByRole('option').map(option => option.textContent);
  expect(options).toEqual(['TCP 入口 · 203.0.113.20:30022', '整 IP 入口 · 198.51.100.30:22', '宿主内管理地址 · 192.168.0.21:22']);
  expect(panel.textContent).not.toContain('30023');
  entries = [];
});
it('redacts addresses and removes network writes for a read-only viewer', async () => {
  const workspace = cvmWorkspace('detail', 'readonly');
  mount(
    <ManagedNetworking instance={workspace.instances[0]} workspace={workspace} writable={false} onDetach={() => {}} />,
  );
  await screen.findByText('两端已连接');
  expect(screen.queryByRole('button', { name: '更换承载' })).toBeNull();
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
  const save = screen.getByRole('button', { name: '应用所选兼容规格' });
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
