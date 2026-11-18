import { StrictMode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NodesPane } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import { draft } from '../src/draft';
import type { Win } from '../src/wm/store';
import { cacheEntries, responses, nodeId, snapshot } from './fixtures/node-detail-loading-data';

const clients: QueryClient[] = [];
const deferred = () => {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>(done => {
    resolve = done;
  });
  return { promise, resolve };
};
beforeEach(() => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((media: string) => ({
      matches: false,
      media,
      addEventListener() {},
      removeEventListener() {},
    })),
  );
});
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  draft.clear();
  vi.unstubAllGlobals();
});

function setup(path: string, tab = 'chains', readonly = false) {
  const gate = deferred();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  clients.push(client);
  for (const [url, key, value] of cacheEntries) if (url !== path) client.setQueryData(key, value);
  const fetcher = vi.fn((input: string) =>
    input === path ? gate.promise : Promise.resolve(Response.json(responses[input] ?? {})),
  );
  vi.stubGlobal('fetch', fetcher);
  const win: Win = {
    id: 1,
    key: 'tab:nodes',
    title: '机器',
    x: 0,
    y: 0,
    w: 1200,
    h: 900,
    z: 1,
    min: false,
    home: 'desk',
    data: { drill: { p: 'node', id: nodeId, tab } },
  };
  const view = render(
    <StrictMode>
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            who: {
              operator_id: 'test',
              role: readonly ? 'readonly' : 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
            initial: { node_count: 4, chain_group_count: [] },
          }}
        >
          <NodesPane win={win} />
        </SessionProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
  const finish = async (fail = false) => {
    await act(async () => {
      gate.resolve(Response.json(fail ? { error: '读取暂时失败' } : responses[path], { status: fail ? 503 : 200 }));
    });
  };
  return { ...view, client, fetcher, finish };
}

describe('machine detail asynchronous content', () => {
  it.each(['/settings', '/revisions?limit=50', '/compile/1'])(
    'keeps real chain headers and a busy flow while %s is pending, then reveals editors and save controls',
    async path => {
      const view = setup(path);
      const panel = view.getByRole('tabpanel', { name: /规则/ });
      await waitFor(() => expect(panel.querySelectorAll('.flow-well-loading')).toHaveLength(2));
      const head = panel.querySelector('.chain-rule-section-head');
      expect(head?.textContent).toContain(snapshot.snapshot.apps[0].chains[0].name);
      expect(panel.querySelectorAll('.flow-loading-label[role="status"]')).toHaveLength(2);
      expect(panel.querySelector('.flow-machine')).toBeNull();
      expect(panel.querySelector('.rule-draft-save')).toBeNull();
      expect(
        within(panel)
          .getAllByRole('switch')
          .every(button => button.hasAttribute('disabled')),
      ).toBe(true);
      await waitFor(() => expect(view.fetcher.mock.calls.some(([url]) => url === path)).toBe(true));
      await view.finish();
      await waitFor(() => expect(panel.querySelector('.flow-well-loading')).toBeNull());
      expect(panel.querySelector('.chain-rule-section-head')).toBe(head);
      expect(panel.querySelector('.flow-machine')).toBeTruthy();
      await waitFor(() => expect(within(panel).getByRole('button', { name: '保存到草稿' })).toBeTruthy());

      // A refetch must not replace already usable content with the initial loading surface.
      const firstMachine = panel.querySelector('.flow-machine');
      const refresh = deferred();
      view.fetcher.mockImplementation(input =>
        input === path ? refresh.promise : Promise.resolve(Response.json(responses[input] ?? {})),
      );
      const key = cacheEntries.find(([url]) => url === path)![1];
      act(() => {
        void view.client.invalidateQueries({ queryKey: key });
      });
      expect(panel.querySelector('.flow-machine')).toBe(firstMachine);
      expect(panel.querySelector('.flow-well-loading')).toBeNull();
      await act(async () => refresh.resolve(Response.json(responses[path])));
    },
  );

  it('ends initial flow loading with a readable error and no premature save bar', async () => {
    const view = setup('/compile/1');
    await waitFor(() => expect(view.container.querySelector('.flow-well-loading')).toBeTruthy());
    await view.finish(true);
    await view.findAllByText('读取暂时失败');
    expect(view.container.querySelector('.flow-well-loading')).toBeNull();
    expect(view.container.querySelector('.rule-draft-save')).toBeNull();
  });

  it('does not wait for settings that readonly viewers cannot read', async () => {
    const view = setup('/settings', 'chains', true);
    await waitFor(() => expect(view.container.querySelector('.flow-machine')).toBeTruthy());
    expect(view.container.querySelector('.flow-well-loading')).toBeNull();
    expect(view.fetcher.mock.calls.some(([url]) => url === '/settings')).toBe(false);
    expect(view.container.querySelector('.rule-draft-save')).toBeNull();
  });

  it('reserves the traffic card until real values arrive and shows read errors in its slot', async () => {
    const view = setup('/node-traffic', 'config');
    const pending = await view.findByRole('status', { name: '读取流量统计…' });
    expect(pending.closest('.panel')?.textContent).toContain('流量统计');
    expect(view.queryByRole('combobox', { name: '机器流量计量口径' })).toBeNull();
    await view.finish(true);
    await view.findByText('读取暂时失败');
    expect(view.queryByRole('status', { name: '读取流量统计…' })).toBeNull();
    expect(view.container.querySelector('.node-config-card-state[data-kind="usage"]')).toBeTruthy();
  });

  it('places the DNS waiting card before independently loading maintenance', async () => {
    const view = setup('/model/snapshot', 'config');
    await view.findByRole('status', { name: '读取DNS…' });
    await view.findByText('IP 养护');
    const dns = view.container.querySelector('.node-config-card-state[data-kind="dns"]');
    expect(dns?.nextElementSibling?.textContent).toContain('IP 养护');
    await view.finish();
    await waitFor(() => expect(view.container.querySelector('.node-config-card-state[data-kind="dns"]')).toBeNull());
  });

  it('keeps unlock reads quiet and reveals real service tiles after initial and family requests', async () => {
    const path = `/nodes/${nodeId}/unlock-probe?family=v4`;
    const view = setup(path, 'unlock');
    const panel = () => view.container.querySelector('.node-unlock');
    await waitFor(() => expect(panel()?.getAttribute('aria-busy')).toBe('true'));
    expect(view.queryByText('读取解锁结果…')).toBeNull();
    expect(view.queryByRole('article', { name: 'ChatGPT' })).toBeNull();
    await view.finish();
    await view.findByRole('article', { name: 'ChatGPT' });
    expect(panel()?.getAttribute('aria-busy')).toBe('false');
    const v6 = deferred();
    view.fetcher.mockImplementation(input =>
      input.endsWith('family=v6') ? v6.promise : Promise.resolve(Response.json(responses[input] ?? {})),
    );
    fireEvent.click(view.getByRole('button', { name: 'IPv6' }));
    await waitFor(() => expect(panel()?.getAttribute('aria-busy')).toBe('true'));
    expect(view.queryByText('读取解锁结果…')).toBeNull();
    await act(async () => v6.resolve(Response.json(responses[`/nodes/${nodeId}/unlock-probe?family=v6`])));
    await view.findByRole('article', { name: 'ChatGPT' });
  });
});
