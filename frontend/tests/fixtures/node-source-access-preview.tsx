// Production App and machine configuration, using the same scenarios as the approved C mockup.
// No candidate panel, portal or mockup styles are loaded here.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { theme } from '../../src/forge/theme';
import { installBackend, NODE, resetWorld, SCENE_KEYS, type SceneKey } from './node-source-ban-mockup-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'typical';
installBackend();
resetWorld(SCENE_KEYS.includes(scene as SceneKey) ? (scene as SceneKey) : 'typical');
observeBrowserAppearance();
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
location.hash = `#/nodes/node/${NODE}`;
const observer = new MutationObserver(() => {
  const tab = [...document.querySelectorAll<HTMLButtonElement>('#root [role="tab"]')].find(
    item => item.textContent?.trim() === '配置',
  );
  if (!tab) return;
  observer.disconnect();
  if (tab.getAttribute('aria-selected') !== 'true') tab.click();
});
observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
