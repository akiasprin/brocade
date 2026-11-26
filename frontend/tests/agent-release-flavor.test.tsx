import { useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { BinaryCatalog, BinaryReleaseView } from '../src/api';
import { AgentReleaseTab, useAgentRelease } from '../src/panes/agent-release';

const OLD = 'a'.repeat(64);
const BASE_NEXT = 'b'.repeat(64);
const CVM_NEXT = 'c'.repeat(64);

const catalog = (flavor: 'base' | 'cvm'): BinaryCatalog => ({
  agent_flavor: flavor,
  component: 'agent',
  build_id: (flavor === 'base' ? 'd' : 'e').repeat(64),
  version: 'v0.3.9',
  artifacts: [{ arch: 'x86_64', sha256: flavor === 'base' ? BASE_NEXT : CVM_NEXT }],
  source: 'embedded',
});
/** 容器版 Console 同时内嵌两种 Agent；基础版 Console 只有基础版。 */
const cvmConsole = (): BinaryReleaseView => ({
  available: catalog('base'),
  available_variants: [catalog('base'), catalog('cvm')],
  current: null,
  legacy_approval: null,
});
const baseConsole = (): BinaryReleaseView => ({
  available: catalog('base'),
  available_variants: [catalog('base')],
  current: null,
  legacy_approval: null,
});

const node = (node_id: string, name: string, hash: string, flavor?: 'base' | 'cvm' | null) => ({
  node_id,
  name,
  agent_version: `brocade-agent/${hash}`,
  lifecycle_phase: 'active',
  desired_poll_fresh: true,
  runtime_report_fresh: true,
  runtime_versions: flavor === undefined ? {} : { agent_flavor: flavor },
});
const fleet = {
  nodes: [
    node('legacy', '旧 Agent', OLD),
    node('base', '基础版机器', OLD, 'base'),
    node('base-latest', '基础版最新', BASE_NEXT, 'base'),
    node('cvm', '容器版机器', OLD, 'cvm'),
    node('cvm-latest', '容器版最新', CVM_NEXT, 'cvm'),
  ],
};

function Harness() {
  const agent = useAgentRelease();
  const [editing, setEditing] = useState(true);
  return <AgentReleaseTab agent={agent} editable editing={editing} onEditingChange={setEditing} />;
}

const clients: QueryClient[] = [];
function mount(view: BinaryReleaseView) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY }, mutations: { retry: false } },
  });
  clients.push(client);
  client.setQueryData(['binary-releases', 'agent'], view);
  client.setQueryData(['nodes'], fleet);
  return render(
    <QueryClientProvider client={client}>
      <Harness />
    </QueryClientProvider>,
  );
}

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
function captureCreate(view: BinaryReleaseView) {
  const sent: { body?: Record<string, unknown> } = {};
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input) === '/nodes/agent-state') return json(fleet);
      expect(String(input)).toBe('/binary-releases/agent');
      expect(init?.method).toBe('POST');
      sent.body = JSON.parse(String(init?.body));
      return json(view);
    }),
  );
  return sent;
}
const checkbox = (name: string) => screen.getByLabelText(`升级 ${name}`) as HTMLInputElement;

afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  vi.unstubAllGlobals();
});

describe('Agent 类型跟随 Console', () => {
  it('容器版 Console 不再提供变体切换，基础版机器统一升级为容器版', async () => {
    const view = cvmConsole();
    const sent = captureCreate(view);
    mount(view);

    expect(screen.queryByRole('group', { name: 'Agent 产品版本' })).toBeNull();
    expect(screen.queryByText('允许基础版机器升级为容器版')).toBeNull();
    const ledger = screen.getByRole('region', { name: '已一致' });
    expect(within(ledger).getByText('容器版 · 跟随 Console')).toBeTruthy();
    expect(within(ledger).getByText('可发 v0.3.9')).toBeTruthy();
    // 旧 Agent、基础版（即使已是最新基础版构建）和容器版旧版本都在同一份可升级名单里。
    for (const name of ['旧 Agent', '基础版机器', '基础版最新', '容器版机器'])
      expect(checkbox(name).disabled).toBe(false);
    expect(checkbox('容器版最新').disabled).toBe(true);

    fireEvent.click(checkbox('基础版机器'));
    fireEvent.click(checkbox('容器版机器'));
    const scope = screen.getByRole('group', { name: '升级范围' });
    expect(within(scope).getByText('已选 2 台')).toBeTruthy();
    expect(within(scope).getByText(/其中 1 台由基础版换为容器版。/)).toBeTruthy();
    fireEvent.click(within(scope).getByRole('button', { name: '批准' }));

    await waitFor(() =>
      expect(sent.body).toEqual({
        idempotency_key: expect.any(String),
        build_id: 'e'.repeat(64),
        nodes: ['base', 'cvm'],
        note: null,
        agent_flavor: 'cvm',
        allow_flavor_change: true,
      }),
    );
  });

  it('名单里没有要换变体的机器时不授权变体变更', async () => {
    const view = cvmConsole();
    const sent = captureCreate(view);
    mount(view);

    fireEvent.click(checkbox('容器版机器'));
    const scope = screen.getByRole('group', { name: '升级范围' });
    expect(within(scope).queryByText(/由基础版换为/)).toBeNull();
    fireEvent.click(within(scope).getByRole('button', { name: '批准' }));

    await waitFor(() =>
      expect(sent.body).toMatchObject({ nodes: ['cvm'], agent_flavor: 'cvm', allow_flavor_change: false }),
    );
  });

  it('基础版 Console 只下发基础版，容器版 Agent 写明须停机重装', async () => {
    const view = baseConsole();
    const sent = captureCreate(view);
    mount(view);

    expect(within(screen.getByRole('region', { name: '已一致' })).getByText('基础版 · 跟随 Console')).toBeTruthy();
    expect(checkbox('旧 Agent').disabled).toBe(false);
    expect(checkbox('基础版机器').disabled).toBe(false);
    expect(checkbox('基础版最新').disabled).toBe(true);
    expect(checkbox('容器版机器').disabled).toBe(true);
    expect(checkbox('容器版最新').disabled).toBe(true);
    expect(screen.getAllByText('容器版 Agent 换回基础版须停机重装')).toHaveLength(2);

    fireEvent.click(checkbox('旧 Agent'));
    fireEvent.click(screen.getByRole('button', { name: '批准' }));

    await waitFor(() =>
      expect(sent.body).toMatchObject({
        build_id: 'd'.repeat(64),
        nodes: ['legacy'],
        agent_flavor: 'base',
        allow_flavor_change: false,
      }),
    );
  });
});
