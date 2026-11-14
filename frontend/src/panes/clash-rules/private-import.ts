import { parseDocument } from 'yaml';

export interface PrivateProxy {
  name: string;
  type: string;
  server: string;
  port: number;
  username?: string;
  password?: string;
  tls?: boolean;
  sni?: string;
  [key: string]: unknown;
}

export const LINK_PROTOCOLS = 'VLESS、VMess、Shadowsocks、Trojan、Hysteria 2、AnyTLS、TUIC、SOCKS5、HTTP(S)';
const NATIVE_TYPES = new Set([
  'vless',
  'vmess',
  'ss',
  'ssr',
  'trojan',
  'hysteria',
  'hysteria2',
  'anytls',
  'tuic',
  'socks5',
  'http',
  'wireguard',
  'ssh',
  'snell',
  'mieru',
  'shadowquic',
  'masque',
  'trusttunnel',
  'easytier',
  'openvpn',
  'zerotier',
  'sudoku',
]);
const MAX_INPUT = 1_048_576;

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    throw new Error('内容中的百分号编码不完整，请重新复制。');
  }
}
function base64(value: string): string {
  try {
    const normalized = value.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(normalized + '='.repeat((4 - (normalized.length % 4)) % 4));
    return new TextDecoder('utf-8', { fatal: true }).decode(Uint8Array.from(binary, char => char.charCodeAt(0)));
  } catch {
    throw new Error('Base64 内容不完整或不是有效的 UTF-8，请重新复制。');
  }
}
function port(value: unknown, fallback?: number): number {
  if (value === '' || value == null) value = fallback;
  if ((typeof value !== 'number' && typeof value !== 'string') || !/^\d+$/.test(String(value)))
    throw new Error('节点需要有效的端口。');
  const number = Number(value);
  if (!Number.isInteger(number) || number < 1 || number > 65535) throw new Error('端口需在 1–65535 之间。');
  return number;
}
function name(value: unknown, fallback = '我的自建出口'): string {
  if (value == null || value === '') return fallback;
  if (typeof value !== 'string' || value.length > 120 || /[\r\n]/.test(value) || value.includes(String.fromCharCode(0)))
    throw new Error('节点名称需为 120 字以内的单行文字。');
  return value;
}
function uuid(value: string): string {
  if (!/^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i.test(value))
    throw new Error('节点的 UUID 格式不完整。');
  return value;
}
function bool(value: string | undefined): boolean | undefined {
  if (value == null) return undefined;
  if (['1', 'true', 'yes'].includes(value.toLowerCase())) return true;
  if (['0', 'false', 'no'].includes(value.toLowerCase())) return false;
  throw new Error('布尔参数需要使用 true/false 或 1/0。');
}

class Params {
  private remaining: Set<string>;
  constructor(private values: URLSearchParams) {
    this.remaining = new Set(values.keys());
  }
  take(...keys: string[]): string | undefined {
    const found = keys.flatMap(key => this.values.getAll(key));
    keys.forEach(key => this.remaining.delete(key));
    if (new Set(found).size > 1) throw new Error('同一连接参数出现了互相冲突的值。');
    return found[0];
  }
  done() {
    if (this.remaining.size) {
      const keys = [...this.remaining].filter(key => /^[\w-]{1,32}$/.test(key)).slice(0, 4);
      throw new Error(
        `链接参数尚未支持${keys.length ? `（${keys.join('、')}）` : ''}，请使用 Clash YAML / JSON 保留完整配置。`,
      );
    }
  }
}

