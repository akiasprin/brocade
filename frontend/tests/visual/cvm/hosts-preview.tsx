import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { AdoptForm } from '../../../src/panes/cvm/adopt';
import { HostList } from '../../../src/panes/cvm/hosts';
import { CvmPageHeader } from '../../../src/panes/cvm/presentation';
import { baremetalWorkspace, cvmWorkspace } from '../../fixtures/cvm/data';
import '../../../src/styles.css';
import '../../../src/panes/cvm/styles.css';

const params = new URLSearchParams(location.search);
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
document.documentElement.dataset.palette = 'dailan';
const workspace = cvmWorkspace('list');
workspace.hosts[0].capabilities.host_system = { os: 'Debian 13', primary_device: 'eth0' };
workspace.hosts.push({
  ...baremetalWorkspace('baremetal-hosts').hosts[0],
  name: '构建裸机',
  node_id: 'build-host',
});

function Preview() {
  const [adopting, setAdopting] = useState(params.get('view') === 'adopt');
  const [createHost, setCreateHost] = useState('');
  return (
    <main className="cvm-page nd-page nd-sheet node-cardpage cvm-design">
      <div className="fg-sheet nd-paper">
        <CvmPageHeader title={adopting ? '纳管裸机' : '宿主'} />
        <div className="nd-paper-body">
          {adopting ? (
            <AdoptForm workspace={workspace} onViewHosts={() => setAdopting(false)} />
          ) : (
            <HostList workspace={workspace} onCreateOnHost={setCreateHost} />
          )}
          {createHost && <output>{createHost}</output>}
        </div>
      </div>
    </main>
  );
}

const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <Preview />
  </QueryClientProvider>,
);
