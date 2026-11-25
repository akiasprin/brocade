import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmAdopt, CvmWorkspace } from '../../src/api';
import { navigate } from '../../src/forge/route';
import { AdoptForm } from '../../src/panes/cvm/adopt';
import { baremetalWorkspace, cvmWorkspace } from '../fixtures/cvm/data';

vi.mock('../../src/forge/route', () => ({ navigate: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function mount(workspace: CvmWorkspace) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  client.setQueryData(['cvm'], workspace);
  const onViewHosts = vi.fn();
  const page = (data: CvmWorkspace) => (
    <QueryClientProvider client={client}>
      <AdoptForm workspace={data} onViewHosts={onViewHosts} />
    </QueryClientProvider>
  );
  const view = render(page(workspace));
  return { client, onViewHosts, update: (data: CvmWorkspace) => view.rerender(page(data)) };
}

it('shows the bare-metal empty state on the page and calls the host callback', () => {
  const { onViewHosts } = mount(cvmWorkspace('list'));
  const form = screen.getByRole('form', { name: '纳管裸机' });
  expect(within(form).getByText('尚无可纳管的裸机')).toBeTruthy();
  expect(within(form).queryByRole('button', { name: '确认纳管' })).toBeNull();
  expect(screen.queryByRole('dialog')).toBeNull();
  fireEvent.click(within(form).getByRole('button', { name: '查看宿主' }));
  expect(onViewHosts).toHaveBeenCalledOnce();
});

it('requires confirmation for the selected machine identity and adopts using the native inventory', async () => {
  const workspace = baremetalWorkspace('baremetal-hosts');
  const host = workspace.hosts[0];
  workspace.hosts.push({ ...structuredClone(host), node_id: 'stale', name: '过期裸机', fresh: false });
  workspace.hosts.push({ ...cvmWorkspace('list').hosts[0], node_id: 'cube', name: '仅容器模式' });
  const result = baremetalWorkspace().instances[0];
  const fetch = vi.fn(async (_url: RequestInfo | URL, _init?: RequestInit) => Response.json(result));
  vi.stubGlobal('fetch', fetch);
  const { client, update } = mount(workspace);
  const picker = screen.getByRole('combobox', { name: '选择裸机' });
  expect(within(picker).getByRole<HTMLOptionElement>('option', { name: /过期裸机/ }).disabled).toBe(true);
  expect(within(picker).queryByRole('option', { name: '仅容器模式' })).toBeNull();
  const confirm = screen.getByRole<HTMLButtonElement>('button', { name: '确认纳管' });
  expect(confirm.disabled).toBe(true);
  fireEvent.click(screen.getByRole('checkbox'));
  expect(confirm.disabled).toBe(false);
  const refreshed = structuredClone(workspace);
  refreshed.hosts[0].capabilities.host_system!.identity!.machine = 'updated-machine';
  update(refreshed);
  expect(confirm.disabled).toBe(true);
  fireEvent.click(screen.getByRole('checkbox'));
  fireEvent.click(confirm);
  await waitFor(() => expect(navigate).toHaveBeenCalledWith('cvm', { p: 'instance', id: result.desired.id }));
  expect(fetch).toHaveBeenCalledOnce();
  expect(fetch.mock.calls[0][0]).toBe('/cvm/adopt');
  expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toEqual({
    request_id: expect.any(String),
    node_id: host.node_id,
    tenant_id: workspace.owners[0].tenant_id,
    user_id: workspace.owners[0].user_id,
    name: host.name,
    identity: refreshed.hosts[0].capabilities.host_system!.identity,
  });
  expect(client.getQueryData<CvmWorkspace>(['cvm'])?.instances).toEqual([result]);
});

it('retries the identical request after a refresh shows the host has already been adopted', async () => {
  const workspace = baremetalWorkspace('baremetal-hosts');
  const completed = baremetalWorkspace();
  const requests: CvmAdopt[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)));
      return requests.length === 1
        ? Response.json({ error: '执行结果尚未确认' }, { status: 503 })
        : Response.json(completed.instances[0]);
    }),
  );
  const { update } = mount(workspace);
  fireEvent.click(screen.getByRole('checkbox'));
  fireEvent.click(screen.getByRole('button', { name: '确认纳管' }));
  const retry = await screen.findByRole('button', { name: '重试原请求' });
  update(completed);
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: '实例名称' }).disabled).toBe(true);
  fireEvent.click(retry);
  await waitFor(() => expect(navigate).toHaveBeenCalledOnce());
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
});
