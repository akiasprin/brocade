// Parts for the 来源封禁 mockup. The 在线设备 card is a faithful copy of production (same markup and classes, so the
// production styles apply) with ban affordances added; each option arranges the ban list differently.
import {
  Fragment,
  memo,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type FormEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import type {
  NetworkOperator,
  SnapshotApp,
  SourceAccessNode,
  UserOnlineSource,
  UserOnlineSourceAccess,
} from '../../src/api';
import { sessionDurationText } from '../../src/panes/users';
import { ErrorBox } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { CopyButton } from '../../src/ui/copy-button';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { bytes } from '../../src/ui/format';
import { Icon, PanelTitle } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { compareIp, normalizeIp, type World } from './source-access-mockup-data';

export interface BanApi {
  world: World;
  editable: boolean;
  apps: SnapshotApp[];
  nameOf: (id: string) => string;
  /** PUT of the whole list; resolves once the mock Console accepted it. */
  save: (ips: string[]) => Promise<void>;
}

const MAX_BANS = 256;
const BAN_PREVIEW_LIMIT = 6;

/* ══ 与生产一致的在线设备工具（users.tsx 内部函数的副本） ══ */

const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const OPERATOR_NAMES = new Map<NetworkOperator, string>([
  ['chinanet', '电信'],
  ['cmcc', '移动'],
  ['unicom', '联通'],
  ['cernet', '教育网'],
  ['cstnet', '科技网'],
]);
const PROTOCOL_NAMES = new Map([
  ['vless', 'VLESS'],
  ['anytls', 'AnyTLS'],
  ['hysteria2', 'Hysteria2'],
  ['mtproto', 'MTProxy'],
  ['unknown', '协议未知'],
]);

const protocolSet = (accesses: UserOnlineSourceAccess[]) => {
  const protocols = new Set(
    accesses.flatMap(access =>
      access.protocols?.length
        ? access.protocols.map(protocol => (PROTOCOL_NAMES.has(protocol) ? protocol : 'unknown'))
        : ['unknown'],
    ),
  );
  if (!protocols.size) protocols.add('unknown');
  return [...PROTOCOL_NAMES].filter(([protocol]) => protocols.has(protocol)).map(([, name]) => name);
};
const protocolText = (accesses: UserOnlineSourceAccess[]) => protocolSet(accesses).join(' · ');
const protocolSummary = (accesses: UserOnlineSourceAccess[]) => {
  const labels = protocolSet(accesses);
  return labels.length <= 2 ? labels.join(' / ') : `${labels.length} 种协议`;
};

type PresenceChain = { key: string; name: string; accesses: UserOnlineSourceAccess[] };

function chainsOf(source: UserOnlineSource, apps: SnapshotApp[]): PresenceChain[] {
  const index = new Map(
    apps.flatMap(app =>
      app.ingresses.map(ingress => [ingress.id, { app, chain: app.chains.find(item => item.id === ingress.chain) }]),
    ),
  );
  const grouped = new Map<string, PresenceChain>();
  for (const access of source.accesses ?? []) {
    const resolved = index.get(access.ingress_id);
    const key = resolved?.chain ? `${resolved.app.id}/${resolved.chain.id}` : `unknown/${access.ingress_id}`;
    const current = grouped.get(key);
    if (current) current.accesses.push(access);
    else
      grouped.set(key, { key, name: resolved?.chain?.name ?? `接入链未知 · ${access.ingress_id}`, accesses: [access] });
  }
  return [...grouped.values()].sort((a, b) => a.name.localeCompare(b.name, 'zh-CN'));
}

const accessSummaryOf = (chains: PresenceChain[]) => {
  const all = chains.flatMap(chain => chain.accesses);
  if (chains.length === 0) return '接入链未知';
  return chains.length === 1
    ? `${chains[0].name} · ${protocolSummary(all)}`
    : `${chains.length} 条接入链 · ${protocolSummary(all)}`;
};

const LOCATION_TITLE = '本地 GeoIP · 国家／地区与网络归属参考，非精确位置或宽带品牌';

export function sourceNetwork(country: string | undefined, operator: NetworkOperator | undefined) {
  const code = country?.trim().toUpperCase();
  const name = code && /^[A-Z]{2}$/.test(code) && code !== 'ZZ' ? REGION_NAMES.of(code) : undefined;
  const region = name && name !== code ? name : undefined;
  const operatorName = operator ? OPERATOR_NAMES.get(operator) : undefined;
  if (!region) return { code: undefined, label: operatorName ? `位置未知 · ${operatorName}` : '位置未知' };
  if (!operatorName) return { code, label: region };
  return { code, label: code === 'CN' ? `中国${operatorName}` : `${region} · ${operatorName}` };
}

const observedAt = (at: string) => Date.parse(at);
const two = (value: number) => String(value).padStart(2, '0');
const clockOf = (t: number) => `${two(new Date(t).getHours())}:${two(new Date(t).getMinutes())}`;
const monthDayOf = (t: number) => `${two(new Date(t).getMonth() + 1)}-${two(new Date(t).getDate())}`;
const calendarDaysAgo = (t: number, today: number) =>
  Math.round((today - new Date(t).setHours(0, 0, 0, 0)) / 86_400_000);
const localTime = (at: string) => new Date(observedAt(at)).toLocaleString();
const observedTitle = (source: UserOnlineSource, online: boolean) =>
  [
    ...(source.online_since_at ? [`本次上线 ${localTime(source.online_since_at)}`] : []),
    `首次出现 ${localTime(source.first_observed_at)}`,
    `${online ? '最近确认在线' : '确认离线'} ${localTime(source.last_observed_at)}`,
  ].join('\n');

/** 同一天写时刻，更早写月-日：封禁组里的行不按天分组，单写时刻会读不出是哪天。 */
const seenStamp = (t: number, today: number) => (calendarDaysAgo(t, today) <= 0 ? clockOf(t) : monthDayOf(t));

function SessionFacts({
  uplink,
  downlink,
  onlineSince,
  until,
}: {
  uplink: number | null | undefined;
  downlink: number | null | undefined;
  onlineSince?: string | null;
  until: number;
}) {
  const hasTraffic = uplink != null && downlink != null;
  if (!hasTraffic && !onlineSince) return null;
  return (
    <span className="user-presence-session">
      {onlineSince && (
        <span className="user-presence-duration" title={`本次上线 ${localTime(onlineSince)}`}>
          <Icon of="clock" size={11} />
          <b>{sessionDurationText(onlineSince, until)}</b>
        </span>
      )}
      {hasTraffic && (
        <span className="user-presence-traffic" title={`本次会话流量：下行 ${bytes(downlink)} · 上行 ${bytes(uplink)}`}>
          <Icon of="usage" size={11} />
          <b>{bytes(uplink + downlink)}</b>
        </span>
      )}
    </span>
  );
}

type Bucket = 'online' | 'today' | 'yesterday' | 'week' | 'retention';
const BUCKETS: Bucket[] = ['online', 'today', 'yesterday', 'week', 'retention'];
const OLDER = new Set<Bucket>(['week', 'retention']);
const HISTORY_PREVIEW_LIMIT = 5;
const bucketOf = (lastSeen: number, today: number): Bucket => {
  const days = calendarDaysAgo(lastSeen, today);
  return days <= 0 ? 'today' : days === 1 ? 'yesterday' : days < 7 ? 'week' : 'retention';
};

function SourceAddress({ ip }: { ip: string }) {
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

function AddressCopy({ ip }: { ip: string }) {
  return (
    <CopyButton
      className="user-fcopy"
      text={ip}
      label={`复制来源 IP ${ip}`}
      successLabel="来源 IP 已复制"
      failureLabel="来源 IP 复制失败"
      iconOnly
    />
  );
}

/* ══ 封禁共用部件 ══ */

/** 禁行标记：圆 + 斜杠。尺寸与描边跟复制按钮里的图标一致。 */
export function BanGlyph({ size = 11 }: { size?: number }) {
  return (
    <svg className="sam-glyph" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="5.7" />
      <path d="M4 4 L12 12" />
    </svg>
  );
}

export interface BanRow {
  ip: string;
  online: boolean;
  source: UserOnlineSource | null;
  country: string | undefined;
  operator: NetworkOperator | undefined;
}

/** 名单行的次序：仍在线的在最前，其余按最后出现由近到远，30 天内没出现过的按地址排在最后。 */
export function banRowsOf(world: World): BanRow[] {
  const online = new Map(world.online.map(source => [source.ip, source]));
  const history = new Map(world.history.map(source => [source.ip, source]));
  const seen = (row: BanRow) => (row.source ? observedAt(row.source.last_observed_at) : 0);
  return world.bans
    .map(ip => ({
      ip,
      online: online.has(ip),
      source: online.get(ip) ?? history.get(ip) ?? null,
      country: world.countries[ip],
      operator: world.operators[ip],
    }))
    .sort((a, b) => Number(b.online) - Number(a.online) || seen(b) - seen(a) || compareIp(a.ip, b.ip));
}

type Tone = 'ok' | 'gold' | 'err' | 'idle';
const PHASE: Record<SourceAccessNode['phase'], { text: string; tone: Tone }> = {
  applied: { text: '已生效', tone: 'ok' },
  pending: { text: '待同步', tone: 'gold' },
  unsupported: { text: '需升级 Agent 与 Xray', tone: 'gold' },
  error: { text: '执行异常', tone: 'err' },
};

/** 名单整体的执行情况：一份名单一个版本，各机器回执的是整份名单，不是单个地址。 */
export function rolloutOf(nodes: SourceAccessNode[]): { tone: Tone; text: string; title: string } {
  if (nodes.length === 0) return { tone: 'idle', text: '无相关机器', title: '该用户没有接入授权，授权后自动执行' };
  const count = (phase: SourceAccessNode['phase']) => nodes.filter(node => node.phase === phase).length;
  const parts: string[] = [];
  if (count('error')) parts.push(`${count('error')} 台异常`);
  if (count('unsupported')) parts.push(`${count('unsupported')} 台待升级`);
  if (count('pending')) parts.push(`同步中 ${count('applied')}/${nodes.length}`);
  const tone: Tone = count('error') ? 'err' : parts.length ? 'gold' : 'ok';
  return { tone, text: parts.length ? parts.join(' · ') : '已生效', title: `${nodes.length} 台相关机器` };
}

function NodeList({ nodes, nameOf, only }: { nodes: SourceAccessNode[]; nameOf: (id: string) => string; only?: true }) {
  const shown = only ? nodes.filter(node => node.phase !== 'applied') : nodes;
  return (
    <ul className="sam-nodes" aria-label="各机器执行情况">
      {shown.map(node => (
        <li key={node.node_id}>
          <span className="sam-node-name">{nameOf(node.node_id)}</span>
          <span className={`sam-node-phase ${PHASE[node.phase].tone}`}>{PHASE[node.phase].text}</span>
          {node.error && <small>{node.error}</small>}
        </li>
      ))}
    </ul>
  );
}

/**
 * 卡头右端打开的面板与锚点右缘对齐，而不是按左缘往右伸出卡片（共享 popover 只会左对齐后夹在视口内）。
 * 锚点在视口左半时保持共享 popover 的放法。
 */
function useEndAlign(anchor: RefObject<HTMLElement | null>) {
  const [style, setStyle] = useState<CSSProperties | undefined>(undefined);
  // 打开即显示时，面板与锚点同一次提交挂载，渲染期间 ref 尚未就位，只能在布局阶段量。
  useLayoutEffect(() => {
    const rect = anchor.current?.getBoundingClientRect();
    if (rect && rect.left >= window.innerWidth / 2)
      setStyle({ '--sam-anchor-right': `${rect.right}px` } as CSSProperties);
  }, [anchor]);
  return { style, className: style ? ' end' : '' };
}

function NodesPopover({
  nodes,
  nameOf,
  anchor,
  onClose,
}: {
  nodes: SourceAccessNode[];
  nameOf: (id: string) => string;
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
}) {
  const align = useEndAlign(anchor);
  return (
    <DialogLayer mode="popover" anchor={anchor} label="各机器执行情况" onClose={onClose}>
      <div className={`dialog-surface sam-nodes-pop${align.className}`} style={align.style}>
        <NodeList nodes={nodes} nameOf={nameOf} />
      </div>
    </DialogLayer>
  );
}

/** 「已生效」等状态读数；点开列出每台机器。 */
export function RolloutStatus({
  nodes,
  nameOf,
  openOnLoad = false,
}: {
  nodes: SourceAccessNode[];
  nameOf: (id: string) => string;
  openOnLoad?: boolean;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(openOnLoad);
  const roll = rolloutOf(nodes);
  if (roll.tone === 'idle')
    return (
      <span className="sam-state idle" title={roll.title}>
        {roll.text}
      </span>
    );
  return (
    <>
      <button
        ref={ref}
        type="button"
        className={`sam-state ${roll.tone}`}
        aria-expanded={open}
        title={roll.title}
        onClick={() => setOpen(true)}
      >
        <span className="sam-dot" aria-hidden="true" />
        {roll.text}
      </button>
      {open && <NodesPopover nodes={nodes} nameOf={nameOf} anchor={ref} onClose={() => setOpen(false)} />}
    </>
  );
}

/** 点行内禁行钮后贴着它弹出的确认：写对象和后果，不另写说明。 */
function BanConfirm({
  ip,
  online,
  anchor,
  onClose,
  onConfirm,
}: {
  ip: string;
  online: boolean;
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  return (
    <DialogLayer mode="popover" anchor={anchor} label={`封禁 ${ip}`} onClose={onClose} canClose={() => !busy}>
      <div className="dialog-surface sam-confirm">
        <p className="sam-confirm-title">
          封禁 <SourceAddress ip={ip} />
        </p>
        <p className="sam-confirm-body">{online ? '断开该来源的会话，并拒绝再次接入。' : '拒绝该来源再次接入。'}</p>
        {!!error && <ErrorBox error={error} />}
        <footer>
          <DialogClose className="btn sm" disabled={busy}>
            取消
          </DialogClose>
          <button
            type="button"
            className="btn sm danger"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                await onConfirm();
                onClose();
              } catch (failure) {
                setError(failure);
                setBusy(false);
              }
            }}
          >
            {busy ? '封禁中…' : '封禁'}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}

function BanButton({
  ip,
  online,
  onBan,
  openOnLoad,
}: {
  ip: string;
  online: boolean;
  onBan: (ip: string) => Promise<void>;
  openOnLoad?: boolean;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(!!openOnLoad);
  return (
    <>
      <button
        ref={ref}
        type="button"
        className="user-fcopy sam-ban"
        aria-label={`封禁 ${ip}`}
        title="封禁此来源"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <BanGlyph />
      </button>
      {open && (
        <BanConfirm ip={ip} online={online} anchor={ref} onClose={() => setOpen(false)} onConfirm={() => onBan(ip)} />
      )}
    </>
  );
}

/** 手动输入的地址：输入框下方写它是谁（在线、地区、接入链），不再弹确认。 */
function AddBan({
  api,
  variant,
  onDone,
  autoFocus = false,
  initial = '',
}: {
  api: BanApi;
  variant: 'inline' | 'foot' | 'pop';
  onDone?: () => void;
  autoFocus?: boolean;
  initial?: string;
}) {
  const [value, setValue] = useState(initial);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const ip = normalizeIp(value);
  const known = ip
    ? (api.world.online.find(source => source.ip === ip) ?? api.world.history.find(source => source.ip === ip))
    : undefined;
  const online = !!ip && api.world.online.some(source => source.ip === ip);
  const full = api.world.bans.length >= MAX_BANS;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!value.trim() || busy) return;
    if (!ip) return setProblem('不是有效的 IPv4 / IPv6 地址');
    if (api.world.bans.includes(ip)) return setProblem('已在名单中');
    setBusy(true);
    setError(null);
    try {
      await api.save([...api.world.bans, ip]);
      setValue('');
      onDone?.();
    } catch (failure) {
      setError(failure);
    } finally {
      setBusy(false);
    }
  };
  const network = ip ? sourceNetwork(api.world.countries[ip], api.world.operators[ip]) : null;
  return (
    <div className={`sam-add ${variant}`}>
      <form onSubmit={submit}>
        <input
          className="f mono"
          value={value}
          placeholder="IPv4 / IPv6 地址"
          aria-label="来源 IP"
          aria-invalid={!!problem || undefined}
          autoComplete="off"
          spellCheck={false}
          autoFocus={autoFocus}
          disabled={busy || full}
          maxLength={45}
          onChange={event => {
            setValue(event.target.value);
            setProblem(null);
          }}
          onKeyDown={event => {
            if (event.key === 'Escape' && onDone) {
              event.preventDefault();
              onDone();
            }
          }}
        />
        <button type="submit" className="btn danger" disabled={!value.trim() || busy || full}>
          {busy ? '封禁中…' : '封禁'}
        </button>
        {onDone && variant === 'inline' && (
          <button type="button" className="btn" disabled={busy} onClick={onDone}>
            取消
          </button>
        )}
      </form>
      {problem ? (
        <p className="sam-add-note err" role="alert">
          {problem}
        </p>
      ) : full ? (
        <p className="sam-add-note">名单已满 256 个</p>
      ) : known && network ? (
        <p className={`sam-add-note${online ? ' gold' : ''}`}>
          {network.code && <RegionFlag code={network.code} />}
          {network.label} · {online ? '在线，封禁后断开' : `最后出现 ${monthDayOf(observedAt(known.last_observed_at))}`}
        </p>
      ) : null}
      {!!error && <ErrorBox error={error} />}
    </div>
  );
}

/** 每个方案都能从在线设备的行里直接封禁；被封禁的来源在行上标出。 */
function useUnban(api: BanApi) {
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  return {
    pending,
    error,
    unban: async (ip: string) => {
      setPending(ip);
      setError(null);
      try {
        await api.save(api.world.bans.filter(value => value !== ip));
      } catch (failure) {
        setError(failure);
      } finally {
        setPending(null);
      }
    },
  };
}

function UnbanButton({ ip, pending, onUnban }: { ip: string; pending: string | null; onUnban: (ip: string) => void }) {
  return (
    <button
      type="button"
      className="sam-link"
      disabled={pending !== null}
      aria-label={`解除封禁 ${ip}`}
      onClick={() => onUnban(ip)}
    >
      {pending === ip ? '解除中…' : '解除'}
    </button>
  );
}

/* ══ 在线设备卡（生产副本 + 封禁） ══ */

const PresenceRow = memo(function PresenceRow({
  source,
  online,
  older,
  country,
  operator,
  apps,
  now,
  ban,
}: {
  source: UserOnlineSource;
  online: boolean;
  older: boolean;
  country: string | undefined;
  operator: NetworkOperator | undefined;
  apps: SnapshotApp[];
  now: number;
  /** banned 标出状态；onBan 给出禁行钮。二者都没有时与生产相同。 */
  ban: { banned: boolean; onBan?: (ip: string) => Promise<void>; openOnLoad?: boolean };
}) {
  const [expanded, setExpanded] = useState(false);
  const accessId = useId();
  const lastSeen = observedAt(source.last_observed_at);
  const network = sourceNetwork(country, operator);
  const chains = chainsOf(source, apps);
  const time = online ? '' : older ? monthDayOf(lastSeen) : clockOf(lastSeen);
  return (
    <li
      className={`user-presence-row${online ? '' : ' off'}${expanded ? ' expanded' : ''}${ban.banned ? ' sam-row-banned' : ''}`}
    >
      <span className="user-presence-time" title={observedTitle(source, online)}>
        {time}
      </span>
      <span className={`user-presence-mark${ban.banned ? ' sam-mark-ban' : ''}`} aria-hidden="true" />
      <span className="user-presence-ip">
        <SourceAddress ip={source.ip} />
        {ban.banned && (
          <span
            className={`sam-tag ${online ? 'gold' : 'err'}`}
            title={online ? '名单已保存，等待机器断开' : undefined}
          >
            {online ? '封禁待生效' : '已封禁'}
          </span>
        )}
        <AddressCopy ip={source.ip} />
        {!ban.banned && ban.onBan && (
          <BanButton ip={source.ip} online={online} onBan={ban.onBan} openOnLoad={ban.openOnLoad} />
        )}
      </span>
      <span className="user-presence-sub">
        <span className="user-presence-location" title={LOCATION_TITLE}>
          {network.code && <RegionFlag code={network.code} />}
          <span>{network.label}</span>
        </span>
        <button
          type="button"
          className="user-presence-access-toggle"
          aria-expanded={expanded}
          aria-controls={accessId}
          disabled={chains.length === 0}
          onClick={() => setExpanded(value => !value)}
        >
          <span>{accessSummaryOf(chains)}</span>
          {chains.length > 0 && (
            <span className="user-presence-access-caret" aria-hidden="true">
              ›
            </span>
          )}
        </button>
      </span>
      <SessionFacts
        uplink={source.session_uplink_bytes}
        downlink={source.session_downlink_bytes}
        until={online ? now : lastSeen}
      />
      {expanded && chains.length > 0 && (
        <div id={accessId} className="user-presence-access">
          {chains.map(chain => (
            <span key={chain.key} className="user-presence-access-item">
              <span className="user-presence-access-name">
                <span className="user-presence-chain-link">{chain.name}</span>
                <span className="user-presence-protocols">{protocolText(chain.accesses)}</span>
              </span>
              <SessionFacts
                uplink={chain.accesses.reduce((sum, access) => sum + (access.session_uplink_bytes ?? 0), 0)}
                downlink={chain.accesses.reduce((sum, access) => sum + (access.session_downlink_bytes ?? 0), 0)}
                onlineSince={chain.accesses[0]?.online_since_at}
                until={online ? now : lastSeen}
              />
            </span>
          ))}
        </div>
      )}
    </li>
  );
});

/** 方案 A 的「已封禁」组：与时间线同一套列，时刻列写最后出现，节点换成禁行标记，行尾是解除。 */
function BannedGroup({
  api,
  rows,
  now,
  statusOpen,
}: {
  api: BanApi;
  rows: BanRow[];
  now: number;
  statusOpen: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const { pending, error, unban } = useUnban(api);
  const today = new Date(now).setHours(0, 0, 0, 0);
  const visible = expanded ? rows : rows.slice(0, BAN_PREVIEW_LIMIT);
  return (
    <div className="sam-banned">
      <ol className="user-presence-timeline sam-banned-list" aria-label="已封禁的来源">
        <li className="user-presence-day sam-banned-day">
          <span>已封禁</span>
          <RolloutStatus nodes={api.world.nodes} nameOf={api.nameOf} openOnLoad={statusOpen} />
        </li>
        {visible.map(row => {
          const network = sourceNetwork(row.country, row.operator);
          const seen = row.source ? observedAt(row.source.last_observed_at) : NaN;
          const chains = row.source ? chainsOf(row.source, api.apps) : [];
          return (
            <li key={row.ip} className={`user-presence-row sam-banned-row${row.online ? ' live' : ' off'}`}>
              <span
                className="user-presence-time"
                title={row.source ? observedTitle(row.source, row.online) : undefined}
              >
                {row.online || Number.isNaN(seen) ? '' : seenStamp(seen, today)}
              </span>
              <span className="user-presence-mark sam-mark-ban" aria-hidden="true" />
              <span className="user-presence-ip">
                <SourceAddress ip={row.ip} />
                <AddressCopy ip={row.ip} />
              </span>
              <span className="user-presence-sub">
                <span className="user-presence-location" title={LOCATION_TITLE}>
                  {network.code && <RegionFlag code={network.code} />}
                  <span>{network.label}</span>
                </span>
                <span className={`sam-banned-seen${row.online ? ' gold' : ''}`}>
                  {row.online
                    ? `仍在线 · ${accessSummaryOf(chains)}`
                    : row.source
                      ? accessSummaryOf(chains)
                      : api.world.presence === 'unavailable'
                        ? ''
                        : '30 天内未出现'}
                </span>
              </span>
              <span className="sam-row-end">
                {api.editable && <UnbanButton ip={row.ip} pending={pending} onUnban={unban} />}
              </span>
            </li>
          );
        })}
      </ol>
      {rows.length > BAN_PREVIEW_LIMIT && (
        <button
          type="button"
          className="user-presence-more"
          aria-expanded={expanded}
          onClick={() => setExpanded(value => !value)}
        >
          {expanded ? '收起' : `显示其余 ${rows.length - BAN_PREVIEW_LIMIT} 个`}
        </button>
      )}
      {!!error && <ErrorBox error={error} />}
    </div>
  );
}

export interface PresenceOptions {
  /** group：封禁地址移出时间线，在卡底自成一组（方案 A）。tags：留在原位、行上标出（B / C / D）。 */
  mode: 'group' | 'tags';
  headerExtra?: ReactNode;
  open?: OpenTarget;
}

export type OpenTarget = 'none' | 'ban' | 'add' | 'status' | 'list' | 'edit' | 'paste';

export function PresenceCard({ api, mode, headerExtra, open = 'none' }: { api: BanApi } & PresenceOptions) {
  const now = useNow();
  const today = new Date(now).setHours(0, 0, 0, 0);
  const [historyExpanded, setHistoryExpanded] = useState(false);
  const [adding, setAdding] = useState(open === 'add');
  const { world } = api;
  const banned = new Set(world.bans);
  const grouped = mode === 'group';
  const keep = (source: UserOnlineSource) => !grouped || !banned.has(source.ip);
  const online = world.online.filter(keep);
  const offline = world.history
    .filter(keep)
    .filter(source => !world.online.some(item => item.ip === source.ip))
    .sort((a, b) => observedAt(b.last_observed_at) - observedAt(a.last_observed_at));
  const visibleOffline = historyExpanded ? offline : offline.slice(0, HISTORY_PREVIEW_LIMIT);
  const groups = new Map<Bucket, UserOnlineSource[]>();
  if (online.length)
    groups.set(
      'online',
      [...online].sort((a, b) => observedAt(b.first_observed_at) - observedAt(a.first_observed_at)),
    );
  for (const source of visibleOffline) {
    const bucket = bucketOf(observedAt(source.last_observed_at), today);
    groups.set(bucket, [...(groups.get(bucket) ?? []), source]);
  }
  const label: Record<Bucket, string> = {
    online: '在线',
    today: '今天',
    yesterday: '昨天',
    week: '7 天内',
    retention: '30 天内',
  };
  const visibleBuckets = BUCKETS.filter(bucket => groups.has(bucket));
  const rows = banRowsOf(world);
  const onBan = api.editable ? (ip: string) => api.save([...world.bans, ip]) : undefined;
  // 打开即弹确认：取在线组里第一个尚未封禁的来源。
  const confirmTarget = open === 'ban' ? world.online.find(source => !banned.has(source.ip))?.ip : undefined;
  const allOnline = world.online.length;
  const seenTotal = new Set([...world.online, ...world.history].map(source => source.ip)).size;

  const reading =
    world.presence === 'unavailable' ? (
      '在线来源 —'
    ) : allOnline > 0 ? (
      <>
        在线 <b>{allOnline}</b> · 30 天 <b>{seenTotal}</b> 个地址
      </>
    ) : (
      '暂无在线连接'
    );
  const notice =
    world.presence === 'unavailable'
      ? { tone: 'warn', text: '尚未收到在线来源统计，暂时无法判断是否在线；这不代表用户已离线。' }
      : allOnline === 0 && offline.length > 0
        ? { tone: 'idle', text: '暂无在线连接' }
        : null;

  return (
    <section className="panel config-panel user-dcard user-presence-card sam-own">
      <header>
        <PanelTitle of="client">在线设备</PanelTitle>
        <span className="rt" title="按公网 IP 去重，不等于连接数；同一 IP 可同时使用多条接入链">
          {reading}
          {grouped && rows.length > 0 && (
            <>
              {' '}
              · 封禁 <b>{rows.length}</b>
            </>
          )}
        </span>
        {headerExtra}
      </header>
      <div className="user-dcard-body">
        {notice && <div className={`user-presence-note ${notice.tone}`}>{notice.text}</div>}
        {visibleBuckets.length > 0 && (
          <ol className="user-presence-timeline" aria-label="在线与最近 30 天的来源">
            {visibleBuckets.map(bucket => (
              <Fragment key={bucket}>
                <li className="user-presence-day">
                  <span>{label[bucket]}</span>
                </li>
                {groups.get(bucket)!.map(source => (
                  <PresenceRow
                    key={source.ip}
                    source={source}
                    online={bucket === 'online'}
                    older={OLDER.has(bucket)}
                    country={world.countries[source.ip]}
                    operator={world.operators[source.ip]}
                    apps={api.apps}
                    now={now}
                    ban={{
                      banned: banned.has(source.ip),
                      onBan,
                      openOnLoad: source.ip === confirmTarget,
                    }}
                  />
                ))}
              </Fragment>
            ))}
          </ol>
        )}
        {offline.length > HISTORY_PREVIEW_LIMIT && (
          <button
            type="button"
            className="user-presence-more"
            aria-expanded={historyExpanded}
            onClick={() => setHistoryExpanded(value => !value)}
          >
            {historyExpanded ? '收起历史来源' : `显示其余 ${offline.length - HISTORY_PREVIEW_LIMIT} 个历史来源`}
          </button>
        )}
        {grouped && rows.length > 0 && <BannedGroup api={api} rows={rows} now={now} statusOpen={open === 'status'} />}
        {grouped &&
          api.editable &&
          (adding ? (
            <AddBan api={api} variant="inline" autoFocus onDone={() => setAdding(false)} />
          ) : (
            <button type="button" className="user-presence-more sam-add-link" onClick={() => setAdding(true)}>
              <Icon of="plus" size={11} />
              {rows.length > 0 || visibleBuckets.length > 0 ? '封禁其他 IP' : '封禁 IP'}
            </button>
          ))}
      </div>
    </section>
  );
}

/* ══ 方案 B：独立名单卡 ══ */

/** 在线统计不可用时不知道是否出现过：写「—」，不写「未出现」。 */
function seenText(row: BanRow, today: number, known = true) {
  if (row.online) return '仍在线';
  if (!row.source) return known ? '30 天内未出现' : '—';
  return `最后出现 ${seenStamp(observedAt(row.source.last_observed_at), today)}`;
}

export function BanListCard({ api, open }: { api: BanApi; open: OpenTarget }) {
  const now = useNow();
  const today = new Date(now).setHours(0, 0, 0, 0);
  const [adding, setAdding] = useState(open === 'add');
  const [expanded, setExpanded] = useState(false);
  const { pending, error, unban } = useUnban(api);
  const rows = banRowsOf(api.world);
  const empty = rows.length === 0;
  const visible = expanded ? rows : rows.slice(0, BAN_PREVIEW_LIMIT);
  useEffect(() => {
    if (!empty) setAdding(false);
  }, [empty]);
  return (
    <section className={`panel config-panel user-dcard sam-list-card${empty && !adding ? ' empty' : ''}`}>
      <header>
        <PanelTitle of="security">来源封禁</PanelTitle>
        <span className="rt">
          {empty ? (
            '未封禁'
          ) : (
            <>
              <b>{rows.length}</b> 个地址
            </>
          )}
        </span>
        {!empty && <RolloutStatus nodes={api.world.nodes} nameOf={api.nameOf} openOnLoad={open === 'status'} />}
        {api.editable && empty && !adding && (
          <button type="button" className="sam-head-btn" onClick={() => setAdding(true)}>
            <Icon of="plus" size={11} />
            封禁 IP
          </button>
        )}
      </header>
      {(!empty || adding) && (
        <div className="user-dcard-body panel-full-bleed-body sam-list">
          {!empty && (
            <ul className="sam-list-rows" aria-label="已封禁的来源">
              {visible.map(row => {
                const network = sourceNetwork(row.country, row.operator);
                return (
                  <li key={row.ip} className="sam-list-row">
                    <span className="sam-list-ip">
                      <SourceAddress ip={row.ip} />
                      <AddressCopy ip={row.ip} />
                    </span>
                    <span className="sam-list-geo" title={LOCATION_TITLE}>
                      {network.code && <RegionFlag code={network.code} />}
                      <span>{network.label}</span>
                    </span>
                    <span className={`sam-list-seen${row.online ? ' gold' : row.source ? '' : ' dim'}`}>
                      {seenText(row, today, api.world.presence !== 'unavailable')}
                    </span>
                    <span className="sam-row-end">
                      {api.editable && <UnbanButton ip={row.ip} pending={pending} onUnban={unban} />}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
          {rows.length > BAN_PREVIEW_LIMIT && (
            <button
              type="button"
              className="sam-list-more"
              aria-expanded={expanded}
              onClick={() => setExpanded(value => !value)}
            >
              {expanded ? '收起' : `显示其余 ${rows.length - BAN_PREVIEW_LIMIT} 个`}
            </button>
          )}
          {!!error && <ErrorBox error={error} />}
          {api.editable && (
            <AddBan api={api} variant="foot" autoFocus={adding} onDone={empty ? () => setAdding(false) : undefined} />
          )}
        </div>
      )}
    </section>
  );
}

/* ══ 方案 C：卡头入口 + 弹出面板 ══ */

export function BanEntry({ api, open }: { api: BanApi; open: OpenTarget }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [shown, setShown] = useState(open === 'list' || open === 'add');
  const count = api.world.bans.length;
  const roll = rolloutOf(api.world.nodes);
  if (!api.editable && count === 0) return null;
  return (
    <>
      <button
        ref={ref}
        type="button"
        className="sam-entry"
        aria-expanded={shown}
        aria-haspopup="dialog"
        onClick={() => setShown(true)}
      >
        <BanGlyph size={12} />
        封禁
        {count > 0 && <b>{count}</b>}
        {count > 0 && (roll.tone === 'gold' || roll.tone === 'err') && (
          <span className={`sam-dot ${roll.tone}`} aria-label={roll.text} />
        )}
      </button>
      {shown && <BanPopover api={api} anchor={ref} addOnLoad={open === 'add'} onClose={() => setShown(false)} />}
    </>
  );
}

function BanPopover({
  api,
  anchor,
  addOnLoad,
  onClose,
}: {
  api: BanApi;
  anchor: RefObject<HTMLElement | null>;
  addOnLoad: boolean;
  onClose: () => void;
}) {
  const now = useNow();
  const today = new Date(now).setHours(0, 0, 0, 0);
  const { pending, error, unban } = useUnban(api);
  const rows = banRowsOf(api.world);
  const roll = rolloutOf(api.world.nodes);
  const align = useEndAlign(anchor);
  return (
    <DialogLayer mode="popover" anchor={anchor} label="来源封禁" onClose={onClose}>
      <div className={`dialog-surface sam-pop${align.className}`} style={align.style}>
        <header>
          <h3>来源封禁</h3>
          {rows.length > 0 && (
            <span className={`sam-state static ${roll.tone}`} title={roll.title}>
              {roll.tone !== 'idle' && <span className="sam-dot" aria-hidden="true" />}
              {roll.text}
            </span>
          )}
        </header>
        {api.editable && <AddBan api={api} variant="pop" autoFocus={addOnLoad || rows.length === 0} />}
        {rows.length > 0 && (roll.tone === 'gold' || roll.tone === 'err') && (
          <NodeList nodes={api.world.nodes} nameOf={api.nameOf} only />
        )}
        {rows.length > 0 ? (
          <ul className="sam-pop-rows" aria-label="已封禁的来源">
            {rows.map(row => {
              const network = sourceNetwork(row.country, row.operator);
              return (
                <li key={row.ip}>
                  <span className="sam-pop-ip">
                    <SourceAddress ip={row.ip} />
                  </span>
                  <span className="sam-pop-geo">
                    {network.code && <RegionFlag code={network.code} />}
                    <span>{network.label}</span>
                    <span className={`sam-pop-seen${row.online ? ' gold' : ''}`}>
                      {seenText(row, today, api.world.presence !== 'unavailable')}
                    </span>
                  </span>
                  {api.editable && <UnbanButton ip={row.ip} pending={pending} onUnban={unban} />}
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="sam-pop-empty">未封禁任何来源</p>
        )}
        {!!error && <ErrorBox error={error} />}
      </div>
    </DialogLayer>
  );
}

/* ══ 方案 D：整份名单批量编辑 ══ */

function parseBatch(text: string) {
  const tokens = text.split(/[\s,;，；]+/).filter(Boolean);
  const valid: string[] = [];
  const invalid: string[] = [];
  for (const token of tokens) {
    const ip = normalizeIp(token);
    if (!ip) invalid.push(token);
    else if (!valid.includes(ip)) valid.push(ip);
  }
  return { valid, invalid: [...new Set(invalid)] };
}

export function BanBatchCard({ api, open }: { api: BanApi; open: OpenTarget }) {
  const now = useNow();
  const today = new Date(now).setHours(0, 0, 0, 0);
  const [editing, setEditing] = useState((open === 'edit' || open === 'paste') && api.editable);
  const rows = banRowsOf(api.world);
  const empty = rows.length === 0;
  return (
    <section
      className={`panel config-panel user-dcard sam-list-card sam-batch-card${empty && !editing ? ' empty' : ''}`}
    >
      <header>
        <PanelTitle of="security">来源封禁</PanelTitle>
        <span className="rt">
          {empty ? (
            '未封禁'
          ) : (
            <>
              <b>{rows.length}</b> 个地址
            </>
          )}
        </span>
        {!empty && <RolloutStatus nodes={api.world.nodes} nameOf={api.nameOf} openOnLoad={open === 'status'} />}
        {api.editable && !editing && (
          <button type="button" className="sam-head-btn" onClick={() => setEditing(true)}>
            <Icon of={empty ? 'plus' : 'pencil'} size={11} />
            {empty ? '封禁 IP' : '编辑'}
          </button>
        )}
      </header>
      {editing ? (
        <BatchEditor api={api} pasted={open === 'paste'} onDone={() => setEditing(false)} />
      ) : (
        !empty && (
          <div className="user-dcard-body sam-chips-body">
            <ul className="sam-chips" aria-label="已封禁的来源">
              {rows.map(row => {
                const network = sourceNetwork(row.country, row.operator);
                return (
                  <li
                    key={row.ip}
                    className={`sam-chip${row.online ? ' live' : ''}`}
                    title={`${network.label} · ${seenText(row, today, api.world.presence !== 'unavailable')}`}
                  >
                    {network.code && <RegionFlag code={network.code} />}
                    <code>{row.ip}</code>
                  </li>
                );
              })}
            </ul>
          </div>
        )
      )}
    </section>
  );
}

/** 审阅用：粘贴了一段外部名单之后的样子（去掉一条、加三条，其中一条在线、一条写错）。 */
const PASTED = ['192.0.2.7', '198.51.100.77', '203.0.113.300', '2001:DB8:91::0a'];

function BatchEditor({ api, pasted, onDone }: { api: BanApi; pasted: boolean; onDone: () => void }) {
  const current = banRowsOf(api.world).map(row => row.ip);
  const [text, setText] = useState(() => {
    const lines = pasted ? [...current.slice(0, -1), ...PASTED] : current;
    return lines.length ? `${lines.join('\n')}\n` : '';
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const element = area.current;
    if (!element) return;
    element.focus();
    element.setSelectionRange(element.value.length, element.value.length);
  }, []);
  const { valid, invalid } = parseBatch(text);
  const added = valid.filter(ip => !api.world.bans.includes(ip));
  const removed = api.world.bans.filter(ip => !valid.includes(ip));
  const onlineIps = new Set(api.world.online.map(source => source.ip));
  const over = valid.length > MAX_BANS;
  const changed = added.length + removed.length > 0;
  const lines = Math.min(14, Math.max(4, text.split('\n').length));
  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.save(valid);
      onDone();
    } catch (failure) {
      setError(failure);
      setBusy(false);
    }
  };
  return (
    <div className="user-dcard-body sam-batch">
      <textarea
        ref={area}
        className="f mono"
        rows={lines}
        value={text}
        spellCheck={false}
        disabled={busy}
        aria-label="封禁名单，每行一个地址"
        placeholder={'每行一个 IPv4 / IPv6 地址\n可直接粘贴，空格或逗号分隔亦可'}
        onChange={event => setText(event.target.value)}
        onKeyDown={event => {
          if (event.key === 'Escape') onDone();
        }}
      />
      {(changed || invalid.length > 0) && (
        <ul className="sam-diff" aria-label="改动">
          {added.map(ip => (
            <li key={`+${ip}`} className="sam-chip add">
              <span aria-hidden="true">+</span>
              <code>{ip}</code>
              {onlineIps.has(ip) && <em>在线</em>}
            </li>
          ))}
          {removed.map(ip => (
            <li key={`-${ip}`} className="sam-chip del">
              <span aria-hidden="true">−</span>
              <code>{ip}</code>
            </li>
          ))}
          {invalid.map(token => (
            <li key={`!${token}`} className="sam-chip bad" title="不是有效的 IPv4 / IPv6 地址">
              <span aria-hidden="true">!</span>
              <code>{token}</code>
            </li>
          ))}
        </ul>
      )}
      {!!error && <ErrorBox error={error} />}
      <footer>
        <span className={`sam-batch-count${over ? ' err' : ''}`}>
          {valid.length} / {MAX_BANS}
        </span>
        {invalid.length > 0 && <span className="sam-batch-count err">{invalid.length} 个无效</span>}
        <span className="sp" />
        <button type="button" className="btn" disabled={busy} onClick={onDone}>
          取消
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={!changed || invalid.length > 0 || over || busy}
          onClick={save}
        >
          {busy ? '保存中…' : '保存'}
        </button>
      </footer>
    </div>
  );
}
