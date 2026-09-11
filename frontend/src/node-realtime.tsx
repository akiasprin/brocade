import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';

export interface NodeRealtimeEvent {
  node_id?: string;
  received_at_unix_millis: number;
  sample: {
    sampled_at_unix_millis: number;
    reverse_health?: unknown;
    mux?: unknown;
  };
}

export interface NodeRealtimeState {
  nodeId: string;
  last: { event: NodeRealtimeEvent; arrived: number } | null;
  now: number;
  connected: boolean;
}

const NodeRealtimeContext = createContext<NodeRealtimeState | null>(null);

function useNodeRealtimeSource(nodeId: string | null): NodeRealtimeState {
  const [last, setLast] = useState<{ nodeId: string; event: NodeRealtimeEvent; arrived: number } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [connectedNodeId, setConnectedNodeId] = useState<string | null>(null);

  useEffect(() => {
    if (!nodeId || typeof EventSource === 'undefined') return;
    let source: EventSource;
    let stopped = false;
    const accept = (event: NodeRealtimeEvent) => {
      setLast({ nodeId, event, arrived: Date.now() });
      setConnectedNodeId(nodeId);
    };
    const open = () => {
      if (stopped) return;
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
          const nodes = JSON.parse((e as MessageEvent).data).nodes as {
            node_id?: string;
            samples: NodeRealtimeEvent[];
          }[];
          const node = nodes.find(value => value.node_id === nodeId) ?? nodes[0];
          const sample = node?.samples.at(-1);
          if (sample) accept(sample);
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
      opened.onerror = () => setConnectedNodeId(null);
    };
    open();
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      stopped = true;
      source?.close();
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
