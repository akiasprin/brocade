import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { DraftBar } from '../../src/forge/draft-bar';
import { DraftStorage } from '../../src/draft-storage';
import { draft, type DraftEntry } from '../../src/draft';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const owner = 'coverage-three-branches';
const storage = new DraftStorage(owner, localStorage);
for (const key of Object.keys(localStorage)) {
  if (key.startsWith(storage.prefix)) localStorage.removeItem(key);
}
// Separate branch heads model concurrent writes from the same parent. The live audit
// separately verifies delivery and resolution across three actual browser tabs.
for (let branch = 0; branch < 3; branch++) {
  const entries: DraftEntry[] = Array.from({ length: 24 }, (_, i) => ({
    key: `node:coverage-${i}`,
    label: `东京新加坡备援机器 ${i} / ${'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.repeat(2)}`,
    editId: `branch-${branch}-edit-${i}`,
    op: { op: 'update_node', node_id: `coverage-${i}`, node: { name: `分支 ${branch + 1} 机器 ${i}` } },
  }));
  storage.write(entries, []);
}
draft.init(owner);
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['nodes'], { nodes: [] });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: owner,
          role: 'system-admin',
          tenant_scope: null,
          masked_assets: false,
          token_prefix: null,
        },
        initial: { node_count: 24, chain_group_count: [] },
      }}
    >
      <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
        <main className="fg-desk">
          <p>生产草稿组件 · 三份各 24 项的冲突记录 · 仅浏览器存储</p>
        </main>
        <DraftBar current={14} />
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
