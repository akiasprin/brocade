import { useEffect, useState, type CSSProperties } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  createUser,
  cancelGrantProbe,
  fetchGrantProbeCapability,
  fetchGrantProbeJob,
  fetchQuotas,
  fetchSnapshot,
  fetchTenants,
  fetchUsageMonthly,
  fetchUsers,
  fetchUserGrantProbePlan,
  grantProbeEventsUrl,
  rotateUserUuid,
  setQuota,
  setUserStatus,
  startUserGrantProbe,
  upsertGrant,
  type SnapshotApp,
  type GrantProbeJob,
  type GrantProbeJobItem,
  type GrantProbePlanItem,
  type UsageMonthlyViewRow,
  type UserListItem,
} from '../api';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { Icon, ListIcon } from '../ui/icons';
import { bytes } from '../ui/format';
import { useNodeNames } from '../ui/node-name';
import { RegionFlag } from '../ui/region-flag';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { isValidSlug } from './ports';
import { SubscriptionViewer, type SubscriptionKind } from './subscription';

/* 服务端返回的月界是 `2026-08-01 00:00:00` 形式的 +08 本地时间，此处转换为面板标题用的年月 */
const monthLabel = (s: string) => `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月`;

// 用户列表：一行一个用户，点击后就地展开。
// 此处原为授权矩阵，行是用户、列是接入面。列数随数据增长：每个接入点一列，每个线路再加
// 一列月流量，三个线路九个接入点即 13 列，打开产物栏后必然横向滚动。改为展开式后，授权项
// 以网格排列，接入点增多只会换行，不增加版面宽度。
//
// 一个授权项对应一条 grant。点击只触发 grant-sync 批次，不重启进程，不断开连接。
//
// 开户与「机器」面的纳管采用同一结构：列表页只放一个按钮，表单通过下钻显示为独立 sheet。
// 若把表单放进列表顶部的 toolbar，字段增多后无处容纳；且开户与纳管同属一次性操作，
// 不应常驻占用列表首屏。

type Drill = { p: 'list' } | { p: 'new' } | { p: 'user'; tenant: string; id: string };

// 行左侧的状态条。与机器面共用同一组件和同一套判读规则（styles.css 的 .lst-bar），
// 但表示的状态不同：机器面表示 agent 是否仍在拉取配置，此处表示该用户当前能否连接。
//
// 红色有两种成因，结果都是无法连接：用户被停用，或某个线路流量用尽。状态条不区分二者，
// 逐行扫视时需要的是定位到异常用户；具体成因写在第二行和展开后的额度栏里。
//
// 空心表示已开户但未授权任何接入点。同样无法连接，但成因是尚未授权而非被中断，
// 与机器面「从未上报 ≠ 掉线」属同一类区分，因此沿用同一记号。
type UserTone = 'ok' | 'bad' | 'idle';

interface UserFacts {
  status: string;
  grants: number;
  // 额度已用尽的线路。空数组不表示未设置额度：未设额度的线路和已设未超的线路
  // 都不会进入此数组。
  exhausted: string[];
  // 被配额执行器撤销的接入点数量。撤销后 `grants` 归零，仅依据 `grants` 会判定为
  // 未授权并显示空心状态，而该用户实际是被中断的。
  suspended: number;
}

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

function GeneratedUserAvatar({
  id,
  detail = false,
  lampClass,
  lampTitle,
}: {
  id: string;
  detail?: boolean;
  lampClass: string;
  lampTitle: string;
}) {
  return (
    <span className={`${detail ? 'plate' : 'user-avatar'} user-generated-avatar`} style={userAvatarStyle(id)}>
      {initialsOf(id)}
      <i className={`node-lamp${lampClass ? ` ${lampClass}` : ''}`} title={lampTitle} aria-label={lampTitle} />
    </span>
  );
}

export const userMatchesSearch = (user: UserListItem, rawQuery: string) => {
  const query = rawQuery.trim().toLocaleLowerCase();
  if (!query) return true;
  return [user.id, user.tenant_id, user.uuid]
    .filter((value): value is string => !!value)
    .some(value => value.toLocaleLowerCase().includes(query));
};

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「用户」）由外壳补全。 */
const crumbOf = (d: Drill): CrumbSeg[] =>
  d.p === 'new' ? [{ label: '开户' }] : d.p === 'user' ? [{ label: d.id }] : [];

