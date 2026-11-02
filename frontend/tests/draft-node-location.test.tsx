import { StrictMode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { draft } from '../src/draft';
import { DraftBar } from '../src/forge/draft-bar';
import {
  clearNodeDraftFocus,
  focusNodeDraftPanel,
  nodeDraftLocations,
  nodeDraftPanelKey,
  useNodeDraftFocus,
  useDraftPanelFocusStatus,
} from '../src/forge/draft-node-location';
import { navigate } from '../src/forge/route';
import { SessionProvider } from '../src/session';

vi.mock('../src/forge/route', async importOriginal => ({
  ...(await importOriginal<typeof import('../src/forge/route')>()),
  navigate: vi.fn(() => true),
}));

const originalScroll = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'scrollIntoView');

afterEach(() => {
  cleanup();
  clearNodeDraftFocus();
  draft.clear();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalScroll) Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', originalScroll);
  else Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
});

const machines = [
  { node_id: 'n1', name: '香港示例 01' },
  { node_id: 'n2', name: '东京示例 01' },
];
const scroll = vi.fn();
function installBrowser() {
  scroll.mockClear();
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({ matches: query.includes('reduce'), addEventListener() {}, removeEventListener() {} })),
  );
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: scroll });
}
function Target({ available = true, nodeId = 'n1' }: { available?: boolean; nodeId?: string }) {
  useNodeDraftFocus(nodeId);
  const request = useDraftPanelFocusStatus();
  return (
    <>
      <output>{request?.status}</output>
      {available && (
        <details data-draft-panel={nodeDraftPanelKey(nodeId, 'connection')}>
          <summary>连接策略</summary>
          <input aria-label="连接配置" />
        </details>
      )}
    </>
  );
}

describe('machine draft descriptions', () => {
  it('derives every changed panel from a merged operation, using the pending name', () => {
    draft.push({ op: 'update_node', node_id: 'n1', node: { dns: { t: 'system' }, domain_strategy: 'as_is' } });
    draft.push({ op: 'update_node', node_id: 'n1', node: { name: '香港新名称', wg_listen_port: 51821, mtu: 1420 } });
    const entries = draft.snapshot();
    expect(entries).toHaveLength(1);
    expect(nodeDraftLocations(entries[0], machines, entries)).toEqual({
      name: '香港新名称',
      targets: [
        { nodeId: 'n1', panel: 'identity', label: '身份' },
        { nodeId: 'n1', panel: 'wireguard', label: 'WIREGUARD' },
        { nodeId: 'n1', panel: 'dns', label: 'DNS' },
      ],
    });
  });
  it('uses actual names for certificate, DNS policy, rule and WireGuard link operations', () => {
    draft.push({ op: 'set_node_cert_group', node_id: 'n1', label_id: 'cert' });
    draft.push({ op: 'reorder_node_egress_dns', node_id: 'n1', selectors: [] });
    draft.push({ op: 'delete_step', node_id: 'n1', app_id: 'a', chain_id: 'c' });
    draft.push({ op: 'set_wireguard_link_disabled', a: 'n1', b: 'n2', disabled: true });
    const entries = draft.snapshot();
    const views = entries.map(entry => nodeDraftLocations(entry, machines, entries));
    expect(views.slice(0, 3).map(view => [view?.name, view?.targets[0].label])).toEqual([
      ['香港示例 01', '证书组'],
      ['香港示例 01', 'DNS 解析策略'],
      ['香港示例 01', '链路规则'],
    ]);
    expect(views[3]?.targets.map(target => target.label)).toEqual([
      '香港示例 01 · WIREGUARD',
      '东京示例 01 · WIREGUARD',
    ]);
  });
  it('shows one machine entry with separate panel links without changing discard semantics', async () => {
    draft.push({ op: 'update_node', node_id: 'n1', node: { wg_listen_port: 51821, dns: { t: 'system' } } });
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['nodes'], { nodes: machines });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            who: {
              operator_id: 'test',
              role: 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
            initial: { node_count: 2, chain_group_count: [] },
          }}
        >
          <DraftBar current={1956} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('button', { name: /1 处未提交/ }));
    expect(view.container.querySelectorAll('.fg-draft-list li')).toHaveLength(1);
    expect(view.getByText('香港示例 01')).toBeTruthy();
    expect(view.getByRole('button', { name: '定位 香港示例 01 的WIREGUARD面板' })).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '定位 香港示例 01 的DNS面板' }));
    expect(navigate).toHaveBeenCalledWith('nodes', { p: 'node', id: 'n1', tab: 'config' });
    expect(draft.ops()).toHaveLength(1);
    expect(view.container.querySelector('.fg-draft-list')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: /1 处未提交/ }));
    fireEvent.click(view.getByRole('button', { name: '丢弃' }));
    expect(draft.isEmpty()).toBe(true);
    client.clear();
  });
});

