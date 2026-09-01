import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { AgentLogPolicyView } from '../src/api';
import { AgentLogPolicySection } from '../src/panes/settings';

const policy = (globalMax = 100): AgentLogPolicyView => ({
  global_max_mib: globalMax,
  nodes: [
    {
      node_id: 'hk',
      tenant_id: 'platform',
      name: '香港',
      override_max_mib: null,
      effective_max_mib: globalMax,
    },
    {
      node_id: 'sg',
      tenant_id: 'platform',
      name: '新加坡',
      override_max_mib: 64,
      effective_max_mib: 64,
    },
  ],
});

function mounted(data = policy()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const renderSection = (next: AgentLogPolicyView) => (
    <QueryClientProvider client={client}>
      <AgentLogPolicySection editable data={next} />
    </QueryClientProvider>
  );
  const view = render(renderSection(data));
  return { ...view, renderSection };
}

describe('Agent log policy inheritance', () => {
  it('keeps inherited machines following global changes while preserving machine overrides', () => {
    const view = mounted();
    expect((view.getByLabelText('香港 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('香港 日志上限') as HTMLInputElement).disabled).toBe(true);
    expect((view.getByLabelText('新加坡 日志上限') as HTMLInputElement).value).toBe('64');

    view.rerender(view.renderSection(policy(240)));
    expect((view.getByLabelText('全局日志上限') as HTMLInputElement).value).toBe('240');
    expect((view.getByLabelText('香港 日志上限') as HTMLInputElement).value).toBe('240');
    expect((view.getByLabelText('新加坡 日志上限') as HTMLInputElement).value).toBe('64');
    view.unmount();
  });

  it('writes a machine override and clears it as null rather than copying the global value', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => policy(),
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = mounted();

    const inheritedRow = view.getByText('香港').closest<HTMLElement>('.agent-log-node');
    if (!inheritedRow) throw new Error('missing inherited machine row');
    fireEvent.click(within(inheritedRow).getByRole('button', { name: '设置覆盖' }));
    fireEvent.change(within(inheritedRow).getByLabelText('香港 日志上限'), { target: { value: '80' } });
    fireEvent.click(within(inheritedRow).getByRole('button', { name: '保存' }));

    const overriddenRow = view.getByText('新加坡').closest<HTMLElement>('.agent-log-node');
    if (!overriddenRow) throw new Error('missing overridden machine row');
    fireEvent.click(within(overriddenRow).getByRole('button', { name: '取消覆盖' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [overrideUrl, overrideInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    const [clearUrl, clearInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(overrideUrl).toBe('/agent-log-policy/nodes/hk');
    expect(JSON.parse(String(overrideInit.body))).toEqual({ max_mib: 80 });
    expect(clearUrl).toBe('/agent-log-policy/nodes/sg');
    expect(JSON.parse(String(clearInit.body))).toEqual({ max_mib: null });
    view.unmount();
    vi.unstubAllGlobals();
  });
});
