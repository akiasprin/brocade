import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { AgentLogPolicyView } from '../src/api';
import { AgentLogPolicySection, NodeLogPolicyRow } from '../src/panes/settings';

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
  it('lists each independently capped log scope without implementation details and updates the input', () => {
    const view = mounted();
    const scope = view.getByLabelText('日志额度作用范围');
    expect(within(scope).getByText('Agent')).toBeTruthy();
    expect(within(scope).getByText('XRAY')).toBeTruthy();
    expect(within(scope).getByText('Phantun')).toBeTruthy();
    expect(within(scope).queryByText('独立 journal 命名空间')).toBeNull();
    expect(within(scope).queryByText('xray.log + xray.log.1')).toBeNull();
    expect(within(scope).queryByText('每个实例的 .log + .log.1')).toBeNull();
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 XRAY 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 Phantun 日志上限') as HTMLInputElement).value).toBe('100');

    fireEvent.change(view.getByLabelText('全局 XRAY 日志上限'), { target: { value: '240' } });
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('100');
    expect((view.getByLabelText('全局 XRAY 日志上限') as HTMLInputElement).value).toBe('240');
    expect((view.getByLabelText('全局 Phantun 日志上限') as HTMLInputElement).value).toBe('100');
    view.unmount();
  });

  it('does not render per-machine overrides on the global settings page', () => {
    const view = mounted();
    expect(view.queryByText('香港')).toBeNull();
    expect(view.queryByText('新加坡')).toBeNull();
    expect(view.queryByText('机器覆盖')).toBeNull();

    view.rerender(view.renderSection(policy(240)));
    expect((view.getByLabelText('全局 Agent 日志上限') as HTMLInputElement).value).toBe('240');
    view.unmount();
  });

  it('preserves an unfinished machine override when inherited limits or its name refresh', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const row = (data: AgentLogPolicyView) => (
      <QueryClientProvider client={client}>
        <NodeLogPolicyRow editable node={data.nodes[0]} global={data.global} />
      </QueryClientProvider>
    );
    const view = render(row(policy()));
    fireEvent.change(view.getByLabelText('香港 Agent 日志上限'), { target: { value: '80' } });
    const refreshed = policy(240);
    refreshed.nodes[0].name = '香港新名称';
    view.rerender(row(refreshed));
    expect((view.getByLabelText('香港新名称 Agent 日志上限') as HTMLInputElement).value).toBe('80');
    expect(view.getByRole('button', { name: '保存覆盖' })).toBeTruthy();
    view.unmount();
  });

  it('keeps machine override editing available to the machine configuration panel', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => policy(),
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const inherited = render(
      <QueryClientProvider client={client}>
        <NodeLogPolicyRow editable node={policy().nodes[0]} global={policy().global} />
      </QueryClientProvider>,
    );
    fireEvent.change(inherited.getByLabelText('香港 Agent 日志上限'), { target: { value: '80' } });
    fireEvent.click(inherited.getByRole('button', { name: '保存覆盖' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const [overrideUrl, overrideInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(overrideUrl).toBe('/agent-log-policy/nodes/hk');
    expect(JSON.parse(String(overrideInit.body))).toEqual({
      agent_journal_mib: 80,
      xray_mib: null,
      phantun_mib: null,
    });

    inherited.unmount();
    const overridden = render(
      <QueryClientProvider client={client}>
        <NodeLogPolicyRow editable node={policy().nodes[1]} global={policy().global} />
      </QueryClientProvider>,
    );
    fireEvent.click(overridden.getByRole('button', { name: '与全局一致' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [clearUrl, clearInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(clearUrl).toBe('/agent-log-policy/nodes/sg');
    expect(JSON.parse(String(clearInit.body))).toEqual({
      agent_journal_mib: null,
      xray_mib: null,
      phantun_mib: null,
    });
    overridden.unmount();
    vi.unstubAllGlobals();
  });
});
