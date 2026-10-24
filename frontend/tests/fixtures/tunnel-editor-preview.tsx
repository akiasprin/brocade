import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ExternalOutboundEditor } from '../../src/panes/rules';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'jinzi';
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <ExternalOutboundEditor
      tenantId="platform"
      purpose="resource"
      existing={{
        id: 'custom-1111-1111',
        tenant: 'platform',
        name: 'AnyTLS 供应商',
        address: 'edge.example.com',
        port: 443,
        protocol: { t: 'anytls', v: { credential: '<redacted>' } },
        security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
        bindings: [],
      }}
      onClose={() => undefined}
      onSaved={() => undefined}
    />
  </QueryClientProvider>,
);
