/* 端口转发的目标地址：用户在一栏里写「主机:端口」。
 *
 * 服务端只接受主机与端口两个字段，并按 `BLOCKED_TARGET_CIDRS` 拒绝私网、环回、链路本地、
 * 文档与组播等目标；这里在提交前给出同样的结论，免得用户填完才收到一条服务端错误。
 * 网段清单与 crates/brocade-core/src/forwarding.rs 的同名常量逐项一致，
 * tests/forwarding-target-policy.test.mjs 比对两边。域名在入口机器上解析，提交时无法判断。 */

export const BLOCKED_TARGET_CIDRS = [
  '0.0.0.0/8',
  '10.0.0.0/8',
  '100.64.0.0/10',
  '100.100.100.200/32',
  '127.0.0.0/8',
  '169.254.0.0/16',
  '172.16.0.0/12',
  '192.0.0.0/24',
  '192.0.2.0/24',
  '192.88.99.0/24',
  '192.168.0.0/16',
  '198.18.0.0/15',
  '198.51.100.0/24',
  '203.0.113.0/24',
  '224.0.0.0/3',
  '::/127',
  '2001:db8::/32',
  'fc00::/7',
  'fd00:ec2::254/128',
  'fe80::/10',
  'ff00::/8',
] as const;

export const PRIVATE_TARGET_CIDRS = [
  '10.0.0.0/8',
  '100.64.0.0/10',
  '172.16.0.0/12',
  '192.168.0.0/16',
  'fc00::/7',
] as const;
const PRIVATE_NETWORKS = new Set<string>(PRIVATE_TARGET_CIDRS);

export type TargetKind = 'IPv4' | 'IPv6' | '域名';

export interface ParsedTarget {
  /** 不带方括号的主机：域名、IPv4 或 IPv6。 */
  host: string;
  port: number;
  kind: TargetKind;
  /** 落在 BLOCKED_TARGET_CIDRS 里的地址；授权未允许私网目标时服务端会拒绝。 */
  blocked: boolean;
}

/** 已有规则的 IPv6 目标按方括号形式存储，显示前先去掉再统一加，避免出现 `[[…]]`。 */
export const bareHost = (host: string) => host.trim().replace(/^\[(.*)\]$/, '$1');

export const hostText = (host: string) => {
  const bare = bareHost(host);
  return bare.includes(':') ? `[${bare}]` : bare;
};

export const endpointText = (host: string, port: number | '***') => `${hostText(host)}:${port}`;

/** 提交给服务端的主机：IPv6 带方括号，与线上已有规则的存储形式一致；服务端与 Agent 都会去掉方括号再解析。 */
export const submittedHost = (target: ParsedTarget) => (target.kind === 'IPv6' ? `[${target.host}]` : target.host);

function ipv4Bytes(text: string): number[] | null {
  const parts = text.split('.');
  if (parts.length !== 4 || parts.some(part => !/^\d{1,3}$/.test(part))) return null;
  const bytes = parts.map(Number);
  return bytes.every(byte => byte <= 255) ? bytes : null;
}

function ipv6Bytes(text: string): number[] | null {
  if (!text.includes(':') || !/^[0-9a-f:.]+$/i.test(text)) return null;
  const halves = text.split('::');
  if (halves.length > 2) return null;
  const groups = (part: string) => (part === '' ? [] : part.split(':'));
  const head = groups(halves[0]);
  const tail = halves.length === 2 ? groups(halves[1]) : [];
  const words: number[] = [];
  const push = (list: string[]) => {
    for (const [index, group] of list.entries()) {
      // 末尾可以是嵌入的 IPv4（::ffff:192.0.2.1）
      if (group.includes('.') && index === list.length - 1) {
        const v4 = ipv4Bytes(group);
        if (!v4) return false;
        words.push((v4[0] << 8) | v4[1], (v4[2] << 8) | v4[3]);
        continue;
      }
      if (!/^[0-9a-f]{1,4}$/i.test(group)) return false;
      words.push(parseInt(group, 16));
    }
    return true;
  };
  if (!push(head)) return null;
  const headLength = words.length;
  if (!push(tail)) return null;
  const missing = 8 - words.length;
  if (halves.length === 2 ? missing < 1 : missing !== 0) return null;
  words.splice(headLength, 0, ...new Array<number>(missing).fill(0));
  return words.flatMap(word => [word >> 8, word & 0xff]);
}

const prefixMatches = (address: number[], network: number[], length: number) => {
  for (let bit = 0; bit < length; bit += 1) {
    const mask = 0x80 >> (bit % 8);
    if ((address[bit >> 3] & mask) !== (network[bit >> 3] & mask)) return false;
  }
  return true;
};

const NETWORKS = BLOCKED_TARGET_CIDRS.map(cidr => {
  const [network, length] = cidr.split('/');
  return {
    bytes: ipv4Bytes(network) ?? ipv6Bytes(network) ?? [],
    length: Number(length),
    private: PRIVATE_NETWORKS.has(cidr),
  };
});

/** 与 Rust `target_ip_is_blocked` 相同：IPv4 映射的 IPv6 先还原成 IPv4 再比对。 */
export function blockedAddress(host: string, allowPrivateTargets = false): boolean {
  const bare = bareHost(host);
  let bytes = ipv4Bytes(bare) ?? ipv6Bytes(bare);
  if (!bytes) return false;
  if (bytes.length === 16 && bytes.slice(0, 10).every(byte => byte === 0) && bytes[10] === 0xff && bytes[11] === 0xff) {
    bytes = bytes.slice(12);
  }
  const address = bytes;
  return NETWORKS.some(
    network =>
      !(allowPrivateTargets && network.private) &&
      network.bytes.length === address.length &&
      prefixMatches(address, network.bytes, network.length),
  );
}

/** 解析「主机:端口」。粘贴的 URL 去掉协议头与路径；IPv6 必须写成 [地址]:端口。 */
export function parseTarget(text: string): ParsedTarget | { error: string } {
  const raw = text
    .trim()
    .replace(/^[a-z][a-z0-9+.-]*:\/\//i, '')
    .replace(/[/?#].*$/, '');
  if (!raw) return { error: '填写目标地址和端口，例如 example.com:443' };
  const bracketed = /^\[([^\]]+)\]:(\d{1,5})$/.exec(raw);
  const plain = bracketed ? null : /^([^\s:[\]]+):(\d{1,5})$/.exec(raw);
  const match = bracketed ?? plain;
  if (!match) {
    if (ipv6Bytes(raw)) return { error: 'IPv6 地址加方括号再写端口，例如 [2001:db8::1]:443' };
    return { error: '缺少端口，写成 主机:端口' };
  }
  const host = match[1].toLowerCase();
  const port = Number(match[2]);
  if (port < 1 || port > 65_535) return { error: '目标端口应在 1–65535' };
  if (bracketed) {
    if (!ipv6Bytes(host)) return { error: '方括号里的 IPv6 地址无效' };
    return { host, port, kind: 'IPv6', blocked: blockedAddress(host) };
  }
  if (/^[\d.]+$/.test(host)) {
    if (!ipv4Bytes(host)) return { error: 'IPv4 地址无效' };
    return { host, port, kind: 'IPv4', blocked: blockedAddress(host) };
  }
  if (host.length > 253) return { error: '域名过长' };
  return { host, port, kind: '域名', blocked: false };
}
