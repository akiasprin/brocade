import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createUser,
  fetchQuotas,
  fetchSnapshot,
  fetchTenants,
  fetchUsageMonthly,
  fetchUsers,
  rotateUserUuid,
  setQuota,
  setUserStatus,
  upsertGrant,
  type SnapshotApp,
  type UsageMonthlyViewRow,
  type UserListItem,
} from '../api';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { ListIcon } from '../ui/icons';
import { bytes } from '../ui/format';
import { useNodeNames } from '../ui/node-name';
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

type Drill = { p: 'list' } | { p: 'new' };

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

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「用户」）由外壳补全。 */
const crumbOf = (d: Drill): CrumbSeg[] => (d.p === 'new' ? [{ label: '开户' }] : []);

export function UsersPane({ win, bare = false }: { win: Win; bare?: boolean }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => wm.setData(win.id, { ...win.data, drill: d });
  useCrumb(win, crumbOf(drill));

  if (drill.p === 'new') {
    const body = <NewUser go={go} />;
    return bare ? <div className="fg-sheet">{body}</div> : body;
  }
  return <UserList go={go} sheeted={bare} />;
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
      go({ p: 'list' });
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

// 一个线路一行，显示已用量、额度和剩余量。
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
      <span className="qta-n">
        {bytes(used)}
        {limit === null ? <em> / 不限</em> : <em> / {bytes(limit)}</em>}
      </span>
      {limit !== null && (
        <span className="qta-t" title={`${pct.toFixed(0)}%`}>
          <i style={{ width: `${Math.max(1, pct)}%` }} />
        </span>
      )}
      <span className="qta-acts">
        <span className="sp" />
        <button
          className="btn"
          disabled={!editable}
          onClick={() => setDraftValue(limit === null ? '' : String(toGiB(limit)))}
        >
          {limit === null ? '设额度' : '改额度'}
        </button>
      </span>
    </div>
  );
}

