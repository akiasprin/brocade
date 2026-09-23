export interface ProvisionFormValues {
  id: string;
  public_ipv4: string;
  public_ipv6: string;
  wg_listen_port: string;
  api_port: string;
  dns: string;
}

export interface ProvisionFormErrors {
  id: string | null;
  public_ipv4: string | null;
  public_ipv6: string | null;
  wg_listen_port: string | null;
  api_port: string | null;
  dns: string | null;
}

const SLUG_RE = /^[a-z0-9._-]{1,32}$/;

export function nodeIdError(value: string, existingIds: ReadonlySet<string>): string | null {
  const id = value.trim();
  if (!id) return '填写机器 ID';
  if (!SLUG_RE.test(id)) return '只能使用 a-z、0-9、点、下划线或连字符，最长 32 个字符';
  return existingIds.has(id) ? '该 ID 已存在' : null;
}

export function validIpv4Literal(value: string): boolean {
  const octets = value.split('.');
  return (
    octets.length === 4 && octets.every(octet => /^\d{1,3}$/.test(octet) && Number(octet) >= 0 && Number(octet) <= 255)
  );
}

// 公网 IPv4 字段在模型里也是一个可拨号地址：节点使用动态 DNS 时允许保存主机名。
// 纯数字点分串仍按 IPv4 校验，避免把 203.0.113.999 误当成合法 DNS 名称。
export function validIpv4Endpoint(value: string): boolean {
  const host = value.trim().replace(/\.$/, '');
  if (validIpv4Literal(host)) return true;
  if (!host || host.length > 253 || /^[\d.]+$/.test(host)) return false;
  const labels = host.split('.');
  return labels.every(
    label => label.length >= 1 && label.length <= 63 && /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/i.test(label),
  );
}

export function validIpv6Literal(value: string): boolean {
  let host = value.trim();
  if (host.startsWith('[') || host.endsWith(']')) {
    if (!(host.startsWith('[') && host.endsWith(']'))) return false;
    host = host.slice(1, -1);
  }
  if (!host || !host.includes(':') || /[\s/?#]/.test(host)) return false;
  try {
    const parsed = new URL(`http://[${host}]/`);
    return parsed.hostname.startsWith('[') && parsed.hostname.endsWith(']');
  } catch {
    return false;
  }
}

export function portInputError(value: string, optional = false): string | null {
  const raw = value.trim();
  if (optional && raw === '') return null;
  if (!/^\d+$/.test(raw)) return '请输入 1–65535 的整数';
  const port = Number(raw);
  return Number.isSafeInteger(port) && port >= 1 && port <= 65_535 ? null : '请输入 1–65535 的整数';
}

export function dnsInputError(value: string): string | null {
  const raw = value.trim();
  if (raw === '' || raw === 'system') return null;
  const servers = raw.split(',').map(server => server.trim());
  if (servers.some(server => server === '')) return '多个 DNS 地址请用单个逗号分隔，不能留空项';
  return null;
}

export function provisionFormErrors(
  values: ProvisionFormValues,
  existingIds: ReadonlySet<string>,
): ProvisionFormErrors {
  return {
    id: nodeIdError(values.id, existingIds),
    public_ipv4:
      values.public_ipv4.trim() && !validIpv4Endpoint(values.public_ipv4.trim())
        ? '请输入完整的 IPv4 地址或主机名'
        : null,
    public_ipv6:
      values.public_ipv6.trim() && !validIpv6Literal(values.public_ipv6.trim()) ? '请输入完整的 IPv6 地址' : null,
    wg_listen_port: portInputError(values.wg_listen_port),
    api_port: portInputError(values.api_port, true),
    dns: dnsInputError(values.dns),
  };
}

export function firstProvisionError(errors: ProvisionFormErrors): string | null {
  return (
    errors.id ?? errors.public_ipv4 ?? errors.public_ipv6 ?? errors.wg_listen_port ?? errors.api_port ?? errors.dns
  );
}
