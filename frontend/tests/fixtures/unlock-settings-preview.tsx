import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { UnlockSection } from '../../src/panes/settings';
import type { DeploymentSettings } from '../../src/api';
import '../../src/styles.css';
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
function Preview() {
  const [settings, setSettings] = useState<DeploymentSettings>({
    unattended: true,
    xray_handoff_beta: false,
    unlock_detection_mode: 'scheduled',
    unlock_detection_interval_hours: 6,
  });
  window.fetch = async (path, init) => {
    if (String(path) === '/deployment-settings' && init?.method === 'PUT') {
      const next = { ...settings, ...JSON.parse(String(init.body)) };
      setSettings(next);
      return Response.json(next);
    }
    if (String(path) === '/unlock-probes/refresh')
      return Response.json({
        mode: settings.unlock_detection_mode,
        interval_hours: settings.unlock_detection_interval_hours,
        requested_at: 1,
        next_run_at: null,
      });
    throw new Error('Unexpected preview request');
  };
  return (
    <div className="cardpage">
      <div className="duo settings-layout">
        <div className="col">
          <fieldset className="settings-column-fields">
            <UnlockSection editable settings={settings} />
          </fieldset>
        </div>
      </div>
    </div>
  );
}
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <Preview />
  </QueryClientProvider>,
);
