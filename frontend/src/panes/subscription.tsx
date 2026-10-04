import { useState, type CSSProperties, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchArtifactContent,
  fetchClashSubscription,
  fetchMyArtifact,
  fetchMyClashSubscription,
  fetchRevisions,
  issueClashHaitunSubscription,
  issueMyClashHaitunSubscription,
  revokeClashHaitunSubscription,
  revokeMyClashHaitunSubscription,
  type ArtifactFamily,
  type ArtifactProtocol,
  type ClashSubscriptionInfo,
} from '../api';
import { ErrorBox, Loading, SegmentedControl } from '../ui/bits';
import { CopyButton } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { bytes } from '../ui/format';
import { Icon } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';

export type SubscriptionKind = 'uri' | 'clash';

type FamilyPick = 'both' | ArtifactFamily;
type ProtocolPick = 'both' | ArtifactProtocol;

const FAMILY_PICKS: { value: FamilyPick; label: string }[] = [
  { value: 'both', label: '双栈' },
  { value: 'v4', label: 'IPv4' },
  { value: 'v6', label: 'IPv6' },
];

const PROTOCOL_PICKS: { value: ProtocolPick; label: string }[] = [
  { value: 'both', label: '全部' },
  { value: 'vless', label: 'VLESS' },
  { value: 'anytls', label: 'AnyTLS' },
  { value: 'hysteria2', label: 'Hysteria 2' },
];

/* 订阅与节点共用一个弹窗：标题栏切换「订阅 / 节点」，协议与地址族筛选两个页签共用。
 * 两边的内容都由服务端按筛选条件生成：订阅是带 family / protocol 参数的地址，节点是同一条件下的
 * uri.txt。浏览器只把 uri.txt 解析成列表用于展示，不自行过滤条目。 */
