import { useSourceAccess } from './source-access';
import { SourceAccessRollout, SourceBanAdd, SourceBanButton } from './user-source-access';
import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react';
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createUser,
  fetchMyUser,
  fetchQuotas,
  fetchSnapshot,
  fetchTenants,
  fetchUserCycleUsage,
  fetchUserOnlineSourceHistory,
  fetchUserPresence,
  fetchUsers,
  issueUserDirectLogin,
  issueUserLogin,
  revokeUserDirectLogin,
  rotateMyUuid,
  rotateUserUuid,
  setUserPassword,
  setUserStatus,
  updateUserProfile,
  upsertGrant,
  type SnapshotApp,
  type SourceAccessNode,
  type GrantWrite,
  type IssuedUserDirectLogin,
  type NetworkOperator,
  type UsageMonthlyViewRow,
  type UserListItem,
  type UserTrafficAccountingMode,
  type UserTrafficPolicyView,
  type UserOnlineSourceAccess,
  type UserOnlineSource,
  type UserPresence,
} from '../api';
import { can, isVisitor, useSession } from '../session';
import { Ago, Confirm, EmptyState, ErrorBox, Loading, SegmentedControl } from '../ui/bits';
import { Icon, ListIcon, PanelTitle } from '../ui/icons';
import { bytes, shareOf } from '../ui/format';
import { useNodeNames } from '../ui/node-name';
import { RegionFlag } from '../ui/region-flag';
import { GeneratedUserAvatar, type AvatarLamp } from '../ui/user-avatar';
import { FieldLoading, PanelLoading } from '../ui/loading';
import { useNow } from '../ui/clock';
import { type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { isValidSlug } from './ports';
import { SubscriptionViewer, type SubscriptionKind } from './subscription';
import { GRANT_MUTATION_KEY, GRANT_REFRESH_MS, GRANT_REFRESH_WINDOW_MS, useGrantEdits } from '../grant-edits';
import { navigate } from '../forge/route';
import { CopyButton, copyStateIcon } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { PASSWORD_MIN_LENGTH, passwordConfirmation } from '../ui/password-policy';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import { useNarrow } from '../ui/viewport';
import { directLoginUrl } from '../ui/login';
import { canViewUserRealtimeTraffic } from '../session';
import { UserRealtimeTrafficCard } from '../user-traffic-realtime';
import { UserSiteUsageCard } from '../user-site-usage';
import { UserMachineUsageCard } from '../user-machine-usage';
import { UserTrafficAccountingSetting } from '../user-traffic-accounting';
import { UserForwardingCard } from './user-forwarding';
import { UserServiceTerms } from './user-service-policy';
import {
  UserAppTrafficPolicyEditor,
  useUserTrafficPolicies,
  prefetchUserTrafficPolicies,
  UserTrafficQuotaMeter,
} from './user-traffic-policy';

// 用户列表：一行一个用户，点击后就地展开。
// 此处原为授权矩阵，行是用户、列是接入面。列数随数据增长：每个接入点一列，每个线路再加
// 一列月流量，三个线路九个接入点即 13 列，打开产物栏后必然横向滚动。改为展开式后，授权项
// 以网格排列，接入点增多只会换行，不增加版面宽度。
//
// 一个授权项对应一条 grant。卡片选择先留在本地，显式保存后才触发 grant-sync 批次；
// grant-sync 不重启进程，也不断开连接。
//
// 新用户直接作为名册中的一条可编辑行出现；添加成功即写库并刷新名册，无需再提交草稿。

type Drill = { p: 'list' } | { p: 'user'; id: string };

// 行左侧的状态条。与机器面共用同一组件和同一套判读规则（styles.css 的 .lst-bar），
// 但表示的状态不同：机器面表示 agent 是否仍在拉取配置，此处表示该用户当前能否连接。
//
// 红色有两种成因，结果都是无法连接：用户被停用，或某个线路流量用尽。状态条不区分二者，
// 逐行扫视时需要的是定位到异常用户；具体成因写在第二行和展开后的额度栏里。
//
// 空心表示已开户但未授权任何接入点。同样无法连接，但成因是尚未授权而非被中断，
// 与机器面「从未上报 ≠ 掉线」属同一类区分，因此沿用同一记号。
type UserTone = 'ok' | 'bad' | 'idle';

export interface UserFacts {
  status: string;
  serviceState?: UserListItem['service_state'];
  grants: number;
  // 额度已用尽的线路。空数组不表示未设置额度：未设额度的线路和已设未超的线路
  // 都不会进入此数组。
  exhausted: string[];
  // 被配额执行器撤销的接入点数量。撤销后 `grants` 归零，仅依据 `grants` 会判定为
  // 未授权并显示空心状态，而该用户实际是被中断的。
  suspended: number;
  throttledApps?: string[];
}

export type CycleUsageState = 'pending' | 'failed' | 'ready';

export const cycleUsageText = (state: CycleUsageState, rowCount: number, total: number) => {
  if (state === 'pending') return '读取中…';
  if (state === 'failed') return '暂不可用';
  return rowCount > 0 ? bytes(total) : '—';
};

const trafficMultiplier = (mode: UserTrafficAccountingMode | undefined) => (mode === 'double' ? 2 : 1);

const userTone = (f: UserFacts): UserTone =>
  f.status === 'disabled' ||
  (f.serviceState !== undefined && f.serviceState !== 'active') ||
  f.exhausted.length > 0 ||
  f.suspended > 0
    ? 'bad'
    : f.grants === 0
      ? 'idle'
      : 'ok';

const userLampTitle = (f: UserFacts) => {
  const causes: string[] = [];
  if (f.status === 'disabled') causes.push('已停用：旧凭据立即失效');
  if (f.serviceState === 'pending') causes.push('服务尚未生效');
  if (f.serviceState === 'expired') causes.push('服务已到期');
  if (f.exhausted.length > 0) causes.push(`流量已用尽：${f.exhausted.join('、')}`);
  if (f.suspended > 0) causes.push(`${f.suspended} 个接入点已被系统停用`);
  if (causes.length > 0) return `无法连接 · ${causes.join('；')}`;
  if (f.throttledApps?.length) return `额度已用满 · ${f.throttledApps.join('、')} 已限速`;
  return f.grants === 0 ? '未授权任何接入点，无法连接' : `可用 · ${f.grants} 个接入点`;
};

export const userMatchesSearch = (user: UserListItem, rawQuery: string) => {
  const query = rawQuery.trim().toLocaleLowerCase();
  if (!query) return true;
  return [user.id, user.uuid]
    .filter((value): value is string => !!value)
    .some(value => value.toLocaleLowerCase().includes(query));
};

// 名册只标出测试账号；正式账号是常态，不再逐行标注。
export const accountTypeChip = (accountType: UserListItem['account_type']) => (accountType === 'test' ? '测试' : null);

export type RosterFilter = 'all' | 'attention' | 'test' | 'disabled';

const ROSTER_FILTERS: { value: RosterFilter; label: string }[] = [
  { value: 'all', label: '全部' },
  { value: 'attention', label: '需处理' },
  { value: 'test', label: '测试' },
  { value: 'disabled', label: '已停用' },
];

// 需处理：流量用尽或接入点被系统停用。已停用由操作者设置，单独成组；未授权不算异常。
const needsAttention = (user: UserListItem, facts: UserFacts) =>
  user.status !== 'disabled' &&
  (facts.serviceState === 'pending' ||
    facts.serviceState === 'expired' ||
    facts.exhausted.length > 0 ||
    facts.suspended > 0);

export const rosterFilterMatches = (filter: RosterFilter, user: UserListItem, facts: UserFacts) =>
  filter === 'all' ||
  (filter === 'attention' && needsAttention(user, facts)) ||
  (filter === 'test' && user.account_type === 'test') ||
  (filter === 'disabled' && user.status === 'disabled');

export const userPresenceText = (presence: UserPresence | undefined) => {
  if (!presence || presence.state === 'unavailable') return '在线来源 —';
  const count = presence.sources.length;
  if (presence.state === 'partial') return count > 0 ? `至少 ${count} 个在线来源` : '在线来源 —';
  return count > 0 ? `在线来源 ${count}` : '暂无在线连接';
};

const SOURCE_REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const SOURCE_OPERATOR_NAMES = new Map<NetworkOperator, string>([
  ['chinanet', '电信'],
  ['cmcc', '移动'],
  ['unicom', '联通'],
  ['cernet', '教育网'],
  ['cstnet', '科技网'],
]);

const SOURCE_PROTOCOL_NAMES = new Map([
  ['vless', 'VLESS'],
  ['anytls', 'AnyTLS'],
  ['hysteria2', 'Hysteria2'],
  ['mtproto', 'MTProxy'],
  ['unknown', '协议未知'],
]);

function protocolText(accesses: UserOnlineSourceAccess[]): string {
  const protocols = new Set(
    accesses.flatMap(access =>
      access.protocols?.length
        ? access.protocols.map(protocol => (SOURCE_PROTOCOL_NAMES.has(protocol) ? protocol : 'unknown'))
        : ['unknown'],
    ),
  );
  if (!protocols.size) protocols.add('unknown');
  return [...SOURCE_PROTOCOL_NAMES]
    .filter(([protocol]) => protocols.has(protocol))
    .map(([, name]) => name)
    .join(' · ');
}

function protocolSummary(accesses: UserOnlineSourceAccess[]): string {
  const protocols = new Set(
    accesses.flatMap(access =>
      access.protocols?.length
        ? access.protocols.map(protocol => (SOURCE_PROTOCOL_NAMES.has(protocol) ? protocol : 'unknown'))
        : ['unknown'],
    ),
  );
  if (!protocols.size) protocols.add('unknown');
  const labels = [...SOURCE_PROTOCOL_NAMES].filter(([protocol]) => protocols.has(protocol)).map(([, name]) => name);
  return labels.length <= 2 ? labels.join(' / ') : `${labels.length} 种协议`;
}

type PresenceChain = {
  key: string;
  appId: string | null;
  chainId: string | null;
  name: string;
  accesses: UserOnlineSourceAccess[];
};

function chainsOf(source: UserOnlineSource, apps: SnapshotApp[]): PresenceChain[] {
  const ingressIndex = new Map(
    apps.flatMap(app =>
      app.ingresses.map(ingress => {
        const chain = app.chains.find(item => item.id === ingress.chain);
        return [ingress.id, { app, ingress, chain }] as const;
      }),
    ),
  );
  const accesses = source.accesses?.length
    ? source.accesses
    : source.ingress_ids.map(ingressId => ({
        node_id: '',
        ingress_id: ingressId,
        protocols: null,
      }));
  const grouped = new Map<string, PresenceChain>();
  for (const access of accesses) {
    const resolved = ingressIndex.get(access.ingress_id);
    const key = resolved?.chain ? `${resolved.app.id}/${resolved.chain.id}` : `unknown/${access.ingress_id}`;
    const current = grouped.get(key);
    if (current) {
      current.accesses.push(access);
      continue;
    }
    grouped.set(key, {
      key,
      appId: resolved?.chain ? resolved.app.id : null,
      chainId: resolved?.chain?.id ?? null,
      name: resolved?.chain?.name ?? `接入链未知 · ${access.ingress_id}`,
      accesses: [access],
    });
  }
  return [...grouped.values()].sort((a, b) => a.name.localeCompare(b.name, 'zh-CN') || a.key.localeCompare(b.key));
}

const accessSummaryOf = (chains: PresenceChain[]) => {
  const accesses = chains.flatMap(chain => chain.accesses);
  if (chains.length === 0) return '接入链未知';
  return chains.length === 1
    ? `${chains[0].name} · ${protocolSummary(accesses)}`
    : `${chains.length} 条接入链 · ${protocolSummary(accesses)}`;
};

function PresenceChainLink({ chain }: { chain: PresenceChain }) {
  if (!chain.appId || !chain.chainId) return <span className="user-presence-chain-unknown">{chain.name}</span>;
  return (
    <button
      type="button"
      className="user-presence-chain-link"
      title={`${chain.appId} / ${chain.chainId}`}
      onClick={() => navigate('chains', { p: 'chain', app: chain.appId!, chain: chain.chainId! })}
    >
      {chain.name}
    </button>
  );
}

const SOURCE_LOCATION_TITLE = '本地 GeoIP · 国家／地区与网络归属参考，非精确位置或宽带品牌';

// 来源的网络归属：本地 GeoIP 国家／地区 + BGP 运营商。国内运营商合写为网络名（「中国电信」
// 「中国教育网」）；境外地区有运营商归属时写「香港 · 移动」。国家库未命中时保留运营商，
// 不从运营商反推国家。
function sourceNetwork(
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

// PostgreSQL 的 timestamptz::text 形如「2026-10-05 06:21:22.409123+00」：日期与时刻以空格分隔，
// 时区偏移只写小时，秒的小数为 6 位，均不是 ECMAScript 规定的日期格式。先改写为该格式再解析，
// 不依赖各浏览器 Date.parse 对其他写法的宽松解析。
function observedAt(at: string): number {
  const iso = at
    .replace(/^(\d{4}-\d{2}-\d{2}) /, '$1T')
    .replace(/(\.\d{3})\d+/, '$1')
    .replace(/(T[\d:.]+[+-]\d{2})$/, '$1:00');
  return Date.parse(/(?:Z|[+-]\d{2}:\d{2})$/.test(iso) ? iso : `${iso}Z`);
}

const two = (value: number) => String(value).padStart(2, '0');
const clockOf = (t: number) => {
  const d = new Date(t);
  return `${two(d.getHours())}:${two(d.getMinutes())}`;
};
const monthDayOf = (t: number) => {
  const d = new Date(t);
  return `${two(d.getMonth() + 1)}-${two(d.getDate())}`;
};
// 本地日历的天数差：0 为今天，1 为昨天，与发布流水按天分组的口径一致。today 是本地当天零点；
// 跨夏令时切换的那一天不是整 24 小时，取最接近的整数。
const calendarDaysAgo = (t: number, today: number) =>
  Math.round((today - new Date(t).setHours(0, 0, 0, 0)) / 86_400_000);

const localTime = (at: string) => {
  const t = observedAt(at);
  return Number.isNaN(t) ? at : new Date(t).toLocaleString();
};

const observedTitle = (source: UserOnlineSource, online: boolean) =>
  [
    ...(source.online_since_at ? [`本次上线 ${localTime(source.online_since_at)}`] : []),
    `首次出现 ${localTime(source.first_observed_at)}`,
    `${online ? '最近确认在线' : '确认离线'} ${localTime(source.last_observed_at)}`,
    `最近新建连接 ${localTime(source.xray_last_seen_at)}`,
  ].join('\n');

export function sessionDurationText(from: string | null | undefined, until: number): string {
  if (!from) return '—';
  const started = observedAt(from);
  if (Number.isNaN(started) || Number.isNaN(until) || until < started) return '—';
  const elapsed = until - started;
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return `${Math.floor(elapsed / 1_000)}秒`;
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  const rest = minutes % 60;
  if (days > 0) return `${days}天${hours > 0 ? ` ${hours}小时` : ''}`;
  if (hours > 0) return `${hours}小时${rest > 0 ? ` ${rest}分` : ''}`;
  return `${rest}分`;
}

const accessMetric = (accesses: UserOnlineSourceAccess[], key: 'session_uplink_bytes' | 'session_downlink_bytes') => {
  if (accesses.length === 0 || accesses.some(access => access[key] == null)) return null;
  return accesses.reduce((sum, access) => sum + (access[key] ?? 0), 0);
};

const accessOnlineSince = (accesses: UserOnlineSourceAccess[]) => {
  const values = accesses.map(access => access.online_since_at).filter((value): value is string => !!value);
  if (values.length !== accesses.length) return null;
  return values.reduce((earliest, value) => (observedAt(value) < observedAt(earliest) ? value : earliest));
};

const accessLastObserved = (accesses: UserOnlineSourceAccess[], fallback: string) =>
  accesses
    .map(access => access.last_observed_at)
    .filter((value): value is string => !!value)
    .reduce((latest, value) => (observedAt(value) > observedAt(latest) ? value : latest), fallback);

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
        <span
          className="user-presence-duration"
          aria-label={`在线时长 ${sessionDurationText(onlineSince, until)}`}
          title={`本次上线 ${localTime(onlineSince)}`}
        >
          <Icon of="clock" size={11} />
          <b>{sessionDurationText(onlineSince, until)}</b>
        </span>
      )}
      {hasTraffic && (
        <span
          className="user-presence-traffic"
          aria-label={`本次会话流量 ${bytes(uplink + downlink)}`}
          title={`本次会话流量：下行 ${bytes(downlink)} · 上行 ${bytes(uplink)}`}
        >
          <Icon of="usage" size={11} />
          <b>{bytes(uplink + downlink)}</b>
        </span>
      )}
    </span>
  );
}

