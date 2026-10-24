import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ConsoleSnapshot, ExternalOutbound } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { TunnelsPane } from '../../src/panes/tunnels';
import { SessionProvider } from '../../src/session';
import type { Win } from '../../src/wm/store';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'jinzi';

// Keep enough real rows to expose truncation and scrolling with both short and long labels.
const tunnels: ExternalOutbound[] = Array.from({ length: 60 }, (_, index) => ({
  id: `custom-${String(index + 1).padStart(4, '0')}-1111`,
  tenant: 'platform',
  name: index === 0 ? '审查出口 01' : `多区域供应商出口 ${index + 1} · 香港 / 东京 / Singapore`,
  address: index % 2 === 0 ? `edge-${index + 1}.example.com` : '2001:db8::1234',
  port: 443,
  protocol: { t: 'http_connect', v: { credential: '' } },
  security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
  bindings: [],
}));
const snapshot: ConsoleSnapshot = {
  snapshot: { revision: 1, apps: [], external_outbounds: tunnels },
  node_egress_dns: [],
  redacted: false,
};
const client = new QueryClient({
  defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
});
client.setQueryData(['snapshot'], snapshot);
const win: Win = {
  id: 9001,
  key: 'tab:tunnels',
  title: '隧道',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: {},
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'editor',
          role: 'editor',
          tenant_scope: 'platform',
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 0, chain_group_count: [] },
      }}
    >
      <TunnelsPane win={win} />
    </SessionProvider>
  </QueryClientProvider>,
);