export function SubscriptionViewer({
  tenant,
  user,
  kind,
  selfService = false,
  identity,
  onClose,
}: {
  tenant: string;
  user: string;
  kind: SubscriptionKind;
  selfService?: boolean;
  identity?: ReactNode;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<SubscriptionKind>(kind);
  const [protocol, setProtocol] = useState<ProtocolPick>('both');
  const [family, setFamily] = useState<FamilyPick>('both');
  const [allowInsecure, setAllowInsecure] = useState(false);

  return (
    <DialogLayer label={`${user} 的订阅与节点`} onClose={onClose}>
      <section className="dialog-surface sub-dialog">
        <header className="sub-dialog-head">
          {identity}
          <b className="sub-dialog-user">{user}</b>
          <SegmentedControl
            className="sub-dialog-tabs"
            ariaLabel="订阅与节点"
            value={tab}
            onChange={setTab}
            options={[
              {
                value: 'clash',
                label: (
                  <>
                    <Icon of="subscription" size={12} className="sub-dialog-tab-icon" />
                    订阅
                  </>
                ),
              },
              {
                value: 'uri',
                label: (
                  <>
                    <Icon of="client" size={12} className="sub-dialog-tab-icon" />
                    节点
                  </>
                ),
              },
            ]}
          />
          <DialogClose className="sub-dialog-close" title="关闭" aria-label="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="sub-dialog-filter">
          <span className="sub-dialog-field">
            <span>协议</span>
            <SegmentedControl
              ariaLabel="订阅协议"
              value={protocol}
              options={PROTOCOL_PICKS}
              onChange={next => {
                setProtocol(next);
                // VLESS 没有可互通的自签证书链接表达，切到 VLESS 时收回 insecure。
                if (next === 'vless') setAllowInsecure(false);
              }}
            />
          </span>
          <span className="sub-dialog-field">
            <span>地址族</span>
            <SegmentedControl ariaLabel="地址族" value={family} options={FAMILY_PICKS} onChange={setFamily} />
          </span>
        </div>
        {tab === 'clash' ? (
          <ClashSubscription
            tenant={tenant}
            user={user}
            selfService={selfService}
            protocol={protocol}
            family={family}
          />
        ) : (
          <NodeLinks
            tenant={tenant}
            user={user}
            selfService={selfService}
            protocol={protocol}
            family={family}
            allowInsecure={allowInsecure}
            onAllowInsecure={setAllowInsecure}
            onUseSubscription={() => setTab('clash')}
          />
        )}
      </section>
    </DialogLayer>
  );
}

/* ── 订阅 ── */

function ClashSubscription({
  tenant,
  user,
  selfService,
  protocol,
  family,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  protocol: ProtocolPick;
  family: FamilyPick;
}) {
  const qc = useQueryClient();
  const queryKey = ['clash-subscription', tenant, user] as const;
  // 地址本身是访问凭据：只在打开「订阅」页签时读取，不随用户列表下发。
  const subscription = useQuery({
    queryKey,
    queryFn: () => (selfService ? fetchMyClashSubscription() : fetchClashSubscription(tenant, user)),
    staleTime: 0,
  });
  const updateHaitun = (haitun: ClashSubscriptionInfo['haitun']) => {
    qc.setQueryData<ClashSubscriptionInfo>(queryKey, current => (current ? { ...current, haitun } : current));
  };
  const issueHaitun = useMutation({
    mutationFn: () => (selfService ? issueMyClashHaitunSubscription() : issueClashHaitunSubscription(tenant, user)),
    onSuccess: updateHaitun,
  });
  const revokeHaitun = useMutation({
    mutationFn: () => (selfService ? revokeMyClashHaitunSubscription() : revokeClashHaitunSubscription(tenant, user)),
    onSuccess: updateHaitun,
  });

  if (subscription.isPending || subscription.error) {
    return (
      <div className="sub-dialog-body">
        {subscription.error ? <ErrorBox error={subscription.error} /> : <Loading variant="code" />}
      </div>
    );
  }

  const value = subscription.data;
  const standardUrl = withSubscriptionProtocol(value.urls[family], protocol);
  const haitun = value.haitun;
  const haitunUrls = haitun.status === 'active' ? haitun.urls : null;
  const haitunUrl = haitunUrls ? withSubscriptionProtocol(haitunUrls[family], protocol) : '';
  const actionError = issueHaitun.error ?? revokeHaitun.error;

  return (
    <>
      <div className="sub-dialog-body">
        <section className="sub-section">
          <div className="sub-section-head">
            <b>Clash 订阅</b>
            <span className="sub-section-meta">模板 {value.template} · 适用 Mihomo / Clash Meta</span>
          </div>
          {/* key 随地址变化：切换协议或地址族后重新遮罩。 */}
          <SubscriptionUrl key={standardUrl} url={standardUrl} name="Clash 订阅地址" />
          <dl className="sub-readings">
            <div>
              <dt>剩余流量</dt>
              <dd className={value.remaining_bytes === 0 ? 'bad' : undefined}>
                {value.remaining_bytes === null ? '不限量' : bytes(value.remaining_bytes)}
              </dd>
            </div>
            <div>
              <dt>重置时间</dt>
              <dd>{formatResetAt(value.reset_at)}</dd>
            </div>
          </dl>
        </section>
        <section className="sub-section">
          <div className="sub-section-head">
            <b>koipy 测速订阅</b>
            <span className={`sub-live${haitunUrls ? ' ok' : haitun.status === 'revoked' ? ' warn' : ''}`}>
              {haitunUrls ? '可用' : haitun.status === 'revoked' ? '已撤销' : '尚未生成'}
            </span>
            {haitunUrls ? (
              <button
                type="button"
                className="sub-text-action danger"
                aria-label="撤销 koipy 测速地址"
                disabled={revokeHaitun.isPending}
                onClick={() => revokeHaitun.mutate()}
              >
                {revokeHaitun.isPending ? '撤销中…' : '撤销'}
              </button>
            ) : (
              <button
                type="button"
                className="btn sub-section-action"
                aria-label={haitun.status === 'revoked' ? '重新生成 koipy 测速地址' : '生成 koipy 测速地址'}
                disabled={issueHaitun.isPending}
                onClick={() => issueHaitun.mutate()}
              >
                {issueHaitun.isPending ? '生成中…' : haitun.status === 'revoked' ? '重新生成' : '生成'}
              </button>
            )}
          </div>
          {haitunUrls && <SubscriptionUrl key={haitunUrl} url={haitunUrl} name="koipy 测速地址" />}
          <p className="sub-section-note">
            {haitunUrls
              ? '独立 Token，只含测速需要的节点与链路。撤销后测速端无法再拉取；已下载的节点凭据需更换 UUID 才失效。'
              : haitun.status === 'revoked'
                ? '旧测速地址已失效。重新生成会签发新的 Token。'
                : '生成独立 Token 的测速订阅，不影响上方的 Clash 订阅。'}
          </p>
          {actionError && <ErrorBox error={actionError} />}
        </section>
      </div>
      <footer className="sub-dialog-foot">
        <span>每次拉取按当前授权实时生成，不缓存</span>
        <span>更换 UUID 后旧地址立即失效</span>
      </footer>
    </>
  );
}

function SubscriptionUrl({ url, name }: { url: string; name: string }) {
  const [revealed, setRevealed] = useState(false);
  return (
    <div className="sub-url">
      <code title={revealed ? url : undefined}>{revealed ? url : maskSubscriptionUrl(url)}</code>
      <button
        type="button"
        className="sub-url-reveal"
        aria-label={`${revealed ? '隐藏' : '显示'} ${name}`}
        onClick={() => setRevealed(current => !current)}
      >
        {revealed ? '隐藏' : '显示'}
      </button>
      <CopyButton className="sub-url-copy" text={url} />
    </div>
  );
}

/* ── 节点 ── */

const NODE_PROTOCOL_LABEL: Record<NodeProtocol, string> = {
  vless: 'VLESS',
  'vless-encryption': 'VLESS',
  anytls: 'AnyTLS',
  hysteria2: 'Hysteria 2',
  other: '其他',
};

type NodeSlotsStyle = CSSProperties & { '--node-slots': number };

function NodeLinks({
  tenant,
  user,
  selfService,
  protocol,
  family,
  allowInsecure,
  onAllowInsecure,
  onUseSubscription,
}: {
  tenant: string;
  user: string;
  selfService: boolean;
  protocol: ProtocolPick;
  family: FamilyPick;
  allowInsecure: boolean;
  onAllowInsecure: (allow: boolean) => void;
  onUseSubscription: () => void;
}) {
  const [view, setView] = useState<'list' | 'raw'>('list');
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), refetchInterval: 10_000 });
  const revision = revisions.data?.current_revision;
  const content = useQuery({
    queryKey: ['artifact', revision, 'user', `${tenant}:${user}`, 'uri', family, protocol, allowInsecure],
    queryFn: () =>
      selfService
        ? fetchMyArtifact(
            family === 'both' ? undefined : family,
            protocol === 'both' ? undefined : protocol,
            allowInsecure,
          )
        : fetchArtifactContent(
            'user',
            `${tenant}:${user}`,
            'uri',
            undefined,
            family === 'both' ? undefined : family,
            protocol === 'both' ? undefined : protocol,
            true,
            allowInsecure,
          ),
    enabled: !!revision,
  });

  if (!revision || content.isPending || content.error) {
    return (
      <div className="sub-dialog-body">
        {content.error ? <ErrorBox error={content.error} /> : <Loading variant="code" />}
      </div>
    );
  }

  const text = content.data.content ?? '';
  const listing = parseNodeListing(text);
  const groups = groupNodeLinks(listing.links);
  const slots: NodeFamily[] = family === 'both' ? ['ipv4', 'ipv6'] : [family === 'v4' ? 'ipv4' : 'ipv6'];
  const slotsStyle: NodeSlotsStyle = { '--node-slots': slots.length };
  const insecureCount = listing.links.filter(link => link.insecure).length;
  const lines = text ? text.replace(/\n$/, '').split('\n') : [];

  return (
    <>
      <div className="sub-dialog-body">
        <div className="node-toolbar">
          <span className="node-count">
            <b>{listing.links.length}</b> 个节点
          </span>
          <span className="sp" />
          {protocol !== 'vless' && (
            <button
              type="button"
              className="node-insecure"
              aria-pressed={allowInsecure}
              title="列出自签证书节点；客户端不验证证书"
              onClick={() => onAllowInsecure(!allowInsecure)}
            >
              <span className="node-insecure-switch" aria-hidden="true" />
              允许 insecure
            </button>
          )}
          <SegmentedControl
            className="node-view"
            ariaLabel="显示方式"
            value={view}
            onChange={setView}
            options={[
              { value: 'list', label: '列表' },
              { value: 'raw', label: '原文' },
            ]}
          />
          <CopyButton
            className="btn node-copy-all"
            text={listing.links.map(link => link.uri).join('\n')}
            label="复制全部"
          />
        </div>
        {(listing.notes.length > 0 || (allowInsecure && insecureCount > 0)) && (
          <div className="node-notes">
            {allowInsecure && insecureCount > 0 && (
              <p className="warn">
                <Icon of="warn" size={13} className="node-note-icon" />
                <span className="node-note-text">已列出 {insecureCount} 个自签证书节点，客户端不验证证书</span>
              </p>
            )}
            {listing.notes.map(note => (
              <p key={note}>
                <Icon of="info" size={13} className="node-note-icon" />
                <span className="node-note-text">{note}</span>
                {note.includes('Clash') && (
                  <button type="button" className="sub-text-action" onClick={onUseSubscription}>
                    改用订阅
                  </button>
                )}
              </p>
            ))}
          </div>
        )}
        {view === 'raw' ? (
          <div className="cfg-code node-raw">
            <div className="lnum">{lines.map((_, index) => index + 1).join('\n')}</div>
            <pre className="cd">{lines.join('\n')}</pre>
          </div>
        ) : groups.length === 0 ? (
          <div className="node-empty">没有符合当前协议与地址族的节点</div>
        ) : (
          <div className="node-groups" style={slotsStyle}>
            {groups.map(group => (
              <section className="node-group" key={group.key} aria-label={group.name}>
                <div className="node-group-head">
                  <RegionFlag code={group.region} />
                  <b>{group.name}</b>
                </div>
                {group.rows.map(row => (
                  <div className="node-row" key={row.key}>
                    <span className="node-protocol">{NODE_PROTOCOL_LABEL[row.protocol]}</span>
                    <span className="node-stack">
                      {row.stack}
                      {row.insecure && <em>insecure</em>}
                    </span>
                    <code className="node-endpoint">{row.endpoint}</code>
                    <span className="node-copies">
                      {slots.map(slot => {
                        const link = row.links[slot];
                        const familyLabel = slot === 'ipv6' ? 'IPv6' : 'IPv4';
                        return link ? (
                          <CopyButton
                            key={slot}
                            className="node-copy"
                            text={link.uri}
                            label={familyLabel}
                            aria-label={`复制 ${link.name}（${familyLabel}）`}
                            title={`${link.name} · ${link.endpoint}`}
                          />
                        ) : (
                          <span key={slot} className="node-copy-gap" aria-hidden="true" />
                        );
                      })}
                    </span>
                  </div>
                ))}
              </section>
            ))}
          </div>
        )}
      </div>
      <footer className="sub-dialog-foot">
        <span>修订 {content.data.revision}</span>
        <span>更换 UUID 后全部节点链接失效</span>
      </footer>
    </>
  );
}

