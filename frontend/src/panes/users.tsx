import { Fragment, memo, useEffect, useState, type CSSProperties, type ReactNode } from 'react';
import { keepPreviousData, useMutation, useMutationState, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  createUser,
  cancelGrantProbe,
  fetchGrantProbeCapability,
  fetchGrantProbeJob,
  fetchMyUser,
  fetchQuotas,
  fetchSnapshot,
  fetchTenants,
  fetchUsageMonthly,
  fetchUserOnlineSourceHistory,
  fetchUserPresence,
  fetchUsers,
  fetchUserGrantProbePlan,
  grantProbeEventsUrl,
  issueUserDirectLogin,
  issueUserLogin,
  revokeUserDirectLogin,
  rotateMyUuid,
  rotateUserUuid,
  setQuota,
  setUserPassword,
  setUserStatus,
  startUserGrantProbe,
  updateUserProfile,
  upsertGrant,
  type SnapshotApp,
  type GrantProbeJob,
  type GrantProbeJobItem,
  type GrantProbePlanItem,
  type GrantWrite,
  type IssuedUserDirectLogin,
  type NetworkOperator,
  type UsageMonthlyViewRow,
  type UserListItem,
  type UserOnlineSource,
  type UserPresence,
} from '../api';
import { can, isVisitor, useSession } from '../session';
import { Ago, EmptyState, ErrorBox, Loading, SegmentedControl, SegSwitch } from '../ui/bits';
import { Icon, ListIcon, PanelTitle } from '../ui/icons';
import { bytes, shareOf } from '../ui/format';
import { useNodeNames } from '../ui/node-name';
import { RegionFlag } from '../ui/region-flag';
import { FieldLoading } from '../ui/loading';
import { useNow } from '../ui/clock';
import { type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { isValidSlug } from './ports';
import { SubscriptionViewer, type SubscriptionKind } from './subscription';
import { navigate } from '../forge/route';
import { CopyButton } from '../ui/copy-button';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { PASSWORD_MIN_LENGTH, passwordConfirmation } from '../ui/password-policy';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import { useNarrow } from '../ui/viewport';
import { directLoginUrl } from '../ui/login';

// 用户列表：一行一个用户，点击后就地展开。
// 此处原为授权矩阵，行是用户、列是接入面。列数随数据增长：每个接入点一列，每个线路再加
// 一列月流量，三个线路九个接入点即 13 列，打开产物栏后必然横向滚动。改为展开式后，授权项
// 以网格排列，接入点增多只会换行，不增加版面宽度。
//
// 一个授权项对应一条 grant。点击只触发 grant-sync 批次，不重启进程，不断开连接。
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
  grants: number;
  // 额度已用尽的线路。空数组不表示未设置额度：未设额度的线路和已设未超的线路
  // 都不会进入此数组。
  exhausted: string[];
  // 被配额执行器撤销的接入点数量。撤销后 `grants` 归零，仅依据 `grants` 会判定为
  // 未授权并显示空心状态，而该用户实际是被中断的。
  suspended: number;
}

export type MonthlyUsageState = 'pending' | 'failed' | 'ready';

export const monthlyUsageText = (state: MonthlyUsageState, rowCount: number, total: number) => {
  if (state === 'pending') return '读取中…';
  if (state === 'failed') return '暂不可用';
  return rowCount > 0 ? bytes(total) : '—';
};

const userTone = (f: UserFacts): UserTone =>
  f.status === 'disabled' || f.exhausted.length > 0 || f.suspended > 0 ? 'bad' : f.grants === 0 ? 'idle' : 'ok';

const userLampTitle = (f: UserFacts) => {
  const causes: string[] = [];
  if (f.status === 'disabled') causes.push('已停用：旧凭据立即失效');
  if (f.exhausted.length > 0) causes.push(`流量已用尽：${f.exhausted.join('、')}`);
  if (f.suspended > 0) causes.push(`${f.suspended} 个接入点已被系统停用`);
  if (causes.length > 0) return `无法连接 · ${causes.join('；')}`;
  return f.grants === 0 ? '未授权任何接入点，无法连接' : `可用 · ${f.grants} 个接入点`;
};

// 名册与详情标题条的两字缩写牌。仅取用户名前两位字母数字：它是身份记号，不表示位置
// （位置刻度 .lst-no 在双栏里已撤，见 UserList 的说明）。
const initialsOf = (id: string) => {
  const s = id.replace(/[^A-Za-z0-9]/g, '');
  return (s.slice(0, 2) || id.slice(0, 2) || '··').toUpperCase();
};

type UserAvatarStyle = CSSProperties & {
  '--avatar-hue': number;
};

// 同一用户名始终生成同一张抽象头像；不把随机值存在数据库，也不在每次 render 时变化。
// FNV-1a 的分布足够承担视觉种子，不承担任何安全用途。
const userAvatarStyle = (id: string): UserAvatarStyle => {
  let hash = 0x811c9dc5;
  for (const char of id) {
    hash ^= char.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return {
    '--avatar-hue': hash % 360,
  };
};

interface AvatarLamp {
  tone: '' | 'bad' | 'idle';
  title: string;
}

// 名册行（row）、详情身份栏（plate）、订阅与节点弹窗标题（mini）共用同一张身份牌。
// 名册行只在需要处理时给灯，详情身份栏始终给灯；lamp 为空时不渲染状态灯。
function GeneratedUserAvatar({
  id,
  variant = 'row',
  lamp = null,
}: {
  id: string;
  variant?: 'row' | 'plate' | 'mini';
  lamp?: AvatarLamp | null;
}) {
  const shape = variant === 'plate' ? 'plate' : variant === 'mini' ? 'user-avatar mini' : 'user-avatar';
  return (
    <span className={`${shape} user-generated-avatar`} style={userAvatarStyle(id)}>
      {initialsOf(id)}
      {lamp && (
        <i className={`node-lamp${lamp.tone ? ` ${lamp.tone}` : ''}`} title={lamp.title} aria-label={lamp.title} />
      )}
    </span>
  );
}

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
  user.status !== 'disabled' && (facts.exhausted.length > 0 || facts.suspended > 0);

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
  ['unknown', '协议未知'],
]);