export function UsersPane({ win, bare = false }: { win: Win; bare?: boolean }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => wm.setData(win.id, { ...win.data, drill: d });
  useCrumb(win, crumbOf(drill));

  if (drill.p === 'new') {
    const body = <NewUser go={go} />;
    return bare ? <div className="fg-sheet">{body}</div> : body;
  }
  return <UserList drill={drill} go={go} sheeted={bare} />;
}

/* 开户表单。UUID 不在此填写也不显示：它由 store 生成，浏览器不接触凭据。 */
function NewUser({ go }: { go: (d: Drill) => void }) {
  const { who } = useSession();
  const qc = useQueryClient();
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });

  const [id, setId] = useState('');
  const [tenant, setTenant] = useState('');

  const options = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  /* 归属租户取默认值：操作者绑定了子树时用该子树，否则取排序后的第一个。与纳管向导一致。 */
  const defaultTenant = options.find(t => t.id === who.tenant_scope)?.id ?? options[0]?.id ?? '';
  const tenantId = tenant || defaultTenant;

  const create = useMutation({
    mutationFn: () => createUser({ tenant_id: tenantId, id: id.trim() }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['users'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      go({ p: 'user', tenant: tenantId, id: trimmed });
    },
  });

  // 服务端会拒绝（required_slug / ensure_user_missing），但那是一次往返之后返回的错误文本。
  // 这两项校验用已有数据即可在本地完成，无需先提交一次。
  // user.dup 只在单个租户内查重：同名不同租户是允许的。
  const trimmed = id.trim();
  const badSlug = trimmed && !isValidSlug(trimmed) ? 'id 只能用 [a-z0-9._-]，最长 32。' : null;
  const dup = (users.data?.users ?? []).some(u => u.tenant_id === tenantId && u.id === trimmed)
    ? `${tenantId} 里已经有 ${trimmed} 了。`
    : null;
  const editable = can(who.role, 'edit');
  const ready = !!trimmed && !!tenantId && !badSlug && !dup && editable;

  return (
    <>
      <header>
        <h4>开户</h4>
      </header>

      <form
        onSubmit={e => {
          e.preventDefault();
          if (ready) create.mutate();
        }}
      >
        <dl className="kv form2">
          <dt>用户 ID</dt>
          <dd>
            <input className="f" value={id} placeholder="alice" autoFocus onChange={e => setId(e.target.value)} />
            {badSlug ? (
              <div className="note" style={{ color: 'var(--warn)' }}>
                {badSlug}
              </div>
            ) : dup ? (
              <div className="note" style={{ color: 'var(--warn)' }}>
                {dup}
              </div>
            ) : (
              <div className="note">
                将拼入 email（
                <span className="mono">
                  {trimmed || 'alice'}@{tenantId || '租户'}#接入面
                </span>
                ）。这是 XRAY 用于权限与数据统计的内部对象，创建后不应修改。
              </div>
            )}
          </dd>
          <dt>归属租户</dt>
          <dd>
            {options.length > 1 ? (
              <select className="f" value={tenantId} onChange={e => setTenant(e.target.value)}>
                {options.map(t => (
                  <option key={t.id} value={t.id}>
                    {t.id}
                  </option>
                ))}
              </select>
            ) : (
              <span className="mono">{defaultTenant || <span className="dim">（尚无租户）</span>}</span>
            )}
            <div className="note">email 使用完整租户路径，不使用叶子名。</div>
          </dd>
        </dl>

        {options.length === 0 && <div className="callout warn">尚无租户。请先在「租户」页创建。</div>}
        {create.error && <ErrorBox error={create.error} />}

        <div className="toolbar">
          <button className="btn" type="button" onClick={() => go({ p: 'list' })}>
            取消
          </button>
          <span className="sp" />
          <button className="btn primary" type="submit" disabled={!ready || create.isPending}>
            {create.isPending ? '开户中…' : '开户'}
          </button>
        </div>
      </form>
    </>
  );
}

// 额度以 GiB 为单位收发：操作者设定额度时使用的单位是 GiB，不是字节数。
// 整数 GiB 的往返转换是精确的；通过其他 API 设置的非整值在此会被截断到 KB 级，
// 这类额度不应在此输入框中修改。
const GiB = 1024 ** 3;
const toGiB = (n: number) => Number((n / GiB).toFixed(3));