/* ── 节点链接解析 ──
 * 输入是服务端生成的 uri.txt：每行一条分享链接，# 开头的行是说明，空行分隔说明段落。
 * 名称的写法与 core/physical/user.rs 一致：可选的地区旗（两个区域指示符）+ 链名 + 协议后缀
 * （「 | VLESS Encryption」「 | QUIC」「 | AnyTLS」）+ 地址族后缀（「 | v6」）。 */

export type NodeProtocol = 'vless' | 'vless-encryption' | 'anytls' | 'hysteria2' | 'other';
export type NodeFamily = 'ipv4' | 'ipv6';

export interface NodeLink {
  uri: string;
  /** 客户端看到的名称，去掉地区旗。 */
  name: string;
  /** 去掉协议与地址族后缀的链名，用于分组。 */
  base: string;
  region: string | null;
  protocol: NodeProtocol;
  family: NodeFamily;
  endpoint: string;
  stack: string;
  insecure: boolean;
}

export interface NodeRow {
  key: string;
  protocol: NodeProtocol;
  stack: string;
  endpoint: string;
  insecure: boolean;
  links: Partial<Record<NodeFamily, NodeLink>>;
}

export interface NodeGroup {
  key: string;
  name: string;
  region: string | null;
  rows: NodeRow[];
}