// 时间线分组：在线来源在最前，离线来源按最后出现分到今天、昨天、7 天内与保留期内。
type PresenceBucket = 'online' | 'today' | 'yesterday' | 'week' | 'retention';
const PRESENCE_BUCKETS: PresenceBucket[] = ['online', 'today', 'yesterday', 'week', 'retention'];
const OLDER_PRESENCE_BUCKETS = new Set<PresenceBucket>(['week', 'retention']);
const PRESENCE_HISTORY_PREVIEW_LIMIT = 5;

function offlineBucket(lastSeen: number, today: number): PresenceBucket {
  if (Number.isNaN(lastSeen)) return 'retention';
  const days = calendarDaysAgo(lastSeen, today);
  if (days <= 0) return 'today';
  if (days === 1) return 'yesterday';
  return days < 7 ? 'week' : 'retention';
}

// IPv6 的前 64 位用正文墨色，接口标识降一档：同一 /64 下轮换的隐私地址可以直接对上。
// 接口标识的冒号后允许断行，窄卡里不会把一组十六进制拆到两行。
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

// 时间线的一行：最后出现时刻 │ 来源 IP │ 网络 │ 接入链 │ 本次流量。
// 在线时长只在展开后的接入链明细中显示，排在该链流量之前。
// 在线来源不写时刻，所在分组「在线」已经表达它此刻仍在线。卡片每秒随时钟重渲染，
// 行只在数据或日期变化时重渲染。
const PresenceRow = memo(function PresenceRow({
  source,
  online,
  older,
  country,
  operator,
  apps,
  now,
  onBan,
}: {
  source: UserOnlineSource;
  online: boolean;
  older: boolean;
  country: string | undefined;
  operator: NetworkOperator | undefined;
  apps: SnapshotApp[];
  now: number;
  /** 有修改名单的权限且名单已读到时给出：复制钮旁多一枚禁行钮。 */
  onBan?: (ip: string) => Promise<unknown>;
}) {
  const [accessExpanded, setAccessExpanded] = useState(false);
  const accessId = useId();
  const lastSeen = observedAt(source.last_observed_at);
  const network = sourceNetwork(country, operator);
  const title = observedTitle(source, online);
  const time = online ? '' : Number.isNaN(lastSeen) ? '—' : older ? monthDayOf(lastSeen) : clockOf(lastSeen);
  const past = online ? '' : '历史';
  const chains = chainsOf(source, apps);
  const accessCount = chains.length;
  const accessSummary = accessSummaryOf(chains);
  const until = online ? now : lastSeen;
  return (
    <li className={`user-presence-row${online ? '' : ' off'}${accessExpanded ? ' expanded' : ''}`}>
      <span className="user-presence-time" title={title}>
        {time}
      </span>
      <span className="user-presence-mark" aria-hidden="true" />
      <span className="user-presence-ip">
        <SourceAddress ip={source.ip} />
        <CopyButton
          className="user-fcopy"
          text={source.ip}
          label={`复制${past}来源 IP ${source.ip}`}
          successLabel={`${past}来源 IP 已复制`}
          failureLabel={`${past}来源 IP 复制失败`}
          iconOnly
        />
        {onBan && <SourceBanButton ip={source.ip} online={online} onBan={onBan} />}
      </span>
      <span className="user-presence-sub">
        <span className="user-presence-location" title={SOURCE_LOCATION_TITLE}>
          {network.code && <RegionFlag code={network.code} />}
          <span>{network.label}</span>
        </span>
        <button
          type="button"
          className="user-presence-access-toggle"
          aria-expanded={accessExpanded}
          aria-controls={accessId}
          aria-label={`${accessExpanded ? '收起' : '展开'} ${accessCount} 条接入链详情`}
          disabled={accessCount === 0}
          onClick={() => setAccessExpanded(expanded => !expanded)}
        >
          <span>{accessSummary}</span>
          {accessCount > 0 && (
            <span className="user-presence-access-caret" aria-hidden="true">
              ›
            </span>
          )}
        </button>
      </span>
      <SessionFacts uplink={source.session_uplink_bytes} downlink={source.session_downlink_bytes} until={until} />
      {accessExpanded && accessCount > 0 && (
        <div id={accessId} className="user-presence-access">
          {chains.map(chain => {
            const uplink = accessMetric(chain.accesses, 'session_uplink_bytes');
            const downlink = accessMetric(chain.accesses, 'session_downlink_bytes');
            const onlineSince = accessOnlineSince(chain.accesses);
            const lastObserved = accessLastObserved(chain.accesses, source.last_observed_at);
            return (
              <span key={chain.key} className="user-presence-access-item">
                <span className="user-presence-access-name">
                  <PresenceChainLink chain={chain} />
                  <span className="user-presence-protocols">{protocolText(chain.accesses)}</span>
                </span>
                <SessionFacts
                  uplink={uplink}
                  downlink={downlink}
                  onlineSince={onlineSince}
                  until={online ? now : observedAt(lastObserved)}
                />
              </span>
            );
          })}
        </div>
      )}
    </li>
  );
});

const firstSeenOrder = (source: UserOnlineSource) => {
  const t = observedAt(source.first_observed_at);
  return Number.isNaN(t) ? 0 : t;
};

const lastSeenOrder = (source: UserOnlineSource) => {
  const t = observedAt(source.last_observed_at);
  return Number.isNaN(t) ? 0 : t;
};

// 来源封禁在在线设备卡底自成「已封禁」一组，与时间线同一套列：时刻列写最后出现，节点换成禁行标记，
// 网络列之后写接入链或出现情况，行尾解除。
type BannedSourceState = 'live' | 'seen' | 'unseen' | 'unknown';

interface BannedSource {
  key: string;
  ip: string;
  /** live：仍有经过它的机器未确认当前名单，会话可能还在；seen：有出现记录；unseen：30 天内确实没出现过。 */
  state: BannedSourceState;
  source: UserOnlineSource | null;
  country: string | undefined;
  operator: NetworkOperator | undefined;
}

// 同一天写时刻，更早写月-日：这一组不按天分组，单写时刻读不出是哪天。
const bannedSeenStamp = (t: number, today: number) =>
  Number.isNaN(t) ? '' : calendarDaysAgo(t, today) <= 0 ? clockOf(t) : monthDayOf(t);

