import { VPNGATE_MAX_CANDIDATES, type ExternalOutbound, type VpngateCountrySummary } from './api';
import { vpngateTunnelId } from './model-id';

const regionNames = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
export const vpngateRegionName = (code: string): string =>
  code === 'ZZ' ? '地区未知' : isSelectableVpngateRegion(code) ? (regionNames.of(code) ?? code) : code;

export const isSelectableVpngateRegion = (code: string): boolean => /^[A-Z]{2}$/.test(code) && code !== 'ZZ';

export const vpngateDirectoryServerCount = (country: VpngateCountrySummary): number =>
  country.retained_servers ?? country.current_servers;

export type VpngateOutbound = ExternalOutbound & { protocol: Extract<ExternalOutbound['protocol'], { t: 'vpngate' }> };

export const isVpngateOutbound = (outbound: ExternalOutbound): outbound is VpngateOutbound =>
  outbound.protocol.t === 'vpngate';

export const vpngateServerIds = (outbound?: VpngateOutbound | null): string[] =>
  outbound?.protocol.v.server_ids?.length
    ? outbound.protocol.v.server_ids
    : outbound?.protocol.v.server_id
      ? [outbound.protocol.v.server_id]
      : [];

export function selectVpngatePool(
  outbounds: ExternalOutbound[],
  tenant: string,
  country: VpngateCountrySummary,
  serverIds: string[],
): VpngateOutbound {
  const ids = [...new Set(serverIds)].sort();
  const existing = outbounds
    .filter(isVpngateOutbound)
    .find(
      outbound =>
        outbound.tenant === tenant &&
        outbound.protocol.v.country_code === country.country_code &&
        JSON.stringify([...vpngateServerIds(outbound)].sort()) === JSON.stringify(ids),
    );
  if (existing) return existing;
  return {
    id: vpngateTunnelId(new Set(outbounds.map(outbound => outbound.id))),
    tenant,
    name: `VPN Gate · ${vpngateRegionName(country.country_code)} · ${ids.length ? `手动池 ${ids.length} 节点` : '自动池'}`,
    address: 'managed.vpngate.invalid',
    port: 1,
    protocol: {
      t: 'vpngate',
      v: {
        country_code: country.country_code,
        ...(ids.length ? { server_ids: ids } : {}),
        max_connect_ms: 15_000,
        min_download_bps: 1_000_000,
        max_candidates: ids.length || VPNGATE_MAX_CANDIDATES,
      },
    },
    security: { t: 'none' },
    bindings: [],
  };
}
