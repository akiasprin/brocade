// The accepted demo now runs the complete production page and policy editor, with a local Console.
import { createRoot } from 'react-dom/client';
import { useEffect, useSyncExternalStore } from 'react';
import { QueryClientProvider } from '@tanstack/react-query';
import { ForgeShell } from '../../src/forge/shell';
import { theme } from '../../src/forge/theme';
import { SessionProvider } from '../../src/session';
import { wm } from '../../src/wm/store';
import { client, fillQuota, newCycle, session, users } from './user-app-rate-demo-data';
import '../../src/styles.css';
import './user-app-rate-demo.css';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'dark' ? 'dark' : 'light')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
history.replaceState(null, '', `${location.pathname}${location.search}${location.hash || '#/users/user/alice'}`);
function Controls() {
  const state = useSyncExternalStore(wm.subscribe, wm.snapshot);
  const drill = state.wins.find(win => win.key === 'tab:users')?.data.drill as { p?: string; id?: string } | undefined;
  const user = users.find(user => drill?.p === 'user' && user.id === drill.id) ?? users[0];
  useEffect(() => {
    if (!params.get('edit')) return;
    const place = () => {
      const button = Array.from(document.querySelectorAll<HTMLButtonElement>('.qta-pencil')).find(button =>
        button
          .getAttribute('aria-label')
          ?.startsWith(params.get('edit') === 'apac-premium' ? 'APAC Premium' : 'APAC Promising'),
      );
      if (button && !button.disabled) {
        observer.disconnect();
        button.click();
      }
    };
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    place();
    return () => observer.disconnect();
  }, []);
  return params.get('controls') === '0' ? null : (
    <aside className="rate-demo-tools" aria-label="本地演示控制">
      <span>本地演示</span>
      <button className="btn" type="button" onClick={() => fillQuota(user.id)}>
        模拟 APAC Premium 用满
      </button>
      <button className="btn" type="button" onClick={() => newCycle(user.id)}>
        新账期
      </button>
    </aside>
  );
}
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider value={session}>
      <ForgeShell branding={{ site_name: 'TAT.AC', icon_data_url: null }} session={session} onLogout={() => {}} />
      <Controls />
    </SessionProvider>
  </QueryClientProvider>,
);
