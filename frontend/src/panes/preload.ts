/**
 * Start fetching the routed page before the authenticated shell mounts.
 *
 * The URL lives in the hash, so the server cannot emit a route-specific modulepreload link. Keep
 * this table free of static page imports: App may consult it while `/bootstrap` is still running,
 * and each entry must preserve the page's existing split chunk.
 */
const PANE_LOADERS = {
  nodes: () => import('./nodes'),
  chains: () => import('./chains'),
  tunnels: () => import('./tunnels'),
  users: () => import('./users'),
  deploy: () => import('./deploy'),
  usage: () => import('./usage'),
  settings: () => import('./settings'),
  password: () => import('./password'),
  topo: () => import('../topo/canvas'),
} as const;

export type PreloadablePane = keyof typeof PANE_LOADERS;

/** Invalid and empty hashes follow the router's default and open the machine list. */
export function initialPaneFromHash(hash: string): PreloadablePane {
  const candidate = hash.replace(/^#\/?/, '').split('/', 1)[0];
  return Object.hasOwn(PANE_LOADERS, candidate) ? (candidate as PreloadablePane) : 'nodes';
}

export function preloadPaneForHash(hash: string): Promise<unknown> {
  return PANE_LOADERS[initialPaneFromHash(hash)]();
}

/** Return the node named by an exact machine-detail route.
 *
 * Keep this small parser beside the pane preloader rather than importing the full router into the
 * authentication entry point. Invalid percent escapes and partial routes follow the router's
 * normal fallback and therefore do not start a speculative detail fetch. */
export function initialNodeDetailFromHash(hash: string): string | null {
  // 查询段是观测时间范围（?range= / ?from=&to=），不属于机器标识。
  const parts = hash.replace(/^#\/?/, '').split('?', 1)[0].split('/').filter(Boolean);
  if (parts.length !== 3 || parts[0] !== 'nodes' || parts[1] !== 'node') return null;
  try {
    const nodeId = decodeURIComponent(parts[2]).trim();
    return nodeId || null;
  } catch {
    return null;
  }
}
