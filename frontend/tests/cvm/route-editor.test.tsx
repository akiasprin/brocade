import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { useState, type ComponentProps } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmRouteMatchRequest } from '../../src/api';
import { RouteEditor } from '../../src/panes/cvm/route-editor';
import { chainCatalog } from '../fixtures/cvm/chain-data';
import { cvmInstance, cvmWorkspace } from '../fixtures/cvm/data';

type Props = ComponentProps<typeof RouteEditor>;
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
});

function setup(
  props: Partial<Props> = {},
  match = (_body: CvmRouteMatchRequest): Promise<Response> | Response => Response.json({ kind: 'default' }),
) {
  const instance = props.instance ?? cvmInstance();
  const calls = vi.fn(async (input: string, init?: RequestInit) => {
    if (input.startsWith('/cvm/chain-egress?')) {
      return Response.json(chainCatalog({ ...cvmWorkspace(), instances: [instance] }, instance.desired.id));
    }
    if (input === '/cvm/cvm-alice/routes/match' && init?.method === 'POST') return match(JSON.parse(String(init.body)));
    throw new Error(`unexpected ${input}`);
  });
  vi.stubGlobal('fetch', calls);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const changed = vi.fn();
  function Controlled() {
    const [routing, setRouting] = useState(props.routing ?? instance.desired.spec.routing);
    return (
      <RouteEditor
        instance={instance}
        writable
        {...props}
        routing={routing}
        onChange={next => {
          changed(next);
          setRouting(next);
        }}
      />
    );
  }
  render(
    <QueryClientProvider client={client}>
      <Controlled />
    </QueryClientProvider>,
  );
  return { calls, changed, instance };
}

function editRow(destination: string) {
  fireEvent.click(screen.getByRole('button', { name: `编辑路由 ${destination}` }));
  return within(screen.getByRole('button', { name: '应用修改' }).closest('tr')!);
}

it('edits a route in place and stages target, service, and DNS without using a write API', async () => {
  const { calls, changed } = setup();
  await screen.findAllByRole('option', { name: '洛杉矶 · 三跳线路' });
  // Rows read as text until edited; the add row is the only standing form.
  expect(screen.getAllByRole('textbox', { name: '目标 IP／网段' })).toHaveLength(1);
  const row = editRow('0.0.0.0/0');
  const via = row.getByRole('combobox', { name: '经由' });
  expect(via.querySelectorAll('optgroup')).toHaveLength(2);
  expect((via.querySelector('option[value="nic-business"]') as HTMLOptionElement).text).toContain('业务网卡');
  fireEvent.change(via, { target: { value: 'chain:chain-us' } });
  fireEvent.change(row.getByRole('textbox', { name: '目标 IP／网段' }), { target: { value: '203.0.113.0/24' } });
  fireEvent.change(row.getByRole('textbox', { name: '协议端口' }), { target: { value: 'udp/400-443' } });
  fireEvent.click(row.getByRole('button', { name: '应用修改' }));
  const dns = screen.getByRole('textbox', { name: '容器 DNS' }) as HTMLInputElement;
  fireEvent.change(dns, { target: { value: '192.0.2.53, ' } });
  expect(dns.value).toBe('192.0.2.53, ');
  fireEvent.change(dns, { target: { value: '192.0.2.53, 2001:db8::53' } });
  expect(changed.mock.lastCall?.[0]).toMatchObject({
    priorities: [
      { interface_id: 'nic-business', ipv4: 10, ipv6: 10 },
      { chain_id: 'chain-us', ipv4: 20, ipv6: 20 },
    ],
    routes: [
      {
        destination: '203.0.113.0/24',
        chain_id: 'chain-us',
        service: { protocol: 'udp', port_start: 400, port_end: 443 },
      },
      { destination: '::/0' },
    ],
    dns: ['192.0.2.53', '2001:db8::53'],
  });
  expect(changed.mock.lastCall?.[0].routes[0]).not.toHaveProperty('interface_id');
  expect(calls.mock.calls.every(([, init]) => !init?.method)).toBe(true);
});