function tls(node: PrivateProxy, q: Params, defaultSecurity: 'none' | 'tls' = 'none') {
  const security = q.take('security') ?? defaultSecurity;
  if (!['none', 'tls', 'reality'].includes(security)) throw new Error('不支持此安全层，请使用 Clash 原生配置。');
  const sni = q.take('sni', 'servername', 'serverName', 'peer');
  const alpn = q.take('alpn');
  const fp = q.take('fp', 'client-fingerprint');
  const insecure = bool(q.take('allowInsecure', 'insecure', 'skip-cert-verify'));
  const publicKey = q.take('pbk', 'publicKey', 'public-key');
  const shortId = q.take('sid', 'shortId', 'short-id');
  const pin = q.take('pinSHA256', 'fingerprint');
  if (security === 'none' && [sni, alpn, fp, insecure, publicKey, shortId, pin].some(value => value !== undefined))
    throw new Error('链接带有 TLS 参数，但未启用 TLS / Reality。');
  if (security === 'none') return;
  node.tls = true;
  if (sni) node[node.type === 'vless' || node.type === 'vmess' ? 'servername' : 'sni'] = sni;
  if (alpn)
    node.alpn = alpn
      .split(',')
      .map(item => item.trim())
      .filter(Boolean);
  if (fp) node['client-fingerprint'] = fp;
  if (insecure !== undefined) node['skip-cert-verify'] = insecure;
  if (pin) {
    const hex = pin.replace(/:/g, '');
    if (!/^[a-f\d]{64}$/i.test(hex)) throw new Error('证书指纹需为 SHA-256 十六进制格式；其他格式请用原生配置。');
    node.fingerprint = hex;
  }
  if (security === 'reality') {
    if (node.type === 'anytls')
      throw new Error('Mihomo 不支持 AnyTLS + Reality，请使用普通 AnyTLS TLS 或 VLESS Reality。');
    if (!publicKey || !/^[\w-]{43}=?$/.test(publicKey) || !sni)
      throw new Error('Reality 需要完整的 public key 和 SNI。');
    if (shortId && (!/^[a-f\d]{0,16}$/i.test(shortId) || shortId.length % 2))
      throw new Error('Reality short ID 需为最多 16 位的偶数长度十六进制。');
    node['reality-opts'] = { 'public-key': publicKey, 'short-id': shortId ?? '' };
    node['client-fingerprint'] ??= 'chrome';
  } else if (publicKey !== undefined || shortId !== undefined)
    throw new Error('Reality 参数存在，但 security 不是 reality。');
}

function transport(node: PrivateProxy, q: Params) {
  let network = q.take('type', 'network') ?? 'tcp';
  if (network === 'splithttp') network = 'xhttp';
  const host = q.take('host');
  const path = q.take('path');
  const header = q.take('headerType') ?? 'none';
  if (network === 'tcp' && header === 'http') network = 'http';
  if (!['none', 'http'].includes(header)) throw new Error('此伪装类型请使用 Clash 原生配置导入。');
  if (!['tcp', 'ws', 'grpc', 'h2', 'http', 'httpupgrade', 'xhttp'].includes(network))
    throw new Error('此传输的链接转换尚未支持，请使用 Clash YAML / JSON 保留传输参数。');
  if (
    (node.type === 'trojan' && !['tcp', 'ws', 'grpc', 'httpupgrade'].includes(network)) ||
    (node.type === 'vmess' && network === 'xhttp')
  )
    throw new Error('当前 Mihomo 不支持此协议与传输的组合。');
  if (network === 'tcp') {
    if (host || path) throw new Error('TCP 链接包含额外的传输参数，请检查 type 或改用原生配置。');
    node.network = 'tcp';
  } else if (network === 'ws' || network === 'httpupgrade') {
    node.network = 'ws';
    const early = q.take('ed', 'max-early-data');
    const earlyHeader = q.take('eh', 'early-data-header-name');
    node['ws-opts'] = {
      path: path ?? '/',
      ...(host ? { headers: { Host: host } } : {}),
      ...(network === 'httpupgrade' ? { 'v2ray-http-upgrade': true } : {}),
      ...(early ? { 'max-early-data': Number(early) } : {}),
      ...(earlyHeader ? { 'early-data-header-name': earlyHeader } : {}),
    };
    if (early && (!/^\d+$/.test(early) || Number(early) > 65535)) throw new Error('WebSocket early data 参数无效。');
  } else if (network === 'grpc') {
    const service = q.take('serviceName', 'service-name') ?? path ?? '';
    const mode = q.take('mode');
    if (host || (mode && mode !== 'gun')) throw new Error('这组 gRPC 参数请使用 Clash 原生配置导入。');
    node.network = 'grpc';
    node['grpc-opts'] = { 'grpc-service-name': service };
  } else if (network === 'xhttp') {
    const mode = q.take('mode') ?? 'auto';
    if (!['auto', 'packet-up', 'stream-up', 'stream-one'].includes(mode)) throw new Error('XHTTP mode 无效。');
    node.network = 'xhttp';
    node['xhttp-opts'] = { path: path ?? '/', ...(host ? { host } : {}), mode };
  } else {
    node.network = network;
    node[network === 'h2' ? 'h2-opts' : 'http-opts'] = {
      ...(host
        ? { [network === 'h2' ? 'host' : 'headers']: network === 'h2' ? host.split(',') : { Host: host.split(',') } }
        : {}),
      path: network === 'h2' ? (path ?? '/') : [path ?? '/'],
    };
  }
}

