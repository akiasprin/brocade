import { useCallback, useEffect, useState } from 'react';

import { mergeNodeRealtimeEvent, type NodeRealtimeEvent } from './node-realtime';
import { realtimeFacts } from './realtime-facts';
import { olderRealtimeEvent } from './realtime-order';
import { useNow } from './ui/clock';
import { useMetricValue } from './ui/metric-cadence';

export interface FleetTrafficExpectedUser {
  tenant_id: string;
  user_id: string;
}

export interface FleetTrafficExpectedNode {
  node_id: string;
  users: FleetTrafficExpectedUser[];
}

export interface FleetTrafficNode {
  node_id: string;
  connected: boolean;
  active: boolean;
  interval_secs: number;
  samples: NodeRealtimeEvent[];
}

export interface FleetTrafficSnapshot {
  policy: { enabled: boolean };
  nodes: FleetTrafficNode[];
  /** SSE snapshots omit grants; absence is unknown until an HTTP snapshot supplies them. */
  expected?: FleetTrafficExpectedNode[];
}

interface FleetTrafficState {
  payload: FleetTrafficSnapshot | null;
  failed: boolean;
}

const EMPTY: FleetTrafficState = { payload: null, failed: false };
const FALLBACK_POLL_MS = 5_000;

function mergeNodes(previous: FleetTrafficNode[], incoming: FleetTrafficNode[]): FleetTrafficNode[] {
  const before = new Map(previous.map(node => [node.node_id, node]));
  return incoming.map(node => {
    const old = before.get(node.node_id);
    const event = node.samples.at(-1);
    const prior = old?.samples.at(-1);
    if (!event || !prior || !olderRealtimeEvent(event, prior)) return node;
    return { ...node, samples: [prior] };
  });
}

/** One demand lease for the whole fleet page. It stays separate from the global header's NIC-only
 * stream: opening any Console route must not permanently enable per-user sampling on every node. */