it('adds a route from the standing row and rejects malformed input', async () => {
  const { changed } = setup();
  await screen.findAllByRole('option', { name: '洛杉矶 · 三跳线路' });
  const destination = screen.getByRole('textbox', { name: '目标 IP／网段' });
  const add = screen.getByRole('button', { name: '添加路由' }) as HTMLButtonElement;
  fireEvent.change(destination, { target: { value: '10.0.0.0/33' } });
  fireEvent.change(screen.getAllByRole('combobox', { name: '经由' }).at(-1)!, { target: { value: 'nic-business' } });
  expect(add.disabled).toBe(true);
  fireEvent.change(destination, { target: { value: '10.0.0.0/8' } });
  fireEvent.change(screen.getByRole('textbox', { name: '协议端口' }), { target: { value: 'tcp/0' } });
  expect(add.disabled).toBe(true);
  fireEvent.change(screen.getByRole('textbox', { name: '协议端口' }), { target: { value: 'tcp' } });
  fireEvent.click(add);
  expect(changed.mock.lastCall?.[0].routes.at(-1)).toEqual({
    destination: '10.0.0.0/8',
    interface_id: 'nic-business',
    service: { protocol: 'tcp', port_start: 1, port_end: 65535 },
  });
});

it('reorders exits for both families with keys and drag while keeping each family’s priority slots', async () => {
  const instance = cvmInstance();
  instance.desired.spec.routing.priorities.push({ chain_id: 'chain-us', ipv4: 70, ipv6: 5 });
  const { changed } = setup({ instance });
  const line = await screen.findByRole('button', { name: '排序 线路 · 洛杉矶 · 三跳线路' });
  const nic = screen.getByRole('button', { name: /^排序 网卡/ });
  // The two families disagree, so the strip says it will unify them.
  expect(screen.getByText(/IPv4 与 IPv6 的出口顺序不同/)).toBeTruthy();
  fireEvent.keyDown(line, { key: 'ArrowDown' });
  expect(changed.mock.lastCall?.[0].priorities).toEqual([
    { interface_id: 'nic-business', ipv4: 10, ipv6: 5 },
    { chain_id: 'chain-us', ipv4: 70, ipv6: 10 },
  ]);
  const transfer = { setData: vi.fn(), effectAllowed: '', dropEffect: '' };
  const lineChip = screen.getByRole('button', { name: '排序 线路 · 洛杉矶 · 三跳线路' }).closest('[role="listitem"]')!;
  fireEvent.dragStart(screen.getByRole('button', { name: '排序 线路 · 洛杉矶 · 三跳线路' }), {
    dataTransfer: transfer,
  });
  fireEvent.dragOver(nic.closest('[role="listitem"]')!, { dataTransfer: transfer });
  fireEvent.drop(nic.closest('[role="listitem"]')!, { dataTransfer: transfer });
  expect(lineChip).toBeTruthy();
  expect(changed.mock.lastCall?.[0].priorities).toEqual([
    { interface_id: 'nic-business', ipv4: 70, ipv6: 10 },
    { chain_id: 'chain-us', ipv4: 10, ipv6: 5 },
  ]);
  fireEvent.click(screen.getByRole('button', { name: /^线路 · 洛杉矶 · 三跳线路 · IPv4 10/ }));
  fireEvent.change(screen.getByRole('spinbutton', { name: 'IPv6 优先级：线路 · 洛杉矶 · 三跳线路' }), {
    target: { value: '' },
  });
  expect(changed.mock.lastCall?.[0].priorities).toEqual([
    { interface_id: 'nic-business', ipv4: 70, ipv6: 10 },
    { chain_id: 'chain-us', ipv4: 10, ipv6: null },
  ]);
});

it('matches current draft with the server and does not retain a stale hit after an edit', async () => {
  const match = vi.fn((body: CvmRouteMatchRequest) =>
    Response.json({
      kind: 'route',
      route_index: 1,
      route: body.routes?.routes[1],
      priority: 10,
    }),
  );
  setup({}, match);
  fireEvent.change(screen.getByRole('textbox', { name: '测试目标' }), { target: { value: '2001:db8::1' } });
  fireEvent.click(screen.getByRole('button', { name: '测试命中' }));
  const hit = await screen.findByText('命中', { exact: true });
  expect(hit.closest('tr')!.textContent).toContain('::/0');
  expect(match).toHaveBeenCalledWith(
    expect.objectContaining({
      destination: '2001:db8::1',
      protocol: 'tcp',
      port: 443,
      routes: expect.objectContaining({
        routes: expect.arrayContaining([expect.objectContaining({ destination: '::/0' })]),
      }),
    }),
  );
  const row = editRow('::/0');
  fireEvent.change(row.getByRole('textbox', { name: '目标 IP／网段' }), { target: { value: '2001:db8::/32' } });
  fireEvent.click(row.getByRole('button', { name: '应用修改' }));
  expect(screen.queryByText('命中', { exact: true })).toBeNull();
});

