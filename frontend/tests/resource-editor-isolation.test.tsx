import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ChainsPane, IngressStreamRow } from '../src/panes/chains';
import { NodesPane } from '../src/panes/nodes';
import { WarpEdit } from '../src/panes/tunnels';
import { SessionProvider } from '../src/session';
import { draft } from '../src/draft';
import { ingressUpsertBody } from '../src/api';
import { appId, pairs, nodes, snapshot, settings, compiled, warps } from './fixtures/ui-regression-data';
import type { SnapshotIngress } from '../src/api';
import type { Win } from '../src/wm/store';
const clients: QueryClient[] = [];
beforeEach(() => {
  localStorage.clear();
  draft.init('resource-regressions');
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    'fetch',
    vi.fn(async input => {
      const path = String(input);
      if (path === '/settings') return Response.json(settings);
      if (path === '/model/snapshot') return Response.json(snapshot);
      if (path === '/revisions?limit=50') return Response.json({ current_revision: 7, revisions: [] });
      if (path.startsWith('/compile/')) return Response.json(compiled);
      if (path === '/nodes/agent-state') return Response.json({ nodes });
      if (path.startsWith('/probes/e2e')) return Response.json({ chains: [] });
      if (path === '/node-traffic') return Response.json({ nodes: [] });
      return Response.json({ error: `Unexpected test request: ${path}` }, { status: 404 });
    }),
  );
});
afterEach(() => {
  cleanup();
  clients.forEach(c => c.clear());
  clients.length = 0;
  draft.clear();
  vi.unstubAllGlobals();
});
function setup() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  clients.push(client);
  for (const [key, value] of [
    [['nodes'], { nodes }],
    [['snapshot'], snapshot],
    [['snapshot', 'committed'], snapshot],
    [['settings'], settings],
    [['revisions'], { current_revision: 7, revisions: [] }],
    [['compile', 7], compiled],
    [['e2e-probes'], { chains: [] }],
    [['deployments'], { deployments: [] }],
    [['certs'], { groups: [], nodes: [] }],
  ] as const)
    client.setQueryData(key, value);
  const wrap = (children: React.ReactNode) => (
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'resource-regressions',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 12, chain_group_count: [] },
        }}
      >
        {children}
      </SessionProvider>
    </QueryClientProvider>
  );
  const win = (drill: unknown): Win => ({
    id: 1,
    key: 'tab:test',
    title: 'test',
    x: 0,
    y: 0,
    w: 1000,
    h: 800,
    z: 1,
    min: false,
    home: 'desk',
    data: { drill },
  });
  return { client, wrap, win };
}
it('starts the other machine with its own identity fields and closes the force confirmation', async () => {
  const { wrap, win } = setup();
  const view = render(wrap(<NodesPane win={win({ p: 'node', id: 'node-0', tab: 'config' })} />));
  const input = screen.getByDisplayValue(nodes[0].name);
  fireEvent.change(input, { target: { value: 'A unsaved' } });
  fireEvent.click(screen.getByRole('button', { name: '更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /强制完成 Xray 交接/ }));
  expect(screen.getByRole('dialog').textContent).toContain('#100');
  view.rerender(wrap(<NodesPane win={win({ p: 'node', id: 'node-1', tab: 'config' })} />));
  expect(screen.getByDisplayValue(nodes[1].name)).toBeTruthy();
  expect(screen.queryByDisplayValue('A unsaved')).toBeNull();
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(draft.ops().some(op => op.op === 'update_node' && op.node_id === 'node-1')).toBe(false);
});
it('drops a different chain’s pending port instead of registering it under the new chain', async () => {
  const { wrap, win } = setup();
  const view = render(wrap(<ChainsPane win={win({ p: 'chain', app: appId, chain: pairs[1].chainId })} />));
  const disclosure = document.querySelector('details.ingress-protocol-panel');
  if (disclosure) fireEvent.click(disclosure.querySelector('summary')!);
  const port = await screen.findByLabelText('VLESS · TLS / REALITY 监听端口');
  fireEvent.change(port, { target: { value: '62001' } });
  view.rerender(wrap(<ChainsPane win={win({ p: 'chain', app: appId, chain: pairs[0].chainId })} />));
  const open = document.querySelector('details.ingress-protocol-panel');
  if (open) fireEvent.click(open.querySelector('summary')!);
  await waitFor(() =>
    expect((screen.getByLabelText('VLESS · TLS / REALITY 监听端口') as HTMLInputElement).value).toBe('63001'),
  );
  expect(screen.queryByDisplayValue('62001')).toBeNull();
});
it('reinitializes the actual WARP drawer when its resource changes', async () => {
  const { wrap } = setup();
  const view = render(wrap(<WarpEdit tunnel={warps[1]} onClose={() => {}} />));
  fireEvent.change(screen.getByDisplayValue('WARP B'), { target: { value: 'B unsaved' } });
  fireEvent.change(screen.getByDisplayValue(warps[1].address), { target: { value: '162.159.198.4' } });
  view.rerender(wrap(<WarpEdit tunnel={warps[0]} onClose={() => {}} />));
  expect(screen.getByDisplayValue('WARP A')).toBeTruthy();
  expect(screen.getByDisplayValue(warps[0].address)).toBeTruthy();
  expect(screen.queryByDisplayValue('B unsaved')).toBeNull();
});
for (const pending of ['upsert_ingress', 'create_ingress'] as const)
  it(`keeps sequential protocol switches in an existing ${pending} draft`, async () => {
    const { client, wrap } = setup();
    const ingress: SnapshotIngress = {
      ...snapshot.snapshot.apps[0].ingresses[0],
      wires: { vless: { kind: 'vless-reality' } },
    };
    const body = ingressUpsertBody(ingress);
    const { note: _note, ...payload } = body;
    draft.push({ op: pending, app_id: appId, ingress: payload });
    const view = render(
      wrap(<IngressStreamRow appId={appId} ingress={ingress} editable settingsReadable section="protocols" />),
    );
    fireEvent.click(screen.getByLabelText('AnyTLS（TCP）'));
    await waitFor(() => expect(draft.ops()[0]).toMatchObject({ ingress: { wires: { anytls: expect.anything() } } }));
    const first = draft.ops()[0];
    if (first.op !== 'upsert_ingress' && first.op !== 'create_ingress') throw new Error('wrong operation');
    const next = { ...ingress, wires: first.ingress.wires! };
    await act(async () => {
      client.setQueryData(['snapshot'], {
        ...snapshot,
        snapshot: { ...snapshot.snapshot, apps: [{ ...snapshot.snapshot.apps[0], ingresses: [next] }] },
      });
    });
    view.rerender(
      wrap(<IngressStreamRow appId={appId} ingress={next} editable settingsReadable section="protocols" />),
    );
    fireEvent.click(screen.getByLabelText('Hysteria 2（QUIC over UDP）'));
    await waitFor(() =>
      expect(draft.ops()[0]).toMatchObject({
        ingress: { wires: { anytls: expect.anything(), hysteria2: expect.anything() } },
      }),
    );
    expect(draft.ops()).toHaveLength(1);
  });

it('clears a different machine’s installation confirmation and does not reuse its command', async () => {
  const { wrap, win, client } = setup();
  const view = render(wrap(<NodesPane win={win({ p: 'install', node: 'node-1', step: 2 })} />));
  fireEvent.click(await screen.findByRole('button', { name: '重新生成命令' }));
  expect(screen.getByRole('dialog', { name: '重新生成安装命令' })).toBeTruthy();
  view.rerender(wrap(<NodesPane win={win({ p: 'install', node: 'node-0', step: 2 })} />));
  expect(screen.queryByRole('dialog')).toBeNull();
  await act(async () => {
    client.setQueryData(['node-token-receipt', 'node-1'], {
      node_id: 'node-1',
      token: 'TEST_ONLY',
      token_prefix: 'different-test-prefix',
      install_command: 'TEST_ONLY_MACHINE_B',
    });
  });
  expect(screen.queryByText('TEST_ONLY_MACHINE_B')).toBeNull();
  view.rerender(wrap(<NodesPane win={win({ p: 'install', node: 'node-1', step: 2 })} />));
  expect(await screen.findByLabelText('安装命令')).toBeTruthy();
  expect(screen.getByLabelText('安装命令').textContent).toContain('TEST_ONLY_MACHINE_B');
});

it('receives a token requested before navigation under the original machine and restores it there', async () => {
  const { wrap, win } = setup();
  const fallback = globalThis.fetch;
  let finish!: (response: Response) => void;
  const pending = new Promise<Response>(resolve => {
    finish = resolve;
  });
  const request = vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
    String(input).endsWith('/agent-token') ? pending : fallback(input, init),
  );
  vi.stubGlobal('fetch', request);
  const view = render(wrap(<NodesPane win={win({ p: 'node', id: 'node-0', tab: 'config' })} />));
  fireEvent.click(screen.getByRole('button', { name: '更多操作' }));
  fireEvent.click(screen.getByRole('menuitem', { name: /重签 token/ }));
  await waitFor(() =>
    expect(request.mock.calls.some(([path]) => String(path) === '/nodes/node-0/agent-token')).toBe(true),
  );
  view.rerender(wrap(<NodesPane win={win({ p: 'node', id: 'node-1', tab: 'config' })} />));
  await act(async () => {
    finish(
      Response.json({
        node_id: 'node-0',
        token: 'TEST_ONLY_A',
        token_prefix: 'new-a',
        install_command: 'TEST_ONLY_COMMAND_A',
      }),
    );
  });
  expect(screen.queryByText('TEST_ONLY_COMMAND_A')).toBeNull();
  view.rerender(wrap(<NodesPane win={win({ p: 'node', id: 'node-0', tab: 'config' })} />));
  expect(await screen.findByText('TEST_ONLY_COMMAND_A')).toBeTruthy();
  expect(screen.getByText(/的 node token/).textContent).toContain('node-0');
});
