import { useEffect } from 'react';
import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { invalidateDeploymentReads } from './api';

const QUERY_GROUPS: Record<string, readonly string[]> = {
  nodes: ['nodes'],
  load: ['node-load-list', 'node-nic-list', 'node-load-history', 'node-load-metrics'],
  traffic: ['node-traffic', 'usage-node-series', 'usage-chain'],
  e2e: ['e2e-probes'],
  unlock: ['chain-unlock-probes', 'node-unlock-probe', 'chain-unlock-probe'],
  deployments: ['deployments', 'revisions'],
  tunnel_probes: ['tunnel-probes', 'tunnel-probe'],
};
// These are whole-fleet snapshots. A fleet reporting continuously must not turn one-second
// notification batches into one full HTTP download per second. Faster job hints stay immediate.
const REFRESH_MIN_MS: Record<string, number> = {
  nodes: 10_000,
  'node-load-list': 30_000,
  'node-nic-list': 30_000,
  'node-load-history': 30_000,
  'node-load-metrics': 30_000,
  'node-traffic': 30_000,
  'usage-node-series': 30_000,
  'usage-chain': 30_000,
};

/** One stream per query cache. Hints refresh active observation reads, never model snapshots or
 * controlled editors. Existing query intervals remain the fallback for old Consoles and proxies. */
function createSource(client: QueryClient) {
  let users = 0;
  let source: EventSource | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let timerDueAt = 0;
  let reopenTimer: ReturnType<typeof setTimeout> | undefined;
  const pending = new Set<string>();
  let flushing = false;
  const lastRefreshed = new Map<string, number>();

  const schedule = () => {
    if (!users || flushing || !pending.size) return;
    // A batch of reports from many Agents must not cancel reads or fetch the fleet per Agent.
    const now = Date.now();
    const dueAt = (key: string) => (lastRefreshed.get(key) ?? -Infinity) + (REFRESH_MIN_MS[key] ?? 0);
    const delay = Math.max(1_000, Math.min(...[...pending].map(key => dueAt(key) - now)));
    if (timer && timerDueAt <= now + delay) return;
    if (timer) clearTimeout(timer);
    timerDueAt = now + delay;
    timer = setTimeout(() => {
      timer = undefined;
      const now = Date.now();
      const keys = new Set([...pending].filter(key => dueAt(key) <= now));
      for (const key of keys) {
        pending.delete(key);
        lastRefreshed.set(key, now);
      }
      flushing = true;
      void client
        .invalidateQueries(
          { predicate: query => keys.has(String(query.queryKey[0])), refetchType: 'active' },
          { cancelRefetch: false },
        )
        .finally(() => {
          flushing = false;
          schedule();
        });
    }, delay);
  };
  const enqueue = (domains: unknown) => {
    if (!Array.isArray(domains)) return;
    for (const domain of domains) {
      if (typeof domain !== 'string' || !Object.hasOwn(QUERY_GROUPS, domain)) continue;
      if (domain === 'nodes' || domain === 'deployments') invalidateDeploymentReads();
      for (const key of QUERY_GROUPS[domain]) pending.add(key);
    }
    schedule();
  };
  const close = () => {
    source?.close();
    source = undefined;
    if (timer) clearTimeout(timer);
    if (reopenTimer) clearTimeout(reopenTimer);
    timer = undefined;
    reopenTimer = undefined;
    pending.clear();
    lastRefreshed.clear();
  };
  const open = () => {
    if (!users || source || document.visibilityState === 'hidden' || typeof EventSource === 'undefined') return;
    const opened = new EventSource('/realtime/observations/events', { withCredentials: true });
    source = opened;
    opened.addEventListener('ready', () => {
      if (source === opened) enqueue(Object.keys(QUERY_GROUPS));
    });
    opened.addEventListener('invalidate', raw => {
      if (source !== opened) return;
      try {
        enqueue(JSON.parse((raw as MessageEvent).data));
      } catch {
        // A malformed hint cannot supply data; ordinary read queries retain their polling fallback.
      }
    });
    opened.addEventListener('reset', () => {
      if (source !== opened) return;
      close();
      reopenTimer = setTimeout(open, 0);
    });
    // EventSource retries the connection; each ready frame refreshes reads missed while offline.
    opened.onerror = () => undefined;
  };
  const visibility = () => (document.visibilityState === 'hidden' ? close() : open());
  return {
    retain() {
      users += 1;
      if (users === 1) {
        document.addEventListener('visibilitychange', visibility);
        open();
      }
      return () => {
        users -= 1;
        if (users === 0) {
          document.removeEventListener('visibilitychange', visibility);
          close();
        }
      };
    },
  };
}

const sources = new WeakMap<QueryClient, ReturnType<typeof createSource>>();

export function useObservationUpdates() {
  const client = useQueryClient();
  useEffect(() => {
    let source = sources.get(client);
    if (!source) {
      source = createSource(client);
      sources.set(client, source);
    }
    return source.retain();
  }, [client]);
}