function sourceProtocols(source: UserOnlineSource, nodeId: string): string {
  const accesses = source.accesses?.filter(access => access.node_id === nodeId) ?? [];
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

function PresenceNodeLink({ id, nameOf }: { id: string; nameOf: (id: string) => string }) {
  return (
    <button
      type="button"
      className="user-presence-node-link"
      title={id}
      onClick={() => navigate('nodes', { p: 'node', id })}
    >
      {nameOf(id)}
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

// 首次出现：今天只写时刻，昨天加「昨天」，更早写月-日与时刻
function firstSeenLabel(t: number, today: number): string {
  if (Number.isNaN(t)) return '—';
  const days = calendarDaysAgo(t, today);
  if (days <= 0) return clockOf(t);
  if (days === 1) return `昨天 ${clockOf(t)}`;
  return `${monthDayOf(t)} ${clockOf(t)}`;
}

const localTime = (at: string) => {
  const t = observedAt(at);
  return Number.isNaN(t) ? at : new Date(t).toLocaleString();
};

const observedTitle = (source: UserOnlineSource, online: boolean) =>
  [
    `首次出现 ${localTime(source.first_observed_at)}`,
    `${online ? '最近观测' : '最后出现'} ${localTime(source.last_observed_at)}`,
    `最近新建连接 ${localTime(source.xray_last_seen_at)}`,
  ].join('\n');

// 时间线分组：在线来源在最前，离线来源按最后出现分到今天、昨天、7 天内与保留期内。
// 7 天内与保留期内默认折叠。
type PresenceBucket = 'online' | 'today' | 'yesterday' | 'week' | 'retention';
const PRESENCE_BUCKETS: PresenceBucket[] = ['online', 'today', 'yesterday', 'week', 'retention'];
const OLDER_PRESENCE_BUCKETS = new Set<PresenceBucket>(['week', 'retention']);

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

// 时间线的一行：最后出现时刻 │ 节点 │ 来源 IP │ 网络 │ 接入（机器 + 协议）│ 首次出现。
// 在线来源不写时刻，所在分组「在线」已经表达它此刻仍在线。卡片每秒随时钟重渲染，
// 行只在数据或日期变化时重渲染。
const PresenceRow = memo(function PresenceRow({
  source,
  online,
  older,
  country,
  operator,
  nameOf,
  today,
}: {
  source: UserOnlineSource;
  online: boolean;
  older: boolean;
  country: string | undefined;
  operator: NetworkOperator | undefined;
  nameOf: (id: string) => string;
  today: number;
}) {
  const lastSeen = observedAt(source.last_observed_at);
  const network = sourceNetwork(country, operator);
  const title = observedTitle(source, online);
  const time = online ? '' : Number.isNaN(lastSeen) ? '—' : older ? monthDayOf(lastSeen) : clockOf(lastSeen);
  const past = online ? '' : '历史';
  return (
    <li className={`user-presence-row${online ? '' : ' off'}`}>
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
      </span>
      <span className="user-presence-sub">
        <span className="user-presence-location" title={SOURCE_LOCATION_TITLE}>
          {network.code && <RegionFlag code={network.code} />}
          <span>{network.label}</span>
        </span>
        <span className="user-presence-access">
          {source.node_ids.length === 0 ? (
            <span className="user-presence-unknown">接入节点未知</span>
          ) : (
            source.node_ids.map(id => (
              <span key={id} className="user-presence-access-item">
                <PresenceNodeLink id={id} nameOf={nameOf} />
                <span className="user-presence-protocols">{sourceProtocols(source, id)}</span>
              </span>
            ))
          )}
        </span>
      </span>
      <span className="user-presence-first" title={title}>
        首次 {firstSeenLabel(observedAt(source.first_observed_at), today)}
      </span>
    </li>
  );
});

const firstSeenOrder = (source: UserOnlineSource) => {
  const t = observedAt(source.first_observed_at);
  return Number.isNaN(t) ? 0 : t;
};

function UserPresenceCard({
  user,
  presence,
  countries,
  operators,
  nameOf,
  pending,
  error,
}: {
  user: UserListItem;
  presence: UserPresence | undefined;
  countries: Record<string, string> | undefined;
  operators: Record<string, NetworkOperator> | undefined;
  nameOf: (id: string) => string;
  pending: boolean;
  error: unknown;
}) {
  const [olderExpanded, setOlderExpanded] = useState(false);
  const today = new Date(useNow()).setHours(0, 0, 0, 0);
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
  const offline = (history.data?.sources ?? []).filter(source => !onlineIps.has(source.ip));
  const retentionDays = history.data?.retention_days ?? 30;
  const historyCountries = new Map(history.data?.source_countries?.map(({ ip, country }) => [ip, country]));
  const historyOperators = new Map(history.data?.source_operators?.map(({ ip, operator }) => [ip, operator]));

  const groups = new Map<PresenceBucket, UserOnlineSource[]>();
  // 在线来源按首次出现由近到远：新出现的地址排在最上面
  if (online.length) {
    groups.set(
      'online',
      [...online].sort((a, b) => firstSeenOrder(b) - firstSeenOrder(a) || a.ip.localeCompare(b.ip)),
    );
  }
  for (const source of offline) {
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
  const olderCount = [...OLDER_PRESENCE_BUCKETS].reduce((sum, bucket) => sum + (groups.get(bucket)?.length ?? 0), 0);
  // 只有更早的来源时直接展开，否则整张卡只剩一个按钮
  const olderOnly = PRESENCE_BUCKETS.every(bucket => OLDER_PRESENCE_BUCKETS.has(bucket) || !groups.has(bucket));
  const showOlder = olderExpanded || olderOnly;
  const visibleBuckets = PRESENCE_BUCKETS.filter(
    bucket => groups.has(bucket) && (showOlder || !OLDER_PRESENCE_BUCKETS.has(bucket)),
  );

  const partial = presence?.state === 'partial';
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
  const notice: { tone: 'warn' | 'idle'; text: string } | null =
    error || pending
      ? null
      : presence?.state === 'partial'
        ? {
            tone: 'warn',
            text: `当前仅收到 ${presence.reporting_nodes} / ${presence.expected_nodes} 台入口节点的最新快照，在线来源可能不完整`,
          }
        : !presence || presence.state === 'unavailable'
          ? { tone: 'warn', text: '在线来源暂不可用：Agent 尚未上报，或在线来源统计尚未启用' }
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
        <PanelTitle of="client">在线接入</PanelTitle>
        <span className="rt" title="按公网 IP 去重，不等于连接数；跨节点的同一 IP 只计一次">
          {reading}
        </span>
      </header>
      <div className="user-dcard-body">
        {!!error && <ErrorBox error={error} />}
        {!!error && presence && <span className="dim">当前显示上次成功读取的快照。</span>}
        {notice && <div className={`user-presence-note ${notice.tone}`}>{notice.text}</div>}
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
                    nameOf={nameOf}
                    today={today}
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
        {history.data?.truncated && showOlder && (
          <span className="user-presence-history-note">记录较多，仅显示最近 256 个来源 IP。</span>
        )}
        {olderCount > 0 && !olderOnly && (
          <button
            type="button"
            className="user-presence-more"
            aria-expanded={olderExpanded}
            onClick={() => setOlderExpanded(expanded => !expanded)}
          >
            {olderExpanded ? '收起更早的来源' : `显示更早的 ${olderCount} 个来源`}
          </button>
        )}
      </div>
    </section>
  );
}

// 名册右列的本月用量只保留一位小数；精确值在详情的「用量与额度」里。
const compactUsage = (total: number) => {
  const [value, unit] = bytes(total).split(' ');
  return { value: String(Number(Number(value).toFixed(1))), unit };
};

// 额度用到这一比例即进入注意档：名册从这里开始显示具体百分比，详情的额度条改用注意色。
const QUOTA_ATTENTION_PCT = 95;

// 名册不暴露低用量的精确百分比；进入注意档后才显示具体数值。
export function quotaStage(pct: number): string {
  if (pct < QUOTA_ATTENTION_PCT) return '余裕';
  return `${pct.toFixed(0)}%`;
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
const toGiB = (n: number) => Number((n / GiB).toFixed(3));
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
  editable,
  busy,
  onSave,
}: {
  app: SnapshotApp;
  used: number | null;
  limit: number | null;
  over: boolean;
  usageState?: MonthlyUsageState;
  access?: QuotaRouteAccess;
  editable: boolean;
  busy: boolean;
  onSave: (limit: number | null) => Promise<unknown>;
}) {
  const [draftValue, setDraftValue] = useState<string | null>(null);
  const editing = draftValue !== null;
  const label = app.label || app.id;
  const initialValue = limit === null ? '' : String(toGiB(limit));
  const guardScope = `quota:${app.id}`;
  const dirty = editing && draftValue !== initialValue;
  useUnsavedChanges(dirty, `${label} 的月度额度`, guardScope);
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
          title={`${[...new Set(access.suspended)].join('、')} 已被系统停用；补足额度或月初重置后自动恢复`}
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

  if (editing) {
    const value = draftValue.trim();
    const n = Number(value);
    const bad = value !== '' && (!Number.isFinite(n) || n <= 0);
    const submit = async () => {
      if (bad || busy) return;
      try {
        await onSave(value === '' ? null : Math.round(n * GiB));
        // 只有服务端确认保存后才退出编辑。失败时保留输入，方便修正或重试。
        setDraftValue(null);
      } catch {
        // mutation.error 由父组件显示；这里仅阻止 rejected promise 变成未处理异常。
      }
    };
    return (
      <div className="qta-r editing" role="listitem">
        {name}
        <div className="qta-editor">
          <label className="qta-input">
            <input
              className="f qta-in"
              autoFocus
              disabled={busy}
              value={draftValue}
              placeholder="不限"
              inputMode="decimal"
              aria-label={`${label} 的月度额度（GiB）`}
              aria-invalid={bad || undefined}
              onChange={e => setDraftValue(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Escape' && !busy && confirmDiscardChanges(guardScope)) setDraftValue(null);
                if (e.key === 'Enter' && !bad && !busy) {
                  e.preventDefault();
                  void submit();
                }
              }}
            />
            <span className="qta-u">GiB</span>
          </label>
          <span className={`qta-hint${bad ? ' bad' : ''}`}>{bad ? '请输入大于 0 的数字' : '留空表示不限'}</span>
          <span className="sp" />
          <button
            className="btn"
            disabled={busy}
            onClick={() => confirmDiscardChanges(guardScope) && setDraftValue(null)}
          >
            取消
          </button>
          <button className="btn primary" disabled={bad || busy} onClick={() => void submit()}>
            {busy ? '保存中…' : '保存'}
          </button>
        </div>
      </div>
    );
  }

  // 用到注意档（≥95%）改用注意色，用尽改用告警色：后者即名册红点的成因，两处同色。
  const tone =
    used === null
      ? 'unknown'
      : limit === null
        ? 'unlimited'
        : over
          ? 'over'
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
    <div className={`qta-r ${tone}`} role="listitem">
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
      {left !== null && <span className="qta-left">{left}</span>}
      {pct !== null && used !== null && (
        <span className="qta-meter">
          <span className="qta-t" aria-hidden="true">
            <i style={{ width: `${used > 0 ? Math.min(100, Math.max(1, pct)) : 0}%` }} />
          </span>
          <span className="qta-pct">{pct.toFixed(0)}%</span>
        </span>
      )}
      <span className="qta-acts">
        <button
          className="btn qta-pencil"
          disabled={!editable}
          aria-label={limit === null ? '设额度' : '改额度'}
          title={limit === null ? '设额度' : '改额度'}
          onClick={() => setDraftValue(limit === null ? '' : String(toGiB(limit)))}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="m3 11.8.6-2.7L10.8 2l2.2 2.2-7.1 7.1-2.9.5Z" />
            <path d="m9.7 3.1 2.2 2.2" />
          </svg>
        </button>
      </span>
    </div>
  );
}

// 详情卡的本月读数栏：与用量页读数栏同一写法（合计 + 上下行组成条与数值），尺寸按子卡缩小一档。
function UserUsageLedger({
  state,
  rows,
  monthEnd,
}: {
  state: MonthlyUsageState;
  rows: UsageMonthlyViewRow[];
  monthEnd: string | null;
}) {
  const uplink = rows.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const downlink = rows.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const total = uplink + downlink;
  const known = state === 'ready' && rows.length > 0;
  const [figure, unit] = bytes(total).split(' ');
  const upShare = total > 0 ? (uplink / total) * 100 : 0;
  return (
    <section className="user-usage-ledger" aria-label="本月用量">
      <div className="user-usage-hero">
        <span className="user-usage-hero-label">本月合计</span>
        {known ? (
          <strong className="user-usage-hero-value">
            {figure} <small>{unit}</small>
          </strong>
        ) : (
          <strong className="user-usage-hero-value none">{monthlyUsageText(state, rows.length, total)}</strong>
        )}
        {monthEnd && <QuotaReset monthEnd={monthEnd} />}
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
            <dd className="usage-share">{known ? shareOf(uplink, total) : ''}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{known ? bytes(downlink) : '—'}</dd>
            <dd className="usage-share">{known ? shareOf(downlink, total) : ''}</dd>
          </div>
        </dl>
      </div>
    </section>
  );
}

// 额度按 +08 自然月统计，到月界重新计算，被系统停用的接入点也在此时恢复。月界取服务端返回的
// month_end（+08 墙钟时刻），浏览器不另算一份；剩余天数随共享时钟更新。
function QuotaReset({ monthEnd }: { monthEnd: string }) {
  const now = useNow();
  const resetAt = Date.parse(`${monthEnd.replace(' ', 'T')}+08:00`);
  if (!Number.isFinite(resetAt) || resetAt <= now) return null;
  const month = Number(monthEnd.slice(5, 7));
  const days = Math.ceil((resetAt - now) / 86_400_000);
  return (
    <span
      className="user-usage-reset"
      title={`额度按 UTC+8 自然月统计，${month} 月 1 日 00:00 重新计算；被系统停用的接入点同时恢复`}
    >
      {month} 月 1 日重置
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
      <section className="dialog-surface user-password-card user-login-issued-card">
        <header>
          <span>
            <b>{user.direct_login_enabled ? '直达页面已重新生成' : '直达页面已生成'}</b>
            <small>完整链接和 TOKEN 只显示这一次</small>
          </span>
          <DialogClose aria-label="关闭" title="关闭">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="user-login-issued">
          <span>
            直达页面 <code>{url}</code>{' '}
            <CopyButton className="user-fcopy" text={url} label="复制直达页面" successLabel="直达页面已复制" iconOnly />
          </span>
          <span>
            UUID <code>{issued.uuid}</code> <CopyButton className="user-fcopy" text={issued.uuid} iconOnly />
          </span>
          <span>
            TOKEN <code>{issued.token}</code> <CopyButton className="user-fcopy" text={issued.token} iconOnly />
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

type GrantProbeDisplayItem = GrantProbePlanItem & Partial<Pick<GrantProbeJobItem, 'status' | 'ttfb_ms' | 'detail'>>;

const GRANT_PROBE_SLOTS = [
  { protocol: 'vless', family: 'ipv4', protocolLabel: 'VLESS', familyLabel: 'V4' },
  { protocol: 'vless', family: 'ipv6', protocolLabel: 'VLESS', familyLabel: 'V6' },
  { protocol: 'vless-encryption', family: 'ipv4', protocolLabel: 'VLESS · Encryption', familyLabel: 'V4' },
  { protocol: 'vless-encryption', family: 'ipv6', protocolLabel: 'VLESS · Encryption', familyLabel: 'V6' },
  { protocol: 'anytls', family: 'ipv4', protocolLabel: 'AnyTLS', familyLabel: 'V4' },
  { protocol: 'anytls', family: 'ipv6', protocolLabel: 'AnyTLS', familyLabel: 'V6' },
  { protocol: 'hysteria2', family: 'ipv4', protocolLabel: 'Hysteria2', familyLabel: 'V4' },
  { protocol: 'hysteria2', family: 'ipv6', protocolLabel: 'Hysteria2', familyLabel: 'V6' },
] as const;

type GrantProbeMatrixStyle = CSSProperties & { '--grant-probe-slot-count': number };

const GRANT_PROBE_POLL_MS = 1_000;
const GRANT_REFRESH_MS = 3_000;
const GRANT_REFRESH_WINDOW_MS = 60_000;
const GRANT_MUTATION_KEY = ['grant'] as const;

function grantWrite(value: unknown): GrantWrite | null {
  if (!value || typeof value !== 'object') return null;
  if (
    !('app_id' in value) ||
    typeof value.app_id !== 'string' ||
    !('tenant_id' in value) ||
    typeof value.tenant_id !== 'string' ||
    !('user_id' in value) ||
    typeof value.user_id !== 'string' ||
    !('ingress_id' in value) ||
    typeof value.ingress_id !== 'string' ||
    !('enabled' in value) ||
    typeof value.enabled !== 'boolean'
  )
    return null;
  return {
    app_id: value.app_id,
    tenant_id: value.tenant_id,
    user_id: value.user_id,
    ingress_id: value.ingress_id,
    enabled: value.enabled,
  };
}

function grantRevision(value: unknown): number | null {
  return value &&
    typeof value === 'object' &&
    'revision_id' in value &&
    typeof value.revision_id === 'number' &&
    Number.isSafeInteger(value.revision_id)
    ? value.revision_id
    : null;
}

const grantKey = (value: GrantWrite) => `${value.app_id}/${value.tenant_id}/${value.user_id}/${value.ingress_id}`;

function useGrantEdits() {
  return useMutationState({
    filters: { mutationKey: GRANT_MUTATION_KEY, exact: true },
    select: mutation => ({
      write: grantWrite(mutation.state.variables),
      revision: grantRevision(mutation.state.data),
      status: mutation.state.status,
      submittedAt: mutation.state.submittedAt,
      error: mutation.state.error,
    }),
  });
}

// Each mounted card keeps its own mutation observer (including an unconfirmed write receipt).
// A global single "busy" value lets clicking B unlock A; a refetch promise alone is also not an
// acknowledgement, because cancellation or a failed refresh can resolve without newer data.
function GrantToggle({
  write,
  confirmedRevision,
  disabled,
  className,
  title,
  refresh,
  children,
}: {
  write: GrantWrite;
  confirmedRevision: number;
  disabled: boolean;
  className: string;
  title: string;
  refresh: () => Promise<void>;
  children: ReactNode;
}) {
  const qc = useQueryClient();
  const mutation = useMutation({
    mutationKey: GRANT_MUTATION_KEY,
    mutationFn: upsertGrant,
    onSettled: (_result, _error, value) => {
      void qc.invalidateQueries({ queryKey: ['grant-probe-plan', value.tenant_id, value.user_id] });
      return refresh();
    },
  });
  const waiting = mutation.isPending || (mutation.data?.revision_id ?? 0) > confirmedRevision;
  return (
    <button
      className={className}
      title={title}
      aria-pressed={!write.enabled}
      aria-busy={waiting}
      disabled={disabled || waiting}
      onClick={() => mutation.mutate(write)}
    >
      {children}
    </button>
  );
}

const probeBaseName = (item: GrantProbeDisplayItem) => {
  const withoutFamily = item.family === 'ipv6' ? item.name.replace(/ \| v6$/, '') : item.name;
  if (item.protocol === 'hysteria2') return withoutFamily.replace(/ \| QUIC$/, '');
  if (item.protocol === 'anytls') return withoutFamily.replace(/ \| AnyTLS$/, '');
  if (item.protocol === 'vless-encryption') return withoutFamily.replace(/ \| VLESS Encryption$/, '');
  return withoutFamily;
};

const probeStatusText = (item: GrantProbeDisplayItem) => {
  switch (item.status) {
    case 'waiting':
      return '等待';
    case 'running':
      return '连接中…';
    case 'passed':
      return item.ttfb_ms === null || item.ttfb_ms === undefined ? '通过' : `通过 · ${item.ttfb_ms}ms`;
    case 'failed':
      return '失败';
    case 'canceled':
      return '已取消';
    default:
      return '';
  }
};

const probeVisibleStatusText = (item: GrantProbeDisplayItem) => {
  if (item.status === 'passed' && item.ttfb_ms !== null && item.ttfb_ms !== undefined) return `${item.ttfb_ms}ms`;
  return probeStatusText(item);
};

/**
 * 人工网络拨测。它与周期 E2E 有意分开：此处用真实用户凭据，回答“alice 现在能不能
 * 从这个授权入口走通”；周期 E2E 用专用 probe 身份，回答线路总体是否健康。
 */
export function GrantProbePanel({ user, readOnly = false }: { user: UserListItem; readOnly?: boolean }) {
  const qc = useQueryClient();
  const edits = useGrantEdits();
  const latestEdit = edits.reduce(
    (latest, edit) =>
      edit.write?.tenant_id === user.tenant_id && edit.write.user_id === user.id
        ? Math.max(latest, edit.submittedAt)
        : latest,
    0,
  );
  const capability = useQuery({
    queryKey: ['grant-probe-capability'],
    queryFn: fetchGrantProbeCapability,
    staleTime: 60_000,
    enabled: !readOnly,
  });
  const plan = useQuery({
    queryKey: ['grant-probe-plan', user.tenant_id, user.id],
    queryFn: async () => {
      try {
        return await fetchUserGrantProbePlan(user.tenant_id, user.id);
      } catch (error) {
        // Revoking the last Serving grant returns 404. Cache an explicit empty plan so an older
        // successful result (or completed probe job) cannot keep advertising a removed entry.
        if (error instanceof ApiError && error.status === 404) return null;
        throw error;
      }
    },
    // 只读访客只需要无连接材料的 Serving 计划，不读取本机 Xray 能力，更不会创建任务。
    enabled: readOnly || capability.data?.available === true,
    retry: false,
    // Head changes precede Serving. Refresh only this user's recently edited plan, and stop
    // after a bounded window even when a node is offline or an order is halted.
    refetchInterval: () =>
      latestEdit > 0 && Date.now() < latestEdit + GRANT_REFRESH_WINDOW_MS ? GRANT_REFRESH_MS : false,
  });
  const [job, setJob] = useState<GrantProbeJob | null>(null);
  const [showEncryption, setShowEncryption] = useState(false);
  const activeJobId = job?.id;
  const activeJobStatus = job?.status;

  useEffect(() => {
    if (!activeJobId || activeJobStatus !== 'running') return;
    let closed = false;
    let polling = false;
    const events = new EventSource(grantProbeEventsUrl(activeJobId), { withCredentials: true });
    const update = (event: Event) => {
      try {
        setJob(JSON.parse((event as MessageEvent<string>).data) as GrantProbeJob);
      } catch {
        /* A malformed frame is ignored; the next snapshot is complete and catches up. */
      }
    };
    const poll = () => {
      if (closed || polling) return;
      polling = true;
      void fetchGrantProbeJob(activeJobId).then(
        snapshot => {
          if (!closed) setJob(snapshot);
          polling = false;
        },
        error => {
          polling = false;
          if (closed || !(error instanceof ApiError) || error.status !== 404) return;
          // Jobs are intentionally memory-only. A Console restart forgets them; without this
          // terminal projection an already open browser would retain "running" forever.
          setJob(current => {
            if (!current || current.id !== activeJobId || current.status !== 'running') return current;
            return {
              ...current,
              status: 'canceled',
              message: '拨测任务已失效，请重新发起',
              finished_at_unix_secs: Math.floor(Date.now() / 1_000),
              items: current.items.map(item =>
                item.status === 'waiting' || item.status === 'running'
                  ? { ...item, status: 'canceled', ttfb_ms: null, detail: null }
                  : item,
              ),
            };
          });
        },
      );
    };
    events.addEventListener('snapshot', update);
    events.onerror = poll;
    // SSE is the fast path. Polling is a bounded fallback for proxies that buffer an event or a
    // browser that silently loses the stream without firing a useful error.
    const timer = window.setInterval(poll, GRANT_PROBE_POLL_MS);
    return () => {
      closed = true;
      window.clearInterval(timer);
      events.close();
    };
  }, [activeJobId, activeJobStatus]);

  const start = useMutation({
    mutationFn: (ids: string[]) => startUserGrantProbe(user.tenant_id, user.id, ids),
    onSuccess: response => setJob(response.job),
  });
  const cancel = useMutation({
    mutationFn: (id: string) => cancelGrantProbe(id),
    onSuccess: setJob,
  });

  useEffect(() => {
    if (!activeJobStatus || activeJobStatus === 'running') return;
    void qc.invalidateQueries({ queryKey: ['grant-probe-plan', user.tenant_id, user.id] });
  }, [activeJobStatus, qc, user.id, user.tenant_id]);

  // A row probe returns only the selected items.  Keep the complete frozen plan on screen and
  // project the current job state over it; otherwise every unrelated authorization disappears
  // while one row is being tested.
  const jobItems = new Map(job?.items.map(item => [item.id, item]) ?? []);
  const source: GrantProbeDisplayItem[] = (plan.data === null ? [] : (plan.data?.items ?? job?.items ?? [])).map(
    item => ({
      ...item,
      ...jobItems.get(item.id),
    }),
  );
  // 同一接入点同时提供 VLESS 与 VLESS Encryption 时，Encryption 项默认不显示，勾选后才进入
  // 矩阵和「拨测全部」。只提供 Encryption 的接入点没有其他拨测项，隐藏后该接入点会从列表中
  // 消失，因此始终显示。
  const groupKey = (item: GrantProbePlanItem) => `${item.app_id}/${item.chain_id}/${item.ingress_id}`;
  const plainGroups = new Set(source.filter(item => item.protocol !== 'vless-encryption').map(groupKey));
  const optionalEncryption = (item: GrantProbePlanItem) =>
    item.protocol === 'vless-encryption' && plainGroups.has(groupKey(item));
  const optionalEncryptionCount = source.filter(optionalEncryption).length;
  const isShown = (item: GrantProbePlanItem) => showEncryption || !optionalEncryption(item);
  const shown = source.filter(isShown);
  // 只画本次 Serving 计划中确实存在的协议 / 地址族列。固定画满所有能力会让完全没有
  // 启用 Encryption 的授权列表仍出现两个空列，读起来像“尚未验证”，而不是“没有此入口”。
  const visibleSlots = GRANT_PROBE_SLOTS.filter(slot =>
    shown.some(item => item.protocol === slot.protocol && item.family === slot.family),
  );
  const matrixStyle: GrantProbeMatrixStyle = {
    '--grant-probe-slot-count': visibleSlots.length,
  };
  const groups = new Map<string, { name: string; items: GrantProbeDisplayItem[] }>();
  for (const item of shown) {
    const key = groupKey(item);
    const group = groups.get(key) ?? {
      name: probeBaseName(item),
      items: [],
    };
    group.items.push(item);
    groups.set(key, group);
  }
  const order = (item: GrantProbeDisplayItem) =>
    (item.protocol === 'vless' ? 0 : item.protocol === 'vless-encryption' ? 2 : item.protocol === 'anytls' ? 4 : 6) +
    (item.family === 'ipv6' ? 1 : 0);
  for (const group of groups.values()) group.items.sort((a, b) => order(a) - order(b));

  const busy = job?.status === 'running' || start.isPending || cancel.isPending;
  // 进度与「重试失败项」只统计当前显示的项，隐藏的 Encryption 项不计入。
  const jobItemsShown = job?.items.filter(isShown) ?? [];
  const failedIds = jobItemsShown.filter(item => item.status === 'failed').map(item => item.id);
  const complete = jobItemsShown.filter(item => ['passed', 'failed', 'canceled'].includes(item.status)).length;
  const total = jobItemsShown.length;
  const passed = jobItemsShown.filter(item => item.status === 'passed').length;
  const failed = failedIds.length;
  const waiting = jobItemsShown.filter(item => item.status === 'waiting').length;
  const running = jobItemsShown.filter(item => item.status === 'running').length;
  // 空列表表示服务端计划中的全部项；有项被隐藏时改为显式列出显示中的项。
  const probeAllIds = shown.length < source.length ? shown.map(item => item.id) : [];
  const unavailable = !readOnly && capability.data && !capability.data.available ? capability.data.reason : null;
  const loadError = (readOnly ? null : capability.error) ?? plan.error ?? start.error ?? cancel.error;

  const initialPending = readOnly
    ? plan.isPending
    : capability.isPending || (capability.data?.available === true && plan.isPending);
  if (initialPending) return <Loading variant="users" />;

  return (
    <section className="panel config-panel user-dcard grant-probe">
      <header>
        <PanelTitle of="diag">网络拨测</PanelTitle>
        {plan.data && <span className="grant-probe-serving">Serving R{plan.data.serving_revision}</span>}
        {optionalEncryptionCount > 0 && (
          <label
            className="grant-probe-toggle"
            title={`同一接入点的 VLESS Encryption 拨测项，共 ${optionalEncryptionCount} 项`}
          >
            <input
              type="checkbox"
              checked={showEncryption}
              disabled={busy}
              onChange={event => setShowEncryption(event.target.checked)}
            />
            VLESS Encryption
          </label>
        )}
        <span className="grant-probe-actions">
          <button
            className="btn grant-probe-retry"
            disabled={readOnly || busy || failedIds.length === 0}
            onClick={() => start.mutate(failedIds)}
          >
            重试失败项
          </button>
          <button
            className={`btn${busy ? '' : ' primary'}`}
            disabled={
              readOnly ||
              start.isPending ||
              cancel.isPending ||
              !!unavailable ||
              !!capability.error ||
              (!busy && (!plan.data || plan.data.items.length === 0))
            }
            onClick={() => {
              if (job?.status === 'running') cancel.mutate(job.id);
              else start.mutate(probeAllIds);
            }}
          >
            {job?.status === 'running' ? '取消拨测' : start.isPending ? '创建中…' : '拨测全部'}
          </button>
        </span>
      </header>
      {unavailable && <div className="grant-probe-banner bad">{unavailable}</div>}
      {loadError && !unavailable && (
        <div className="grant-probe-banner warn">
          {loadError instanceof Error ? loadError.message : '暂时无法读取 Serving 授权'}
        </div>
      )}
      {job && (
        <div className="grant-probe-progress">
          <span className="grant-probe-progress-top">
            <b>{job.message || (job.status === 'running' ? '正在并发拨测' : '本轮拨测结束')}</b>
            <code>
              通过 {passed} · 运行 {running} · 等待 {waiting} · 失败 {failed}
            </code>
          </span>
          <i>
            <b style={{ width: `${total ? Math.round((complete / total) * 100) : 0}%` }} />
          </i>
        </div>
      )}
      <div className="grant-probe-list">
        {[...groups.values()].length > 0 && (
          <div className="grant-probe-table-head" aria-hidden="true">
            <span>线路 / Route</span>
            <span className="grant-probe-matrix grant-probe-matrix-head" style={matrixStyle}>
              {visibleSlots.map(slot => (
                <span key={`${slot.protocol}/${slot.family}`}>
                  {slot.protocolLabel} <i>○</i> {slot.familyLabel}
                </span>
              ))}
            </span>
            <span>拨测</span>
          </div>
        )}
        {[...groups.entries()].map(([key, group]) => {
          const ids = group.items.map(item => item.id);
          return (
            <div className="grant-probe-row" key={key}>
              <span className="grant-probe-name">
                <b>{group.name}</b>
              </span>
              <span className="grant-probe-matrix" style={matrixStyle}>
                {visibleSlots.map(slot => {
                  const item = group.items.find(
                    candidate => candidate.protocol === slot.protocol && candidate.family === slot.family,
                  );
                  const label = `${slot.protocolLabel} · ${slot.familyLabel}`;
                  if (!item) {
                    return (
                      <span className="grant-probe-gap" key={`${slot.protocol}/${slot.family}`} aria-hidden="true" />
                    );
                  }
                  const status = probeStatusText(item);
                  const visibleStatus = probeVisibleStatusText(item);
                  return (
                    <span
                      className={`grant-probe-result ${item.status ?? 'idle'}`}
                      key={item.id}
                      data-label={label}
                      data-protocol={slot.protocolLabel}
                      data-stack={slot.familyLabel}
                      title={item.detail ? `${label}：${item.detail}` : `${label}：${status || '尚未验证'}`}
                      aria-label={`${label}：${status || '尚未验证'}`}
                    >
                      <i className="grant-probe-dot" aria-hidden="true" />
                      {visibleStatus && <b>{visibleStatus}</b>}
                    </span>
                  );
                })}
              </span>
              <span className="grant-probe-row-action">
                <button
                  className="btn grant-probe-one"
                  disabled={readOnly || busy || !plan.data}
                  onClick={() => start.mutate(ids)}
                >
                  {group.items.some(item => item.status === 'failed') ? '重试' : '拨测'}
                </button>
              </span>
            </div>
          );
        })}
        {!loadError && !unavailable && source.length === 0 && (
          <div className="grant-probe-empty">没有可拨测的生效授权</div>
        )}
      </div>
      {!readOnly && (
        <footer
          className="user-dcard-foot"
          title="拨测只检查本次授权是否可用，不代替周期健康观测；每项会产生极小真实流量。"
        >
          {`使用 ${user.id} 当前生效的真实用户凭据从公网验证；流量计入该用户用量。`}
        </footer>
      )}
    </section>
  );
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
  const editable = can(who.role, 'edit');
  const canViewPresence = !isVisitor(who);
  const grantEdits = useGrantEdits();
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });
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
        : false,
  });
  /* 自然月汇总单独查询：该请求失败不影响授权操作，数字显示为 — 即可 */
  const monthly = useQuery({ queryKey: ['usage-monthly'], queryFn: () => fetchUsageMonthly() });
  // 额度是直写的运营参数，不进草稿也不随发布变化，因此与用量分开查询，
  // 也不随 refresh() 中的三个查询一起失效：修改额度只失效额度本身。
  const quotas = useQuery({ queryKey: ['quotas'], queryFn: () => fetchQuotas() });
  const monthlyState: MonthlyUsageState = monthly.data ? 'ready' : monthly.isPending ? 'pending' : 'failed';
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
  const [passwordUser, setPasswordUser] = useState<UserListItem | null>(null);
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
      if (event.key === 'Escape') close();
    };
    document.addEventListener('click', close);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [detailActionsOpen]);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
    qc.invalidateQueries({ queryKey: ['grant-automation'] });
    return qc.invalidateQueries({ queryKey: ['snapshot'] });
  };
  const profile = useMutation({
    mutationFn: (value: { user: UserListItem; accountType: 'formal' | 'test' }) =>
      updateUserProfile(value.user.tenant_id, value.user.id, { account_type: value.accountType }),
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
  // 额度直写，不进草稿：保存后立即生效，无需再到发布页操作。
  // 因此只失效 quotas 查询，不涉及 revisions。
  const quota = useMutation({
    mutationFn: (v: { user: UserListItem; app: string; limit: number | null }) =>
      setQuota({
        tenant_id: v.user.tenant_id,
        user_id: v.user.id,
        app_id: v.app,
        limit_bytes: v.limit,
      }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['quotas'] }),
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

  if (
    users.isPending ||
    snapshot.isPending ||
    quotas.isPending ||
    (editable && tenants.isPending) ||
    (who.role === 'user' && me.isPending)
  ) {
    return <Loading variant="users" sheeted={sheeted} userDetail={drill.p === 'user'} />;
  }
  if (users.error) return <ErrorBox error={users.error} />;
  if (!snapshot.data) return <ErrorBox error={snapshot.error} />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;
  // 额度缺失不能回退成“不限量”：那会把读取失败显示成一个有效、且风险相反的配置。
  if (quotas.error) return <ErrorBox error={quotas.error} />;
  if (me.error) return <ErrorBox error={me.error} />;

  const apps: SnapshotApp[] = snapshot.data.snapshot.apps ?? [];
  const latestGrantEdits = new Map(
    grantEdits.flatMap(edit => (edit.write ? [[grantKey(edit.write), edit] as const] : [])),
  );
  const unconfirmedGrantKeys = new Set(
    [...latestGrantEdits]
      .filter(([, edit]) => edit.status === 'pending' || (edit.revision ?? 0) > snapshot.data.snapshot.revision)
      .map(([key]) => key),
  );
  const needsGrantRefresh = grantEdits.some(edit => (edit.revision ?? 0) > snapshot.data.snapshot.revision);
  const grantError = [...latestGrantEdits.values()].find(edit => edit.status === 'error')?.error;
  const columns = apps.flatMap(a => a.ingresses.map(i => ({ app: a, ingress: i })));
  const granted = new Set(apps.flatMap(a => a.grants.map(g => `${g.tenant}/${g.user}/${g.ingress}`)));
  const selfKey = who.self_user ? `${who.self_user.tenant_id}/${who.self_user.user_id}` : null;
  const listedUsers = users.data.users;
  const serverUsers = me.data
    ? listedUsers.some(user => `${user.tenant_id}/${user.id}` === selfKey)
      ? listedUsers.map(user => (`${user.tenant_id}/${user.id}` === selfKey ? me.data : user))
      : [me.data, ...listedUsers]
    : listedUsers;
  const list = serverUsers;
  const canManageLogin = can(who.role, 'manage-tenants');
  /* 订阅是完整可用的配置，readonly 角色在 API 侧返回 403（见 session.tsx 的 can）。
     入口同步隐藏，避免点击后只得到一个错误。 */
  const canReadArtifacts = can(who.role, 'artifacts');
  /* 按（用户 × 线路）索引：每个线路组右侧的「本月」列读取这一行 */
  const usageByView = new Map(
    (monthly.data?.views ?? []).map((r: UsageMonthlyViewRow) => [`${r.tenant_id}/${r.user_id}/${r.app_id}`, r]),
  );

  // 单个用户的本月合计：累加其名下各线路的行。
  const usageOf = (u: UserListItem) => {
    const rows = apps
      .map(a => usageByView.get(`${u.tenant_id}/${u.id}/${a.id}`))
      .filter((r): r is UsageMonthlyViewRow => !!r);
    return {
      rows,
      total: rows.reduce((n, r) => n + r.uplink_bytes + r.downlink_bytes, 0),
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
        // 月用量是可选观测；读取失败时显示未知，绝不能把未知当成 0 后再算出“额度充足”。
        const used = monthlyState === 'ready' ? (row ? row.uplink_bytes + row.downlink_bytes : 0) : null;
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
    const facts: UserFacts = {
      status: u.status,
      grants: mine.length,
      exhausted: quotaRows.filter(q => q.over).map(q => q.app.label || q.app.id),
      suspended: suspended.size,
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

  // 右栏详情。内容与此前就地展开时完全一致（身份与操作 + 用量额度 + 接入授权，
  // 沿用列表页原有的 .lst-acts / .fgrid / .qta / .grant-cards），只是从行内移到常驻的右栏，
  // 并在顶部补一条身份标题条。
  const detailOf = (r: (typeof rows)[number]) => {
    const { u, mine, suspended, use, quotaRows, facts, tone } = r;
    const usageText = monthlyUsageText(monthlyState, use.rows.length, use.total);
    const disabled = u.status === 'disabled';
    const isMe = r.key === selfKey;
    const selfService = who.role === 'user' && isMe;
    const canOpenSubscription = canReadArtifacts || selfService;
    const headCls = disabled ? 'off' : tone === 'idle' ? 'idle' : '';
    const lampCls = tone === 'ok' ? '' : tone; // '' | 'bad' | 'idle'
    const userPresence = presenceByUser.get(r.key);
    const presenceText = userPresenceText(userPresence);
    const presenceTitle = userPresence?.sources.length
      ? userPresence.sources.map(source => source.ip).join(' · ')
      : presenceText;
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
      <section key={r.key} className="panel user-split-detail">
        <div className={`user-dhead${headCls ? ` ${headCls}` : ''}`}>
          <div className="user-dhead-main">
            <GeneratedUserAvatar id={u.id} variant="plate" lamp={{ tone: lampCls, title: userLampTitle(facts) }} />
            <div className="dtitle">
              <div className="dname">
                <b className="mono">{u.id}</b>
                {isMe && <span className="st st-ok">我</span>}
                <span className="dsub">
                  <span
                    className="dstat"
                    title={
                      mine.length > 0
                        ? `已授权 ${mine.length} 个接入点`
                        : suspended.size > 0
                          ? `${suspended.size} 个接入点已停用`
                          : '尚未授权接入点'
                    }
                    aria-label={
                      mine.length > 0
                        ? `已授权 ${mine.length} 个接入点`
                        : suspended.size > 0
                          ? `${suspended.size} 个接入点已停用`
                          : '尚未授权接入点'
                    }
                  >
                    <Icon of="chains" size={12} className="dstat-ic" />
                    <b>
                      {mine.length > 0 ? '已授权' : suspended.size > 0 ? '已停用' : '已授权'}{' '}
                      {mine.length || suspended.size}
                    </b>
                  </span>
                  <span className="dstat" title={`本月合计 ${usageText}`} aria-label={`本月合计 ${usageText}`}>
                    <Icon of="usage" size={12} className="dstat-ic" />
                    <b className="user-usage-value">{usageText}</b>
                  </span>
                  {canViewPresence && (
                    <span className="dstat" title={presenceTitle} aria-label={presenceText}>
                      <Icon of="client" size={12} className="dstat-ic" />
                      <b>{userPresence?.sources.length ?? '—'}</b>
                    </span>
                  )}
                </span>
              </div>
              {u.uuid && (
                <div className="user-duuid">
                  <span>UUID</span>
                  <code>{u.uuid}</code>
                  <CopyButton
                    className="user-fcopy user-duuid-copy"
                    text={u.uuid}
                    label="复制 UUID"
                    successLabel="UUID 已复制"
                    failureLabel="UUID 复制失败"
                    iconOnly
                  />
                </div>
              )}
            </div>
            <div className="user-dhead-actions">
              <div className="user-dacts">
                {!editable && selfService && <span className="user-readonly">自助访问</span>}
                <span className="user-account-type" title="用户资料 · 账号类型">
                  <SegSwitch
                    checked={u.account_type === 'test'}
                    disabled={!editable || profile.isPending}
                    off="正式"
                    on="测试"
                    onChange={test =>
                      profile.mutate({
                        user: u,
                        accountType: test ? 'test' : 'formal',
                      })
                    }
                  />
                </span>
                <button
                  className="btn user-dact"
                  disabled={selfService ? !u.login_enabled : !canManageLogin || !u.login_enabled}
                  title={
                    selfService
                      ? u.login_enabled
                        ? '修改自己的登录密码'
                        : '当前只开通了直达登录，尚未设置密码'
                      : u.login_enabled
                        ? '为该用户设置新的登录密码'
                        : '请先在“更多”中开通登录'
                  }
                  onClick={() => {
                    if (selfService) {
                      if (!u.login_enabled) return;
                      navigate('password');
                    } else {
                      setPasswordUser(u);
                    }
                  }}
                >
                  <Icon of="security" size={13} className="user-dact-icon" />
                  修改密码
                </button>
                <button
                  className="btn user-dact"
                  disabled={!canOpenSubscription}
                  title="打开该用户的 vless:// 节点，可选择地址族"
                  onClick={() => setSub({ user: u, kind: 'uri' })}
                >
                  <Icon of="client" size={13} className="user-dact-icon" />
                  节点
                </button>
                <button
                  className="btn user-dact"
                  disabled={!canOpenSubscription}
                  title="打开 Clash 订阅，可选择地址族"
                  onClick={() => setSub({ user: u, kind: 'clash' })}
                >
                  <Icon of="subscription" size={13} className="user-dact-icon" />
                  订阅
                </button>
                <div className="fg-menuwrap user-action-menuwrap">
                  <button
                    className="btn user-dact user-dact-more"
                    disabled={!editable && !selfService}
                    aria-label="更多操作"
                    title="更多操作"
                    aria-haspopup="menu"
                    aria-expanded={detailActionsOpen}
                    onClick={event => {
                      event.stopPropagation();
                      setDetailActionsOpen(open => !open);
                    }}
                  >
                    <Icon of="more" size={13} className="user-dact-icon" />
                  </button>
                  {detailActionsOpen && (editable || selfService) && (
                    <div className="fg-menu user-action-menu" role="menu" onClick={() => setDetailActionsOpen(false)}>
                      {canManageLogin && (
                        <button
                          role="menuitem"
                          className={u.login_enabled ? 'dg' : undefined}
                          disabled={login.isPending}
                          onClick={() => {
                            if (
                              u.login_enabled &&
                              !window.confirm(`确定重置 ${u.id} 的登录密码？现有登录会话将失效。`)
                            ) {
                              return;
                            }
                            login.mutate(u);
                          }}
                        >
                          <Icon of="access" size={14} className="user-action-menu-icon" />
                          <span>
                            {u.login_enabled ? '重置登录密码' : '开通登录'}
                            <small>
                              {u.login_enabled ? '生成一次性密码并注销现有会话' : '生成该用户的首次登录密码'}
                            </small>
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
                        disabled={rotate.isPending}
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
        </div>
        <div className="user-dbody">
          {/* 用量与额度：左侧读数栏沿用用量页写法（本月合计、重置日、上下行），右侧每条线路一行。
              合计已在读数栏，标题读数只写线路数。 */}
          <section className="panel config-panel user-dcard user-usage-card">
            <header>
              <PanelTitle of="usage">用量与额度</PanelTitle>
              <span className="rt">
                <b>{quotaRows.length}</b> 条线路
              </span>
            </header>
            <div className="user-dcard-body user-usage">
              <UserUsageLedger state={monthlyState} rows={use.rows} monthEnd={monthly.data?.month_end ?? null} />
              {quotaRows.length === 0 ? (
                <span className="dim qta-empty">尚未授权任何线路</span>
              ) : (
                <div className="qta" role="list" aria-label="各线路的用量与额度">
                  {quotaRows.map(q => (
                    <QuotaRow
                      key={q.app.id}
                      app={q.app}
                      used={q.used}
                      limit={q.limit}
                      over={q.over}
                      usageState={monthlyState}
                      access={routeAccessOf(q.app)}
                      editable={editable}
                      busy={quota.isPending}
                      onSave={limit => quota.mutateAsync({ user: u, app: q.app.id, limit })}
                    />
                  ))}
                </div>
              )}
            </div>
          </section>

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
              nameOf={nameOf}
              pending={presence.isPending}
              error={presence.error}
            />
          )}

          {/* 接入授权保留落地版：真实的授权卡（链名 + 节点:端口 + 线路水印 + ✓/⦸），
              只补一层与用量卡一致的标题条。 */}
          <section className="panel config-panel user-dcard">
            <header>
              <PanelTitle of="access">接入授权</PanelTitle>
              <span className="rt">
                <b>{mine.length}</b> / {columns.length} 已授权
              </span>
            </header>
            <div className="user-dcard-body">
              {columns.length === 0 ? (
                <span className="dim">尚无接入面，没有可授权的对象。</span>
              ) : (
                <div className="grant-cards">
                  {columns.map(({ app: a, ingress: c }) => {
                    const gk = `${u.tenant_id}/${u.id}/${c.id}`;
                    const on = granted.has(gk);
                    const held = !on && suspended.has(c.id);
                    const chain = (a.chains ?? []).find(x => x.id === c.chain);
                    const chainName = chain?.name || c.chain;
                    const appName = a.label || a.id;
                    return (
                      <GrantToggle
                        key={`${a.id}/${gk}`}
                        write={{ app_id: a.id, tenant_id: u.tenant_id, user_id: u.id, ingress_id: c.id, enabled: !on }}
                        confirmedRevision={snapshot.data.snapshot.revision}
                        refresh={refresh}
                        className={`grant-card${held ? ' held' : ''}`}
                        disabled={
                          !editable ||
                          !!snapshot.error ||
                          snapshot.fetchStatus === 'paused' ||
                          unconfirmedGrantKeys.has(`${a.id}/${gk}`)
                        }
                        title={
                          held
                            ? '流量已用尽，系统已停用。补足额度后自动恢复；此时手动授权在下一轮仍会被撤销。'
                            : `${chainName} · ${appName} · ${c.id} · ${on ? '点击取消授权' : '点击授权'}`
                        }
                      >
                        <span className="gc-head">
                          <span className="gc-app">
                            <span className="gc-name">
                              <RegionFlag code={chain?.subscription_country} />
                              <span>{chainName}</span>
                            </span>
                            <i>{c.chain}</i>
                          </span>
                          <span className="gc-mark" aria-hidden="true">
                            {on ? '✓' : held ? '⦸' : ''}
                          </span>
                        </span>
                        <span className="gc-at">{held ? '流量用尽已停用' : `${nameOf(c.node)}:${c.port}`}</span>
                        <span className="gc-wm">{appName}</span>
                      </GrantToggle>
                    );
                  })}
                </div>
              )}
            </div>
          </section>
          <GrantProbePanel key={r.key} user={u} readOnly={!canReadArtifacts && !selfService} />
        </div>
      </section>
    );
  };

  // 名册项：身份牌 │ 用户名 + 第二行 │ 右列（本月用量 / 额度）。
  // 常态不画状态灯：流量用尽与系统停用为红点，未授权为空心灰点，已停用整行降一档、不再叠灯。
  // 管理员第二行放在线来源，普通用户只看接入点数量；异常、未授权和停用状态优先于观测值，
  // 避免把诊断信息盖住权限事实。
  // 右列定宽，用量读数与额度百分比的右缘逐行对齐；额度取各线路中最接近额度的一条。
  const rosterRow = (row: (typeof allRows)[number]) => {
    const { u, key, mine, suspended, use, quotaRows, facts, tone } = row;
    const disabled = u.status === 'disabled';
    const picked = selected?.key === key;
    const lamp: AvatarLamp | null = disabled || tone === 'ok' ? null : { tone, title: userLampTitle(facts) };
    const state = disabled
      ? { text: '已停用', bad: false }
      : facts.exhausted.length > 0
        ? { text: `流量已用尽 · ${facts.exhausted.join('、')}`, bad: true }
        : suspended.size > 0
          ? { text: `${suspended.size} 个接入点已被系统停用`, bad: true }
          : mine.length === 0
            ? { text: '未授权', bad: false }
            : null;
    const usage = monthlyState === 'ready' && use.rows.length > 0 ? compactUsage(use.total) : null;
    const limited = quotaRows.filter(q => q.limit !== null && q.used !== null);
    const maxPct = limited.length
      ? Math.max(...limited.map(q => ((q.used as number) / (q.limit as number)) * 100))
      : null;
    const over = facts.exhausted.length > 0;
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
            monthlyUsageText(monthlyState, use.rows.length, use.total)
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
        <span className="user-row-quota" title={maxPct === null ? undefined : `额度 ${quotaStage(maxPct)}`}>
          {maxPct !== null && (
            <>
              <QuotaRing pct={maxPct} over={over} />
              <span className={over ? 'over' : undefined}>{quotaStage(maxPct)}</span>
            </>
          )}
        </span>
      </button>
    );
  };
  const activeRows = rows.filter(row => row.u.status !== 'disabled');
  const disabledRows = rows.filter(row => row.u.status === 'disabled');

  /* 名册与详情是两张同级面板：名册承担搜索和开户入口，详情只承担当前用户。
     避免一条跨栏标题把名册读成详情的附属筛选器。 */
  const body = (
    <>
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
        quota.error ||
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
            quota.error ??
            create.error
          }
        />
      )}
      {list.length === 0 ? (
        <section className="panel titled user-list-panel user-empty-panel">
          <header>
            <ListIcon of="users" />
            <h4>用户</h4>
            <span className="user-roster-head-actions">
              <button
                className="btn primary"
                disabled={!editable || tenantOptions.length !== 1 || !!newUser}
                title={tenantOptions.length !== 1 ? '系统归属配置必须恰好有一条' : undefined}
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
          <section className="panel titled user-list-panel user-split-roster">
            <header>
              <ListIcon of="users" />
              <h4>用户</h4>
              <span className="user-roster-head-actions">
                <button
                  className="btn primary"
                  disabled={!editable || tenantOptions.length !== 1 || !!newUser}
                  title={tenantOptions.length !== 1 ? '系统归属配置必须恰好有一条' : undefined}
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
            (selected ? (
              detailOf(selected)
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
