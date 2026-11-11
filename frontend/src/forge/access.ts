import { CVM_ENABLED } from '../product';
import type { Whoami } from '../api';
import { can, isPublic } from '../session';
import type { Loc } from './route';

/**
 * Keep a restored/deep-linked route inside the surface the current identity can actually open.
 *
 * This runs before a pane mounts. Disabling buttons alone is insufficient: the hash survives a
 * logout, so a public visitor can otherwise inherit an administrator's settings or deployment
 * route and immediately start queries which the server must reject.
 */
export function routeForViewer(location: Loc, who: Whoami): Loc {
  const fallback: Loc = { nav: who.role === 'user' ? 'users' : 'nodes' };

  if (location.nav === 'cvm' && !CVM_ENABLED) return fallback;

  if ((location.nav === 'settings' || location.nav === 'deploy') && !can(who.role, 'edit')) return fallback;
  if (location.nav === 'capacity' && (who.role === 'user' || isPublic(who))) return fallback;
  if (location.nav === 'password' && isPublic(who)) return fallback;

  const page = location.drill?.p;
  if (location.nav === 'cvm' && page === 'new' && !['user', 'tenant-admin', 'system-admin'].includes(who.role))
    return { nav: 'cvm' };
  if (location.nav === 'nodes') {
    if ((page === 'provision' || page === 'install') && !can(who.role, 'system')) return { nav: 'nodes' };
    if (page === 'chain' && !can(who.role, 'edit')) return { nav: 'nodes' };
  }
  if (location.nav === 'chains' && page === 'new' && !can(who.role, 'edit')) return { nav: 'chains' };

  return location;
}
