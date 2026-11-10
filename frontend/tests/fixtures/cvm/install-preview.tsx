// Synthetic transport and cache for the real machine installation component.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ProvisionInstall } from '../../../src/panes/nodes';
import { SessionProvider } from '../../../src/session';
import { theme } from '../../../src/forge/theme';
import type { ProvisionNodeResult } from '../../../src/api';
import { agentNodes } from '../console-model-data';
import { cvmWorkspace } from './data';
import '../../../src/styles.css';

const parameters = new URLSearchParams(location.search);
const online = parameters.get('online') === '1';
if (theme.snapshot() !== (parameters.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const now = new Date().toISOString();
const node = {
  ...agentNodes()[0],
  token_last_used_at: online ? now : null,
  last_poll_at: online ? now : null,
  token_prefix: 'fixture-only',
  name: '香港 CVM 承载机器',
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['nodes'], { nodes: [node] });
client.setQueryData(['revisions'], { current_revision: 7, revisions: [] });
client.setQueryData(['certs'], { domain: null, groups: [], nodes: [], sealing_available: true });
client.setQueryData(['cvm'], cvmWorkspace('detail'));
const result: ProvisionNodeResult = {
  revision_id: 7,
  node: {
    id: node.node_id,
    tenant_id: 'platform',
    name: node.name,
    public_ipv4: '203.0.113.11',
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    overlay_addr: '10.66.0.1',
    wg_public_key: 'fixture-public-key',
    wg_listen_port: 51820,
    api_port: 10085,
    overlay: true,
    egress_allowed: true,
    dns: { t: 'system' },
    domain_strategy: 'as_is',
  },
  enrollment: {
    token: 'fixture-only-not-a-credential',
    token_prefix: 'fixture-only',
    expires_at: null,
    script_url: 'https://console.invalid/enroll/install.sh',
    script_sha256: 'a'.repeat(64),
    install_command: 'sh brocade-install.sh --server https://console.invalid',
  },
};
window.fetch = async () => Response.json({ error: 'No business write is provided by this fixture' }, { status: 404 });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'fixture-admin',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 1, chain_group_count: [] },
      }}
    >
      <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
        <main className="fg-desk" style={{ padding: 12 }}>
          <ProvisionInstall node={node.node_id} result={result} go={() => {}} />
        </main>
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
