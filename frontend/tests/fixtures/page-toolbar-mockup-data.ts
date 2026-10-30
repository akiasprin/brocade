// Synthetic backend for the page-toolbar mockup. Never imported by the app.
// Reuses the palette mockup's fleet and fills the reads that leave the toolbars' pages half-drawn: the user
// page's quota rows and source list, and a pending revision so the deploy toolbar shows its primary action.
import { installBackend as installFleet } from './palette-mockup-data';

type Quota = { tenant_id: string; user_id: string; app_id: string; limit_bytes: number };
type CycleView = { tenant_id: string; user_id: string; app_id: string; uplink_bytes: number; downlink_bytes: number };

export function installBackend() {
  installFleet();
  const fleet = globalThis.fetch;
  const read = async <T>(path: string) => (await fleet(path)).json() as Promise<T>;

  /** 额度行与「用量与额度」同源：每个授权项目一行，首个项目带月额度，其余不限。 */
  async function trafficPolicies(tenant: string, user: string) {
    const [{ quotas }, { views }] = await Promise.all([
      read<{ quotas: Quota[] }>('/quotas'),
      read<{ views: CycleView[] }>('/usage/user-cycles'),
    ]);
    return views
      .filter(view => view.tenant_id === tenant && view.user_id === user)
      .map(view => {
        const quota = quotas.find(item => item.user_id === user && item.app_id === view.app_id);
        const used = view.uplink_bytes + view.downlink_bytes;
        return {
          kind: 'access',
          app_id: view.app_id,
          policy: quota
            ? {
                quota_bytes: String(quota.limit_bytes),
                exhausted_action: { kind: 'throttle', rate_bps: 2_000_000 },
              }
            : null,
          used_bytes: String(Math.round(used)),
          exhausted: quota ? used >= quota.limit_bytes : false,
          service_available: true,
          unsupported_nodes: [],
        };
      });
  }

  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = input instanceof Request ? input.url : String(input);
    const url = new URL(raw, location.origin);
    const parts = url.pathname.split('/').map(decodeURIComponent);
    if (parts[1] === 'users' && parts[4] === 'traffic-policy' && parts.length === 5) {
      return Response.json(await trafficPolicies(parts[2], parts[3]));
    }
    if (parts[1] === 'users' && parts[4] === 'source-access' && parts.length === 5) {
      return Response.json({ revision: 1, ips: [], nodes: [] });
    }
    // 解锁页签：Agent 支持双栈检测、从未检测过，页头的「开始检测」是这一页的主操作。
    if (url.pathname === '/nodes/agent-state') {
      const { nodes } = await read<{ nodes: { agent_protocol_version: number }[] }>(url.pathname);
      return Response.json({ nodes: nodes.map(node => ({ ...node, agent_protocol_version: 50 })) });
    }
    if (parts[1] === 'nodes' && parts[3] === 'unlock-probe') {
      return Response.json(
        parts[4] === 'capabilities'
          ? { family_mode: 'split', ipv6_available: false, ipv6_reason: '示例机器没有 IPv6', detection_mode: 'manual' }
          : null,
      );
    }
    if (url.pathname === '/deployments/verify') {
      return Response.json({
        revision_id: 41,
        converged: false,
        summary: {
          total_targets: 12,
          changed_targets: 3,
          skipped_targets: 9,
          deferred_targets: 0,
          disruptive_targets: 0,
          max_wave: 1,
        },
        targets: [],
      });
    }
    return fleet(input, init);
  };
}
