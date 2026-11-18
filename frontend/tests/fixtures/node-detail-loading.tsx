// Production machine detail on synthetic API data; requests can be held/released independently.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NodesPane } from '../../src/panes/nodes';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import { palette } from '../../src/forge/palette';
import { draft } from '../../src/draft';
import type { Win } from '../../src/wm/store';
import { installBackend } from './console-backend';
import { cacheEntries, responses, nodeId } from './node-detail-loading-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
palette.set('dailan');
draft.init('machine-loading-fixture');
installBackend();
const backend = globalThis.fetch;
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
const gates = new Map<string, { promise: Promise<boolean>; release: (fail: boolean) => void }>();
const requests: string[] = [];
const hold = (path: string) => {
  let release!: (fail: boolean) => void;
  const promise = new Promise<boolean>(done => {
    release = done;
  });
  gates.set(path, { promise, release });
};
for (const path of params.getAll('hold')) hold(path);
for (const [path, key, value] of cacheEntries) if (!gates.has(path)) client.setQueryData(key, value);
globalThis.fetch = async (input, init) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const path = url.pathname + url.search;
  requests.push(path);
  const gate = gates.get(path);
  if (gate && (await gate.promise)) return Response.json({ error: '合成接口暂时不可用' }, { status: 503 });
  if (path in responses) return Response.json(responses[path]);
  return backend(input, init);
};
Object.assign(window, {
  __nodeLoading: {
    requests,
    hold,
    release: (path: string, fail = false) => {
      gates.get(path)?.release(fail);
      gates.delete(path);
    },
    refresh: (key: readonly unknown[]) => {
      void client.invalidateQueries({ queryKey: key });
    },
  },
});
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
  data: { drill: { p: 'node', id: nodeId, tab: params.get('tab') ?? 'chains' } },
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'fixture-admin',
          role: params.has('readonly') ? 'readonly' : 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 4, chain_group_count: [] },
      }}
    >
      <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
        <div className="fg-left">
          <main className="fg-desk" style={{ padding: 16 }}>
            <div className="fg-view">
              <NodesPane win={win} bare />
            </div>
          </main>
        </div>
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
