import { useEffect } from 'react';
import { useQueryClient, type QueryClient } from '@tanstack/react-query';

const QUERY_GROUPS: Record<string, readonly string[]> = {
  nodes: ['nodes'],
  load: ['node-load-list', 'node-nic-list', 'node-load-history', 'node-load-metrics'],
  traffic: ['node-traffic', 'usage-node-series', 'usage-chain'],
  e2e: ['e2e-probes'],
  unlock: ['chain-unlock-probes', 'node-unlock-probe', 'chain-unlock-probe'],
  deployments: ['deployments'],
  tunnel_probes: ['tunnel-probes', 'tunnel-probe'],
};

/** One stream per query cache. Hints refresh active observation reads, never model snapshots or
 * controlled editors. Existing query intervals remain the fallback for old Consoles and proxies. */
function createSource(client: QueryClient) {
  let users = 0;
  let source: EventSource | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let reopenTimer: ReturnType<typeof setTimeout> | undefined;
  let pending = new Set<string>();
  let flushing = false;

  const schedule = () => {
    if (!users || timer || flushing || !pending.size) return;
    // A batch of reports from many Agents must not cancel reads or fetch the fleet per Agent.
    timer = setTimeout(() => {
      timer = undefined;
      const keys = pending;
      pending = new Set();
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
    }, 1_000);
  };
  const enqueue = (domains: unknown) => {
    if (!Array.isArray(domains)) return;
    for (const domain of domains) {
      if (typeof domain !== 'string' || !Object.hasOwn(QUERY_GROUPS, domain)) continue;
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