const REGIONAL_INDICATOR_A = 0x1f1e6;
const FAMILY_SUFFIX = ' | v6';
const WIRE_SUFFIXES = [' | VLESS Encryption', ' | QUIC', ' | AnyTLS'];

// 浏览器不一定装有彩色 emoji 字体，地区旗拆成代码后交给 RegionFlag 的雪碧图显示。
function splitRegion(name: string): { region: string | null; label: string } {
  const points = [...name];
  const letters = points.slice(0, 2).map(point => (point.codePointAt(0) ?? 0) - REGIONAL_INDICATOR_A);
  if (letters.length === 2 && letters.every(letter => letter >= 0 && letter < 26)) {
    return { region: String.fromCharCode(...letters.map(letter => 65 + letter)), label: points.slice(2).join('') };
  }
  return { region: null, label: name };
}

const stripRegions = (text: string) => text.replace(/[\u{1F1E6}-\u{1F1FF}]{2}/gu, '');

function baseName(label: string) {
  let base = label.endsWith(FAMILY_SUFFIX) ? label.slice(0, -FAMILY_SUFFIX.length) : label;
  const wire = WIRE_SUFFIXES.find(suffix => base.endsWith(suffix));
  if (wire) base = base.slice(0, -wire.length);
  return base;
}

function parseNodeLink(line: string): NodeLink {
  const hashAt = line.indexOf('#');
  let fragment = hashAt >= 0 ? line.slice(hashAt + 1) : '';
  try {
    fragment = decodeURIComponent(fragment);
  } catch {
    // 名称不是合法的百分号编码时按原文显示。
  }
  const { region, label } = splitRegion(fragment);
  const scheme = line.slice(0, Math.max(0, line.indexOf('://'))).toLowerCase();
  let endpoint = '—';
  let params = new URLSearchParams();
  try {
    const url = new URL(line);
    endpoint = url.host || endpoint;
    params = url.searchParams;
  } catch {
    // 没有可用地址的条目（服务端写作「?」）仍然列出，便于对照原文。
  }
  let protocol: NodeProtocol = 'other';
  let stack = '';
  if (scheme === 'vless') {
    const encryption = params.get('encryption');
    protocol = encryption && encryption !== 'none' ? 'vless-encryption' : 'vless';
    const security = params.get('security');
    const securityLabel =
      protocol === 'vless-encryption'
        ? 'Encryption'
        : security === 'reality'
          ? 'REALITY'
          : (security ?? '').toUpperCase();
    stack = [securityLabel, (params.get('type') ?? '').toUpperCase()].filter(Boolean).join(' · ');
  } else if (scheme === 'anytls') {
    protocol = 'anytls';
    stack = params.get('security') === 'reality' ? 'REALITY' : 'TLS';
  } else if (scheme === 'hysteria2' || scheme === 'hy2') {
    protocol = 'hysteria2';
    stack = 'QUIC';
  }
  return {
    uri: line,
    name: label,
    base: baseName(label),
    region,
    protocol,
    family: label.endsWith(FAMILY_SUFFIX) ? 'ipv6' : 'ipv4',
    endpoint,
    stack,
    insecure: params.get('insecure') === '1',
  };
}

