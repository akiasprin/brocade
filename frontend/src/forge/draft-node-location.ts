import { useEffect, useRef, useSyncExternalStore } from 'react';
import type { DraftEntry } from '../draft';
import type { NodeAgentStateItem } from '../api';
import { navigate } from './route';

export const NODE_DRAFT_PANELS = {
  identity: { label: '身份', tab: 'config' },
  xray: { label: 'XRAY', tab: 'config' },
  wireguard: { label: 'WIREGUARD', tab: 'config' },
  dns: { label: 'DNS', tab: 'config' },
  connection: { label: '连接策略', tab: 'config' },
  certificates: { label: '证书组', tab: 'config' },
  config: { label: '配置', tab: 'config' },
  'egress-dns': { label: 'DNS 解析策略', tab: 'chains' },
  chains: { label: '链路规则', tab: 'chains' },
  observed: { label: '运行状态', tab: 'observed' },
} as const;
export type NodeDraftPanel = keyof typeof NODE_DRAFT_PANELS;

export const nodeDraftPanelKey = (nodeId: string, panel: NodeDraftPanel) => JSON.stringify([nodeId, panel]);

export function nodeDraftLocations(
  entry: DraftEntry,
  nodes: readonly Pick<NodeAgentStateItem, 'node_id' | 'name'>[],
  entries: readonly DraftEntry[],
): { name: string; targets: { nodeId: string; panel: NodeDraftPanel; label: string }[] } | null {
  const nameOf = (id: string) => {
    const pending = entries.find(e => e.op.op === 'update_node' && e.op.node_id === id)?.op;
    return (
      (pending?.op === 'update_node' ? pending.node.name?.trim() : undefined) ||
      nodes.find(node => node.node_id === id)?.name ||
      id
    );
  };
  const op = entry.op;
  if (op.op === 'set_wireguard_link_disabled') {
    return {
      name: 'WG 链路',
      targets: [op.a, op.b].map(nodeId => ({ nodeId, panel: 'wireguard', label: `${nameOf(nodeId)} · WIREGUARD` })),
    };
  }
  if (!('node_id' in op)) return null;
  const panels = new Set<NodeDraftPanel>();
  switch (op.op) {
    case 'update_node':
      for (const key of Object.keys(op.node) as (keyof typeof op.node)[]) {
        if (op.node[key] === undefined) continue;
        if (
          ['name', 'public_ipv4', 'public_ipv6', 'public_ipv4_nat', 'public_ipv6_nat', 'egress_allowed'].includes(key)
        )
          panels.add('identity');
        else if (['wg_listen_port', 'mtu', 'overlay', 'wg_transport'].includes(key)) panels.add('wireguard');
        else if (key === 'dns' || key === 'domain_strategy') panels.add('dns');
        else if (key === 'xray_enabled') panels.add('xray');
        else if (key === 'connection') panels.add('connection');
        else panels.add('config');
      }
      if (!panels.size) panels.add('config');
      break;
    case 'set_node_cert_group':
      panels.add('certificates');
      break;
    case 'set_node_egress_dns':
    case 'reorder_node_egress_dns':
      panels.add('egress-dns');
      break;
    case 'put_step':
    case 'delete_step':
      panels.add('chains');
      break;
    case 'update_node_status':
      panels.add('observed');
      break;
  }
  return {
    name: nameOf(op.node_id),
    targets: (Object.keys(NODE_DRAFT_PANELS) as NodeDraftPanel[])
      .filter(panel => panels.has(panel))
      .map(panel => ({ nodeId: op.node_id, panel, label: NODE_DRAFT_PANELS[panel].label })),
  };
}

interface PanelFocus {
  token: number;
  nodeId: string;
  panel: NodeDraftPanel;
  status: 'pending' | 'located' | 'unavailable';
}
let focus: PanelFocus | null = null;
let nextToken = 0;
let deadline = 0;
let clearHighlight: (() => void) | undefined;
const listeners = new Set<() => void>();
const publish = (value: PanelFocus | null) => {
  focus = value;
  listeners.forEach(listener => listener());
};
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};
const snapshot = () => focus;
export const useDraftPanelFocusStatus = () => useSyncExternalStore(subscribe, snapshot);

export function clearNodeDraftFocus() {
  window.clearTimeout(deadline);
  clearHighlight?.();
  publish(null);
}

export function focusNodeDraftPanel(nodeId: string, panel: NodeDraftPanel): boolean {
  if (!navigate('nodes', { p: 'node', id: nodeId, tab: NODE_DRAFT_PANELS[panel].tab })) return false;
  clearHighlight?.();
  window.clearTimeout(deadline);
  const request: PanelFocus = { token: ++nextToken, nodeId, panel, status: 'pending' };
  publish(request);
  deadline = window.setTimeout(() => {
    if (focus?.token === request.token && focus.status === 'pending') publish({ ...request, status: 'unavailable' });
  }, 8_000);
  return true;
}

/** Wait for the actual card, not a loading placeholder. Only one bounded request is retained. */
export function useNodeDraftFocus(nodeId: string) {
  const request = useDraftPanelFocusStatus();
  const lifecycle = useRef({ generation: 0 });
  useEffect(() => {
    if (!request || request.nodeId !== nodeId || request.status !== 'pending') return;
    const lifetime = lifecycle.current;
    const generation = ++lifetime.generation;
    let frame = 0;
    let scheduled = false;
    const observer = new MutationObserver(() => locate());
    const locate = () => {
      if (scheduled || focus?.token !== request.token || focus.status !== 'pending') return;
      const surface = document.querySelector('.fg-view') ?? document.body;
      const target = [...surface.querySelectorAll<HTMLElement>('[data-draft-panel]')].find(
        element => element.dataset.draftPanel === nodeDraftPanelKey(nodeId, request.panel),
      );
      if (!target) return;
      scheduled = true;
      observer.disconnect();
      for (
        let details = target.closest('details');
        details;
        details = details.parentElement?.closest('details') ?? null
      ) {
        details.open = true;
      }
      // The route restores its scroll position in the first frame. Reveal the
      // requested card afterwards so that restoration cannot scroll it away.
      frame = window.requestAnimationFrame(() => {
        frame = window.requestAnimationFrame(() => {
          if (!target.isConnected || focus?.token !== request.token || focus.status !== 'pending') return;
          const previousTabIndex = target.getAttribute('tabindex');
          target.setAttribute('tabindex', '-1');
          target.dataset.draftHighlight = 'true';
          target.scrollIntoView?.({
            block: 'center',
            behavior: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth',
          });
          target.focus({ preventScroll: true });
          let timer = 0;
          clearHighlight = () => {
            window.clearTimeout(timer);
            delete target.dataset.draftHighlight;
            if (previousTabIndex === null) target.removeAttribute('tabindex');
            else target.setAttribute('tabindex', previousTabIndex);
            clearHighlight = undefined;
          };
          timer = window.setTimeout(() => clearHighlight?.(), 4_000);
          window.clearTimeout(deadline);
          publish({ ...request, status: 'located' });
        });
      });
    };
    observer.observe(document.body, { childList: true, subtree: true });
    locate();
    return () => {
      observer.disconnect();
      window.cancelAnimationFrame(frame);
      // StrictMode immediately sets the effect up again. Cancel only a real
      // unmount/navigation, not that development-only cleanup/replay.
      queueMicrotask(() => {
        if (lifetime.generation === generation && focus?.token === request.token && focus.status === 'pending') {
          window.clearTimeout(deadline);
          publish(null);
        }
      });
    };
  }, [nodeId, request]);
  return request?.nodeId === nodeId && request.status === 'pending' ? request : null;
}