export function useFleetTrafficSource(enabled = true) {
  const [state, setState] = useState<FleetTrafficState>(EMPTY);
  const now = useNow();

  const update = useCallback((change: (previous: FleetTrafficSnapshot | null) => FleetTrafficSnapshot | null) => {
    setState(previous => ({ payload: change(previous.payload), failed: false }));
  }, []);

  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let source: EventSource | undefined;
    let pollTimer: ReturnType<typeof setTimeout> | undefined;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
    let pollAbort: AbortController | undefined;
    let polling = false;
    let fetching = false;
    let entitlementsKnown = false;
    let snapshotFailures = 0;
    let reconnectFailures = 0;
    const visible = () => !stopped && document.visibilityState !== 'hidden';

    const acceptSnapshot = (next: FleetTrafficSnapshot) => {
      if (stopped || !Array.isArray(next.nodes)) return;
      update(previous => ({
        policy: next.policy,
        nodes: mergeNodes(previous?.nodes ?? [], next.nodes),
        expected: Array.isArray(next.expected) ? next.expected : previous?.expected,
      }));
    };

    const fetchSnapshot = async () => {
      if (!visible() || fetching) return;
      if (pollTimer) clearTimeout(pollTimer);
      fetching = true;
      const abort = new AbortController();
      pollAbort = abort;
      const timeout = setTimeout(() => abort.abort(), 15_000);
      try {
        const response = await fetch('/realtime/fleet-traffic/snapshot', {
          credentials: 'include',
          cache: 'no-store',
          signal: abort.signal,
        });
        if (!response.ok) throw new Error(`fleet traffic snapshot returned ${response.status}`);
        const next = (await response.json()) as FleetTrafficSnapshot;
        if (!Array.isArray(next.nodes) || !Array.isArray(next.expected)) {
          throw new Error('fleet traffic snapshot omitted nodes or authorization');
        }
        acceptSnapshot(next);
        entitlementsKnown = true;
        snapshotFailures = 0;
      } catch {
        if (visible()) {
          snapshotFailures += 1;
          setState(previous => ({ ...previous, failed: true }));
        }
      } finally {
        clearTimeout(timeout);
        fetching = false;
        if (visible() && (polling || !entitlementsKnown)) {
          const delay = snapshotFailures
            ? Math.min(2_000 * 2 ** Math.min(snapshotFailures - 1, 4), 30_000)
            : polling
              ? FALLBACK_POLL_MS
              : 2_000;
          pollTimer = setTimeout(() => void fetchSnapshot(), delay);
        }
      }
    };

    const startPolling = () => {
      if (polling || !visible()) return;
      polling = true;
      void fetchSnapshot();
    };

    const open = () => {
      if (!visible() || source) return;
      if (typeof EventSource === 'undefined') {
        startPolling();
        return;
      }
      const opened = new EventSource('/realtime/fleet-traffic/events', { withCredentials: true });
      source = opened;
      opened.addEventListener('snapshot', raw => {
        if (stopped || source !== opened) return;
        try {
          const next = JSON.parse((raw as MessageEvent).data) as FleetTrafficSnapshot;
          if (!Array.isArray(next.nodes)) return;
          acceptSnapshot(next);
          // A recovered push stream owns readings again; polling remains only for missing grants.
          polling = false;
          reconnectFailures = 0;
          if (entitlementsKnown && pollTimer) {
            clearTimeout(pollTimer);
            pollTimer = undefined;
          }
        } catch {
          /* The last valid per-node readings expire independently. */
        }
      });
      opened.addEventListener('sample', raw => {
        if (stopped || source !== opened) return;
        try {
          const event = JSON.parse((raw as MessageEvent).data) as NodeRealtimeEvent;
          if (!event.node_id) return;
          update(previous => {
            if (!previous) return previous;
            const index = previous.nodes.findIndex(node => node.node_id === event.node_id);
            if (index < 0) return previous;
            const node = previous.nodes[index];
            const prior = node.samples.at(-1);
            if (olderRealtimeEvent(event, prior)) return previous;
            const nodes = [...previous.nodes];
            nodes[index] = {
              ...node,
              connected: true,
              active: true,
              samples: [mergeNodeRealtimeEvent(prior, event)],
            };
            return { ...previous, nodes };
          });
        } catch {
          /* Keep the last valid frame until its own freshness deadline. */
        }
      });
      opened.addEventListener('status', raw => {
        if (stopped || source !== opened) return;
        try {
          const status = JSON.parse((raw as MessageEvent).data) as Partial<FleetTrafficNode>;
          if (!status.node_id) return;
          update(previous => {
            if (!previous) return previous;
            return {
              ...previous,
              nodes: previous.nodes.map(node =>
                node.node_id === status.node_id
                  ? {
                      ...node,
                      connected: status.connected === true,
                      active: status.active === true,
                      interval_secs: status.interval_secs ?? node.interval_secs,
                    }
                  : node,
              ),
            };
          });
        } catch {
          /* Only decoded status events are authoritative. */
        }
      });
      opened.addEventListener('reset', () => {
        opened.close();
        if (source === opened && !stopped) {
          source = undefined;
          entitlementsKnown = false;
          void fetchSnapshot().finally(open);
        }
      });
      opened.onerror = () => {
        if (source !== opened || !visible()) return;
        opened.close();
        source = undefined;
        startPolling();
        const delay = Math.min(2_000 * 2 ** Math.min(reconnectFailures++, 4), 30_000);
        reconnectTimer = setTimeout(() => {
          reconnectTimer = undefined;
          open();
        }, delay);
      };
    };

    const close = () => {
      source?.close();
      source = undefined;
      pollAbort?.abort();
      if (pollTimer) clearTimeout(pollTimer);
      if (reconnectTimer) clearTimeout(reconnectTimer);
      pollTimer = undefined;
      reconnectTimer = undefined;
    };
    const visibility = () => {
      if (!visible()) close();
      else {
        polling = false;
        entitlementsKnown = false;
        snapshotFailures = 0;
        reconnectFailures = 0;
        void fetchSnapshot();
        open();
      }
    };

    // SSE carries no authorization matrix. Retry its HTTP snapshot until grants are known, then
    // let the stream own the readings; an SSE-less proxy continues through bounded polling.
    void fetchSnapshot();
    open();
    document.addEventListener('visibilitychange', visibility);
    return () => {
      stopped = true;
      document.removeEventListener('visibilitychange', visibility);
      close();
    };
  }, [enabled, update]);

  const facts = JSON.stringify([
    state.failed,
    state.payload?.policy.enabled,
    state.payload?.expected !== undefined,
    state.payload?.nodes.map(node => [
      node.node_id,
      node.connected,
      node.active,
      realtimeFacts(node.samples.at(-1), now),
    ]),
  ]);
  const payload = useMetricValue(enabled ? state.payload : null, 'fleet-user-traffic', facts);
  return { payload, now, failed: enabled && state.failed };
}
