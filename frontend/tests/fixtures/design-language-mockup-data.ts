// Synthetic backend for the design-language mockup. Never imported by the app.
// Reuses the palette mockup's fleet and fills the two user-detail reads it leaves empty, so the user page
// renders its quota rows and source list instead of fixture errors.
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
    return fleet(input, init);
  };
}