// 一个线路一个指标列，显示已用量、额度和剩余量。
// 进度条仅在设置了额度时显示：未设额度时不存在用尽的概念，显示一条空槽会被误读为
// 用量为零，而实际含义是不限量。
function QuotaRow({
  app,
  used,
  limit,
  over,
  editable,
  busy,
  onSave,
}: {
  app: SnapshotApp;
  used: number;
  limit: number | null;
  over: boolean;
  editable: boolean;
  busy: boolean;
  onSave: (limit: number | null) => void;
}) {
  const [draftValue, setDraftValue] = useState<string | null>(null);
  const editing = draftValue !== null;
  const pct = limit ? Math.min(100, (used / limit) * 100) : 0;

  if (editing) {
    const n = Number(draftValue.trim());
    const bad = draftValue.trim() !== '' && (!Number.isFinite(n) || n <= 0);
    return (
      <div className="qta-r">
        <span className="qta-head">
          <span className="qta-app">
            {app.label || app.id}
            <i>{app.id}</i>
          </span>
        </span>
        <input
          className="f qta-in"
          autoFocus
          value={draftValue}
          placeholder="留空 = 不限"
          onChange={e => setDraftValue(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Escape') setDraftValue(null);
            if (e.key === 'Enter' && !bad) {
              onSave(draftValue.trim() === '' ? null : Math.round(n * GiB));
              setDraftValue(null);
            }
          }}
        />
        <span className="qta-acts">
          <span className="qta-u">GiB</span>
          <span className="sp" />
          <button className="btn" onClick={() => setDraftValue(null)}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={bad || busy}
            onClick={() => {
              onSave(draftValue.trim() === '' ? null : Math.round(n * GiB));
              setDraftValue(null);
            }}
          >
            保存
          </button>
        </span>
      </div>
    );
  }

  return (
    <div className={`qta-r${over ? ' over' : ''}`}>
      <span className="qta-head">
        <span className="qta-app">
          {app.label || app.id}
          <i>{app.id}</i>
        </span>
      </span>
      <span className="qta-measures">
        <small>本月已用</small>
        <strong>{bytes(used)}</strong>
        <span className="qta-cap">{limit === null ? '不限额度' : `/ ${bytes(limit)}`}</span>
      </span>
      {limit !== null && (
        <span className="qta-t" title={`${pct.toFixed(0)}%`}>
          <i style={{ width: `${used > 0 ? Math.max(1, pct) : 0}%` }} />
        </span>
      )}
      <span className={`qta-meta${over ? ' over' : ''}${limit === null ? ' unlimited' : ''}`}>
        {limit === null ? (
          '未设置月度额度'
        ) : over ? (
          <>
            <span>额度已用尽</span>
            <span>超出 {bytes(Math.max(0, used - limit))}</span>
          </>
        ) : (
          <>
            <span>{pct.toFixed(0)}%</span>
            <span>剩余 {bytes(limit - used)}</span>
          </>
        )}
      </span>
      <span className="qta-acts">
        <span className="sp" />
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

// 复制按钮：写入剪贴板并短暂显示对勾。剪贴板不可用（非安全上下文）时静默失败。
function CopyButton({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className={`user-fcopy${done ? ' done' : ''}`}
      aria-label={done ? '已复制' : '复制'}
      title={done ? '已复制' : '复制'}
      onClick={() => {
        try {
          void navigator.clipboard.writeText(text).then(
            () => {
              setDone(true);
              setTimeout(() => setDone(false), 1200);
            },
            () => {},
          );
        } catch {
          /* 剪贴板不可用时静默 */
        }
      }}
    >
      {done ? (
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="m3.5 8.5 3 3 6-7" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      ) : (
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <rect x="5" y="5" width="8" height="8" rx="1.5" />
          <path d="M11 5V3.5A1.5 1.5 0 0 0 9.5 2h-6A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5" />
        </svg>
      )}
    </button>
  );
}

type GrantProbeDisplayItem = GrantProbePlanItem & Partial<Pick<GrantProbeJobItem, 'status' | 'ttfb_ms' | 'detail'>>;

const GRANT_PROBE_SLOTS = [
  { protocol: 'vless', family: 'ipv4', protocolLabel: 'VLESS', familyLabel: 'V4' },
  { protocol: 'vless', family: 'ipv6', protocolLabel: 'VLESS', familyLabel: 'V6' },
  { protocol: 'anytls', family: 'ipv4', protocolLabel: 'AnyTLS', familyLabel: 'V4' },
  { protocol: 'anytls', family: 'ipv6', protocolLabel: 'AnyTLS', familyLabel: 'V6' },
  { protocol: 'hysteria2', family: 'ipv4', protocolLabel: 'Hysteria2', familyLabel: 'V4' },
  { protocol: 'hysteria2', family: 'ipv6', protocolLabel: 'Hysteria2', familyLabel: 'V6' },
] as const;

const GRANT_PROBE_POLL_MS = 1_000;

const probeBaseName = (item: GrantProbeDisplayItem) => {
  const withoutFamily = item.family === 'ipv6' ? item.name.replace(/ \| v6$/, '') : item.name;
  if (item.protocol === 'hysteria2') return withoutFamily.replace(/ \| QUIC$/, '');
  if (item.protocol === 'anytls') return withoutFamily.replace(/ \| AnyTLS$/, '');
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
 * 人工授权验证。它与周期 E2E 有意分开：此处用真实用户凭据，回答“alice 现在能不能
 * 从这个授权入口走通”；周期 E2E 用专用 probe 身份，回答线路总体是否健康。
 */
export function GrantProbePanel({ user, readOnly = false }: { user: UserListItem; readOnly?: boolean }) {
  const qc = useQueryClient();
  const capability = useQuery({
    queryKey: ['grant-probe-capability'],
    queryFn: fetchGrantProbeCapability,
    staleTime: 60_000,
    enabled: !readOnly,
  });
  const plan = useQuery({
    queryKey: ['grant-probe-plan', user.tenant_id, user.id],
    queryFn: () => fetchUserGrantProbePlan(user.tenant_id, user.id),
    // 只读访客只需要无连接材料的 Serving 计划，不读取本机 Xray 能力，更不会创建任务。
    enabled: readOnly || capability.data?.available === true,
    retry: false,
  });
  const [job, setJob] = useState<GrantProbeJob | null>(null);
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
  const source: GrantProbeDisplayItem[] = (plan.data?.items ?? job?.items ?? []).map(item => ({
    ...item,
    ...jobItems.get(item.id),
  }));
  const groups = new Map<string, { name: string; items: GrantProbeDisplayItem[] }>();
  for (const item of source) {
    const key = `${item.app_id}/${item.chain_id}/${item.ingress_id}`;
    const group = groups.get(key) ?? {
      name: probeBaseName(item),
      items: [],
    };
    group.items.push(item);
    groups.set(key, group);
  }
  const order = (item: GrantProbeDisplayItem) =>
    (item.protocol === 'vless' ? 0 : item.protocol === 'anytls' ? 2 : 4) + (item.family === 'ipv6' ? 1 : 0);
  for (const group of groups.values()) group.items.sort((a, b) => order(a) - order(b));

  const busy = job?.status === 'running' || start.isPending || cancel.isPending;
  const failedIds = job?.items.filter(item => item.status === 'failed').map(item => item.id) ?? [];
  const complete = job?.items.filter(item => ['passed', 'failed', 'canceled'].includes(item.status)).length ?? 0;
  const total = job?.items.length ?? plan.data?.items.length ?? 0;
  const passed = job?.items.filter(item => item.status === 'passed').length ?? 0;
  const failed = failedIds.length;
  const waiting = job?.items.filter(item => item.status === 'waiting').length ?? 0;
  const running = job?.items.filter(item => item.status === 'running').length ?? 0;
  const unavailable = !readOnly && capability.data && !capability.data.available ? capability.data.reason : null;
  const loadError = (readOnly ? null : capability.error) ?? plan.error ?? start.error ?? cancel.error;

  return (
    <section className="panel config-panel user-dcard grant-probe">
      <header>
        <h4>授权验证</h4>
        {plan.data && <span className="grant-probe-serving">Serving R{plan.data.serving_revision}</span>}
        <span className="grant-probe-actions">
          <button
            className="btn ghost grant-probe-retry"
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
              else start.mutate([]);
            }}
          >
            {job?.status === 'running' ? '取消拨测' : start.isPending ? '创建中…' : '拨测全部'}
          </button>
        </span>
      </header>
      <div
        className="grant-probe-note"
        title="结果只验证本次授权是否可用，不代替周期健康观测；每项会产生极小真实流量。"
      >
        {readOnly ? (
          <>
            只读查看 <b>{user.id} 当前生效的 Serving 授权</b>；登录具备操作权限的账号后可发起验证。
          </>
        ) : (
          <>
            使用 <b>{user.id} 当前生效的真实用户凭据</b> 从公网验证；流量计入该用户用量。
          </>
        )}
      </div>
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
        {[...groups.entries()].map(([key, group]) => {
          const ids = group.items.map(item => item.id);
          return (
            <div className="grant-probe-row" key={key}>
              <span className="grant-probe-name">
                <b>{group.name}</b>
              </span>
              <span className="grant-probe-matrix">
                {GRANT_PROBE_SLOTS.map(slot => {
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
          <div className="grant-probe-empty">{plan.isPending ? '正在读取 Serving 授权…' : '没有可验证的生效授权'}</div>
        )}
      </div>
    </section>
  );
}

function UserList({ drill, go, sheeted = false }: { drill: Drill; go: (d: Drill) => void; sheeted?: boolean }) {
  const nameOf = useNodeNames();
  const { who } = useSession();
  const qc = useQueryClient();
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  /* 自然月汇总单独查询：该请求失败不影响授权操作，数字显示为 — 即可 */
  const monthly = useQuery({ queryKey: ['usage-monthly'], queryFn: () => fetchUsageMonthly() });
  // 额度是直写的运营参数，不进草稿也不随发布变化，因此与用量分开查询，
  // 也不随 refresh() 中的三个查询一起失效：修改额度只失效额度本身。
  const quotas = useQuery({ queryKey: ['quotas'], queryFn: () => fetchQuotas() });

  const [busy, setBusy] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  /* 当前查看的订阅（用户 + 格式）。null 表示未打开。同时只显示一份，与上面的展开策略一致。 */
  const [sub, setSub] = useState<{ user: UserListItem; kind: SubscriptionKind } | null>(null);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
    qc.invalidateQueries({ queryKey: ['grant-automation'] });
  };
  const grant = useMutation({
    mutationFn: (v: { app: string; user: UserListItem; ingress: string; enabled: boolean }) =>
      upsertGrant({
        app_id: v.app,
        tenant_id: v.user.tenant_id,
        user_id: v.user.id,
        ingress_id: v.ingress,
        enabled: v.enabled,
      }),
    onSettled: () => {
      setBusy(null);
      refresh();
    },
  });
  const status = useMutation({
    mutationFn: (v: { user: UserListItem; next: 'active' | 'disabled' }) =>
      setUserStatus(v.user.tenant_id, v.user.id, v.next),
    onSuccess: refresh,
  });
  const rotate = useMutation({
    mutationFn: (u: UserListItem) => rotateUserUuid(u.tenant_id, u.id),
    onSuccess: refresh,
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

  if (users.isPending || snapshot.isPending) return <Loading sheeted={sheeted} />;
  if (users.error) return <ErrorBox error={users.error} />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;

  const apps: SnapshotApp[] = snapshot.data.snapshot.apps ?? [];
  const columns = apps.flatMap(a => a.ingresses.map(i => ({ app: a, ingress: i })));
  const granted = new Set(apps.flatMap(a => a.grants.map(g => `${g.tenant}/${g.user}/${g.ingress}`)));
  const list = users.data.users;
  const editable = can(who.role, 'edit');
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
        const used = row ? row.uplink_bytes + row.downlink_bytes : 0;
        const limit = quotaByView.get(key) ?? null;
        return { app: a, used, limit, over: limit !== null && used >= limit };
      });
  };

  // 一次算出每行需要的派生量：标题栏的读数是全表汇总，行内又各自使用，
  // 两处分别计算会出现不一致。
  // 已停用的排到末尾，同档内保持服务端给出的顺序（sort 是稳定的）。排序只依据操作者
  // 设置的状态：流量用尽同样显示红色，但那是随用量变化并会自行恢复的状态，
  // 用它排序会导致列表顺序随用量变动。
  const ordered = [...list].sort((a, b) => Number(a.status === 'disabled') - Number(b.status === 'disabled'));
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
  const rows = allRows.filter(row => userMatchesSearch(row.u, search));
  const okCount = allRows.filter(r => r.tone === 'ok').length;
  const idleCount = allRows.filter(r => r.tone === 'idle').length;
  // 标题栏分别统计「已停用」和「流量用尽」：两者都显示为红色，但处置方式不同——
  // 前者由操作者设置，后者由用量触发。合并统计后必须逐行查看才能判断该做什么。
  const exhaustedCount = allRows.filter(r => r.facts.exhausted.length > 0).length;
  const disabledCount = allRows.filter(r => r.u.status === 'disabled').length;

  // URL 是显式选择的唯一来源，使刷新、分享链接和浏览器前进/后退落到同一用户。根地址仍
  // 默认预览当前筛选的首行；深链接失效时不回落到别人，避免地址写 alice 却展示 bob。
  const routedKey = drill.p === 'user' ? `${drill.tenant}/${drill.id}` : null;
  const selected = routedKey ? allRows.find(r => r.key === routedKey) : rows[0];

  // 右栏详情。内容与此前就地展开时完全一致（身份与操作 + 用量额度 + 接入授权，
  // 沿用列表页原有的 .lst-acts / .fgrid / .qta / .grant-cards），只是从行内移到常驻的右栏，
  // 并在顶部补一条身份标题条。
  const detailOf = (r: (typeof rows)[number]) => {
    const { u, mine, suspended, use, quotaRows, facts, tone } = r;
    const disabled = u.status === 'disabled';
    const headCls = disabled ? 'off' : tone === 'idle' ? 'idle' : '';
    const lampCls = tone === 'ok' ? '' : tone; // '' | 'bad' | 'idle'
    return (
      <div className="user-split-detail">
        <div className={`user-dhead${headCls ? ` ${headCls}` : ''}`}>
          <GeneratedUserAvatar id={u.id} detail lampClass={lampCls} lampTitle={userLampTitle(facts)} />
          <div className="dtitle">
            <div className="dname">
              <b className="mono">{u.id}</b>
              <span className="dsub">
                <span
                  className="dstat"
                  title={
                    mine.length > 0
                      ? `${mine.length} 个接入点`
                      : suspended.size > 0
                        ? `${suspended.size} 个接入点已停用`
                        : '尚未授权接入点'
                  }
                  aria-label={
                    mine.length > 0
                      ? `${mine.length} 个接入点`
                      : suspended.size > 0
                        ? `${suspended.size} 个接入点已停用`
                        : '尚未授权接入点'
                  }
                >
                  <Icon of="chains" size={12} className="dstat-ic" />
                  <b>{mine.length || suspended.size}</b>
                </span>
                <span
                  className="dstat"
                  title={`本月合计 ${use.rows.length > 0 ? bytes(use.total) : '—'}`}
                  aria-label={`本月合计 ${use.rows.length > 0 ? bytes(use.total) : '—'}`}
                >
                  <Icon of="usage" size={12} className="dstat-ic" />
                  <b>{use.rows.length > 0 ? bytes(use.total) : '—'}</b>
                </span>
              </span>
            </div>
            {u.uuid && (
              <div className="user-duuid">
                <span>UUID</span>
                <code>{u.uuid}</code>
                <CopyButton text={u.uuid} />
              </div>
            )}
          </div>
          <div className="user-dacts">
            <button
              className="btn"
              disabled={!editable || status.isPending}
              onClick={() => status.mutate({ user: u, next: disabled ? 'active' : 'disabled' })}
            >
              {disabled ? '启用' : '停用'}
            </button>
            <button
              className="btn"
              disabled={!canReadArtifacts}
              title="打开该用户的 vless:// 链接，可选择地址族"
              onClick={() => setSub({ user: u, kind: 'uri' })}
            >
              VLESS
            </button>
            <button
              className="btn"
              disabled={!canReadArtifacts}
              title="Clash 格式的订阅，可选择地址族"
              onClick={() => setSub({ user: u, kind: 'clash' })}
            >
              Clash
            </button>
            <button
              className="btn danger"
              disabled={!editable || rotate.isPending}
              title="当前连接将断开，订阅需要重新导入"
              onClick={() => rotate.mutate(u)}
            >
              换 UUID
            </button>
          </div>
        </div>
        <div className="user-dbody">
          <section className="panel config-panel user-dcard">
            <header>
              <h4>用量与额度</h4>
              <span className="rt">
                本月合计 <b>{use.rows.length > 0 ? bytes(use.total) : '—'}</b> · {quotaRows.length} 条线路
              </span>
            </header>
            <div className="user-dcard-body">
              {quotaRows.length === 0 ? (
                <span className="dim">
                  {monthly.isPending ? '…' : monthly.error ? '本月流量加载失败' : '尚未授权任何线路'}
                </span>
              ) : (
                <div className="qta">
                  {quotaRows.map(q => (
                    <QuotaRow
                      key={q.app.id}
                      app={q.app}
                      used={q.used}
                      limit={q.limit}
                      over={q.over}
                      editable={editable}
                      busy={quota.isPending}
                      onSave={limit => quota.mutate({ user: u, app: q.app.id, limit })}
                    />
                  ))}
                </div>
              )}
            </div>
            <div className="user-dcard-foot">
              额度保存后<b>立即生效</b>，无需发布；留空表示不限。
            </div>
          </section>

          {/* 接入授权保留落地版：真实的授权卡（链名 + 节点:端口 + 线路水印 + ✓/⦸），
              只补一层与用量卡一致的标题条。 */}
          <section className="panel config-panel user-dcard">
            <header>
              <h4>接入授权</h4>
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
                      <button
                        key={`${a.id}/${c.id}`}
                        className={`grant-card${held ? ' held' : ''}`}
                        aria-pressed={on}
                        disabled={!editable || busy === gk}
                        title={
                          held
                            ? '流量已用尽，系统已停用。补足额度后自动恢复；此时手动授权在下一轮仍会被撤销。'
                            : `${chainName} · ${appName} · ${c.id} · ${on ? '点击取消授权' : '点击授权'}`
                        }
                        onClick={() => {
                          setBusy(gk);
                          grant.mutate({ app: a.id, user: u, ingress: c.id, enabled: !on });
                        }}
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
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          </section>
          <GrantProbePanel key={r.key} user={u} readOnly={!canReadArtifacts} />
        </div>
      </div>
    );
  };

  /* 标题栏到页脚说明的整块，两种外壳共用一份。
     铺纸时套 `.cardpage > .panel.titled`，与机器页、线路页同一层结构（见下方 return）；
     窗口模式仍是一张 `.panel`。此前铺纸时用的是裸 `.fg-sheet > header`：同一个台面上
     切到用户页，标题会从 13px 无衬线的色带标题变成 15px 等宽大写的浮动标题。 */
  const body = (
    <>
      {/* 标题栏与机器面结构相同：标题 + 计数 + 一组读数 + 一个按钮。
          读数在正常状态下全部为灰色，只有「已停用」显示红色。 */}
      <header>
        <ListIcon of="users" />
        <h4>用户</h4>
        <span className="hint">
          {search.trim() ? `${rows.length} / ${list.length}` : list.length} 个
          {monthly.data ? ` · ${monthLabel(monthly.data.month_start)}` : ''}
        </span>
        <span className="rd">
          <b>{okCount}</b> 可用
          {disabledCount > 0 && (
            <>
              {' '}
              · <i>{disabledCount}</i> 已停用
            </>
          )}
          {exhaustedCount > 0 && (
            <>
              {' '}
              · <i>{exhaustedCount}</i> 流量已用尽
            </>
          )}
          {idleCount > 0 && ` · ${idleCount} 未授权`}
        </span>
        <button className="btn primary" disabled={!editable} onClick={() => go({ p: 'new' })}>
          ＋ 开户
        </button>
      </header>
      {(grant.error || status.error || rotate.error) && (
        <ErrorBox error={grant.error ?? status.error ?? rotate.error} />
      )}
      {list.length === 0 ? (
        <Empty>尚无用户。点击「开户」创建。</Empty>
      ) : (
        // 双栏：左名册常驻可扫读，右详情随选中切换。名册项第一行放用户名与接入点数量，
        // 第二行写明状态；身份徽标的悬停提示提供完整原因。
        <div className="user-split">
          <div className="user-split-roster">
            <span className="user-list-search">
              <Icon of="search" size={13} className="user-list-search-icon" />
              <input
                type="search"
                value={search}
                aria-label="搜索用户"
                placeholder="搜索用户"
                autoComplete="off"
                spellCheck={false}
                onChange={event => setSearch(event.target.value)}
                onKeyDown={event => {
                  if (event.key === 'Escape') setSearch('');
                }}
              />
              {search && (
                <button type="button" aria-label="清空用户搜索" title="清空" onClick={() => setSearch('')}>
                  ×
                </button>
              )}
            </span>
            <div className="user-roster-options" role="listbox" aria-label="用户列表">
              {rows.map(row => {
                const { u, key, mine, suspended, use, quotaRows, facts, tone } = row;
                const disabled = u.status === 'disabled';
                const picked = selected?.key === key;
                const lampCls = tone === 'ok' ? '' : tone; // '' | 'bad' | 'idle'
                const accessCount = mine.length || suspended.size;
                const accessTitle =
                  mine.length > 0
                    ? `${mine.length} 个接入点`
                    : suspended.size > 0
                      ? `${suspended.size} 个接入点已停用`
                      : '尚未授权接入点';
                const stateLine = disabled
                  ? '已停用'
                  : facts.exhausted.length > 0
                    ? '流量已用尽'
                    : suspended.size > 0
                      ? '部分接入点已停用'
                      : mine.length > 0
                        ? '可用'
                        : '未授权';
                // 细条取各线路中最接近额度的一条；未设置额度时不显示。
                const limited = quotaRows.filter(q => q.limit !== null);
                const maxPct = limited.length
                  ? Math.min(100, Math.max(...limited.map(q => (q.used / (q.limit as number)) * 100)))
                  : null;
                return (
                  <button
                    key={key}
                    type="button"
                    role="option"
                    aria-selected={picked}
                    className={`user-row${disabled ? ' off' : ''}${picked ? ' picked' : ''}`}
                    onClick={() => go({ p: 'user', tenant: u.tenant_id, id: u.id })}
                  >
                    <GeneratedUserAvatar id={u.id} lampClass={lampCls} lampTitle={userLampTitle(facts)} />
                    <span className="rbody">
                      <span className="r1">
                        <b>{u.id}</b>
                      </span>
                      <span className="r2">
                        <span className={`rstate${lampCls ? ` ${lampCls}` : ''}`}>{stateLine}</span>
                        <span aria-hidden="true">·</span>
                        <span className="raccess" title={accessTitle} aria-label={accessTitle}>
                          接入面 {accessCount}
                        </span>
                      </span>
                    </span>
                    <span className="rtail">
                      <b className={use.rows.length > 0 ? '' : 'none'}>
                        {use.rows.length > 0 ? bytes(use.total) : '—'}
                      </b>
                      {maxPct !== null && (
                        <span className="rmeter" title={`额度使用 ${maxPct.toFixed(0)}%`}>
                          <i
                            className={facts.exhausted.length > 0 ? 'over' : ''}
                            style={{ width: `${Math.max(2, maxPct)}%` }}
                          />
                        </span>
                      )}
                    </span>
                  </button>
                );
              })}
              {rows.length === 0 && <div className="user-search-empty">没有匹配的用户</div>}
            </div>
          </div>
          {selected ? (
            detailOf(selected)
          ) : (
            <div className="user-split-detail">
              <div className="user-detail-empty">
                {routedKey
                  ? '链接指向的用户不存在，或当前账号无权查看'
                  : search.trim()
                    ? '没有匹配的用户'
                    : '从左侧选择一个用户查看详情'}
              </div>
            </div>
          )}
        </div>
      )}
      {columns.length === 0 && list.length > 0 && (
        <p className="note" style={{ marginTop: 10 }}>
          尚无接入面，没有可授权的对象。
        </p>
      )}
      <p className="note" style={{ marginTop: 10 }}>
        权限保存后<b>自动推送</b>，无需另建变更单。同步名单不断开连接、不重启进程。停用或更换 UUID 后旧凭据立即失效。
      </p>
    </>
  );

  return (
    <>
      {sheeted ? (
        <div className="cardpage user-cardpage">
          <section className="panel titled user-list-panel">{body}</section>
        </div>
      ) : (
        <div className="panel">{body}</div>
      )}
      {sub && (
        <SubscriptionViewer
          key={`${sub.user.tenant_id}/${sub.user.id}/${sub.kind}`}
          tenant={sub.user.tenant_id}
          user={sub.user.id}
          kind={sub.kind}
          onClose={() => setSub(null)}
        />
      )}
    </>
  );
}