function BannedSources({
  rows,
  nodes,
  apps,
  today,
  onUnban,
}: {
  rows: BannedSource[];
  nodes: SourceAccessNode[];
  apps: SnapshotApp[];
  today: number;
  /** 有修改名单的权限时给出。 */
  onUnban?: (ip: string) => Promise<unknown>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const visible = expanded ? rows : rows.slice(0, PRESENCE_HISTORY_PREVIEW_LIMIT);
  const unban = async (ip: string) => {
    if (!onUnban) return;
    setRemoving(ip);
    setError(null);
    try {
      await onUnban(ip);
    } catch (failure) {
      setError(failure);
    } finally {
      setRemoving(null);
    }
  };
  return (
    <div className="source-ban-group">
      <ol className="user-presence-timeline" aria-label="已封禁的来源">
        <li className="user-presence-day source-ban-day">
          <span>已封禁</span>
          <SourceAccessRollout nodes={nodes} />
        </li>
        {visible.map(row => {
          const network = sourceNetwork(row.country, row.operator);
          const chains = row.source ? chainsOf(row.source, apps) : [];
          const seen =
            row.state === 'live'
              ? `仍在线 · ${accessSummaryOf(chains)}`
              : row.state === 'seen'
                ? accessSummaryOf(chains)
                : row.state === 'unseen'
                  ? '30 天内未出现'
                  : '';
          return (
            <li key={row.key} className={`user-presence-row source-ban-row${row.state === 'live' ? ' live' : ' off'}`}>
              <span
                className="user-presence-time"
                title={row.source ? observedTitle(row.source, row.state === 'live') : undefined}
              >
                {row.state === 'seen' && row.source
                  ? bannedSeenStamp(observedAt(row.source.last_observed_at), today)
                  : ''}
              </span>
              <span className="user-presence-mark banned" aria-hidden="true" />
              <span className="user-presence-ip">
                <SourceAddress ip={row.ip} />
                <CopyButton
                  className="user-fcopy"
                  text={row.ip}
                  label={`复制已封禁来源 IP ${row.ip}`}
                  successLabel="已封禁来源 IP 已复制"
                  failureLabel="已封禁来源 IP 复制失败"
                  iconOnly
                />
              </span>
              <span className="user-presence-sub">
                <span className="user-presence-location" title={SOURCE_LOCATION_TITLE}>
                  {network.code && <RegionFlag code={network.code} />}
                  <span>{network.label}</span>
                </span>
                <span className={`source-ban-seen${row.state === 'live' ? ' gold' : ''}`}>{seen}</span>
              </span>
              {onUnban && (
                <button
                  type="button"
                  className="source-ban-unban"
                  disabled={removing !== null}
                  aria-label={`解除封禁 ${row.ip}`}
                  onClick={() => void unban(row.ip)}
                >
                  {removing === row.ip ? '解除中…' : '解除'}
                </button>
              )}
            </li>
          );
        })}
      </ol>
      {rows.length > PRESENCE_HISTORY_PREVIEW_LIMIT && (
        <button
          type="button"
          className="user-presence-more"
          aria-expanded={expanded}
          onClick={() => setExpanded(value => !value)}
        >
          {expanded ? '收起已封禁来源' : `显示其余 ${rows.length - PRESENCE_HISTORY_PREVIEW_LIMIT} 个已封禁来源`}
        </button>
      )}
      {!!error && <ErrorBox error={error} />}
    </div>
  );
}

function UserPresenceCard({
  user,
  presence,
  countries,
  operators,
  apps,
  pending,
  error,
  canBan,
  masked,
}: {
  user: UserListItem;
  presence: UserPresence | undefined;
  countries: Record<string, string> | undefined;
  operators: Record<string, NetworkOperator> | undefined;
  apps: SnapshotApp[];
  pending: boolean;
  error: unknown;
  /** 可以修改来源封禁名单（编辑及以上）。 */
  canBan: boolean;
  /** 只读视角：服务端把地址脱敏成网段前缀，不同来源可能同形，不能按地址对照名单与在线来源。 */
  masked: boolean;
}) {
  const nameOf = useNodeNames();
  const [historyExpanded, setHistoryExpanded] = useState(false);
  const [addingBan, setAddingBan] = useState(false);
  const now = useNow();
  const today = new Date(now).setHours(0, 0, 0, 0);
  const sourceAccess = useSourceAccess({ tenantId: user.tenant_id, userId: user.id });
  const bans = sourceAccess.query.data;
  const changeBans = sourceAccess.save.mutateAsync;
  // 行是 memo 的：回调要稳定，否则每秒的时钟重渲染会带着所有行一起重渲染。
  const banSource = useCallback((ip: string) => changeBans(ips => [...ips, ip]), [changeBans]);
  const unbanSource = useCallback((ip: string) => changeBans(ips => ips.filter(value => value !== ip)), [changeBans]);
  const online = presence?.sources ?? [];
  // 在线与离线来源排在同一条时间线上，打开详情即读取离线来源。名册只轮询在线来源，历史只为
  // 当前打开的这一位用户读取（见 store 的 history 说明）。刚离线的地址要立即出现在历史里：
  // 在线地址集合进入查询键，集合变化即重新读取，读取期间沿用上一份结果，时间线不闪烁。
  const onlineKey = online
    .map(source => source.ip)
    .sort()
    .join(' ');
  const history = useQuery({
    queryKey: ['user-presence-history', user.tenant_id, user.id, onlineKey],
    queryFn: () => fetchUserOnlineSourceHistory(user.tenant_id, user.id),
    enabled: !pending,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  // 重新读取完成前，上一份历史里可能还有刚恢复在线的地址；同一地址只在「在线」出现一次。
  const onlineIps = new Set(online.map(source => source.ip));
  const offline = (history.data?.sources ?? [])
    .filter(source => !onlineIps.has(source.ip))
    .sort((a, b) => lastSeenOrder(b) - lastSeenOrder(a) || a.ip.localeCompare(b.ip));
  const retentionDays = history.data?.retention_days ?? 30;
  const historyCountries = new Map(history.data?.source_countries?.map(({ ip, country }) => [ip, country]));
  const historyOperators = new Map(history.data?.source_operators?.map(({ ip, operator }) => [ip, operator]));

  // 已封禁的地址移出时间线，在卡底自成一组；读数与说明仍按全部来源计算。
  const bannedIps = new Set(masked ? [] : (bans?.ips ?? []));
  const timelineOnline = online.filter(source => !bannedIps.has(source.ip));
  const timelineOffline = offline.filter(source => !bannedIps.has(source.ip));
  const visibleOffline = historyExpanded ? timelineOffline : timelineOffline.slice(0, PRESENCE_HISTORY_PREVIEW_LIMIT);

  const groups = new Map<PresenceBucket, UserOnlineSource[]>();
  // 在线来源按首次出现由近到远：新出现的地址排在最上面
  if (timelineOnline.length) {
    groups.set(
      'online',
      [...timelineOnline].sort((a, b) => firstSeenOrder(b) - firstSeenOrder(a) || a.ip.localeCompare(b.ip)),
    );
  }
  for (const source of visibleOffline) {
    const bucket = offlineBucket(observedAt(source.last_observed_at), today);
    groups.set(bucket, [...(groups.get(bucket) ?? []), source]);
  }
  const bucketLabel: Record<PresenceBucket, string> = {
    online: '在线',
    today: '今天',
    yesterday: '昨天',
    week: '7 天内',
    retention: `${retentionDays} 天内`,
  };
  const hiddenHistoryCount = Math.max(0, timelineOffline.length - PRESENCE_HISTORY_PREVIEW_LIMIT);
  const visibleBuckets = PRESENCE_BUCKETS.filter(bucket => groups.has(bucket));

  // 名单与在线来源按地址对照。名单自带全部地址的地区；脱敏后同形的地址对不上号，只在名单里唯一时才写地区。
  const sourceByIp = new Map([...offline, ...online].map(source => [source.ip, source]));
  const banCountries = new Map(bans?.source_countries?.map(({ ip, country }) => [ip, country]));
  const banOperators = new Map(bans?.source_operators?.map(({ ip, operator }) => [ip, operator]));
  const confirmedNodes = new Set(bans?.nodes.filter(node => node.phase === 'applied').map(node => node.node_id));
  // 「30 天内未出现」要有完整的在线统计和未截断的历史作依据，否则只能不写。
  const absenceKnown = !masked && presence?.state === 'complete' && !!history.data && !history.data.truncated;
  const banList = bans?.ips ?? [];
  const bannedRows: BannedSource[] = banList
    .map((ip, index): BannedSource => {
      const unique = banList.indexOf(ip) === banList.lastIndexOf(ip);
      const source = masked ? null : (sourceByIp.get(ip) ?? null);
      const nodesOf = (seen: UserOnlineSource) =>
        seen.accesses?.length ? seen.accesses.map(access => access.node_id) : seen.node_ids;
      // 经过它的机器都已确认当前名单时，会话已经关闭，在线快照只是还没刷新，不再写「仍在线」。
      const live =
        !!source &&
        onlineIps.has(ip) &&
        (nodesOf(source).length === 0 || nodesOf(source).some(node => !confirmedNodes.has(node)));
      return {
        key: masked ? `${index}:${ip}` : ip,
        ip,
        state: live ? 'live' : source ? 'seen' : absenceKnown ? 'unseen' : 'unknown',
        source,
        country: unique
          ? (banCountries.get(ip) ?? (masked ? undefined : (countries?.[ip] ?? historyCountries.get(ip))))
          : undefined,
        operator: unique
          ? (banOperators.get(ip) ?? (masked ? undefined : (operators?.[ip] ?? historyOperators.get(ip))))
          : undefined,
      };
    })
    // 仍在线的在最前，其余按最后出现由近到远；没有出现记录的保持名单顺序排在最后。
    .sort(
      (a, b) =>
        Number(b.state === 'live') - Number(a.state === 'live') ||
        (b.source ? lastSeenOrder(b.source) : 0) - (a.source ? lastSeenOrder(a.source) : 0),
    );
  const describeSource = (ip: string) => {
    const source = sourceByIp.get(ip);
    if (!source) return null;
    const live = onlineIps.has(ip);
    const network = sourceNetwork(
      countries?.[ip] ?? historyCountries.get(ip),
      operators?.[ip] ?? historyOperators.get(ip),
    );
    return {
      online: live,
      content: (
        <>
          {network.code && <RegionFlag code={network.code} />}
          {network.label} ·{' '}
          {live ? '在线，封禁后断开' : `最后出现 ${bannedSeenStamp(observedAt(source.last_observed_at), today)}`}
        </>
      ),
    };
  };

  const partial = presence?.state === 'partial';
  const banCount = banList.length;
  const reading = error ? (
    '在线状态暂不可用'
  ) : pending ? (
    <FieldLoading />
  ) : !presence || presence.state === 'unavailable' ? (
    '在线来源 —'
  ) : online.length > 0 ? (
    <>
      {partial ? '在线至少 ' : '在线 '}
      <b>{online.length}</b>
      {history.data && (
        <>
          {' '}
          · {retentionDays} 天 <b>{online.length + offline.length}</b> 个地址
        </>
      )}
    </>
  ) : partial ? (
    '在线来源 —'
  ) : offline.length > 0 ? (
    <>
      离线 · 最后出现 <Ago at={offline[0].last_observed_at} />
    </>
  ) : (
    '暂无在线连接'
  );
  // 状态行：快照不完整或不可用时为金色点；确认无在线来源时为空心点。错误由 ErrorBox 单独说明。
  const coverageNotice = presence && (
    <>
      在线来源统计不完整：{presence.expected_nodes} 台入口机器中，{presence.reporting_nodes} 台统计已更新
      {presence.missing_nodes?.length ? (
        <>
          ；缺少{' '}
          {presence.missing_nodes.map((node, index) => (
            <Fragment key={node.node_id}>
              {index > 0 && '、'}
              {nameOf(node.node_id)}（
              {node.last_reported_at ? (
                <>
                  上次更新 <Ago at={node.last_reported_at} />
                </>
              ) : (
                '尚未收到统计'
              )}
              ）
            </Fragment>
          ))}
        </>
      ) : null}
      。这些机器的在线连接暂未计入，在线数量可能少算；不代表机器离线。
    </>
  );
  const notice: { tone: 'warn' | 'idle'; text: ReactNode } | null =
    error || pending
      ? null
      : presence?.state === 'partial'
        ? {
            tone: 'warn',
            text: coverageNotice,
          }
        : !presence || presence.state === 'unavailable'
          ? {
              tone: 'warn',
              text: presence?.expected_nodes
                ? coverageNotice
                : '尚未收到在线来源统计，暂时无法判断是否在线；这不代表用户已离线。',
            }
          : online.length > 0
            ? null
            : offline.length > 0
              ? { tone: 'idle', text: '暂无在线连接' }
              : history.data
                ? { tone: 'idle', text: `最近 ${retentionDays} 天没有来源记录` }
                : null;

  return (
    <section className="panel config-panel user-dcard user-presence-card">
      <header>
        <PanelTitle of="client">在线设备</PanelTitle>
        <span className="rt" title="按公网 IP 去重，不等于连接数；同一 IP 可同时使用多条接入链">
          {reading}
          {!pending && banCount > 0 && (
            <>
              {' '}
              · 封禁 <b>{banCount}</b>
            </>
          )}
        </span>
      </header>
      <div className="user-dcard-body">
        {!!error && <ErrorBox error={error} />}
        {!!error && presence && <span className="dim">当前显示上次成功读取的快照。</span>}
        {notice && (
          <div className={`user-presence-note ${notice.tone}`}>
            <span>{notice.text}</span>
          </div>
        )}
        {visibleBuckets.length > 0 && (
          <ol className="user-presence-timeline" aria-label={`在线与最近 ${retentionDays} 天的来源`}>
            {visibleBuckets.map(bucket => (
              <Fragment key={bucket}>
                <li className="user-presence-day">
                  <span>{bucketLabel[bucket]}</span>
                </li>
                {groups.get(bucket)!.map(source => (
                  <PresenceRow
                    key={source.ip}
                    source={source}
                    online={bucket === 'online'}
                    older={OLDER_PRESENCE_BUCKETS.has(bucket)}
                    country={bucket === 'online' ? countries?.[source.ip] : historyCountries.get(source.ip)}
                    operator={bucket === 'online' ? operators?.[source.ip] : historyOperators.get(source.ip)}
                    apps={apps}
                    now={now}
                    onBan={canBan && bans ? banSource : undefined}
                  />
                ))}
              </Fragment>
            ))}
          </ol>
        )}
        {!pending && history.isPending && (
          <div className="user-presence-pending">
            <FieldLoading label="读取离线来源" />
          </div>
        )}
        {!!history.error && <ErrorBox error={history.error} />}
        {history.data?.truncated && (historyExpanded || timelineOffline.length <= PRESENCE_HISTORY_PREVIEW_LIMIT) && (
          <span className="user-presence-history-note">记录较多，仅显示最近 256 个来源 IP。</span>
        )}
        {timelineOffline.length > PRESENCE_HISTORY_PREVIEW_LIMIT && (
          <button
            type="button"
            className="user-presence-more"
            aria-expanded={historyExpanded}
            onClick={() => setHistoryExpanded(expanded => !expanded)}
          >
            {historyExpanded ? '收起历史来源' : `显示其余 ${hiddenHistoryCount} 个历史来源`}
          </button>
        )}
        {bans && bannedRows.length > 0 && (
          <BannedSources
            rows={bannedRows}
            nodes={bans.nodes}
            apps={apps}
            today={today}
            onUnban={canBan ? unbanSource : undefined}
          />
        )}
        {canBan &&
          bans &&
          (addingBan ? (
            <SourceBanAdd
              bans={bans.ips}
              describe={describeSource}
              onSave={banSource}
              onDone={() => setAddingBan(false)}
            />
          ) : (
            <button type="button" className="user-presence-more source-ban-add-link" onClick={() => setAddingBan(true)}>
              <Icon of="plus" size={11} />
              {bannedRows.length > 0 || visibleBuckets.length > 0 ? '封禁其他 IP' : '封禁 IP'}
            </button>
          ))}
        {!!sourceAccess.query.error && <ErrorBox error={sourceAccess.query.error} />}
      </div>
    </section>
  );
}

// 名册右列的本期用量只保留一位小数；精确值在详情的「用量与额度」里。
const compactUsage = (total: number) => {
  const [value, unit] = bytes(total).split(' ');
  return { value: String(Number(Number(value).toFixed(1))), unit };
};

// 额度用到这一比例即进入注意档：名册从这里开始显示具体百分比，详情的额度条改用注意色。
const QUOTA_ATTENTION_PCT = 80;

// 名册不暴露低用量的精确百分比；进入注意档后才显示具体数值。
export function quotaStage(pct: number): string {
  if (pct < QUOTA_ATTENTION_PCT) return '余裕';
  return `${pct.toFixed(0)}%`;
}

/** 名册环形表示用户所有有限额度线路的合计，不让某一条小额度线路支配总览。 */
export function combinedQuotaPercent(rows: { used: number | null; limit: number | null }[]): number | null {
  const limited = rows.filter(
    (row): row is { used: number; limit: number } => row.used !== null && row.limit !== null && row.limit > 0,
  );
  if (limited.length === 0) return null;
  const used = limited.reduce((total, row) => total + row.used, 0);
  const limit = limited.reduce((total, row) => total + row.limit, 0);
  return (used / limit) * 100;
}

// 名册恢复线上原有的中性环形进度；右侧文字单独按阶段显示。
function QuotaRing({ pct, over }: { pct: number; over: boolean }) {
  const clamped = Math.min(100, Math.max(0, pct));
  return (
    <svg
      className={`user-quota-ring${over ? ' over' : ''}`}
      width="12"
      height="12"
      viewBox="0 0 12 12"
      aria-hidden="true"
    >
      <circle className="track" cx="6" cy="6" r="4.5" />
      {clamped > 0 && (
        <circle
          className="value"
          cx="6"
          cy="6"
          r="4.5"
          pathLength="100"
          strokeDasharray={`${clamped} ${100 - clamped}`}
          transform="rotate(-90 6 6)"
        />
      )}
    </svg>
  );
}

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「用户」）由外壳补全。 */
const crumbOf = (d: Drill): CrumbSeg[] => (d.p === 'user' ? [{ label: d.id }] : []);

export function UsersPane({ win, bare = false }: { win: Win; bare?: boolean }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const narrow = useNarrow();
  // 不同用户的详情高度不同；沿用旧偏移会在内容重排后再次跳动。只有历史返回恢复位置。
  const go = (d: Drill) => navigate('users', d);
  useCrumb(win, crumbOf(drill));

  return <UserList drill={drill} go={go} sheeted={bare} narrow={narrow} />;
}

// 额度以 GiB 为单位收发：操作者设定额度时使用的单位是 GiB，不是字节数。
// 整数 GiB 的往返转换是精确的；通过其他 API 设置的非整值在此会被截断到 KB 级，
// 这类额度不应在此输入框中修改。
const GiB = 1024 ** 3;
// 额度通常按整 GiB 设置，整数时不显示小数位（「100 GiB」）；非整值沿用 bytes() 的精度。
const quotaBytes = (n: number) => (n % GiB === 0 ? `${n / GiB} GiB` : bytes(n));

/** 线路在当前用户下的接入情况：地区旗、已授权接入点所在的链、被配额执行器停用的接入点（按链名）。 */
export interface QuotaRouteAccess {
  countries: string[];
  chains: string[];
  suspended: string[];
}

const NO_ROUTE_ACCESS: QuotaRouteAccess = { countries: [], chains: [], suspended: [] };

// 一条线路一行，分两层：名称（线路名 + 地区旗 / 接入链）│ 已用 / 额度 … 剩余；第二层是额度条与百分比。
// 额度条只在设置了额度且用量已知时绘制：未设额度时不存在用尽的概念，空槽会被理解为用量为零，
// 而实际含义是不限量。
export function QuotaRow({
  app,
  used,
  limit,
  over,
  usageState = 'ready',
  access = NO_ROUTE_ACCESS,
  policy,
  user,
  editable = false,
}: {
  app: SnapshotApp;
  used: number | null;
  limit: number | null;
  over: boolean;
  usageState?: CycleUsageState;
  access?: QuotaRouteAccess;
  policy?: UserTrafficPolicyView;
  user?: UserListItem;
  editable?: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const editorId = useId();
  const editButton = useRef<HTMLButtonElement>(null);
  const closeEditor = () => {
    setEditing(false);
    queueMicrotask(() => editButton.current?.focus());
  };
  const label = app.label || app.id;
  const pct = limit !== null && used !== null ? (used / limit) * 100 : null;

  const name = (
    <span className="qta-name">
      <span className="qta-app">
        <b>{label}</b>
        {access.countries.length > 0 && (
          <span className="qta-flags">
            {access.countries.slice(0, 3).map(code => (
              <RegionFlag key={code} code={code} />
            ))}
          </span>
        )}
      </span>
      {access.suspended.length > 0 ? (
        // 行内只写数量；被停用的链与恢复条件放进悬停提示，避免名称列换行。
        <span
          className="qta-sub stop"
          title={`${[...new Set(access.suspended)].join('、')} 已被系统停用；补足额度或账期重置后自动恢复`}
        >
          系统已停用 {access.suspended.length} 个接入点
        </span>
      ) : (
        <span className="qta-sub" title={access.chains.length > 0 ? access.chains.join('、') : undefined}>
          {access.chains.length > 0 ? access.chains.join('、') : '未授权接入点'}
        </span>
      )}
    </span>
  );

  // 限速仍可用，与名册一致使用注意色；服务暂停时额度轨道退为灰色。
  const paused = policy?.service_available === false;
  const throttled = !paused && policy?.exhausted && policy.policy?.exhausted_action.kind === 'throttle';
  const tone =
    used === null
      ? 'unknown'
      : limit === null
        ? 'unlimited'
        : over
          ? throttled
            ? 'warn'
            : 'over'
          : pct !== null && pct >= QUOTA_ATTENTION_PCT
            ? 'warn'
            : 'ok';
  const [figure, unit] = used === null ? ['', ''] : bytes(used).split(' ');
  const left =
    usageState === 'pending' ? null : usageState === 'failed' ? (
      '用量暂不可用'
    ) : used === null ? (
      '用量未知'
    ) : limit === null ? (
      '不限额度'
    ) : over ? (
      <>
        超出<b>{bytes(used - limit)}</b>
      </>
    ) : (
      <>
        剩余<b>{bytes(limit - used)}</b>
      </>
    );
  return (
    <div
      className={`qta-r ${tone}${paused ? ' paused' : throttled ? ' throttled' : ''}${editing ? ' fwe-open' : ''}`}
      role="listitem"
    >
      {name}
      <span className="qta-figs">
        {usageState === 'pending' ? (
          '读取中…'
        ) : used === null ? (
          '—'
        ) : (
          <>
            {figure}
            <small>{unit}</small>
          </>
        )}
        {limit !== null && <span className="qta-cap">/ {quotaBytes(limit)}</span>}
      </span>
      {left !== null && (
        <span className="qta-left">
          <span className="qta-left-value">{left}</span>
        </span>
      )}
      <UserTrafficQuotaMeter used={used} limit={limit} view={policy} />
      {editable && user && (
        <span className="qta-acts">
          <button
            ref={editButton}
            type="button"
            className="btn qta-pencil"
            disabled={!policy || editing}
            aria-label={`${label} · 额度与超额处理`}
            title="编辑额度"
            aria-expanded={editing}
            aria-controls={editing ? editorId : undefined}
            onClick={() => setEditing(true)}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="m3 11.8.6-2.7L10.8 2l2.2 2.2-7.1 7.1-2.9.5Z" />
              <path d="m9.7 3.1 2.2 2.2" />
            </svg>
          </button>
        </span>
      )}
      {editing && user && policy && (
        <div className="fwe-sheet" id={editorId}>
          <UserAppTrafficPolicyEditor
            key={`${user.tenant_id}/${user.id}/${app.id}`}
            user={user}
            view={policy}
            label={`${label} · 额度与超额处理`}
            onClose={closeEditor}
          />
        </div>
      )}
    </div>
  );
}

/** Keep the card in place while the selected user's readings and policy arrive together. */
function UserUsageBody({ pending, children }: { pending: boolean; children: ReactNode }) {
  const body = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(0);
  useLayoutEffect(() => {
    const element = body.current;
    if (pending || !element) return;
    const measure = () => setHeight(element.getBoundingClientRect().height);
    measure();
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(measure);
    observer?.observe(element);
    return () => observer?.disconnect();
  }, [pending]);
  return (
    <div
      ref={body}
      className="user-dcard-body panel-full-bleed-body user-usage"
      aria-busy={pending || undefined}
      style={pending && height > 0 ? { minHeight: height } : undefined}
    >
      {pending ? (
        <div className="user-usage-loading">
          <PanelLoading label="读取用量与额度…" />
        </div>
      ) : (
        children
      )}
    </div>
  );
}

function UserAccessQuotaRows({
  user,
  rows,
  usageState,
  editable,
  routeAccessOf,
  policies,
}: {
  user: UserListItem;
  rows: Array<Pick<Parameters<typeof QuotaRow>[0], 'app' | 'used' | 'limit' | 'over'>>;
  usageState: CycleUsageState;
  editable: boolean;
  routeAccessOf: (app: SnapshotApp) => QuotaRouteAccess;
  policies: ReturnType<typeof useUserTrafficPolicies>;
}) {
  return (
    <div className="user-access-quotas">
      {policies.error && <ErrorBox error={policies.error} />}
      <div className="qta" role="list" aria-label="各线路的用量与额度">
        {rows.map(q => (
          <QuotaRow
            key={q.app.id}
            {...q}
            user={user}
            editable={editable}
            usageState={usageState}
            access={routeAccessOf(q.app)}
            policy={policies.data?.find(row => row.kind === 'access' && row.app_id === q.app.id)}
          />
        ))}
      </div>
    </div>
  );
}
// 详情卡的本期读数栏：与用量页读数栏同一写法（合计 + 上下行组成条与数值），尺寸按子卡缩小一档。
function UserUsageLedger({
  state,
  rows,
  periodEnd,
  accountingMode,
  accountingDisabled,
  onAccountingModeChange,
  servicePolicy,
}: {
  state: CycleUsageState;
  rows: UsageMonthlyViewRow[];
  periodEnd: string | null;
  accountingMode: UserTrafficAccountingMode;
  accountingDisabled: boolean;
  onAccountingModeChange: (mode: UserTrafficAccountingMode) => void;
  /** 账期与有效期（user-service-policy.tsx），排在计量口径之后。 */
  servicePolicy: ReactNode;
}) {
  const uplink = rows.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const downlink = rows.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const rawTotal = uplink + downlink;
  const total = rawTotal * trafficMultiplier(accountingMode);
  const known = state === 'ready' && rows.length > 0;
  const [figure, unit] = bytes(total).split(' ');
  const upShare = rawTotal > 0 ? (uplink / rawTotal) * 100 : 0;
  return (
    <section className="user-usage-ledger" aria-label="本期接入">
      <div className="user-usage-hero">
        <span className="user-usage-hero-label">本期接入</span>
        {known ? (
          <strong className="user-usage-hero-value">
            {figure} <small>{unit}</small>
          </strong>
        ) : (
          <strong className="user-usage-hero-value none">{cycleUsageText(state, rows.length, total)}</strong>
        )}
        {periodEnd && <QuotaReset periodEnd={periodEnd} />}
      </div>
      <div className="user-usage-compose">
        {/* 下方两行写出同样的数值；组成条只表示比例。 */}
        <div className="usage-split" aria-hidden="true">
          {known && total > 0 && (
            <>
              <i className="up" style={{ flexGrow: upShare }} />
              <i className="down" style={{ flexGrow: 100 - upShare }} />
            </>
          )}
        </div>
        <dl className="usage-io">
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{known ? bytes(uplink) : '—'}</dd>
            <dd className="usage-share">{known ? shareOf(uplink, rawTotal) : ''}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{known ? bytes(downlink) : '—'}</dd>
            <dd className="usage-share">{known ? shareOf(downlink, rawTotal) : ''}</dd>
          </div>
        </dl>
      </div>
      <UserTrafficAccountingSetting
        mode={accountingMode}
        disabled={accountingDisabled}
        onChange={onAccountingModeChange}
      />
      {servicePolicy}
    </section>
  );
}

// 重置边界由服务端按该用户的策略返回，浏览器只显示倒计时。

function QuotaReset({ periodEnd }: { periodEnd: string }) {
  const now = useNow();
  const resetAt = Date.parse(periodEnd);
  if (!Number.isFinite(resetAt) || resetAt <= now) return null;
  const date = new Date(resetAt + 8 * 3_600_000);
  const resetDate = `${date.getUTCMonth() + 1} 月 ${date.getUTCDate()} 日`;
  const days = Math.ceil((resetAt - now) / 86_400_000);
  return (
    <span
      className="user-usage-reset"
      title={`额度按 UTC+8 账期统计，${resetDate} 00:00 重新计算；仅恢复仍符合有效期和手动开关的接入点`}
    >
      {resetDate}重置
      <span className="usage-dot" aria-hidden="true">
        ·
      </span>
      {days} 天后
    </span>
  );
}

function UserPasswordDialog({ user, onClose }: { user: UserListItem; onClose: () => void }) {
  const [next, setNext] = useState('');
  const [again, setAgain] = useState('');
  const change = useMutation({
    mutationFn: () => setUserPassword(user.tenant_id, user.id, next),
  });

  const { tooShort, mismatch, ready } = passwordConfirmation(next, again);
  const guardScope = `user-password:${user.tenant_id}:${user.id}`;
  useUnsavedChanges(!change.data && (next.length > 0 || again.length > 0), `${user.id} 的新密码`, guardScope);

  return (
    <DialogLayer
      label={`修改 ${user.id} 的登录密码`}
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface user-password-card">
        <header>
          <span>
            <b>修改密码</b>
            <small className="mono">{user.id}</small>
          </span>
          <DialogClose aria-label="关闭" title="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        {change.data ? (
          <div className="user-password-done">
            <div className="callout">
              密码已修改。
              {change.data.sessions_revoked > 0
                ? `已注销 ${change.data.sessions_revoked} 条旧会话。`
                : '没有需要注销的旧会话。'}
            </div>
            <DialogClose className="btn primary">完成</DialogClose>
          </div>
        ) : (
          <form
            onSubmit={event => {
              event.preventDefault();
              if (ready) change.mutate();
            }}
          >
            <p className="note">直接设置该用户的新登录密码；保存后，其他设备上的登录会话立即失效。</p>
            <label>
              <span>新密码</span>
              <input
                className="f"
                type="password"
                autoComplete="new-password"
                autoFocus
                value={next}
                placeholder={`至少 ${PASSWORD_MIN_LENGTH} 位`}
                onChange={event => setNext(event.target.value)}
              />
            </label>
            <label>
              <span>确认密码</span>
              <input
                className="f"
                type="password"
                autoComplete="new-password"
                value={again}
                placeholder="再输入一遍"
                onChange={event => setAgain(event.target.value)}
              />
            </label>
            {tooShort && <p className="note bad">新密码至少 {PASSWORD_MIN_LENGTH} 位。</p>}
            {mismatch && <p className="note bad">两次输入不一致。</p>}
            {change.error && <ErrorBox error={change.error} />}
            <footer>
              <DialogClose className="btn">取消</DialogClose>
              <button className="btn primary" type="submit" disabled={!ready || change.isPending}>
                {change.isPending ? '保存中…' : '保存密码'}
              </button>
            </footer>
          </form>
        )}
      </section>
    </DialogLayer>
  );
}

function UserLoginIssuedDialog({
  user,
  issued,
  onClose,
}: {
  user: UserListItem;
  issued: { operator_id: string; password: string };
  onClose: () => void;
}) {
  const guardScope = `user-login:${user.tenant_id}:${user.id}`;
  const clearUnsavedChanges = useUnsavedChanges(true, `${user.id} 的一次性登录密码`, guardScope);
  return (
    <DialogLayer label={`${user.id} 的新登录密码`} onClose={onClose} canClose={() => confirmDiscardChanges(guardScope)}>
      <section className="dialog-surface user-password-card user-login-issued-card">
        <header>
          <span>
            <b>{user.login_enabled ? '密码已重置' : '登录已开通'}</b>
            <small>密码只显示这一次</small>
          </span>
          <DialogClose aria-label="关闭" title="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="user-login-issued">
          <span>
            登录名 <code>{user.id}</code> <CopyButton className="user-fcopy" text={user.id} iconOnly />
          </span>
          <span>
            密码 <code>{issued.password}</code> <CopyButton className="user-fcopy" text={issued.password} iconOnly />
          </span>
        </div>
        <footer>
          <DialogClose className="btn primary" onClick={clearUnsavedChanges}>
            我已保存
          </DialogClose>
        </footer>
      </section>
    </DialogLayer>
  );
}

function UserDirectLoginIssuedDialog({
  user,
  issued,
  onClose,
}: {
  user: UserListItem;
  issued: IssuedUserDirectLogin;
  onClose: () => void;
}) {
  const url = directLoginUrl(issued.uuid, issued.token);
  const guardScope = `user-direct-login:${user.tenant_id}:${user.id}`;
  const clearUnsavedChanges = useUnsavedChanges(true, `${user.id} 的直达登录页面`, guardScope);
  return (
    <DialogLayer
      label={`${user.id} 的直达登录页面`}
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface user-password-card user-login-issued-card user-direct-login-card">
        <header>
          <span>
            <b>{user.direct_login_enabled ? '直达页面已重新生成' : '直达页面已生成'}</b>
            <small>请保存完整链接，关闭后无法再次查看 TOKEN</small>
          </span>
          <DialogClose aria-label="关闭" title="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="user-direct-login-body">
          <div className="user-direct-login-field primary">
            <div className="user-direct-login-label">
              <span>直达登录链接</span>
              <CopyButton className="btn sm" text={url} label="复制链接" successLabel="链接已复制" />
            </div>
            <code>{url}</code>
          </div>
          <div className="user-direct-login-credentials">
            <div className="user-direct-login-field">
              <div className="user-direct-login-label">
                <span>UUID</span>
                <CopyButton
                  className="btn icon sm"
                  text={issued.uuid}
                  label="复制 UUID"
                  successLabel="UUID 已复制"
                  iconOnly
                />
              </div>
              <code>{issued.uuid}</code>
            </div>
            <div className="user-direct-login-field">
              <div className="user-direct-login-label">
                <span>TOKEN</span>
                <CopyButton
                  className="btn icon sm"
                  text={issued.token}
                  label="复制 TOKEN"
                  successLabel="TOKEN 已复制"
                  iconOnly
                />
              </div>
              <code>{issued.token}</code>
            </div>
          </div>
        </div>
        <footer>
          <span className="user-direct-login-note">
            <Icon of="security" size={13} />
            链接含登录凭据，请妥善保管
          </span>
          <DialogClose className="btn primary" onClick={clearUnsavedChanges}>
            我已保存
          </DialogClose>
        </footer>
      </section>
    </DialogLayer>
  );
}

const grantKey = (value: GrantWrite) => `${value.app_id}/${value.tenant_id}/${value.user_id}/${value.ingress_id}`;

type GrantColumn = { app: SnapshotApp; ingress: SnapshotApp['ingresses'][number] };

const selectedGrantKey = (appId: string, tenantId: string, userId: string, ingressId: string) =>
  `${appId}/${tenantId}/${userId}/${ingressId}`;

const sameGrantSelection = (left: Set<string>, right: Set<string>) =>
  left.size === right.size && [...left].every(key => right.has(key));

function UserGrantEditor({
  user,
  columns,
  granted,
  suspended,
  editable,
  confirmedRevision,
  snapshotUnavailable,
  unconfirmedGrantKeys,
  nameOf,
  refresh,
}: {
  user: UserListItem;
  columns: GrantColumn[];
  granted: Set<string>;
  suspended: Set<string>;
  editable: boolean;
  confirmedRevision: number;
  snapshotUnavailable: boolean;
  unconfirmedGrantKeys: Set<string>;
  nameOf: ReturnType<typeof useNodeNames>;
  refresh: () => Promise<void>;
}) {
  const qc = useQueryClient();
  const [draftSelection, setDraftSelection] = useState<Set<string> | null>(null);
  const [submittedRevision, setSubmittedRevision] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const serverSelection = new Set(
    columns.flatMap(({ app, ingress }) =>
      granted.has(`${user.tenant_id}/${user.id}/${ingress.id}`)
        ? [selectedGrantKey(app.id, user.tenant_id, user.id, ingress.id)]
        : [],
    ),
  );
  const acknowledged = submittedRevision !== null && confirmedRevision >= submittedRevision;
  const activeDraftSelection = acknowledged ? null : draftSelection;
  const selection = activeDraftSelection ?? serverSelection;
  const dirty = activeDraftSelection !== null && !sameGrantSelection(activeDraftSelection, serverSelection);
  const guardScope = `user-grants:${user.tenant_id}:${user.id}`;
  useUnsavedChanges(dirty, `${user.id} 的接入授权`, guardScope);
  const save = useMutation({
    mutationKey: GRANT_MUTATION_KEY,
    mutationFn: upsertGrant,
    onSettled: (_result, _error, value) => {
      void qc.invalidateQueries({ queryKey: ['grant-probe-plan', value.tenant_id, value.user_id] });
    },
  });
  const waiting = submitting || (submittedRevision !== null && !acknowledged);

  const toggle = (key: string) => {
    save.reset();
    setSubmittedRevision(null);
    setDraftSelection(current => {
      const next = new Set(acknowledged ? serverSelection : (current ?? serverSelection));
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return sameGrantSelection(next, serverSelection) ? null : next;
    });
  };
  const cancel = () => {
    if (waiting || !confirmDiscardChanges(guardScope)) return;
    save.reset();
    setDraftSelection(null);
  };
  const submit = async () => {
    if (!dirty || waiting) return;
    setSubmitting(true);
    const writes = columns.flatMap(({ app, ingress }) => {
      const key = selectedGrantKey(app.id, user.tenant_id, user.id, ingress.id);
      const enabled = selection.has(key);
      if (enabled === serverSelection.has(key)) return [];
      return [
        {
          app_id: app.id,
          tenant_id: user.tenant_id,
          user_id: user.id,
          ingress_id: ingress.id,
          enabled,
        } satisfies GrantWrite,
      ];
    });
    let lastRevision = confirmedRevision;
    let complete = false;
    try {
      for (const write of writes) {
        const result = await save.mutateAsync(write);
        lastRevision = Math.max(lastRevision, result.revision_id);
      }
      complete = true;
      setSubmittedRevision(lastRevision);
    } catch {
      // The shared error surface renders the mutation error. Keep the intended selection open so
      // a partial save can be reconciled against the refreshed snapshot and retried.
    } finally {
      try {
        await refresh();
      } catch {
        // Query state owns refresh failures and keeps the revision-confirmation banner visible.
      } finally {
        setSubmitting(false);
        if (complete && lastRevision <= confirmedRevision) {
          setSubmittedRevision(null);
          setDraftSelection(null);
        }
      }
    }
  };

  return (
    <section className="panel config-panel user-dcard user-grant-editor">
      <header>
        <PanelTitle of="access">接入授权</PanelTitle>
        <span className="rt">
          <b>{selection.size}</b> / {columns.length} 已授权
        </span>
      </header>
      <div className="user-dcard-body">
        {columns.length === 0 ? (
          <span className="dim">尚无接入面，没有可授权的对象。</span>
        ) : (
          <div className="grant-cards">
            {columns.map(({ app, ingress }) => {
              const key = selectedGrantKey(app.id, user.tenant_id, user.id, ingress.id);
              const selected = selection.has(key);
              const heldByQuota =
                !granted.has(`${user.tenant_id}/${user.id}/${ingress.id}`) && suspended.has(ingress.id);
              const held = !selected && heldByQuota;
              const chain = (app.chains ?? []).find(item => item.id === ingress.chain);
              const chainName = chain?.name || ingress.chain;
              const appName = app.label || app.id;
              return (
                <button
                  key={key}
                  type="button"
                  className={`grant-card${held ? ' held' : ''}`}
                  title={
                    heldByQuota
                      ? '流量已用尽，系统已停用。补足额度后自动恢复；此时手动授权在下一轮仍会被撤销。'
                      : `${chainName} · ${appName} · ${ingress.id} · ${selected ? '取消选择' : '选择授权'}`
                  }
                  aria-pressed={selected}
                  aria-busy={waiting}
                  disabled={
                    !editable ||
                    snapshotUnavailable ||
                    waiting ||
                    unconfirmedGrantKeys.has(selectedGrantKey(app.id, user.tenant_id, user.id, ingress.id))
                  }
                  onClick={() => toggle(key)}
                >
                  <span className="gc-head">
                    <span className="gc-app">
                      <span className="gc-name">
                        <RegionFlag code={chain?.subscription_country} />
                        <span>{chainName}</span>
                      </span>
                      <i>{ingress.chain}</i>
                    </span>
                    <span className="gc-mark" aria-hidden="true">
                      {selected ? '✓' : held ? '⦸' : ''}
                    </span>
                  </span>
                  <span className="gc-at">{held ? '流量用尽已停用' : `${nameOf(ingress.node)}:${ingress.port}`}</span>
                  <span className="gc-wm">{appName}</span>
                </button>
              );
            })}
          </div>
        )}
      </div>
      {activeDraftSelection !== null && (
        <footer className="user-dcard-foot grant-editor-actions">
          <button type="button" className="btn" disabled={waiting} onClick={cancel}>
            取消
          </button>
          <button type="button" className="btn primary" disabled={!dirty || waiting} onClick={() => void submit()}>
            {waiting ? '保存中…' : '保存'}
          </button>
        </footer>
      )}
    </section>
  );
}

type UserDetailTab = 'access' | 'forwarding' | 'stats';
const USER_DETAIL_TABS = [
  { key: 'access', label: '接入', icon: 'access' },
  { key: 'forwarding', label: '转发', icon: 'forwarding' },
  { key: 'stats', label: '统计', icon: 'observe' },
] as const;

/** Visit each tab on demand, retaining its forms only for the same user. */
function UserDetailTabContent({
  active,
  userKey,
  children,
}: {
  active: boolean;
  userKey: string;
  children: ReactNode;
}) {
  const [visit, setVisit] = useState({ userKey, opened: active });
  const opened = active || (visit.userKey === userKey && visit.opened);
  if (visit.userKey !== userKey || visit.opened !== opened) setVisit({ userKey, opened });
  return opened ? children : null;
}

function UserList({
  drill,
  go,
  sheeted = false,
  narrow,
}: {
  drill: Drill;
  go: (d: Drill) => void;
  sheeted?: boolean;
  narrow: boolean;
}) {
  const nameOf = useNodeNames();
  const { who } = useSession();
  const qc = useQueryClient();
  const prepareTimer = useRef(0);
  const cancelPrepare = useCallback(() => {
    window.clearTimeout(prepareTimer.current);
    prepareTimer.current = 0;
  }, []);
  useEffect(() => cancelPrepare, [cancelPrepare]);
  const prepareUser = (user: UserListItem) => {
    cancelPrepare();
    void prefetchUserTrafficPolicies(qc, who, user);
  };
  const schedulePrepare = (user: UserListItem) => {
    cancelPrepare();
    prepareTimer.current = window.setTimeout(() => prepareUser(user), 120);
  };
  const editable = can(who.role, 'edit');
  const canViewPresence = !isVisitor(who);
  const [detailTab, setDetailTab] = useState<UserDetailTab>('access');
  const detailTabId = useId();
  const tabId = (tab: UserDetailTab) => `${detailTabId}-${tab}-tab`;
  const panelId = (tab: UserDetailTab) => `${detailTabId}-${tab}-panel`;
  const moveDetailTab = (event: ReactKeyboardEvent<HTMLButtonElement>, current: UserDetailTab) => {
    const index = detailTabs.findIndex(tab => tab.key === current);
    let next: number | null = null;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % detailTabs.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp')
      next = (index - 1 + detailTabs.length) % detailTabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = detailTabs.length - 1;
    if (next === null) return;
    event.preventDefault();
    setDetailTab(detailTabs[next].key);
    document.getElementById(tabId(detailTabs[next].key))?.focus();
  };
  const grantEdits = useGrantEdits();
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true), refetchInterval: 30_000 });
  const presence = useQuery({
    queryKey: ['user-presence', who.role],
    queryFn: fetchUserPresence,
    enabled: canViewPresence,
    refetchInterval: 30_000,
  });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants(), enabled: editable });
  const me = useQuery({ queryKey: ['me-user'], queryFn: fetchMyUser, enabled: who.role === 'user' });
  const snapshot = useQuery({
    queryKey: ['snapshot'],
    queryFn: fetchSnapshot,
    refetchInterval: query =>
      grantEdits.some(
        edit =>
          edit.revision !== null &&
          edit.revision > (query.state.data?.snapshot.revision ?? 0) &&
          Date.now() < edit.submittedAt + GRANT_REFRESH_WINDOW_MS,
      )
        ? GRANT_REFRESH_MS
        : 30_000,
  });
  /* 用量与策略在详情内容中一起等待；读取失败后仍允许授权操作，用量明确显示不可用。 */
  const cycle = useQuery({ queryKey: ['user-cycles'], queryFn: () => fetchUserCycleUsage(), refetchInterval: 30_000 });
  // 额度是直写的运营参数，不进草稿也不随发布变化，因此与用量分开查询，
  // 也不随 refresh() 中的三个查询一起失效：修改额度只失效额度本身。
  const quotas = useQuery({ queryKey: ['quotas'], queryFn: () => fetchQuotas(), refetchInterval: 30_000 });
  const cycleState: CycleUsageState = cycle.data ? 'ready' : cycle.isPending ? 'pending' : 'failed';
  const cyclePeriodOf = (user: UserListItem) =>
    cycle.data?.periods?.find(p => p.tenant_id === user.tenant_id && p.user_id === user.id);
  const presenceByUser = new Map<string, UserPresence>(
    (canViewPresence ? (presence.data?.users ?? []) : []).map(
      item => [`${item.tenant_id}/${item.user_id}`, item] as const,
    ),
  );

  const [search, setSearch] = useState('');
  const [rosterFilter, setRosterFilter] = useState<RosterFilter>('all');
  const [newUser, setNewUser] = useState<{ id: string; tenant: string } | null>(null);
  /* 当前查看的订阅（用户 + 格式）。null 表示未打开。同时只显示一份，与上面的展开策略一致。 */
  const [sub, setSub] = useState<{ user: UserListItem; kind: SubscriptionKind } | null>(null);
  const [detailActionsOpen, setDetailActionsOpen] = useState(false);
  const detailActionsButtonRef = useRef<HTMLButtonElement>(null);
  const detailActionsMenuRef = useRef<HTMLDivElement>(null);
  const [passwordUser, setPasswordUser] = useState<UserListItem | null>(null);
  const [accountingChange, setAccountingChange] = useState<{
    user: UserListItem;
    mode: UserTrafficAccountingMode;
  } | null>(null);
  const [issuedLogin, setIssuedLogin] = useState<{
    user: UserListItem;
    value: { operator_id: string; password: string };
  } | null>(null);
  const [issuedDirectLogin, setIssuedDirectLogin] = useState<{
    user: UserListItem;
    value: IssuedUserDirectLogin;
  } | null>(null);
  const newUserGuardScope = 'new-user';
  useUnsavedChanges(Boolean(newUser?.id.trim()), '新用户资料', newUserGuardScope);

  /* 低频且有破坏性的用户操作收入「更多」菜单。点击外部或按 Escape 都关闭，
     与机器详情和顶栏已有菜单保持同一套交互。 */
  useEffect(() => {
    if (!detailActionsOpen) return;
    const close = () => setDetailActionsOpen(false);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      close();
      requestAnimationFrame(() => detailActionsButtonRef.current?.focus());
    };
    document.addEventListener('click', close);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [detailActionsOpen]);

  const focusDetailActionEdge = (edge: 'first' | 'last') => {
    requestAnimationFrame(() => {
      const items = detailActionsMenuRef.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)');
      if (!items?.length) return;
      items[edge === 'first' ? 0 : items.length - 1]?.focus();
    });
  };
  const moveWithinDetailActions = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Tab') {
      setDetailActionsOpen(false);
      return;
    }
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
    if (items.length === 0) return;
    event.preventDefault();
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === 'Home') return items[0]?.focus();
    if (event.key === 'End') return items.at(-1)?.focus();
    const step = event.key === 'ArrowDown' ? 1 : -1;
    const next = current < 0 ? (step > 0 ? 0 : items.length - 1) : (current + step + items.length) % items.length;
    items[next]?.focus();
  };

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
    qc.invalidateQueries({ queryKey: ['grant-automation'] });
    return qc.invalidateQueries({ queryKey: ['snapshot'] });
  };
  const profile = useMutation({
    mutationFn: (value: {
      user: UserListItem;
      accountType: 'formal' | 'test';
      trafficMode?: UserTrafficAccountingMode;
    }) =>
      updateUserProfile(value.user.tenant_id, value.user.id, {
        account_type: value.accountType,
        traffic_accounting_mode: value.trafficMode,
      }),
    onSuccess: refresh,
  });
  const login = useMutation({
    mutationFn: (user: UserListItem) => issueUserLogin(user.tenant_id, user.id),
    onSuccess: (value, user) => {
      setIssuedLogin({ user, value });
      refresh();
    },
  });
  const directLogin = useMutation({
    mutationFn: (user: UserListItem) => issueUserDirectLogin(user.tenant_id, user.id),
    onSuccess: (value, user) => {
      setIssuedDirectLogin({ user, value });
      refresh();
    },
  });
  const revokeDirectLogin = useMutation({
    mutationFn: (user: UserListItem) => revokeUserDirectLogin(user.tenant_id, user.id),
    onSuccess: refresh,
  });
  const status = useMutation({
    mutationFn: (v: { user: UserListItem; next: 'active' | 'disabled' }) =>
      setUserStatus(v.user.tenant_id, v.user.id, v.next),
    onSuccess: refresh,
  });
  const rotate = useMutation({
    mutationFn: (value: { user: UserListItem; selfService: boolean }) =>
      value.selfService ? rotateMyUuid() : rotateUserUuid(value.user.tenant_id, value.user.id),
    onSuccess: () => {
      refresh();
      void qc.invalidateQueries({ queryKey: ['me-user'] });
    },
  });
  const tenantOptions = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  // 单租户阶段只接受“恰好一条”这个事实，不识别 platform 或操作者 scope 等特殊名字。
  const defaultTenant = tenantOptions.length === 1 ? tenantOptions[0].id : '';
  const newTenant = newUser?.tenant || defaultTenant;
  const create = useMutation({
    mutationFn: () => createUser({ tenant_id: newTenant, id: newUser?.id.trim() ?? '' }),
    onSuccess: () => {
      setNewUser(null);
      refresh();
    },
  });

  const apps: SnapshotApp[] = snapshot.data?.snapshot.apps ?? [];
  const latestGrantEdits = new Map(
    grantEdits.flatMap(edit => (edit.write ? [[grantKey(edit.write), edit] as const] : [])),
  );
  const unconfirmedGrantKeys = new Set(
    [...latestGrantEdits]
      .filter(([, edit]) => edit.status === 'pending' || (edit.revision ?? 0) > (snapshot.data?.snapshot.revision ?? 0))
      .map(([key]) => key),
  );
  const needsGrantRefresh = grantEdits.some(edit => (edit.revision ?? 0) > (snapshot.data?.snapshot.revision ?? 0));
  const grantError = [...latestGrantEdits.values()].find(edit => edit.status === 'error')?.error;
  const columns = apps.flatMap(a => a.ingresses.map(i => ({ app: a, ingress: i })));
  const granted = new Set(apps.flatMap(a => a.grants.map(g => `${g.tenant}/${g.user}/${g.ingress}`)));
  const selfKey = who.self_user ? `${who.self_user.tenant_id}/${who.self_user.user_id}` : null;
  const listedUsers = users.data?.users ?? [];
  const serverUsers = me.data
    ? listedUsers.some(user => `${user.tenant_id}/${user.id}` === selfKey)
      ? listedUsers.map(user => (`${user.tenant_id}/${user.id}` === selfKey ? me.data : user))
      : [me.data, ...listedUsers]
    : listedUsers;
  const list = serverUsers;
  const canManageLogin = can(who.role, 'manage-operators');
  /* 订阅是完整可用的配置，readonly 角色在 API 侧返回 403（见 session.tsx 的 can）。
     入口同步隐藏，避免点击后只得到一个错误。 */
  const canReadArtifacts = can(who.role, 'artifacts');
  /* 按（用户 × 线路）索引：各线路读取同一账期的用量。 */
  const usageByView = new Map(
    (cycle.data?.views ?? []).map((r: UsageMonthlyViewRow) => [`${r.tenant_id}/${r.user_id}/${r.app_id}`, r]),
  );

  // 用户总量包含接入与转发；按线路的额度仍只读取 usageByView。
  const usageOf = (u: UserListItem) => {
    const rows = [...(cycle.data?.views ?? []), ...(cycle.data?.forwarding ?? [])].filter(
      row => row.tenant_id === u.tenant_id && row.user_id === u.id,
    );
    return {
      rows,
      total:
        rows.reduce((n, r) => n + r.uplink_bytes + r.downlink_bytes, 0) * trafficMultiplier(u.traffic_accounting_mode),
    };
  };

  const quotaByView = new Map(
    (quotas.data?.quotas ?? []).map(q => [`${q.tenant_id}/${q.user_id}/${q.app_id}`, q.limit_bytes]),
  );
  // 被配额执行器撤销的接入面，按用户索引。授权项和行内文案都需要读取它：
  // 否则撤销后界面只显示「未授权」，与从未配置过无法区分。
  const suspendedByUser = new Map<string, Set<string>>();
  for (const q of quotas.data?.quotas ?? []) {
    if (q.suspended_ingresses.length === 0) continue;
    const key = `${q.tenant_id}/${q.user_id}`;
    const set = suspendedByUser.get(key) ?? new Set<string>();
    for (const id of q.suspended_ingresses) set.add(id);
    suspendedByUser.set(key, set);
  }

  // 单个用户的「线路 × 额度」。列出的不是全部线路，而是已授权的线路加上已设额度的线路：
  // 未授权的线路不会产生流量，列出只是干扰；但先设额度后撤销授权的那条必须保持可见，
  // 否则它会成为无法修改的隐式限制。
  const quotaRowsOf = (u: UserListItem, mine: { app: SnapshotApp }[]) => {
    const ids = new Set(mine.map(c => c.app.id));
    for (const a of apps) if (quotaByView.has(`${u.tenant_id}/${u.id}/${a.id}`)) ids.add(a.id);
    return apps
      .filter(a => ids.has(a.id))
      .map(a => {
        const key = `${u.tenant_id}/${u.id}/${a.id}`;
        const row = usageByView.get(key);
        // 账期用量是可选观测；读取失败时显示未知，绝不能把未知当成 0 后再算出“额度充足”。
        const used =
          cycleState === 'ready'
            ? (row ? row.uplink_bytes + row.downlink_bytes : 0) * trafficMultiplier(u.traffic_accounting_mode)
            : null;
        const limit = quotaByView.get(key) ?? null;
        return { app: a, used, limit, over: limit !== null && used !== null && used >= limit };
      });
  };

  // 一次算出每行需要的派生量：标题栏的读数是全表汇总，行内又各自使用，
  // 两处分别计算会出现不一致。
  // 已停用的排到末尾，同档内保持服务端给出的顺序（sort 是稳定的）。排序只依据操作者
  // 设置的状态：流量用尽同样显示红色，但那是随用量变化并会自行恢复的状态，
  // 用它排序会导致列表顺序随用量变动。
  const ordered = [...list].sort((a, b) => {
    const aSelf = `${a.tenant_id}/${a.id}` === selfKey;
    const bSelf = `${b.tenant_id}/${b.id}` === selfKey;
    if (aSelf !== bSelf) return aSelf ? -1 : 1;
    return Number(a.status === 'disabled') - Number(b.status === 'disabled');
  });
  const allRows = ordered.map(u => {
    const mine = columns.filter(c => granted.has(`${u.tenant_id}/${u.id}/${c.ingress.id}`));
    const quotaRows = quotaRowsOf(u, mine);
    const suspended = suspendedByUser.get(`${u.tenant_id}/${u.id}`) ?? new Set<string>();
    const appTraffic = new Map(
      (cycle.data?.traffic_policies ?? [])
        .filter(item => item.tenant_id === u.tenant_id && item.user_id === u.id && item.kind === 'access')
        .map(item => [item.app_id, item]),
    );
    const facts: UserFacts = {
      status: u.status,
      serviceState: u.service_state,
      grants: mine.length,
      exhausted: quotaRows.filter(q => q.over && !appTraffic.get(q.app.id)?.rate_bps).map(q => q.app.label || q.app.id),
      suspended: suspended.size,
      throttledApps: quotaRows
        .filter(q => q.over && appTraffic.get(q.app.id)?.rate_bps)
        .map(q => q.app.label || q.app.id),
    };
    return {
      u,
      key: `${u.tenant_id}/${u.id}`,
      mine,
      suspended,
      use: usageOf(u),
      quotaRows,
      facts,
      tone: userTone(facts),
    };
  });
  const rows = allRows.filter(
    row => userMatchesSearch(row.u, search) && rosterFilterMatches(rosterFilter, row.u, row.facts),
  );
  const filterCounts = Object.fromEntries(
    ROSTER_FILTERS.map(option => [
      option.value,
      allRows.filter(row => rosterFilterMatches(option.value, row.u, row.facts)).length,
    ]),
  ) as Record<RosterFilter, number>;
  const newUserId = newUser?.id.trim() ?? '';
  const newUserBadSlug = newUserId && !isValidSlug(newUserId) ? 'ID 只能用 a-z 0-9 . _ -，最长 32' : null;
  const newUserDuplicate = list.some(user => user.tenant_id === newTenant && user.id === newUserId)
    ? `用户 ID ${newUserId} 已存在`
    : null;
  const newUserReady = !!newUserId && !!newTenant && !newUserBadSlug && !newUserDuplicate && editable;
  const newUserEditor = newUser && (
    <form
      className="user-new-row"
      aria-label="新增用户"
      onSubmit={event => {
        event.preventDefault();
        if (newUserReady) create.mutate();
      }}
    >
      <span className="user-new-intro">
        <span className="user-avatar user-new-avatar" aria-hidden="true">
          ＋
        </span>
        <span className="user-new-copy">
          <b>新增用户</b>
          <small>创建后再配置线路、额度与订阅</small>
        </span>
        <span className="st st-succeeded user-new-type">正式</span>
      </span>
      <label className="user-new-field">
        <span>用户 ID</span>
        <input
          className="f mono"
          autoFocus
          aria-label="新用户 ID"
          value={newUser.id}
          placeholder="例如 alice"
          onChange={event => setNewUser({ ...newUser, id: event.target.value })}
          onKeyDown={event => {
            if (event.key === 'Escape' && !create.isPending && confirmDiscardChanges(newUserGuardScope)) {
              setNewUser(null);
            }
          }}
        />
      </label>
      {(newUserBadSlug || newUserDuplicate) && <p className="user-new-error">{newUserBadSlug || newUserDuplicate}</p>}
      <footer className="user-new-actions">
        <button
          className="btn"
          type="button"
          disabled={create.isPending}
          onClick={() => confirmDiscardChanges(newUserGuardScope) && setNewUser(null)}
        >
          取消
        </button>
        <button className="btn primary" type="submit" disabled={!newUserReady || create.isPending}>
          {create.isPending ? '添加中…' : '添加用户'}
        </button>
      </footer>
    </form>
  );

  // URL 是显式选择的唯一来源，使刷新、分享链接和浏览器前进/后退落到同一用户。根地址仍
  // 默认预览当前筛选的首行；深链接失效时不回落到别人，避免地址写 alice 却展示 bob。
  const selected = drill.p === 'user' ? allRows.find(r => r.u.id === drill.id) : rows[0];
  const requestedPolicies = useUserTrafficPolicies(selected?.u);
  const selectionPending = cycle.isPending || (requestedPolicies.readable && requestedPolicies.isPending);
  const [readyDetailKey, setReadyDetailKey] = useState<string | null>(null);
  const selectedKey = selected?.key ?? null;
  if (!selectionPending && readyDetailKey !== selectedKey) setReadyDetailKey(selectedKey);
  // Keep a complete identity and its readings together until the next user's data settles.
  const detail = selectionPending ? (allRows.find(row => row.key === readyDetailKey) ?? selected) : selected;
  const canViewForwarding =
    who.role !== 'user' ||
    (who.self_user?.tenant_id === detail?.u.tenant_id && who.self_user?.user_id === detail?.u.id);
  const detailTabs = USER_DETAIL_TABS.filter(
    tab => (tab.key !== 'stats' || canViewUserRealtimeTraffic(who)) && (tab.key !== 'forwarding' || canViewForwarding),
  );
  const activeDetailTab = detailTabs.some(tab => tab.key === detailTab) ? detailTab : 'access';
  const trafficPolicies = useUserTrafficPolicies(detail?.u);
  const usagePending = cycle.isPending || (trafficPolicies.readable && trafficPolicies.isPending);

  if (
    users.isPending ||
    snapshot.isPending ||
    quotas.isPending ||
    (editable && tenants.isPending) ||
    (who.role === 'user' && me.isPending)
  ) {
    return <Loading variant="users" sheeted={sheeted} userDetail={drill.p === 'user'} />;
  }
  if (!users.data) return <ErrorBox error={users.error} />;
  if (!snapshot.data) return <ErrorBox error={snapshot.error} />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;
  // 额度缺失不能回退成“不限量”：那会把读取失败显示成一个有效、且风险相反的配置。
  if (quotas.error && !quotas.data) return <ErrorBox error={quotas.error} />;
  if (me.error) return <ErrorBox error={me.error} />;

  // 右栏详情。内容与此前就地展开时完全一致（身份与操作 + 用量额度 + 接入授权，
  // 沿用列表页原有的 .lst-acts / .fgrid / .qta / .grant-cards），只是从行内移到常驻的右栏，
  // 并在顶部补一条身份标题条。
  const detailOf = (r: (typeof rows)[number]) => {
    const { u, mine, suspended, use, quotaRows, facts, tone } = r;
    const disabled = u.status === 'disabled';
    const isMe = r.key === selfKey;
    const selfService = who.role === 'user' && isMe;
    const canOpenSubscription = canReadArtifacts || selfService;
    const headCls = disabled ? 'off' : tone === 'idle' ? 'idle' : '';
    const userPresence = presenceByUser.get(r.key);
    const statusText = userLampTitle(facts);
    const statusLabel = disabled
      ? '已停用'
      : facts.serviceState === 'pending'
        ? '未生效'
        : facts.serviceState === 'expired'
          ? '已到期'
          : tone === 'idle'
            ? '未授权'
            : tone === 'bad'
              ? '接入受限'
              : '可用';
    const uuid = u.uuid?.trim() ?? '';
    const shortUuid = uuid.length > 16 ? `${uuid.slice(0, 8)}…${uuid.slice(-4)}` : uuid;
    const testChip = accountTypeChip(u.account_type);
    // UUID 也收在「更多」里：只读身份能看到 UUID 时，菜单为复制它而可用，其余项各自禁用。
    const menuAvailable = editable || selfService || !!u.uuid;
    const changeAccountingMode = (trafficMode: UserTrafficAccountingMode) => {
      if (!editable || profile.isPending || trafficMode === (u.traffic_accounting_mode ?? 'single')) return;
      profile.reset();
      setAccountingChange({ user: u, mode: trafficMode });
    };
    // 用量行名称下方的接入情况。被配额执行器停用的接入点与已授权的分开：撤销后 grants 里
    // 已没有这些记录，与授权卡的 held 判定相同。地区旗依次取已授权、被停用、线路全部的链。
    const routeAccessOf = (app: SnapshotApp): QuotaRouteAccess => {
      const chainOf = (id: string) => (app.chains ?? []).find(chain => chain.id === id);
      const granted = mine.filter(column => column.app.id === app.id).map(column => column.ingress);
      const grantedIds = new Set(granted.map(ingress => ingress.id));
      const held = app.ingresses.filter(ingress => !grantedIds.has(ingress.id) && suspended.has(ingress.id));
      const shown = granted.length > 0 ? granted : held.length > 0 ? held : app.ingresses;
      const distinct = (values: (string | null | undefined)[]) =>
        [...new Set(values)].filter((value): value is string => !!value);
      return {
        countries: distinct(shown.map(ingress => chainOf(ingress.chain)?.subscription_country)),
        chains: distinct(granted.map(ingress => chainOf(ingress.chain)?.name || ingress.chain)),
        suspended: held.map(ingress => chainOf(ingress.chain)?.name || ingress.chain),
      };
    };
    return (
      <section
        key="user-detail"
        className="panel user-split-detail"
        aria-busy={detail?.key !== selected?.key || undefined}
        inert={detail?.key !== selected?.key}
      >
        <div className={`user-dhead${headCls ? ` ${headCls}` : ''}`}>
          {/* 身份列不随 UUID 长度伸缩，让页签位置稳定；窄容器把导航独立到下一行。 */}
          <div className="user-dhead-main">
            <div className="user-dhead-identity">
              <GeneratedUserAvatar id={u.id} variant="plate" />
              <div className="dtitle">
                <div className="dname">
                  <b title={u.id}>{u.id}</b>
                  <span
                    className="user-account-state"
                    data-tone={tone}
                    role="status"
                    title={statusText}
                    aria-label={statusText}
                  >
                    {statusLabel}
                  </span>
                  {isMe && <span className="user-row-chip">我</span>}
                  {testChip && <span className="user-row-chip test">{testChip}</span>}
                </div>
                <p className="dsub">
                  {uuid ? (
                    <CopyButton
                      key={uuid}
                      className="user-uuid-copy"
                      text={uuid}
                      label="复制完整 UUID"
                      successLabel="UUID 已复制"
                      failureLabel="UUID 复制失败"
                      iconOnly
                    >
                      {state => (
                        <>
                          <span className="user-uuid-label">UUID</span>
                          <span className="mono" title={uuid}>
                            {shortUuid}
                          </span>
                          <Icon of={copyStateIcon(state)} size={12} />
                        </>
                      )}
                    </CopyButton>
                  ) : (
                    <span className="user-uuid-empty">UUID —</span>
                  )}
                </p>
              </div>
            </div>
            {detailTabs.length > 1 && (
              <div className="nd-tabs user-detail-tabs" role="tablist" aria-label={`${u.id} 详情页签`}>
                <div className="nd-tabs-seg">
                  {detailTabs.map(tab => (
                    <button
                      key={tab.key}
                      type="button"
                      role="tab"
                      id={tabId(tab.key)}
                      aria-controls={panelId(tab.key)}
                      aria-selected={activeDetailTab === tab.key}
                      tabIndex={activeDetailTab === tab.key ? 0 : -1}
                      onClick={() => setDetailTab(tab.key)}
                      onKeyDown={event => moveDetailTab(event, tab.key)}
                    >
                      <Icon of={tab.icon} size={14} className="nd-tab-ic" />
                      {tab.label}
                    </button>
                  ))}
                </div>
              </div>
            )}
            <div className="user-dhead-tools">
              <div className="user-dacts">
                <button
                  type="button"
                  className="btn user-dact"
                  aria-label="节点"
                  disabled={!canOpenSubscription}
                  title="打开该用户的节点链接：复制分享链接、拨测当前授权，可选择协议与地址族"
                  onClick={() => setSub({ user: u, kind: 'uri' })}
                >
                  <Icon of="client" size={13} className="user-dact-icon" />
                  <span className="user-dact-label">节点</span>
                </button>
                <button
                  type="button"
                  className="btn user-dact"
                  aria-label="订阅"
                  disabled={!canOpenSubscription}
                  title="打开 Clash 订阅，可选择协议与地址族"
                  onClick={() => setSub({ user: u, kind: 'clash' })}
                >
                  <Icon of="subscription" size={13} className="user-dact-icon" />
                  <span className="user-dact-label">订阅</span>
                </button>
              </div>
              <div className="fg-menuwrap user-action-menuwrap">
                <button
                  className="btn user-dact user-dact-more"
                  ref={detailActionsButtonRef}
                  disabled={!menuAvailable}
                  aria-label="更多操作"
                  title="更多操作"
                  aria-haspopup="menu"
                  aria-controls={`${detailTabId}-actions-menu`}
                  aria-expanded={detailActionsOpen}
                  onKeyDown={event => {
                    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
                    event.preventDefault();
                    setDetailActionsOpen(true);
                    focusDetailActionEdge(event.key === 'ArrowDown' ? 'first' : 'last');
                  }}
                  onClick={event => {
                    event.stopPropagation();
                    setDetailActionsOpen(open => {
                      if (!open) focusDetailActionEdge('first');
                      return !open;
                    });
                  }}
                >
                  <Icon of="more" size={13} className="user-dact-icon" />
                </button>
                {detailActionsOpen && menuAvailable && (
                  <div
                    id={`${detailTabId}-actions-menu`}
                    ref={detailActionsMenuRef}
                    className="fg-menu user-action-menu"
                    role="menu"
                    aria-label={`${u.id} 更多操作`}
                    onClick={() => setDetailActionsOpen(false)}
                    onKeyDown={moveWithinDetailActions}
                  >
                    {u.uuid && (
                      // 复制后菜单不关，结果直接写在这一项上。
                      <div role="none" onClick={event => event.stopPropagation()}>
                        <CopyButton
                          role="menuitem"
                          className="user-action-copy"
                          text={u.uuid}
                          label="复制 UUID"
                          successLabel="UUID 已复制"
                          failureLabel="UUID 复制失败"
                        >
                          {(state, label) => (
                            <>
                              <Icon of={copyStateIcon(state)} size={14} className="user-action-menu-icon" />
                              <span>
                                {label}
                                <small className="mono">{u.uuid}</small>
                              </span>
                            </>
                          )}
                        </CopyButton>
                      </div>
                    )}
                    <button
                      role="menuitem"
                      disabled={!editable || profile.isPending}
                      onClick={() => profile.mutate({ user: u, accountType: testChip ? 'formal' : 'test' })}
                    >
                      <Icon of="users" size={14} className="user-action-menu-icon" />
                      <span>
                        {testChip ? '改为正式账号' : '标为测试账号'}
                        <small>
                          {testChip ? '当前是测试账号，名册中单独标注' : '名册中单独标注，可按「测试」筛选'}
                        </small>
                      </span>
                    </button>
                    <hr />
                    <button
                      role="menuitem"
                      disabled={selfService ? !u.login_enabled : !canManageLogin || !u.login_enabled}
                      onClick={() => {
                        if (selfService) navigate('password');
                        else setPasswordUser(u);
                      }}
                    >
                      <Icon of="security" size={14} className="user-action-menu-icon" />
                      <span>
                        修改密码
                        <small>
                          {selfService
                            ? u.login_enabled
                              ? '修改自己的登录密码'
                              : '当前只开通了直达登录，尚未设置密码'
                            : u.login_enabled
                              ? '为该用户设置新的登录密码'
                              : '请先开通登录'}
                        </small>
                      </span>
                    </button>
                    {canManageLogin && (
                      <button
                        role="menuitem"
                        className={u.login_enabled ? 'dg' : undefined}
                        disabled={login.isPending}
                        onClick={() => {
                          if (u.login_enabled && !window.confirm(`确定重置 ${u.id} 的登录密码？现有登录会话将失效。`)) {
                            return;
                          }
                          login.mutate(u);
                        }}
                      >
                        <Icon of="access" size={14} className="user-action-menu-icon" />
                        <span>
                          {u.login_enabled ? '重置登录密码' : '开通登录'}
                          <small>{u.login_enabled ? '生成一次性密码并注销现有会话' : '生成该用户的首次登录密码'}</small>
                        </span>
                      </button>
                    )}
                    {canManageLogin && (
                      <button
                        role="menuitem"
                        className={u.direct_login_enabled ? 'dg' : undefined}
                        disabled={directLogin.isPending}
                        onClick={() => {
                          if (
                            u.direct_login_enabled &&
                            !window.confirm(`确定重新生成 ${u.id} 的直达登录页面？旧页面将立即失效。`)
                          ) {
                            return;
                          }
                          directLogin.mutate(u);
                        }}
                      >
                        <Icon of="access" size={14} className="user-action-menu-icon" />
                        <span>
                          {u.direct_login_enabled ? '重新生成直达登录页' : '生成直达登录页'}
                          <small>
                            {u.direct_login_enabled ? '替换现有 UUID + TOKEN 登录链接' : '无需输入密码即可登录'}
                          </small>
                        </span>
                      </button>
                    )}
                    {canManageLogin && u.direct_login_enabled && (
                      <button
                        role="menuitem"
                        className="dg"
                        disabled={revokeDirectLogin.isPending}
                        onClick={() => {
                          if (!window.confirm(`确定撤销 ${u.id} 的直达登录页面？`)) return;
                          revokeDirectLogin.mutate(u);
                        }}
                      >
                        <Icon of="dash" size={14} className="user-action-menu-icon" />
                        <span>
                          撤销直达登录页
                          <small>已打开的会话不受影响，页面不能再用于登录</small>
                        </span>
                      </button>
                    )}
                    <hr />
                    {editable && (
                      <button
                        role="menuitem"
                        className={disabled ? undefined : 'dg'}
                        disabled={status.isPending}
                        onClick={() => status.mutate({ user: u, next: disabled ? 'active' : 'disabled' })}
                      >
                        <Icon of={disabled ? 'check' : 'dash'} size={14} className="user-action-menu-icon" />
                        <span>
                          {disabled ? '启用用户' : '停用用户'}
                          <small>{disabled ? '恢复该用户的连接权限' : '旧凭据将立即失效'}</small>
                        </span>
                      </button>
                    )}
                    <button
                      role="menuitem"
                      className="dg"
                      disabled={rotate.isPending || (!editable && !selfService)}
                      onClick={() => {
                        if (!window.confirm(`确定更换 ${u.id} 的 UUID？旧订阅和现有连接会立即失效。`)) return;
                        rotate.mutate({ user: u, selfService });
                      }}
                    >
                      <Icon of="settings" size={14} className="user-action-menu-icon" />
                      <span>
                        更换 UUID
                        <small>当前连接将断开，订阅需要重新导入</small>
                      </span>
                    </button>
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>
        {/* 未打开的页签不取数据；已打开的页签保留同一用户的表单。 */}
        <div
          className="user-dbody"
          role="tabpanel"
          id={panelId('access')}
          aria-labelledby={detailTabs.length > 1 ? tabId('access') : undefined}
          aria-label={detailTabs.length === 1 ? '接入' : undefined}
          hidden={activeDetailTab !== 'access'}
        >
          {/* 用量与额度：左侧读数栏沿用用量页写法（本期接入、重置日、上下行），底部是计量口径与账期、有效期，
              右侧每条线路一行。合计已在读数栏，标题读数只写线路数。 */}
          <section className="panel config-panel user-dcard user-usage-card">
            <header>
              <PanelTitle of="usage">用量与额度</PanelTitle>
              <span className="rt">
                <b>{quotaRows.length}</b> 条线路
              </span>
            </header>
            <UserUsageBody pending={usagePending}>
              <UserUsageLedger
                key={`ledger:${r.key}`}
                state={cycleState}
                rows={use.rows.filter((row): row is UsageMonthlyViewRow => 'app_id' in row)}
                periodEnd={cyclePeriodOf(u)?.ends_at ?? null}
                accountingMode={u.traffic_accounting_mode ?? 'single'}
                accountingDisabled={!editable || profile.isPending}
                onAccountingModeChange={changeAccountingMode}
                servicePolicy={
                  <>
                    <UserServiceTerms user={u} editable={editable} onSaved={refresh} />
                  </>
                }
              />
              {quotaRows.length === 0 ? (
                <span className="dim qta-empty">尚未授权任何线路</span>
              ) : (
                <UserAccessQuotaRows
                  key={`quotas:${r.key}`}
                  user={u}
                  rows={quotaRows}
                  usageState={cycleState}
                  editable={editable}
                  routeAccessOf={routeAccessOf}
                  policies={trafficPolicies}
                />
              )}
            </UserUsageBody>
          </section>

          {/* 来源封禁并入在线设备：在行上直接封禁，卡底列出已封禁的来源。访客与普通用户看不到这张卡。 */}
          {canViewPresence && (
            <UserPresenceCard
              key={`presence:${r.key}`}
              user={u}
              presence={userPresence}
              countries={Object.fromEntries(
                presence.data?.source_countries?.map(({ ip, country }) => [ip, country]) ?? [],
              )}
              operators={Object.fromEntries(
                presence.data?.source_operators?.map(({ ip, operator }) => [ip, operator]) ?? [],
              )}
              apps={apps}
              pending={presence.isPending}
              error={presence.error}
              canBan={editable}
              masked={who.masked_assets}
            />
          )}

          <UserGrantEditor
            key={`grants:${r.key}`}
            user={u}
            columns={columns}
            granted={granted}
            suspended={suspended}
            editable={editable}
            confirmedRevision={snapshot.data.snapshot.revision}
            snapshotUnavailable={!!snapshot.error || snapshot.fetchStatus === 'paused'}
            unconfirmedGrantKeys={unconfirmedGrantKeys}
            nameOf={nameOf}
            refresh={refresh}
          />
        </div>
        {canViewForwarding && (
          <div
            className="user-dbody"
            role="tabpanel"
            id={panelId('forwarding')}
            aria-labelledby={tabId('forwarding')}
            hidden={activeDetailTab !== 'forwarding'}
          >
            <UserDetailTabContent active={activeDetailTab === 'forwarding'} userKey={r.key}>
              <UserForwardingCard
                key={`forwarding:${r.key}`}
                user={u}
                nodes={snapshot.data.snapshot.nodes ?? []}
                canAuthorize={can(who.role, 'manage-operators')}
                canManageRules={editable || selfService}
                canViewRealtime={canViewUserRealtimeTraffic(who)}
                accountingDisabled={!editable || profile.isPending}
                onAccountingModeChange={changeAccountingMode}
                servicePolicy={
                  <>
                    <UserServiceTerms user={u} editable={editable} onSaved={refresh} />
                  </>
                }
              />
            </UserDetailTabContent>
          </div>
        )}
        {canViewUserRealtimeTraffic(who) && (
          <div
            className="user-dbody"
            role="tabpanel"
            id={panelId('stats')}
            aria-labelledby={tabId('stats')}
            hidden={activeDetailTab !== 'stats'}
          >
            <UserDetailTabContent active={activeDetailTab === 'stats'} userKey={r.key}>
              <UserRealtimeTrafficCard key={`traffic:${r.key}`} tenantId={u.tenant_id} userId={u.id} nameOf={nameOf} />
              <UserMachineUsageCard key={`machines:${r.key}`} tenantId={u.tenant_id} userId={u.id} nameOf={nameOf} />
              {/* 站点分类、其他站点域名、嗅探失败目标合在同一张卡里（user-site-usage.tsx）。 */}
              <UserSiteUsageCard key={`sites:${r.key}`} tenantId={u.tenant_id} userId={u.id} />
            </UserDetailTabContent>
          </div>
        )}
      </section>
    );
  };

  // 名册项：身份牌 │ 用户名 + 第二行 │ 右列（本期用量 / 额度）。
  // 常态不画状态灯：流量用尽与系统停用为红点，未授权为空心灰点，已停用整行降一档、不再叠灯。
  // 管理员第二行放在线来源，普通用户只看接入点数量；异常、未授权和停用状态优先于观测值，
  // 避免把诊断信息盖住权限事实。
  // 右列定宽，用量读数与额度百分比的右缘逐行对齐；环形按有限额度线路的合计用量 / 合计额度。
  const rosterRow = (row: (typeof allRows)[number]) => {
    const { u, key, mine, suspended, use, quotaRows, facts, tone } = row;
    const disabled = u.status === 'disabled';
    const picked = selected?.key === key;
    const lamp: AvatarLamp | null = disabled || tone === 'ok' ? null : { tone, title: userLampTitle(facts) };
    const state = disabled
      ? { text: '已停用', bad: false }
      : facts.serviceState === 'pending' || facts.serviceState === 'expired'
        ? { text: facts.serviceState === 'pending' ? '未生效' : '已到期', bad: true }
        : facts.exhausted.length > 0
          ? { text: `流量已用尽 · ${facts.exhausted.join('、')}`, bad: true }
          : facts.throttledApps?.length
            ? { text: `额度已用满 · ${facts.throttledApps.join('、')} 已限速`, bad: false }
            : suspended.size > 0
              ? { text: `${suspended.size} 个接入点已被系统停用`, bad: true }
              : mine.length === 0
                ? { text: '未授权', bad: false }
                : null;
    const usage = cycleState === 'ready' && use.rows.length > 0 ? compactUsage(use.total) : null;
    const quotaPct = combinedQuotaPercent(quotaRows);
    const quotaOver = quotaPct !== null && quotaPct >= 100;
    const userPresence = presenceByUser.get(key);
    const presenceText = canViewPresence ? userPresenceText(userPresence) : `${mine.length} 个接入点`;
    const presenceTitle = userPresence?.sources.length
      ? `${presenceText} · ${userPresence.sources.map(source => source.ip).join(' · ')}`
      : presenceText;
    const testChip = accountTypeChip(u.account_type);
    return (
      <button
        key={key}
        type="button"
        role="option"
        aria-selected={picked}
        data-route-focus={`user:${key}`}
        className={`user-row${disabled ? ' off' : ''}${picked ? ' picked' : ''}`}
        onMouseEnter={() => !picked && schedulePrepare(u)}
        onMouseLeave={cancelPrepare}
        onFocus={() => !picked && prepareUser(u)}
        onPointerDown={event => {
          if (event.pointerType !== 'mouse' && !picked) prepareUser(u);
        }}
        onClick={() => go({ p: 'user', id: u.id })}
      >
        <GeneratedUserAvatar id={u.id} lamp={lamp} />
        <span className="user-row-name">
          <b>{u.id}</b>
          {key === selfKey && <span className="user-row-chip">我</span>}
          {testChip && <span className="user-row-chip test">{testChip}</span>}
        </span>
        <span className={`user-row-usage${usage ? '' : ' none'}`}>
          {usage ? (
            <>
              {usage.value}
              <small>{usage.unit}</small>
            </>
          ) : (
            cycleUsageText(cycleState, use.rows.length, use.total)
          )}
        </span>
        <span className="user-row-meta">
          {state ? (
            <span className={`user-row-state${state.bad ? ' bad' : ''}`}>{state.text}</span>
          ) : mine.length > 0 ? (
            <span className={`user-row-presence${userPresence?.sources.length ? ' online' : ''}`} title={presenceTitle}>
              {presenceText}
            </span>
          ) : (
            <span className="user-row-state">{mine.length} 个接入点</span>
          )}
        </span>
        <span className="user-row-quota" title={quotaPct === null ? undefined : `总额度 ${quotaStage(quotaPct)}`}>
          {quotaPct !== null && (
            <>
              <QuotaRing pct={quotaPct} over={quotaOver} />
              <span className={quotaOver ? 'over' : undefined}>{quotaStage(quotaPct)}</span>
            </>
          )}
        </span>
      </button>
    );
  };
  const activeRows = rows.filter(row => row.u.status !== 'disabled');
  const disabledRows = rows.filter(row => row.u.status === 'disabled');

  /* 名册与详情是两张同级面板：名册承担搜索和开户入口，详情保留块档背景，
     内部使用机器详情的紧凑横向页头和独立内容卡。 */
  const body = (
    <>
      {(users.error || quotas.error || cycle.error) && (
        <div className="toolbar" role="status">
          <ErrorBox error={users.error ?? quotas.error ?? cycle.error} />
          <button
            className="btn"
            disabled={users.isFetching || quotas.isFetching || cycle.isFetching}
            onClick={() => void Promise.all([users.refetch(), quotas.refetch(), cycle.refetch()])}
          >
            重新读取用户状态
          </button>
        </div>
      )}
      {(snapshot.error || needsGrantRefresh) && (
        <div className="toolbar" role="status">
          <span>授权状态尚未确认，请重新读取后继续操作。</span>
          <button className="btn" disabled={snapshot.isFetching} onClick={() => void snapshot.refetch()}>
            重试读取授权
          </button>
          {snapshot.error && <ErrorBox error={snapshot.error} />}
        </div>
      )}
      {(grantError ||
        profile.error ||
        login.error ||
        directLogin.error ||
        revokeDirectLogin.error ||
        status.error ||
        rotate.error ||
        create.error) && (
        <ErrorBox
          error={
            grantError ??
            profile.error ??
            login.error ??
            directLogin.error ??
            revokeDirectLogin.error ??
            status.error ??
            rotate.error ??
            create.error
          }
        />
      )}
      {list.length === 0 ? (
        <section className="panel titled user-list-panel user-empty-panel" data-page-title="true">
          <header>
            <ListIcon of="users" />
            <h4>用户</h4>
            <span className="user-roster-head-actions">
              <button
                className="btn primary"
                disabled={!editable || tenantOptions.length !== 1 || !!newUser}
                title={tenantOptions.length !== 1 ? '当前账户无法新增用户，请联系管理员。' : undefined}
                onClick={() => setNewUser({ id: '', tenant: defaultTenant })}
              >
                ＋ 新增用户
              </button>
            </span>
          </header>
          <div className="user-roster-options">
            {newUserEditor}
            {!newUser && (
              <EmptyState
                icon="users"
                title="还没有用户"
                action={
                  <button
                    className="btn primary"
                    disabled={!editable || tenantOptions.length !== 1}
                    onClick={() => setNewUser({ id: '', tenant: defaultTenant })}
                  >
                    创建第一个用户
                  </button>
                }
              >
                创建后可为用户分配线路、额度和订阅入口。
              </EmptyState>
            )}
          </div>
        </section>
      ) : (
        // 双栏：左名册常驻可扫读，右详情随选中切换。名册项的结构见 rosterRow；
        // 已停用的用户在名册末尾单独成组。
        <div className={`user-split${drill.p === 'user' ? ' user-detail-route' : ''}`}>
          <section className="panel titled user-list-panel user-split-roster" data-page-title="true">
            <header>
              <ListIcon of="users" />
              <h4>用户</h4>
              <span className="user-roster-head-actions">
                <button
                  className="btn primary"
                  disabled={!editable || tenantOptions.length !== 1 || !!newUser}
                  title={tenantOptions.length !== 1 ? '当前账户无法新增用户，请联系管理员。' : undefined}
                  onClick={() => setNewUser({ id: '', tenant: defaultTenant })}
                >
                  ＋ 新增用户
                </button>
              </span>
            </header>
            <span className="user-list-search">
              <Icon of="search" size={13} className="user-list-search-icon" />
              <input
                type="search"
                value={search}
                aria-label="搜索用户"
                placeholder="搜索用户名或 UUID"
                autoComplete="off"
                spellCheck={false}
                onChange={event => setSearch(event.target.value)}
                onKeyDown={event => {
                  if (event.key === 'Escape') setSearch('');
                }}
              />
              {search && (
                <button type="button" aria-label="清空用户搜索" title="清空" onClick={() => setSearch('')}>
                  <Icon of="close" size={13} />
                </button>
              )}
            </span>
            <SegmentedControl
              className="user-roster-filter"
              ariaLabel="筛选用户"
              value={rosterFilter}
              onChange={setRosterFilter}
              options={ROSTER_FILTERS.map(option => ({
                value: option.value,
                label: (
                  <>
                    {option.label}{' '}
                    <i className={option.value === 'attention' && filterCounts.attention > 0 ? 'attention' : undefined}>
                      {filterCounts[option.value]}
                    </i>
                  </>
                ),
              }))}
            />
            <div className="user-roster-options" role="listbox" aria-label="用户列表">
              {newUserEditor}
              {activeRows.map(rosterRow)}
              {disabledRows.length > 0 &&
                (activeRows.length > 0 ? (
                  <div className="user-roster-group" role="group" aria-label="已停用">
                    <div className="user-roster-group-head" aria-hidden="true">
                      已停用<span>{disabledRows.length}</span>
                    </div>
                    {disabledRows.map(rosterRow)}
                  </div>
                ) : (
                  disabledRows.map(rosterRow)
                ))}
              {rows.length === 0 && <div className="user-search-empty">没有匹配的用户</div>}
            </div>
          </section>
          {(!narrow || drill.p === 'user') &&
            (detail ? (
              detailOf(detail)
            ) : (
              <section key="user-detail-empty" className="panel user-split-detail">
                <div className="user-detail-empty">
                  {drill.p === 'user'
                    ? '链接指向的用户不存在，或当前账号无权查看'
                    : search.trim() || rosterFilter !== 'all'
                      ? '没有匹配的用户'
                      : '从左侧选择一个用户查看详情'}
                </div>
              </section>
            ))}
        </div>
      )}
      {columns.length === 0 && list.length > 0 && (
        <p className="note" style={{ marginTop: 10 }}>
          尚无接入面，没有可授权的对象。
        </p>
      )}
    </>
  );

  return (
    <>
      <div className="cardpage user-cardpage">{body}</div>
      {accountingChange && (
        <Confirm
          title={`切换为${accountingChange.mode === 'double' ? '双向' : '单向'}计量`}
          body={
            <>
              <p>用户「{accountingChange.user.id}」本期的接入与转发用量将按新口径重新核算。</p>
              <p>
                {accountingChange.mode === 'double'
                  ? '同一业务数据在服务器接收和转发时各计一次。重算后若额度耗尽，相关线路或转发入口将自动暂停。'
                  : '同一业务数据仅计一次。重算后若额度充足，因额度耗尽而暂停的线路或转发入口将自动恢复。'}
              </p>
              {profile.error && <ErrorBox error={profile.error} />}
            </>
          }
          danger={false}
          confirmLabel={profile.isPending ? '切换中…' : '切换并重算'}
          confirmDisabled={profile.isPending}
          onConfirm={() => {
            if (profile.isPending) return;
            profile.mutate(
              {
                user: accountingChange.user,
                accountType: accountingChange.user.account_type ?? 'formal',
                trafficMode: accountingChange.mode,
              },
              { onSuccess: () => setAccountingChange(null) },
            );
          }}
          onCancel={() => setAccountingChange(null)}
        />
      )}
      {/* 订阅与节点弹窗（subscription.tsx）：地址是访问凭据，打开时才读取；「节点」页签并入网络拨测。 */}
      {sub && (
        <SubscriptionViewer
          key={`${sub.user.tenant_id}/${sub.user.id}/${sub.kind}`}
          tenant={sub.user.tenant_id}
          user={sub.user.id}
          kind={sub.kind}
          selfService={who.role === 'user' && `${sub.user.tenant_id}/${sub.user.id}` === selfKey}
          identity={<GeneratedUserAvatar id={sub.user.id} variant="mini" />}
          onClose={() => setSub(null)}
        />
      )}
      {passwordUser && (
        <UserPasswordDialog
          key={`${passwordUser.tenant_id}/${passwordUser.id}`}
          user={passwordUser}
          onClose={() => setPasswordUser(null)}
        />
      )}
      {issuedLogin && (
        <UserLoginIssuedDialog
          key={`${issuedLogin.user.tenant_id}/${issuedLogin.user.id}/${issuedLogin.value.password}`}
          user={issuedLogin.user}
          issued={issuedLogin.value}
          onClose={() => setIssuedLogin(null)}
        />
      )}
      {issuedDirectLogin && (
        <UserDirectLoginIssuedDialog
          key={`${issuedDirectLogin.user.tenant_id}/${issuedDirectLogin.user.id}`}
          user={issuedDirectLogin.user}
          issued={issuedDirectLogin.value}
          onClose={() => {
            setIssuedDirectLogin(null);
            directLogin.reset();
          }}
        />
      )}
    </>
  );
}
