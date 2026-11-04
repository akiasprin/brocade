import { useSyncExternalStore } from 'react';
import { olderRealtimeEvent } from './realtime-order';
import { useNow } from './ui/clock';
import { useMetricValue } from './ui/metric-cadence';

interface FleetRealtimeEvent {
  node_id: string;
  received_at_unix_millis: number;
  sample: {
    sequence?: number;
    rx_bytes_per_sec: number;
    tx_bytes_per_sec: number;
    has_gap: boolean;
  };
}

export interface FleetRealtimeNode {
  connected: boolean;
  active: boolean;
  intervalSecs: number;
  latest: FleetRealtimeEvent | null;
}

/** Connection evidence and sampling freshness describe the NIC reading, not Agent lifecycle. */
export function freshFleetNicSample(node: FleetRealtimeNode | undefined, nowMillis: number) {
  const event = node?.latest;
  // Receipt time comes from Console, not this browser. A slightly faster Console clock must
  // not reject every live reading; connection, gaps and the existing expiry bound still apply.
  if (
    !node?.connected ||
    !node.active ||
    !event ||
    event.sample.has_gap ||
    nowMillis - event.received_at_unix_millis > Math.max(10_000, node.intervalSecs * 3_000)
  )
    return null;
  return event;
}

interface FleetRealtimeView {
  nodes: Map<string, FleetRealtimeNode>;
  nowMillis: number;
}

const recordOf = (value: unknown): Record<string, unknown> | null =>
  typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : null;

const finiteRate = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value >= 0;

function parseRealtimeEvent(value: unknown): FleetRealtimeEvent | null {
  const event = recordOf(value);
  const sample = recordOf(event?.sample);
  if (
    typeof event?.node_id !== 'string' ||
    !finiteRate(event.received_at_unix_millis) ||
    !finiteRate(sample?.rx_bytes_per_sec) ||
    !finiteRate(sample.tx_bytes_per_sec) ||
    typeof sample.has_gap !== 'boolean'
  ) {
    return null;
  }
  return {
    node_id: event.node_id,
    received_at_unix_millis: event.received_at_unix_millis,
    sample: {
      sequence: finiteRate(sample.sequence) ? sample.sequence : undefined,
      rx_bytes_per_sec: sample.rx_bytes_per_sec,
      tx_bytes_per_sec: sample.tx_bytes_per_sec,
      has_gap: sample.has_gap,
    },
  };
}

function latestRealtimeEvent(values: unknown): FleetRealtimeEvent | null {
  if (!Array.isArray(values)) return null;
  let latest: FleetRealtimeEvent | null = null;
  for (const value of values) {
    const event = parseRealtimeEvent(value);
    if (event && (!latest || event.received_at_unix_millis > latest.received_at_unix_millis)) latest = event;
  }
  return latest;
}

let nodes = new Map<string, FleetRealtimeNode>();
const listeners = new Set<() => void>();
let stop: (() => void) | undefined;
const setNodes = (next: Map<string, FleetRealtimeNode>) => {
  nodes = next;
  for (const listener of listeners) listener();
};
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  if (listeners.size === 1) stop = startSource();
  return () => {
    listeners.delete(listener);
    if (!listeners.size) {
      stop?.();
      stop = undefined;
      nodes = new Map();
    }
  };
};
const snapshot = () => nodes;

function startSource() {
  if (typeof EventSource === 'undefined') return () => {};
  let stopped = false;
  let source: EventSource | undefined;
  let reopenTimer: ReturnType<typeof setTimeout> | undefined;
  let pendingNodes = nodes;
  const open = () => {
    if (stopped || document.visibilityState === 'hidden') return;
    const opened = new EventSource('/realtime/nodes/events', { withCredentials: true });
    source = opened;
    opened.addEventListener('snapshot', raw => {
      if (stopped || source !== opened) return;
      try {
        const payload = recordOf(JSON.parse((raw as MessageEvent).data));
        if (!Array.isArray(payload?.nodes)) throw new Error('fleet realtime snapshot has no nodes');
        const next = new Map<string, FleetRealtimeNode>();
        for (const value of payload.nodes) {
          const node = recordOf(value);
          if (typeof node?.node_id !== 'string') continue;
          const incoming = latestRealtimeEvent(node.samples);
          const previous = pendingNodes.get(node.node_id)?.latest;
          next.set(node.node_id, {
            connected: payload.policy && recordOf(payload.policy)?.enabled === false ? false : node.connected === true,
            active: node.active === true,
            intervalSecs: finiteRate(node.interval_secs) ? node.interval_secs : 5,
            latest: incoming && olderRealtimeEvent(incoming, previous) ? previous! : incoming,
          });
        }
        pendingNodes = next;
        setNodes(next);
      } catch {
        // Keep the last valid reading until its original freshness deadline.
      }
    });
    opened.addEventListener('sample', raw => {
      if (stopped || source !== opened) return;
      try {
        const event = parseRealtimeEvent(JSON.parse((raw as MessageEvent).data));
        if (!event) throw new Error('invalid fleet realtime sample');
        const next = new Map(pendingNodes);
        const previous = pendingNodes.get(event.node_id);
        if (olderRealtimeEvent(event, previous?.latest)) return;
        next.set(event.node_id, {
          connected: true,
          active: true,
          intervalSecs: previous?.intervalSecs ?? 5,
          latest: event,
        });
        pendingNodes = next;
        setNodes(next);
      } catch {
        // Keep the last valid reading until its original freshness deadline.
      }
    });
    opened.addEventListener('status', raw => {
      if (stopped || source !== opened) return;
      try {
        const status = recordOf(JSON.parse((raw as MessageEvent).data));
        if (typeof status?.node_id !== 'string') return;
        const nodeId = status.node_id;
        const next = new Map(pendingNodes);
        const previous = pendingNodes.get(nodeId);
        next.set(nodeId, {
          connected: status.connected === true,
          active: status.active === true,
          intervalSecs: finiteRate(status.interval_secs) ? status.interval_secs : (previous?.intervalSecs ?? 5),
          latest: previous?.latest ?? null,
        });
        pendingNodes = next;
        setNodes(next);
      } catch {
        // Keep the last valid reading until its original freshness deadline.
      }
    });
    opened.addEventListener('reset', () => {
      opened.close();
      if (source === opened && !stopped) reopenTimer = setTimeout(open, 0);
    });
    // SSE reconnects and authorization resets are transport events, not a new measurement.
    // Fresh samples remain usable; never flash the slower historical average on reconnect.
    opened.onerror = () => undefined;
  };
  const visibility = () => {
    if (document.visibilityState === 'hidden') {
      source?.close();
      source = undefined;
      if (reopenTimer) clearTimeout(reopenTimer);
    } else open();
  };
  document.addEventListener('visibilitychange', visibility);
  open();
  return () => {
    document.removeEventListener('visibilitychange', visibility);
    stopped = true;
    source?.close();
    if (reopenTimer) clearTimeout(reopenTimer);
  };
}

export function useFleetRealtime(): FleetRealtimeView {
  const nodes = useSyncExternalStore(subscribe, snapshot);
  const nowMillis = useNow();
  const displayedNodes = useMetricValue(
    nodes,
    'fleet',
    JSON.stringify(
      [...nodes].map(([id, node]) => [id, node.connected, node.active, node.latest?.sample.has_gap ?? false]),
    ),
  );

  return { nodes: displayedNodes, nowMillis };
}
