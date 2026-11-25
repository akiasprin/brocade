import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { fetchCvm, type CvmWorkspace } from '../../src/api';
import { HostList } from '../../src/panes/cvm/hosts';
import { cvmWorkspace } from '../fixtures/cvm/data';

vi.mock('../../src/forge/route', () => ({ navigate: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function mount(workspace: CvmWorkspace, status = 200) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  client.setQueryData(['cvm'], workspace);
  let current = workspace;
  const writes: { url: string; body: { enabled: boolean; expected_revision: number } }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === '/cvm/networks') return Response.json({ providers: [] });
      if (url === '/cvm') return Response.json(current);
      if (init?.method !== 'PUT') throw new Error(`Unexpected request: ${url}`);
      const body = JSON.parse(String(init.body));
      writes.push({ url, body });
      if (url.endsWith('/terminal')) {
        current = {
          ...current,
          hosts: current.hosts.map(host => ({
            ...host,
            terminal_enabled: body.enabled,
            terminal_revision: host.terminal_revision + 1,
          })),
        };
      } else {
        if (status === 409) return Response.json({ error: '该宿主仍有待清理资源' }, { status });
        const removed = current.hosts[0];
        current = {
          ...current,
          hosts: [],
          available_hosts: [{ ...removed, host_revision: removed.host_revision + 1 }],
        };
      }
      return Response.json(current);
    }),
  );
  const onCreateOnHost = vi.fn();
  function Workspace() {
    const { data } = useQuery({ queryKey: ['cvm'], queryFn: fetchCvm, staleTime: Infinity });
    return data && <HostList workspace={data} onCreateOnHost={onCreateOnHost} />;
  }
  render(
    <QueryClientProvider client={client}>
      <Workspace />
    </QueryClientProvider>,
  );
  const expand = () => fireEvent.click(screen.getByRole('button', { name: workspace.hosts[0].name }));
  return { writes, expand, onCreateOnHost };
}

it('shows capacity and requires cleanup before removal without disabling a valid creation shortcut', () => {
  const data = cvmWorkspace('list');
  const { expand, writes, onCreateOnHost } = mount(data);
  expect(screen.getByText('1 / 8')).toBeTruthy();
  expand();
  const remove = screen.getByRole<HTMLButtonElement>('button', { name: '移除宿主…' });
  expect(remove.disabled).toBe(true);
  fireEvent.click(remove);
  expect(writes).toEqual([]);
  fireEvent.click(screen.getByRole('button', { name: '在此创建' }));
  expect(onCreateOnHost).toHaveBeenCalledWith(data.hosts[0].node_id);
});

it.each([200, 409])('sends a versioned removal only after confirmation and handles HTTP %s', async status => {
  const data = cvmWorkspace('new');
  const host = data.hosts[0];
  const { expand, writes } = mount(data, status);
  expand();
  fireEvent.click(screen.getByRole('button', { name: '移除宿主…' }));
  const dialog = screen.getByRole('dialog', { name: '移除宿主' });
  expect(within(dialog).getByText(/代理、组网及其他功能会保留/)).toBeTruthy();
  expect(writes).toEqual([]);
  fireEvent.click(within(dialog).getByRole('button', { name: '确认移除' }));
  if (status === 200) await screen.findByText('尚无宿主');
  else expect((await screen.findByRole('alert')).textContent).toContain('该宿主仍有待清理资源');
  expect(writes).toEqual([
    { url: `/cvm/hosts/${host.node_id}`, body: { enabled: false, expected_revision: host.host_revision } },
  ]);
});

it('uses the backed WebShell toggle and does not present missing sessions as zero', async () => {
  const data = cvmWorkspace('list');
  const host = data.hosts[0];
  const { writes } = mount(data);
  const toggle = screen.getByRole<HTMLInputElement>('switch', { name: `${host.name} WebShell` });
  expect(toggle.checked).toBe(true);
  expect(screen.getByText('会话数未上报')).toBeTruthy();
  fireEvent.click(toggle);
  await waitFor(() => expect(toggle.checked).toBe(false));
  expect(screen.queryByText('会话数未上报')).toBeNull();
  expect(writes).toEqual([
    { url: `/cvm/hosts/${host.node_id}/terminal`, body: { enabled: false, expected_revision: host.terminal_revision } },
  ]);
});

it.each([0, 2])('renders the broker session count of %s against its reported limit', used => {
  const data = cvmWorkspace('list');
  data.hosts[0].terminal_sessions = { used, limit: 3 };
  mount(data);
  expect(screen.getByText(`会话 ${used} / 3`)).toBeTruthy();
  expect(screen.queryByText('会话数未上报')).toBeNull();
});

it('does not show WebShell occupancy or controls to a host without terminal management permission', () => {
  const data = cvmWorkspace('list', 'public');
  data.hosts[0].terminal_sessions = { used: 2, limit: 3 };
  mount(data);
  expect(screen.queryByRole('switch')).toBeNull();
  expect(screen.queryByText('会话 2 / 3')).toBeNull();
  expect(screen.queryByText('会话数未上报')).toBeNull();
});
