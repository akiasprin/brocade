import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { AgentLogPolicyView } from '../src/api';
import { AgentLogPolicySection } from '../src/panes/settings';

const policy = (globalMax = 100): AgentLogPolicyView => ({
  global: {
    agent_journal_mib: globalMax,
    xray_mib: globalMax,
    phantun_mib: globalMax,
  },
  nodes: [
    {
      node_id: 'hk',
      tenant_id: 'platform',
      name: '香港',
      overrides: { agent_journal_mib: null, xray_mib: null, phantun_mib: null },
      effective: { agent_journal_mib: globalMax, xray_mib: globalMax, phantun_mib: globalMax },
    },
    {
      node_id: 'sg',
      tenant_id: 'platform',
      name: '新加坡',
      overrides: { agent_journal_mib: null, xray_mib: 64, phantun_mib: null },
      effective: { agent_journal_mib: globalMax, xray_mib: 64, phantun_mib: globalMax },
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
  it('lists each independently capped log scope and updates the displayed limit with the input', () => {
    const view = mounted();
    const scope = view.getByLabelText('日志额度作用范围');
    expect(within(scope).getByText('独立 journal 命名空间')).toBeTruthy();
    expect(within(scope).getByText('xray.log + xray.log.1')).toBeTruthy();
    expect(within(scope).getByText('每个实例的 .log + .log.1')).toBeTruthy();
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 XRAY 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 Phantun 日志上限') as HTMLInputElement).value).toBe('100');

    fireEvent.change(view.getByLabelText('全局 XRAY 日志上限'), { target: { value: '240' } });
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 XRAY 日志上限') as HTMLInputElement).value).toBe('240');
    expect((view.getByLabelText('全局 Phantun 日志上限') as HTMLInputElement).value).toBe('100');
    view.unmount();
  });

  it('keeps inherited machines following global changes while preserving machine overrides', () => {
    const view = mounted();
    expect((view.getByLabelText('香港 Agent 日志上限') as HTMLInputElement).value).toBe('');
    expect((view.getByLabelText('香港 Agent 日志上限') as HTMLInputElement).placeholder).toBe('100');
    expect((view.getByLabelText('新加坡 XRAY 日志上限') as HTMLInputElement).value).toBe('64');

    view.rerender(view.renderSection(policy(240)));
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('240');
    expect((view.getByLabelText('香港 Agent 日志上限') as HTMLInputElement).placeholder).toBe('240');
    expect((view.getByLabelText('新加坡 XRAY 日志上限') as HTMLInputElement).value).toBe('64');
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
    fireEvent.change(within(inheritedRow).getByLabelText('香港 Agent 日志上限'), { target: { value: '80' } });
    fireEvent.click(within(inheritedRow).getByRole('button', { name: '保存覆盖' }));

    const overriddenRow = view.getByText('新加坡').closest<HTMLElement>('.agent-log-node');
    if (!overriddenRow) throw new Error('missing overridden machine row');
    fireEvent.click(within(overriddenRow).getByRole('button', { name: '全部继承' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [overrideUrl, overrideInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    const [clearUrl, clearInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(overrideUrl).toBe('/agent-log-policy/nodes/hk');
    expect(JSON.parse(String(overrideInit.body))).toEqual({
      agent_journal_mib: 80,
      xray_mib: null,
      phantun_mib: null,
    });
    expect(clearUrl).toBe('/agent-log-policy/nodes/sg');
    expect(JSON.parse(String(clearInit.body))).toEqual({
      agent_journal_mib: null,
      xray_mib: null,
      phantun_mib: null,
    });
    view.unmount();
    vi.unstubAllGlobals();
  });
});