it('does not surface an outstanding answer after switching to a newer query', async () => {
  let resolve: ((value: Response) => void) | undefined;
  const match = vi.fn(
    () =>
      new Promise<Response>(done => {
        resolve = done;
      }),
  );
  setup({}, match);
  const destination = screen.getByRole('textbox', { name: '测试目标' });
  fireEvent.change(destination, { target: { value: '203.0.113.1' } });
  fireEvent.click(screen.getByRole('button', { name: '测试命中' }));
  await waitFor(() => expect(match).toHaveBeenCalledOnce());
  fireEvent.change(destination, { target: { value: '203.0.113.2' } });
  await act(async () => resolve?.(Response.json({ kind: 'default' })));
  expect(screen.queryByText(/未命中目标路由/)).toBeNull();
});

it('disables a temporary NIC reference and resumes authoritative testing after removing it', async () => {
  const instance = cvmInstance();
  instance.desired.spec.routing.priorities.push({ interface_id: 'draft-nic', ipv4: 70, ipv6: null });
  const match = vi.fn((_body: CvmRouteMatchRequest) => Response.json({ kind: 'default' }));
  setup(
    { instance, extraTargets: [{ key: 'draft-nic', name: '新网卡', ipv4: true, ipv6: false, available: true }] },
    match,
  );
  const probe = screen.getByRole('textbox', { name: '测试目标' });
  fireEvent.change(probe, { target: { value: '203.0.113.9' } });
  const submit = screen.getByRole('button', { name: '测试命中' }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);
  expect(screen.getByText(/没有挂载草稿的网卡/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: /^网卡 · 新网卡 · IPv4 70/ }));
  fireEvent.click(screen.getByRole('button', { name: '移除此出口的路由与优先级' }));
  fireEvent.change(probe, { target: { value: '203.0.113.9 other' } });
  fireEvent.click(submit);
  await screen.findByText('默认出口');
  expect(match.mock.calls[0]?.[0]).not.toHaveProperty('port');
});

it.each([false, true])('tests staged mounts without writing (explicit routes: %s)', async explicit => {
  const instance = cvmInstance();
  const routing = instance.desired.spec.routing;
  const route = { interface_id: 'draft-nic', destination: '203.0.113.0/24' };
  if (explicit) routing.routes[0] = route;
  const networkEdit = {
    request_id: 'staged',
    expected_generation: 1,
    mounts: [{ draft_id: 'draft-nic', publication_id: 'network-hk', transport: 'wireguard' as const }],
    ...(explicit ? { routes: { expected_revision: routing.revision, routes: routing } } : {}),
  };
  const match = vi.fn((_body: CvmRouteMatchRequest) =>
    Response.json({ kind: 'route', route, route_index: 0, priority: 10 }),
  );
  setup(
    {
      instance,
      networkEdit,
      extraTargets: [{ key: 'draft-nic', name: '新网卡', ipv4: true, ipv6: false, available: true }],
    },
    match,
  );
  fireEvent.change(screen.getByRole('textbox', { name: '测试目标' }), { target: { value: '203.0.113.9' } });
  fireEvent.click(screen.getByRole('button', { name: '测试命中' }));
  const probe = screen.getByRole('form', { name: '路由命中测试' });
  expect((await within(probe).findByRole('status')).textContent).toContain('命中 203.0.113.0/24 → 网卡 · 新网卡');
  expect(match.mock.calls[0]?.[0]).toEqual({
    destination: '203.0.113.9',
    protocol: 'tcp',
    port: 443,
    network_edit: networkEdit,
  });
  expect(screen.queryAllByText('命中', { exact: true })).toHaveLength(explicit ? 1 : 0);
});

it('shows a useful non-editable table but never renders sensitive details in a redacted view', () => {
  setup({ writable: false });
  expect(screen.getByRole('table', { name: '目标路由' })).toBeTruthy();
  expect(screen.getByText('0.0.0.0/0')).toBeTruthy();
  expect(screen.queryByRole('textbox')).toBeNull();
  cleanup();
  const { calls } = setup({ redacted: true });
  expect(screen.getByText(/当前权限不显示目标网段/)).toBeTruthy();
  expect(screen.queryByText('0.0.0.0/0')).toBeNull();
  expect(screen.queryByRole('button', { name: '测试命中' })).toBeNull();
  expect(calls).not.toHaveBeenCalled();
});
