import type { IpMaintenancePolicy, MaintenanceRegion, NodeAgentStateItem } from '../../api';
import { cityName, countryName, isoOf } from './regions-zh';

const normalized = (value: string) => value.toLowerCase().replace(/[\s_-]+/g, '');

/** An unsaved first-use proposal. The explicit saved policy remains the execution authority. */
export function maintenanceDefaults(
  node: NodeAgentStateItem,
  regions: MaintenanceRegion[],
  template: IpMaintenancePolicy,
) {
  const ipv4 = node.public_ipv4 ?? node.observed_public_ipv4?.current_ip;
  const ipv6 = node.public_ipv6 ?? node.observed_public_ipv6?.current_ip;
  const family = ipv4 ? 'v4' : ipv6 ? 'v6' : template.family;
  const address = family === 'v4' ? ipv4 : ipv6;
  const observed = family === 'v4' ? node.observed_public_ipv4 : node.observed_public_ipv6;
  // The same operator correction/GeoIP country drives the machine flag. An observation is a
  // fallback only for the same address; an old exit must not classify a newly configured IP.
  const code =
    node.public_ipv4_country_override ??
    (family === 'v4' ? node.public_ipv4_country : null) ??
    (observed?.current_ip === address ? observed?.country_code : null);
  const country = code ? isoOf(code.toUpperCase()) : null;
  const candidates = country ? regions.filter(region => isoOf(region.country) === country) : [];
  const name = normalized(node.name);
  const named = candidates.filter(region =>
    [cityName(region), region.city].some(city => name.includes(normalized(city))),
  );
  const selected = named.length === 1 ? named[0] : candidates.length === 1 ? candidates[0] : undefined;
  const notice = selected
    ? null
    : !country
      ? '尚未识别公网 IP 所属地区，请选择目标地区'
      : candidates.length
        ? `已按机器旗帜匹配${countryName(country)}，请选择养护城市`
        : `暂无${countryName(country)}养护模板，请选择目标地区`;
  return {
    policy: { ...template, enabled: false, region: selected?.id ?? '', family } satisfies IpMaintenancePolicy,
    country,
    address,
    notice,
  };
}
