import { Fragment } from 'react';
import type { NetworkOperator } from '../api';

/* 来源地址的共同写法：用户页「在线设备」与机器用量的「流入明细」。 */

export const SOURCE_LOCATION_TITLE = '本地 GeoIP · 国家／地区与网络归属参考，非精确位置或宽带品牌';

const SOURCE_REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const SOURCE_OPERATOR_NAMES = new Map<NetworkOperator, string>([
  ['chinanet', '电信'],
  ['cmcc', '移动'],
  ['unicom', '联通'],
  ['cernet', '教育网'],
  ['cstnet', '科技网'],
]);

// 来源的网络归属：本地 GeoIP 国家／地区 + BGP 运营商。国内运营商合写为网络名（「中国电信」
// 「中国教育网」）；境外地区有运营商归属时写「香港 · 移动」。国家库未命中时保留运营商，
// 不从运营商反推国家。
export function sourceNetwork(
  country: string | undefined,
  operator: NetworkOperator | undefined,
): { code: string | undefined; label: string } {
  const code = country?.trim().toUpperCase();
  const name = code && /^[A-Z]{2}$/.test(code) && code !== 'ZZ' ? SOURCE_REGION_NAMES.of(code) : undefined;
  const region = name && name !== code ? name : undefined;
  const operatorName = operator ? SOURCE_OPERATOR_NAMES.get(operator) : undefined;
  if (!region) return { code: undefined, label: operatorName ? `位置未知 · ${operatorName}` : '位置未知' };
  if (!operatorName) return { code, label: region };
  return { code, label: code === 'CN' ? `中国${operatorName}` : `${region} · ${operatorName}` };
}

// IPv6 的前 64 位用正文墨色，接口标识降一档：同一 /64 下轮换的隐私地址可以直接对上。
// 接口标识的冒号后允许断行，窄卡里不会把一组十六进制拆到两行。
export function SourceAddress({ ip }: { ip: string }) {
  const groups = ip.split(':');
  if (groups.length <= 4 || !groups.slice(0, 4).every(Boolean)) return <code>{ip}</code>;
  return (
    <code>
      {groups.slice(0, 4).join(':')}
      <span className="user-presence-iid">
        {groups.slice(4).map((group, index) => (
          <Fragment key={index}>
            :<wbr />
            {group}
          </Fragment>
        ))}
      </span>
    </code>
  );
}

/**
 * 输入地址的规范写法，与 Console 的 normalize_ip 结果一致：IPv6 按 RFC 5952 压缩为小写，
 * IPv4-mapped IPv6 写成 IPv4。只用来对照已知来源和名单；认不出的输入照原样提交，由 Console 给出校验结论。
 */
export function canonicalSourceIp(raw: string): string | null {
  const value = raw.trim();
  if (!value || /[\s/%[\]]/.test(value)) return null;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(value);
  if (v4) {
    const parts = v4.slice(1);
    if (parts.some(part => (part.length > 1 && part.startsWith('0')) || Number(part) > 255)) return null;
    return parts.map(Number).join('.');
  }
  if (!value.includes(':')) return null;
  let host: string;
  try {
    host = new URL(`http://[${value}]/`).hostname.slice(1, -1);
  } catch {
    return null;
  }
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
  if (!mapped) return host;
  const high = parseInt(mapped[1], 16);
  const low = parseInt(mapped[2], 16);
  return [high >> 8, high & 255, low >> 8, low & 255].join('.');
}