function UserList({ go, sheeted = false }: { go: (d: Drill) => void; sheeted?: boolean }) {
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
  // 同时只展开一个用户。展开多个会使列表超出一屏需要滚动，而这块内容的使用方式是
  // 定位到某个用户、修改、收起。
  const [openUser, setOpenUser] = useState<string | null>(null);
  /* 当前查看的订阅（用户 + 格式）。null 表示未打开。同时只显示一份，与上面的展开策略一致。 */
  const [sub, setSub] = useState<{ user: UserListItem; kind: SubscriptionKind } | null>(null);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
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

  // 单个用户的本月合计：累加其名下各线路的行。任一线路采集存在缺口时，
  // 合计值也标记为有缺口——「只小不大」对合计同样成立。
  const usageOf = (u: UserListItem) => {
    const rows = apps
      .map(a => usageByView.get(`${u.tenant_id}/${u.id}/${a.id}`))
      .filter((r): r is UsageMonthlyViewRow => !!r);
    return {
      rows,
      total: rows.reduce((n, r) => n + r.uplink_bytes + r.downlink_bytes, 0),
      gap: rows.some(r => r.has_gap),
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
  const rows = ordered.map(u => {
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
  const okCount = rows.filter(r => r.tone === 'ok').length;
  const idleCount = rows.filter(r => r.tone === 'idle').length;
  // 标题栏分别统计「已停用」和「流量用尽」：两者都显示为红色，但处置方式不同——
  // 前者由操作者设置，后者由用量触发。合并统计后必须逐行查看才能判断该做什么。
  const exhaustedCount = rows.filter(r => r.facts.exhausted.length > 0).length;
  const disabledCount = rows.filter(r => r.u.status === 'disabled').length;

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
          {list.length} 个{monthly.data ? ` · ${monthLabel(monthly.data.month_start)}` : ''}
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
        <div className="lst-fold">
          {rows.map(({ u, key, mine, suspended, use, quotaRows, facts, tone }, i) => {
            const open = openUser === key;
            const disabled = u.status === 'disabled';
            const toggle = () => setOpenUser(open ? null : key);
            return (
              <div className={`lst-fd${open ? ' open' : ''}`} key={key}>
                <div
                  role="button"
                  aria-expanded={open}
                  tabIndex={0}
                  className={`lst-row${disabled ? ' off' : ''}`}
                  onClick={toggle}
                  onKeyDown={e => {
                    if (e.target !== e.currentTarget) return;
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      toggle();
                    }
                  }}
                >
                  {/* 序号 + 状态条，与机器面位置和结构一致。
                        序号表示列表位置而非身份（排序变化时序号随之变化），仅作刻度使用。
                        不设展开箭头：展开时行本身会高亮（.lst-fd.open），箭头是冗余标识。 */}
                  <span className="lst-gut">
                    <span className="lst-no">{String(i + 1).padStart(2, '0')}</span>
                    <i
                      className={`lst-bar${tone === 'ok' ? '' : ` ${tone}`}`}
                      title={userLampTitle(facts)}
                      aria-label={userLampTitle(facts)}
                    />
                  </span>
                  <span className="body">
                    {/* 用户没有描述字段，主位显示 slug（等宽），租户以小字跟在其后。
                          此处不再标注「已停用」：状态条已表示该状态，成因写在第二行。 */}
                    <span className="lst-l1">
                      <b className="mono">{u.id}</b>
                      <span className="id">{u.tenant_id}</span>
                    </span>
                    <span className="lst-l2">
                      {/* 授权被系统撤销后 mine 归零。此时显示「未授权」与事实不符：
                            该用户是被中断的，不是从未配置。 */}
                      <span>
                        {mine.length > 0
                          ? `${mine.length} 个接入点`
                          : suspended.size > 0
                            ? `${suspended.size} 个接入点已停用`
                            : '未授权'}
                      </span>
                      {disabled && <span className="bad">已停用</span>}
                      {facts.exhausted.length > 0 && (
                        <span className="bad" title={`额度用尽的线路：${facts.exhausted.join('、')}`}>
                          流量已用尽
                        </span>
                      )}
                      {use.gap && (
                        <span className="st st-warn" title="本月至少一次采集存在缺口，实际值不小于显示值">
                          ⚠ 采集有缺口
                        </span>
                      )}
                    </span>
                  </span>
                  <span className="lst-tail">
                    {use.rows.length > 0 ? bytes(use.total) : '—'}
                    <small>本月</small>
                  </span>
                </div>

                {open && (
                  <div className="lst-body">
                    {/* 这一块的第一行集中放置该用户的全部操作 */}
                    <div className="lst-acts">
                      <button
                        className="btn"
                        disabled={!editable || status.isPending}
                        onClick={() => status.mutate({ user: u, next: disabled ? 'active' : 'disabled' })}
                      >
                        {disabled ? '启用' : '停用'}
                      </button>
                      {/* 内容显示在覆盖层，不切换右侧产物栏：该栏为单例，打开第二个用户会
                            覆盖第一个。覆盖层内可按地址族过滤。 */}
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
                      <span className="sp" />
                      <button
                        className="btn danger"
                        disabled={!editable || rotate.isPending}
                        title="当前连接将断开，订阅需要重新导入"
                        onClick={() => rotate.mutate(u)}
                      >
                        换 UUID
                      </button>
                    </div>

                    <dl className="fgrid one">
                      {u.uuid && (
                        <div className="row">
                          <span className="k">UUID</span>
                          <span className="v mono">{u.uuid}</span>
                        </div>
                      )}
                      <div className="row">
                        <span className="k">email</span>
                        <span className="v mono">
                          {u.id}@{u.tenant_id}#接入面
                          <span className="sub">XRAY 用于权限与数据统计的内部对象，此处仅作展示</span>
                        </span>
                      </div>
                      {/* 一个线路一张卡，与下方的授权卡结构相同。
                            标题不用「本月」：月份已写在标题栏中，这一块表示用量和额度。
                            已用量和额度是同一个比值的分子与分母，因此放在同一张卡内。 */}
                      <div className="row">
                        <span className="k">用量</span>
                        <span className="v">
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
                          <span className="sub">
                            合计 {bytes(use.total)}
                            {use.gap && ' · ⚠ 采集存在缺口，实际值不小于该数值'}
                            {' · 额度立即生效，无需发布'}
                          </span>
                        </span>
                      </div>
                      <div className="row">
                        <span className="k">授权</span>
                        <span className="v">
                          {columns.length === 0 ? (
                            <span className="dim">尚无接入面，没有可授权的对象。</span>
                          ) : (
                            // 一个接入点一张卡，整卡可点击。一条链对应一个接入面，卡与链
                            // 一一对应，标题使用链名：操作者记住的是链名，不是 `app-hk-01.i2`。
                            // 链名下方补一行链 id：`chains.name` 没有唯一约束，两个线路各有
                            // 一条同名链时仅看标题无法区分。线路名以水印形式放在右下角表示
                            // 归属，不加 aria-hidden，否则读屏会丢失这层信息。
                            <div className="grant-cards">
                              {columns.map(({ app: a, ingress: c }) => {
                                const gk = `${u.tenant_id}/${u.id}/${c.id}`;
                                const on = granted.has(gk);
                                // 被系统撤销的与从未授权的必须区分：前者是流量用尽后被停用，
                                // 补足额度后会自动恢复；手动重新授权无效，下一轮仍会被撤销，
                                // 因此需要先说明这一点。
                                const held = !on && suspended.has(c.id);
                                // 链名可能为空串（建链时未填写），此时回退到链 id：
                                // 它至少能与链页面对应上。
                                const chainName = (a.chains ?? []).find(x => x.id === c.chain)?.name || c.chain;
                                const appName = a.label || a.id;
                                return (
                                  <button
                                    key={`${a.id}/${c.id}`}
                                    className={`grant-card${held ? ' held' : ''}`}
                                    aria-pressed={on}
                                    disabled={!editable || busy === gk}
                                    // 196px 的卡片放不下完整的链名和线路名时截断。整卡的 title
                                    // 带有全名和接入面 id，被截断的部分可在其中查看。
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
                                        {chainName}
                                        <i>{c.chain}</i>
                                      </span>
                                      <span className="gc-mark" aria-hidden="true">
                                        {on ? '✓' : held ? '⦸' : ''}
                                      </span>
                                    </span>
                                    <span className="gc-at">
                                      {held ? '流量用尽已停用' : `${nameOf(c.node)}:${c.port}`}
                                    </span>
                                    <span className="gc-wm">{appName}</span>
                                  </button>
                                );
                              })}
                            </div>
                          )}
                        </span>
                      </div>
                    </dl>
                  </div>
                )}
              </div>
            );
          })}
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
