import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchNodes, type NodeAgentStateItem } from '../src/api';
import { IpChangeNotificationRow } from '../src/panes/nodes';
import { nodes } from './fixtures/ui-regression-data';

let client: QueryClient;
let served: NodeAgentStateItem[];
let fail = false;
let release: (() => void) | undefined;
let hold = false;
const writes: { node: string; enabled: boolean }[] = [];
beforeEach(() => {
  served = [{ ...nodes[0] }, { ...nodes[1] }];
  writes.length = 0;
  fail = hold = false;
  release = undefined;
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['nodes'], { nodes: served });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input, init?: RequestInit) => {
      const path = String(input);
      if (path === '/nodes/agent-state') return Response.json({ nodes: served });
      const match = path.match(/^\/nodes\/([^/]+)\/notifications$/);
      if (match && init?.method === 'PUT') {
        const body = JSON.parse(String(init.body));
        const node = decodeURIComponent(match[1]);
        writes.push({ node, enabled: body.public_ip_changes });
        if (hold)
          await new Promise<void>(resolve => {
            release = resolve;
          });
        if (fail) return Response.json({ error: '通知设置保存失败' }, { status: 503 });
        served = served.map(item =>
          item.node_id === node ? { ...item, public_ip_change_notifications: body.public_ip_changes } : item,
        );
        return Response.json({ node_id: node, public_ip_changes: body.public_ip_changes });
      }
      return Response.json({ error: 'unexpected test request' }, { status: 404 });
    }),
  );
});
afterEach(() => {
  cleanup();
  client.clear();
  vi.unstubAllGlobals();
});
function Harness({ editable = true }: { editable?: boolean }) {
  const query = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  return <IpChangeNotificationRow node={query.data!.nodes[0]} canEdit={editable} />;
}
function mount(editable = true) {
  return render(
    <QueryClientProvider client={client}>
      <Harness editable={editable} />
    </QueryClientProvider>,
  );
}
const selected = (name: string) => screen.getByRole('button', { name }).getAttribute('aria-pressed');

it('defaults on, persists both choices and refreshes notifications without changing another node', async () => {
  const invalidate = vi.spyOn(client, 'invalidateQueries');
  const view = mount();
  expect(screen.getByText('IP 变更通知')).toBeTruthy();
  expect(selected('开启')).toBe('true');
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  await waitFor(() => expect(selected('关闭')).toBe('true'));
  expect(writes).toEqual([{ node: nodes[0].node_id, enabled: false }]);
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ['machine-notifications'] });
  view.unmount();
  mount();
  expect(selected('关闭')).toBe('true');
  fireEvent.click(screen.getByRole('button', { name: '开启' }));
  await waitFor(() => expect(selected('开启')).toBe('true'));
  expect(served[1].public_ip_change_notifications).toBeUndefined();
});
it('keeps the previous value on failure and allows a retry', async () => {
  fail = true;
  mount();
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  await screen.findByText(/通知设置保存失败/);
  expect(selected('开启')).toBe('true');
  fail = false;
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  await waitFor(() => expect(selected('关闭')).toBe('true'));
});
it('disables both choices while saving', async () => {
  hold = true;
  mount();
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  await waitFor(() => expect(writes).toHaveLength(1));
  for (const button of screen.getAllByRole('button')) expect((button as HTMLButtonElement).disabled).toBe(true);
  release!();
  await waitFor(() => expect(selected('关闭')).toBe('true'));
});
it('does not permit readonly writes', () => {
  mount(false);
  for (const button of screen.getAllByRole('button')) expect((button as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  expect(writes).toEqual([]);
});