export function parseNodeListing(text: string): { links: NodeLink[]; notes: string[] } {
  const links: NodeLink[] = [];
  const notes: string[] = [];
  let block: string[] = [];
  const flush = () => {
    // 段内各行接成一句：上一行以冒号或句读结尾时直接相接，否则补一个空格。
    const note = block
      .reduce((joined, line) => (joined && !/[：:，,。]$/.test(joined) ? `${joined} ${line}` : `${joined}${line}`), '')
      .trim();
    // 「（……）」是没有任何条目时的占位行，由空列表本身表达。
    if (note && !/^（.*）$/.test(note) && !notes.includes(note)) notes.push(note);
    block = [];
  };
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) {
      flush();
    } else if (line.startsWith('#')) {
      block.push(stripRegions(line.replace(/^#\s?/, '')).trim());
    } else {
      flush();
      links.push(parseNodeLink(line));
    }
  }
  flush();
  return { links, notes };
}

// 连续同名的链接归为一组（一条链的一个或多个接入点）；同一协议的 IPv6 条目并入前面缺 IPv6 的
// 那一行，作为该行的第二个复制目标。服务端按「协议 → 地址族」的顺序输出。
export function groupNodeLinks(links: NodeLink[]): NodeGroup[] {
  const groups: NodeGroup[] = [];
  for (const link of links) {
    let group = groups.at(-1);
    if (!group || group.name !== link.base || group.region !== link.region) {
      group = { key: `${groups.length}`, name: link.base, region: link.region, rows: [] };
      groups.push(group);
    }
    if (link.family === 'ipv6') {
      const pair = [...group.rows]
        .reverse()
        .find(row => row.protocol === link.protocol && row.stack === link.stack && row.links.ipv4 && !row.links.ipv6);
      if (pair) {
        pair.links.ipv6 = link;
        pair.insecure = pair.insecure || link.insecure;
        continue;
      }
    }
    group.rows.push({
      key: `${group.rows.length}`,
      protocol: link.protocol,
      stack: link.stack,
      endpoint: link.endpoint,
      insecure: link.insecure,
      links: { [link.family]: link },
    });
  }
  return groups;
}

function maskSubscriptionUrl(url: string): string {
  return url.replace(
    /(\/sub\/v1\/(?:haitun\/)?)([^/]+)(\/clash\.yaml)/,
    (_match, prefix: string, token: string, suffix: string) => {
      const tail = token.slice(-4);
      return `${prefix}••••••••-••••-••••-••••-••••••••${tail}${suffix}`;
    },
  );
}

function withSubscriptionProtocol(url: string, protocol: ProtocolPick): string {
  if (!url || protocol === 'both') return url;
  const selected = new URL(url);
  selected.searchParams.set('protocol', protocol);
  return selected.toString();
}

function formatResetAt(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return `${new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Hong_Kong',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(date)} +08`;
}