function ss(text: string): PrivateProxy {
  let source: string;
  const body = text.slice(5).split(/[?#]/)[0];
  if (!body.includes('@')) {
    const decoded = base64(body);
    const at = decoded.lastIndexOf('@');
    const colon = decoded.indexOf(':');
    if (at < 0 || colon < 1 || colon > at) throw new Error('旧版 Shadowsocks 链接不完整。');
    source = `ss://${encodeURIComponent(decoded.slice(0, colon))}:${encodeURIComponent(decoded.slice(colon + 1, at))}@${decoded.slice(at + 1)}${text.slice(5 + body.length)}`;
  } else {
    // 一些客户端仍输出标准 Base64，其中的 / 不能直接交给 URL 当路径分隔符。
    const at = body.lastIndexOf('@');
    let credentials = decode(body.slice(0, at));
    if (!credentials.includes(':')) credentials = base64(credentials);
    const colon = credentials.indexOf(':');
    if (colon < 1) throw new Error('Shadowsocks 认证信息不完整。');
    source = `ss://${encodeURIComponent(credentials.slice(0, colon))}:${encodeURIComponent(credentials.slice(colon + 1))}@${body.slice(at + 1)}${text.slice(5 + body.length)}`;
  }
  let url: URL;
  try {
    url = new URL(source);
  } catch {
    throw new Error('Shadowsocks 地址或端口不完整。');
  }
  let credentials = decode(url.username) + (url.password ? `:${decode(url.password)}` : '');
  if (!url.password && !credentials.includes(':')) credentials = base64(credentials);
  const colon = credentials.indexOf(':');
  if (colon < 1 || !credentials.slice(colon + 1)) throw new Error('Shadowsocks 需要加密方式和密码。');
  const node: PrivateProxy = {
    name: name(decode(url.hash.slice(1))),
    type: 'ss',
    server: url.hostname.replace(/^\[|\]$/g, ''),
    port: port(url.port),
    cipher: credentials.slice(0, colon),
    password: credentials.slice(colon + 1),
    udp: true,
  };
  const q = new Params(url.searchParams);
  const plugin = q.take('plugin');
  if (plugin) {
    // 含转义分隔符的插件配置由原生导入保留，避免错拆后静默丢失。
    if (plugin.includes('\\')) throw new Error('带转义字符的 SS 插件参数请用 Clash 原生配置导入。');
    const [kind, ...fields] = plugin.split(';');
    const options = new Map(
      fields.map(field => {
        const index = field.indexOf('=');
        return index < 0 ? [field, 'true'] : [field.slice(0, index), field.slice(index + 1)];
      }),
    );
    if (['obfs-local', 'simple-obfs', 'obfs'].includes(kind)) {
      if ([...options.keys()].some(key => !['obfs', 'obfs-host'].includes(key)))
        throw new Error('SS obfs 额外参数请用原生配置导入。');
      const mode = options.get('obfs');
      if (!mode || !['http', 'tls'].includes(mode)) throw new Error('SS obfs 需要 http 或 tls 模式。');
      node.plugin = 'obfs';
      node['plugin-opts'] = { mode, ...(options.get('obfs-host') ? { host: options.get('obfs-host') } : {}) };
    } else if (kind === 'v2ray-plugin') {
      if ([...options.keys()].some(key => !['tls', 'host', 'path', 'mode', 'mux'].includes(key)))
        throw new Error('SS v2ray-plugin 额外参数请用原生配置导入。');
      node.plugin = kind;
      node['plugin-opts'] = {
        mode: options.get('mode') ?? 'websocket',
        ...(options.has('tls') ? { tls: bool(options.get('tls')) } : {}),
        ...(options.has('mux') ? { mux: bool(options.get('mux')) } : {}),
        ...(options.get('host') ? { host: options.get('host') } : {}),
        ...(options.get('path') ? { path: options.get('path') } : {}),
      };
    } else throw new Error('此 SS 插件请使用 Clash 原生配置导入。');
  }
  q.done();
  return validateNative(node);
}

function vmess(text: string): PrivateProxy {
  let raw: Record<string, unknown>;
  try {
    raw = JSON.parse(base64(text.slice(8)));
  } catch {
    throw new Error('VMess 链接需要完整的 Base64 JSON 内容。');
  }
  const allowed = new Set([
    'v',
    'ps',
    'add',
    'port',
    'id',
    'aid',
    'scy',
    'security',
    'net',
    'type',
    'host',
    'path',
    'tls',
    'sni',
    'alpn',
    'fp',
    'allowInsecure',
  ]);
  if (!raw || typeof raw !== 'object' || Object.keys(raw).some(key => !allowed.has(key)))
    throw new Error('VMess 含有尚未转换的字段，请使用 Clash 原生配置保留完整参数。');
  const node: PrivateProxy = {
    name: name(raw.ps),
    type: 'vmess',
    server: String(raw.add ?? ''),
    port: port(raw.port),
    uuid: uuid(String(raw.id ?? '')),
    alterId: Number(raw.aid ?? 0),
    cipher: String(raw.scy ?? raw.security ?? 'auto'),
    udp: true,
  };
  if (!Number.isInteger(node.alterId) || Number(node.alterId) < 0) throw new Error('VMess alterId 无效。');
  if (!['auto', 'none', 'zero', 'aes-128-gcm', 'chacha20-poly1305'].includes(String(node.cipher)))
    throw new Error('VMess 加密方式无效。');
  const params = new URLSearchParams();
  params.set('security', raw.tls === 'tls' || raw.tls === true ? 'tls' : 'none');
  if (raw.tls && raw.tls !== 'tls' && raw.tls !== true && raw.tls !== 'none')
    throw new Error('VMess 安全层请用原生配置导入。');
  for (const key of ['sni', 'alpn', 'fp', 'allowInsecure'])
    if (raw[key] != null && raw[key] !== '') params.set(key, String(raw[key]));
  params.set('type', String(raw.net ?? 'tcp'));
  for (const key of ['host', 'path']) if (raw[key]) params.set(key, String(raw[key]));
  if (raw.type && raw.type !== 'none') params.set('headerType', String(raw.type));
  const q = new Params(params);
  tls(node, q);
  transport(node, q);
  q.done();
  return validateNative(node);
}

export function parsePrivateNode(input: string): PrivateProxy {
  const text = input.trim();
  if (!text) throw new Error('请先粘贴自建节点链接。');
  if (new TextEncoder().encode(text).byteLength > MAX_INPUT || /[\r\n]/.test(text))
    throw new Error('单节点解析需要一条完整链接；多行内容请使用批量导入。');
  if (/^ss:\/\//i.test(text)) return ss(text);
  if (/^vmess:\/\//i.test(text)) return vmess(text);
  let source = text;
  let hopPorts: string | undefined;
  if (/^(hysteria2|hy2):\/\//i.test(source)) {
    source = source.replace(/(@|\/\/)(\[[^\]]+\]|[^:/?#@]+):([\d,-]+)(?=[/?#]|$)/, (match, prefix, host, ports) => {
      if (!/[,-]/.test(ports)) return match;
      if (
        !ports.split(',').every((item: string) => {
          const [start, end = start] = item.split('-');
          return (
            /^\d+$/.test(start) &&
            /^\d+$/.test(end) &&
            Number(start) >= 1 &&
            Number(end) <= 65535 &&
            Number(start) <= Number(end)
          );
        })
      )
        throw new Error('Hysteria 2 跳端口范围无效。');
      hopPorts = ports;
      return `${prefix}${host}:${ports.split(/[,-]/)[0]}`;
    });
  }
  let url: URL;
  try {
    url = new URL(source);
  } catch {
    throw new Error('链接格式不完整，请检查协议、地址和端口。');
  }
  const scheme = url.protocol.slice(0, -1).toLowerCase();
  const type = scheme === 'hy2' ? 'hysteria2' : scheme === 'socks' ? 'socks5' : scheme === 'https' ? 'http' : scheme;
  if (!['vless', 'trojan', 'hysteria2', 'anytls', 'tuic', 'socks5', 'http'].includes(type))
    throw new Error('未识别此分享链接。可改贴 Clash YAML / JSON 导入原生节点。');
  if (!url.hostname || (url.pathname && url.pathname !== '/'))
    throw new Error('请粘贴节点链接，而非订阅网址；也可直接粘贴订阅中的 Clash 节点配置。');
  const q = new Params(url.searchParams);
  const queryName = q.take('remarks', 'name');
  const node: PrivateProxy = {
    name: name(decode(url.hash.slice(1)) || queryName),
    type,
    server: url.hostname.replace(/^\[|\]$/g, ''),
    port: port(url.port, type === 'http' && scheme !== 'https' ? 80 : type === 'socks5' ? undefined : 443),
  };
  let user = decode(url.username),
    pass = decode(url.password);
  if (scheme === 'socks' && user && !pass) {
    const credentials = base64(user),
      split = credentials.indexOf(':');
    if (split < 1) throw new Error('SOCKS 认证信息不完整。');
    user = credentials.slice(0, split);
    pass = credentials.slice(split + 1);
  }
  const auth = user + (url.password ? `:${pass}` : '');
  if (type === 'http' || type === 'socks5') {
    if (Boolean(user) !== Boolean(pass)) throw new Error('认证信息需要同时填写用户名和密码。');
    if (user) {
      node.username = user;
      node.password = pass;
    }
    if (scheme === 'https') {
      tls(node, q, 'tls');
      node.sni ??= url.hostname;
    }
    if (type === 'socks5') node.udp = true;
  } else if (type === 'vless') {
    node.uuid = uuid(user);
    if (pass) throw new Error('VLESS 用户信息格式不完整。');
    node.udp = true;
    const encryption = q.take('encryption');
    if (encryption) node.encryption = encryption;
    const flow = q.take('flow');
    if (flow) node.flow = flow;
    const packet = q.take('packetEncoding', 'packet-encoding');
    if (packet) node['packet-encoding'] = packet;
    tls(node, q);
    transport(node, q);
  } else if (type === 'tuic') {
    node.uuid = uuid(user);
    if (!pass) throw new Error('TUIC v5 链接需要密码。');
    node.password = pass;
    const congestion = q.take('congestion_control', 'congestion-controller');
    if (congestion) node['congestion-controller'] = congestion;
    const relay = q.take('udp_relay_mode', 'udp-relay-mode');
    if (relay) node['udp-relay-mode'] = relay;
    for (const [field, aliases] of [
      ['disable-sni', ['disable_sni', 'disable-sni']],
      ['reduce-rtt', ['reduce_rtt', 'reduce-rtt']],
    ] as const) {
      const value = bool(q.take(...aliases));
      if (value !== undefined) node[field] = value;
    }
    tls(node, q, 'tls');
  } else {
    if (!auth) throw new Error('节点链接缺少认证密码。');
    node.password = auth;
    node.udp = true;
    tls(node, q, 'tls');
    if (type === 'trojan') transport(node, q);
    if (type === 'hysteria2') {
      const obfs = q.take('obfs'),
        obfsPassword = q.take('obfs-password');
      if (obfs) {
        if (!['salamander', 'gecko'].includes(obfs) || !obfsPassword)
          throw new Error('Hysteria 2 混淆需要有效类型和混淆密码。');
        node.obfs = obfs;
        node['obfs-password'] = obfsPassword;
      } else if (obfsPassword) throw new Error('混淆密码存在，但未指定混淆类型。');
      if (hopPorts) node.ports = hopPorts;
      const hop = q.take('hop-interval');
      if (hop) {
        if (!/^\d+$/.test(hop)) throw new Error('跳端口间隔需要使用秒数。');
        node['hop-interval'] = Number(hop);
      }
      for (const field of ['up', 'down']) {
        const value = q.take(field);
        if (value) node[field] = value;
      }
    }
    if (type === 'anytls')
      for (const field of ['idle-session-check-interval', 'idle-session-timeout', 'min-idle-session']) {
        const value = q.take(field);
        if (value !== undefined) {
          if (!/^\d+$/.test(value)) throw new Error('AnyTLS 会话参数需要非负整数。');
          node[field] = Number(value);
        }
      }
  }
  q.done();
  return validateNative(node);
}

function clean(value: unknown, depth = 0): unknown {
  if (depth > 20) throw new Error('节点配置嵌套过深。');
  if (typeof value === 'number' && !Number.isFinite(value)) throw new Error('节点配置包含无效数字。');
  if (value === null || ['string', 'number', 'boolean'].includes(typeof value)) return value;
  if (Array.isArray(value)) return value.map(item => clean(item, depth + 1));
  if (typeof value !== 'object' || !value) throw new Error('节点配置只能包含普通数据。');
  const result: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value)) {
    if (['__proto__', 'constructor', 'prototype'].includes(key)) throw new Error('节点配置包含不允许的对象字段。');
    result[key] = clean(item, depth + 1);
  }
  return result;
}
function validateNative(value: unknown): PrivateProxy {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('每个节点需要一个完整的配置对象。');
  const node = clean(value) as PrivateProxy;
  if (!NATIVE_TYPES.has(node.type)) throw new Error('节点 type 不是当前支持的 Mihomo 出站协议。');
  if (
    typeof node.server !== 'string' ||
    !node.server.trim() ||
    /[\s/]/.test(node.server) ||
    node.server.includes(String.fromCharCode(0))
  )
    throw new Error('节点缺少有效的 server 地址。');
  node.name = name(node.name);
  node.port = port(node.port);
  if (node['dialer-proxy'] || node.detour)
    throw new Error('导入节点已设置前置代理，请先移除此字段，再在本页选择平台前置。');
  if (node.type === 'anytls' && node['reality-opts']) throw new Error('Mihomo 不支持 AnyTLS + Reality。');
  if (node.type === 'tuic' && !node.token && !(node.uuid && node.password)) throw new Error('TUIC 节点缺少认证字段。');
  for (const key of node.type === 'vless' || node.type === 'vmess'
    ? ['uuid']
    : node.type === 'ss'
      ? ['password', 'cipher']
      : ['trojan', 'anytls', 'hysteria2'].includes(node.type)
        ? ['password']
        : []) {
    if (typeof node[key] !== 'string' || !node[key]) throw new Error('节点缺少必要的认证字段。');
  }
  return node;
}

/** 原子解析整批内容，不发起网络请求；调用者在全部成功后再更新临时列表。 */
export function parsePrivateNodes(input: string): PrivateProxy[] {
  const text = input.trim();
  if (!text) throw new Error('请先粘贴节点链接或 Clash 配置。');
  if (new TextEncoder().encode(text).byteLength > MAX_INPUT) throw new Error('单次导入内容请控制在 1 MiB 以内。');
  let values: unknown[];
  if (/^[a-z][\w+-]*:\/\//i.test(text)) {
    const lines = text
      .split(/\r?\n/)
      .map(line => line.trim())
      .filter(Boolean);
    values = lines.map((line, index) => {
      try {
        return parsePrivateNode(line);
      } catch (error) {
        throw new Error(`第 ${index + 1} 条未导入：${(error as Error).message}`, { cause: error });
      }
    });
  } else if (/^[A-Za-z0-9+/_=-]+$/.test(text) && text.length > 20) {
    const decoded = base64(text);
    if (!/^[a-z][\w+-]*:\/\//i.test(decoded.trim())) throw new Error('Base64 内容中未识别到节点分享链接。');
    return parsePrivateNodes(decoded);
  } else {
    let document: unknown;
    try {
      const parsed = parseDocument(text, { schema: 'core', uniqueKeys: true, logLevel: 'silent' });
      if (parsed.errors.length || parsed.warnings.length) throw new Error('invalid');
      document = parsed.toJS({ maxAliasCount: 0 });
    } catch {
      throw new Error('Clash YAML / JSON 格式无效；不支持重复键、别名引用或自定义标签。');
    }
    const object = document as Record<string, unknown> | null;
    values = Array.isArray(document) ? document : object && Array.isArray(object.proxies) ? object.proxies : [document];
    values = values.map((item, index) => {
      try {
        return validateNative(item);
      } catch (error) {
        throw new Error(`第 ${index + 1} 个节点未导入：${(error as Error).message}`, { cause: error });
      }
    });
  }
  if (!values.length || values.length > 200) throw new Error('单次请导入 1–200 个节点。');
  return values as PrivateProxy[];
}
