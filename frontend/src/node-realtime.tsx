import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import type { VpngateRealtimeReport } from './vpngate-realtime';

export interface NodeRealtimeEvent {
  node_id?: string;
  received_at_unix_millis: number;
  sample: {
    /** Heavy worker reports are a lower-frequency delta; absent fields retain the last complete
     * report only when this flag is true. Without the flag, absence authoritatively clears it. */
    diagnostics_unchanged?: boolean;
    sampled_at_unix_millis: number;
    reverse_health?: unknown;
    mux?: unknown;
    vpngate?: VpngateRealtimeReport | null;
  };
}

export interface NodeRealtimeState {
  nodeId: string;
  last: { event: NodeRealtimeEvent; arrived: number } | null;
  now: number;
  connected: boolean;
}

interface NodeRealtimeSnapshot {
  node_id?: string;
  connected?: boolean;
  samples: NodeRealtimeEvent[];
}

const NodeRealtimeContext = createContext<NodeRealtimeState | null>(null);

/** Merge only an explicitly marked diagnostics delta. A complete event with absent diagnostics
 * must clear old state, otherwise removing a worker pool would leave a ghost card indefinitely. */
export function mergeNodeRealtimeEvent(
  previous: NodeRealtimeEvent | undefined,
  event: NodeRealtimeEvent,
): NodeRealtimeEvent {
  return event.sample.diagnostics_unchanged && previous
    ? { ...event, sample: { ...previous.sample, ...event.sample } }
    : event;
}

function useNodeRealtimeSource(nodeId: string | null): NodeRealtimeState {
  const [last, setLast] = useState<{ nodeId: string; event: NodeRealtimeEvent; arrived: number } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [connectedNodeId, setConnectedNodeId] = useState<string | null>(null);

  useEffect(() => {
    if (!nodeId) return;
    let source: EventSource | undefined;
    let pollTimer: ReturnType<typeof setTimeout> | undefined;
    let pollAbort: AbortController | undefined;
    let polling = false;
    let stopped = false;
    const accept = (event: NodeRealtimeEvent, connected = true) => {
      setLast(previous => {
        const previousEvent = previous?.nodeId === nodeId ? previous.event : undefined;
        const merged = mergeNodeRealtimeEvent(previousEvent, event);
        return { nodeId, event: merged, arrived: Date.now() };
      });
      if (connected) setConnectedNodeId(nodeId);
    };
    const acceptSnapshot = (nodes: NodeRealtimeSnapshot[]) => {
      const node = nodes.find(value => value.node_id === nodeId) ?? nodes[0];
      setConnectedNodeId(node?.connected ? nodeId : null);
      const sample = node?.samples.at(-1);
      if (sample) accept(sample, false);
    };
    const poll = async () => {
      if (stopped) return;
      const abort = new AbortController();
      pollAbort = abort;
      try {
        const response = await fetch(`/realtime/nodes/${encodeURIComponent(nodeId)}/snapshot`, {
          credentials: 'include',
          cache: 'no-store',
          signal: abort.signal,
        });
        if (!response.ok) throw new Error(`realtime snapshot returned ${response.status}`);
        const payload = (await response.json()) as { nodes?: NodeRealtimeSnapshot[] };
        if (!Array.isArray(payload.nodes)) throw new Error('realtime snapshot has no nodes');
        acceptSnapshot(payload.nodes);
      } catch (error) {
        if (!(error instanceof DOMException && error.name === 'AbortError')) setConnectedNodeId(null);
      } finally {
        if (!stopped) pollTimer = setTimeout(() => void poll(), 2000);
      }
    };
    const startPolling = () => {
      if (stopped || polling) return;
      polling = true;
      source?.close();
      setConnectedNodeId(null);
      void poll();
    };
    const open = () => {
      if (stopped) return;
      if (typeof EventSource === 'undefined') {
        startPolling();
        return;
      }
      const opened = new EventSource(`/realtime/nodes/${encodeURIComponent(nodeId)}/events`, { withCredentials: true });
      source = opened;
      opened.addEventListener('sample', e => {
        try {
          accept(JSON.parse((e as MessageEvent).data));
        } catch {
          setConnectedNodeId(null);
        }
      });
      opened.addEventListener('snapshot', e => {
        try {
          acceptSnapshot(JSON.parse((e as MessageEvent).data).nodes as NodeRealtimeSnapshot[]);
        } catch {
          setConnectedNodeId(null);
        }
      });
      opened.addEventListener('status', e => {
        try {
          const status = JSON.parse((e as MessageEvent).data) as { node_id?: string; connected?: boolean };
          if (!status.node_id || status.node_id === nodeId) setConnectedNodeId(status.connected ? nodeId : null);
        } catch {
          setConnectedNodeId(null);
        }
      });
      opened.addEventListener('reset', () => {
        opened.close();
        setConnectedNodeId(null);
        if (source === opened) open();
      });
      opened.onerror = () => {
        if (source === opened) startPolling();
      };
    };
    open();
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      stopped = true;
      source?.close();
      pollAbort?.abort();
      if (pollTimer) clearTimeout(pollTimer);
      clearInterval(timer);
    };
  }, [nodeId]);

  return {
    nodeId: nodeId ?? '',
    last: last?.nodeId === nodeId ? last : null,
    now,
    connected: connectedNodeId === nodeId,
  };
}

export function NodeRealtimeProvider({ nodeId, children }: { nodeId: string; children: ReactNode }) {
  const state = useNodeRealtimeSource(nodeId);
  return <NodeRealtimeContext.Provider value={state}>{children}</NodeRealtimeContext.Provider>;
}

/** Cards remain independently renderable in tests and other pages. Inside NodeRealtimeProvider
 * they share its one EventSource; outside it a card opens its own source as before. */
export function useNodeRealtime(nodeId: string): NodeRealtimeState {
  const shared = useContext(NodeRealtimeContext);
  const owned = useNodeRealtimeSource(shared?.nodeId === nodeId ? null : nodeId);
  return shared?.nodeId === nodeId ? shared : owned;
}