describe('machine panel focus lifecycle', () => {
  it('does not retain a focus request after its page unmounts', async () => {
    installBrowser();
    const view = render(<Target available={false} />);
    act(() => {
      focusNodeDraftPanel('n1', 'connection');
    });
    view.unmount();
    await act(async () => {});
    render(<Target />);
    expect(scroll).not.toHaveBeenCalled();
    expect(document.querySelector('[data-draft-highlight]')).toBeNull();
  });

  it('does not publish a request when navigation fails', () => {
    vi.mocked(navigate).mockReturnValueOnce(false);
    expect(focusNodeDraftPanel('n1', 'dns')).toBe(false);
    const view = render(<Target available={false} />);
    expect(view.container.querySelector('output')?.textContent).toBe('');
  });

  it('switches the real machine detail from rules to configuration and back on repeated same-machine requests', async () => {
    installBrowser();
    const { NodesPane } = await import('../src/panes/nodes');
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['nodes'], {
      nodes: [{ ...machines[0], dns: { t: 'system' }, domain_strategy: 'as_is', overlay: false }],
    });
    client.setQueryData(['snapshot'], {
      snapshot: {
        revision: 1,
        nodes: [],
        apps: [],
        external_outbounds: [],
        settings: { overlay: { disabled_links: [] } },
      },
      node_egress_dns: [],
      redacted: false,
    });
    client.setQueryData(['certs'], { groups: [], nodes: [] });
    client.setQueryData(['node-traffic'], { nodes: [] });
    client.setQueryData(['agent-log-policy'], { nodes: [] });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string) =>
        Response.json(
          path === '/nodes/agent-state'
            ? { nodes: [{ ...machines[0], dns: { t: 'system' }, domain_strategy: 'as_is', overlay: false }] }
            : {},
        ),
      ),
    );
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            who: {
              operator_id: 'test',
              role: 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
            initial: { node_count: 1, chain_group_count: [] },
          }}
        >
          <NodesPane
            win={{
              id: 1,
              key: 'tab:nodes',
              title: '机器',
              x: 0,
              y: 0,
              w: 1000,
              h: 800,
              z: 1,
              min: false,
              home: 'desk',
              data: { drill: { p: 'node', id: 'n1', tab: 'chains' } },
            }}
          />
        </SessionProvider>
      </QueryClientProvider>,
    );
    expect(view.getByRole('tab', { name: '规则' }).getAttribute('aria-selected')).toBe('true');
    act(() => {
      focusNodeDraftPanel('n1', 'dns');
    });
    await waitFor(() => expect(view.getByRole('tab', { name: '配置' }).getAttribute('aria-selected')).toBe('true'));
    await waitFor(() =>
      expect(view.container.querySelector('[data-draft-highlight]')?.getAttribute('data-draft-panel')).toBe(
        nodeDraftPanelKey('n1', 'dns'),
      ),
    );
    act(() => {
      focusNodeDraftPanel('n1', 'egress-dns');
    });
    await waitFor(() => expect(view.getByRole('tab', { name: '规则' }).getAttribute('aria-selected')).toBe('true'));
    await waitFor(() =>
      expect(view.container.querySelector('[data-draft-highlight]')?.getAttribute('data-draft-panel')).toBe(
        nodeDraftPanelKey('n1', 'egress-dns'),
      ),
    );
    client.clear();
  });

  it('waits for asynchronous content, survives StrictMode and opens the target details', async () => {
    installBrowser();
    focusNodeDraftPanel('n1', 'connection');
    const view = render(
      <StrictMode>
        <Target available={false} />
      </StrictMode>,
    );
    await act(async () => {});
    expect(view.getByText('pending')).toBeTruthy();
    expect(scroll).not.toHaveBeenCalled();
    view.rerender(
      <StrictMode>
        <Target />
      </StrictMode>,
    );
    await waitFor(() => expect(view.getByText('located')).toBeTruthy());
    const target = view.container.querySelector('details')!;
    expect(target.open).toBe(true);
    expect(target.dataset.draftHighlight).toBe('true');
    expect(document.activeElement).toBe(target);
    expect(scroll).toHaveBeenCalledWith({ block: 'center', behavior: 'auto' });
  });
  it('does not focus a different machine and times out without leaving a running observer', async () => {
    installBrowser();
    vi.useFakeTimers();
    const view = render(<Target nodeId="n2" />);
    act(() => {
      focusNodeDraftPanel('n1', 'connection');
    });
    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    expect(view.getByText('unavailable')).toBeTruthy();
    expect(scroll).not.toHaveBeenCalled();
  });
  it('clears a previous highlight when locating again, and expires the last highlight', async () => {
    installBrowser();
    vi.useFakeTimers();
    const view = render(<Target />);
    act(() => {
      focusNodeDraftPanel('n1', 'connection');
    });
    await act(async () => {
      vi.advanceTimersByTime(100);
    });
    expect(view.container.querySelector('[data-draft-highlight]')).not.toBeNull();
    act(() => {
      focusNodeDraftPanel('n1', 'connection');
    });
    expect(view.container.querySelector('[data-draft-highlight]')).toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(100);
    });
    expect(view.container.querySelector('[data-draft-highlight]')).not.toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(4000);
    });
    expect(view.container.querySelector('[data-draft-highlight]')).toBeNull();
    expect(view.container.querySelector('details')?.hasAttribute('tabindex')).toBe(false);
  });
});
