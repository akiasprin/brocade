import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem } from '../src/api';
import { draft } from '../src/draft';
let XrayCard: typeof import('../src/panes/nodes').XrayCard;
beforeAll(async () => {
  window.matchMedia = (() => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ XrayCard } = await import('../src/panes/nodes'));
});
afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});
function mount(enabled: boolean, applied: string, editable = true) {
  draft.init('node-xray-test');
  const model = {
    snapshot: { revision: 1, apps: [], nodes: [{ id: 'n1', xray_enabled: enabled }] },
    node_egress_dns: [],
  };
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  qc.setQueryData(['snapshot'], model);
  vi.stubGlobal(
    'fetch',
    vi.fn(async input => Response.json(String(input).includes('preview') ? { snapshot: model } : model)),
  );
  render(
    <QueryClientProvider client={qc}>
      <XrayCard
        node={
          {
            node_id: 'n1',
            name: '机器一',
            xray_enabled: true,
            applied: { xray: { state: applied } },
          } as unknown as NodeAgentStateItem
        }
        canEdit={editable}
        onSaved={() => {}}
      />
    </QueryClientProvider>,
  );
}
it('stages a shutdown without claiming that the running node has already stopped', async () => {
  mount(true, 'present');
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  expect(draft.ops()).toHaveLength(0);
  expect(screen.getByText(/已启用，配置仍可编辑/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() =>
    expect(draft.ops()).toEqual([{ op: 'update_node', node_id: 'n1', node: { xray_enabled: false } }]),
  );
  expect(screen.getByText(/发布后生效，配置仍可编辑/)).toBeTruthy();
});
it('allows reenable and independent edits while stopped, preserving the combined draft', async () => {
  mount(false, 'disabled');
  expect(screen.getByText(/已关闭，配置仍可编辑/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '启用' }));
  fireEvent.click(screen.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() => expect(draft.ops()[0]).toMatchObject({ node: { xray_enabled: true } }));
  draft.push({
    op: 'update_node',
    node_id: 'n1',
    node: {
      dns: { t: 'servers', v: ['1.1.1.1'] },
      connection: { conn_idle_secs: 600, uplink_only_secs: null, downlink_only_secs: null, buffer_size_kb: null },
    },
  });
  expect(draft.ops()[0]).toMatchObject({
    node: {
      xray_enabled: true,
      dns: { t: 'servers' },
      connection: { conn_idle_secs: 600, uplink_only_secs: null, downlink_only_secs: null, buffer_size_kb: null },
    },
  });
});
it('shows stopped configuration to readers without granting editing permission', () => {
  mount(false, 'disabled', false);
  expect((screen.getByRole('button', { name: '启用' }) as HTMLButtonElement).disabled).toBe(true);
});
