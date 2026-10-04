// 编译台外壳：一条顶栏、一块工作区，以及贯通到顶的产物栏。
// 页面自身负责对象下钻；顶栏负责主导航与诊断入口。

import {
  Fragment,
  lazy,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchActiveDeployments,
  fetchGrantAutomationStatus,
  fetchNodes,
  fetchRevisions,
  fetchSnapshot,
  verifyDeployment,
  type AdminRole,
  type BrandingSettings,
  type DiagNames,
  type Diagnostic,
  type Whoami,
  visibleDiagnostics,
} from '../api';
import { Pane } from '../panes';
import { ErrorBox, Loading, LoadingBoundary } from '../ui/bits';
import { useNarrow } from '../ui/viewport';
import { artifactPanel } from '../ui/artifact-panel';
import { DiagTable } from '../ui/diag-table';
import { WinLayer } from '../ui/windows';
import { wm, type CrumbSeg } from '../wm/store';
import { draft } from '../draft';
import { DraftBar } from './draft-bar';
import { ArtifactRail, blastRadius, useChangedArtifacts } from './artifacts';
import { navigate, returnTo, startRouting, type Loc } from './route';
import { can, isPublic, isVisitor } from '../session';
import { forge, useForge, type NavKey } from './state';
import { theme } from './theme';
import { palette, PALETTES } from './palette';
import { Icon, type IconName } from '../ui/icons';
import { BrandIcon } from '../ui/branding';
import { compactGrantAutomation, RuntimeCrumbStatus, type RuntimeCrumbState } from '../ui/grant-automation';
import { motionOriginFor, runVisualTransition } from '../ui/motion';
import { usePresence } from '../ui/presence';
import { confirmDiscardChanges } from '../ui/navigation-guard';
import { FleetTrafficMeter } from '../ui/fleet-traffic';

const TopoCanvas = lazy(() => import('../topo/canvas').then(module => ({ default: module.TopoCanvas })));

interface Face {
  key: NavKey;
  label: string;
  /* 顶栏和账户牌菜单共用：菜单里的图标用于快速区分导航与账号操作。 */
  icon?: IconName;
  /* 默认对所有角色可见。按角色隐藏入口属于体验优化，不构成安全边界。 */
  roles?: AdminRole[];
}

// 顶栏只显示当前主工作流。规则负责选择转发目标；隧道资源本身的生命周期统一在隧道页管理。
const NAV: Face[] = [
  { key: 'nodes', label: '机器', icon: 'nodes' },
  { key: 'tunnels', label: '隧道', icon: 'tunnels' },
  { key: 'chains', label: '线路', icon: 'chains' },
  { key: 'users', label: '用户', icon: 'users' },
  { key: 'deploy', label: '发布', icon: 'deploy', roles: ['editor', 'publisher', 'tenant-admin', 'system-admin'] },
  { key: 'usage', label: '用量', icon: 'usage' },
];

/* 手机端只把三项高频配置入口留在顶栏，腾出的宽度用于恢复按钮文字。隧道、发布和用量仍
   使用同一份 Face 定义，只是移动到账户牌菜单，避免两套角色权限和名称逐渐分叉。 */
const MOBILE_NAV = NAV.filter(f => f.key === 'nodes' || f.key === 'chains' || f.key === 'users');
const MOBILE_MORE = NAV.filter(f => f.key === 'tunnels' || f.key === 'deploy' || f.key === 'usage');

// 收入账户牌菜单的项：都是低频访问的页面，占用顶栏位置的收益较低。
// 窄屏同理：该行只放 NAV 的主工作流，这些低频页面仍从账户牌菜单进入。
const MORE: Face[] = [
  { key: 'settings', label: '设置', icon: 'settings', roles: ['editor', 'publisher', 'tenant-admin', 'system-admin'] },
  { key: 'topo', label: '拓扑', icon: 'topology' },
];

/* 不进入页面列表，但需要标题：面包屑和窗口名都读取 LABEL。 */
const OFF_NAV: Face[] = [{ key: 'password', label: '改密码' }];

const LABEL: Record<NavKey, string> = Object.fromEntries(
  [...NAV, ...MORE, ...OFF_NAV].map(f => [f.key, f.label]),
) as Record<NavKey, string>;

const ROLE_LABEL: Record<AdminRole, string> = {
  user: '用户',
  readonly: '只读',
  editor: '编辑',
  publisher: '发布',
  'tenant-admin': '租户管理员',
  'system-admin': '系统管理员',
};

/** 顶栏账户牌与菜单头显示的身份。公开访客没有账户名；单租户阶段不显示租户范围。 */
function accountOf(who: Whoami) {
  if (isPublic(who)) return { name: '访客', role: '未登录', initial: '访', guest: true };
  const name = who.self_user?.user_id ?? who.operator_id;
  return { name, role: ROLE_LABEL[who.role], initial: Array.from(name)[0]?.toUpperCase() ?? '?', guest: false };
}

const visible = (faces: Face[], who: Whoami) => faces.filter(f => !f.roles || f.roles.includes(who.role));

export function ForgeShell({
  branding,
  session,
  onLogout,
}: {
  branding: BrandingSettings;
  session: { who: Whoami };
  onLogout: () => void;
}) {
  const st = useForge();
  const narrow = useNarrow();
  /* 评审角色无法获取产物（服务端返回 403），入口一并隐藏 */
  const artifacts = can(session.who.role, 'artifacts');
  /* 公开访客无权访问发布相关的接口。不关闭这些查询时，顶栏会每 5 秒产生一次 403，
     而它们提供的读数（待发布机器数、发布中状态）在该视角下不显示。 */
  const pub = isVisitor(session.who);
  const nav = st.nav;
  const panel = useSyncExternalStore(artifactPanel.subscribe, artifactPanel.snapshot);
  const railPresence = usePresence(panel.open && artifacts, 300);
  // 草稿版本进入该层的查询键，同时统一失效所有页面的读取缓存。
  // 仅依靠查询键不够：各页面使用 `['snapshot']`，键中不含草稿版本，草稿变化后
  // 它们仍会命中缓存——表现为修改后草稿条已出现但表格内容未更新。
  // 订阅集中在此而非分散在各写入点：草稿可能从任意位置写入（api.ts 中的写函数、
  // 连线操作、冒烟脚本），失效逻辑只应有一处。
  const draftVer = useSyncExternalStore(draft.subscribe, draft.version);
  const draftDirty = !draft.isEmpty();
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        qc.invalidateQueries({ queryKey: ['snapshot'] });
        qc.invalidateQueries({ queryKey: ['compile'] });
        // Submission receipts also arrive from other tabs. Settings must stop using their old
        // committed baseline when such a receipt removes the pending settings operation.
        qc.invalidateQueries({ queryKey: ['settings'] });
      }),
    [qc],
  );
  useSyncExternalStore(theme.subscribe, theme.snapshot);

  // 页面的窗内导航状态（列表 ↔ 详情 ↔ 向导）仍保存在 wm 中，只是不再渲染窗框。
  // init 必须在子组件挂载前完成，因此使用 useState 的惰性初值而非 useEffect：effect
  // 的执行顺序是子组件先于父组件，工作区（子组件）会先打开 `tab:nodes`，随后此处（父组件）
  // 清空 wins，两次 emit 又被 React 合并为同一次重渲染——窗口已打开这一中间状态不会被观察到，
  // 工作区不会重新打开，界面停留在读取中。init 是幂等的（见 wm/store.ts），
  // 在 StrictMode 下重复执行安全。
  useState(() => {
    wm.init(session.who.operator_id);
    /* 草稿同样按操作者分键恢复：刷新页面不应丢失未提交的编辑内容。 */
    draft.init(session.who.operator_id);
    // 路由恢复需要排在 init 之后：init 会清空 wins，先恢复地址会导致恢复出的窗口被清除。
    // 两者都对重复调用免疫，在 StrictMode 下重复执行安全。
    startRouting(nav => LABEL[nav]);
    return true;
  });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled: !pub });
  const current = revisions.data?.current_revision;
  // 上一个版本：修订列表按 id 倒序排列，取当前记录的下一条。第一个版本没有上一版，
  // 此时不显示影响范围——将全部产物标记为已变更不提供信息。
  const prev = useMemo(() => {
    const ids = (revisions.data?.revisions ?? []).map(r => r.id).sort((a, b) => b - a);
    const i = ids.indexOf(current ?? -1);
    return i >= 0 ? ids[i + 1] : undefined;
  }, [revisions.data, current]);

  // 存在草稿时编译的是草稿。角标显示「0 错」而草稿中存在无法通过校验的改动时，
  // 会导致据此执行发布。
  const compile = useQuery({
    queryKey: ['compile', current, draftVer],
    queryFn: () => fetchCompileView(current!),
    enabled: !pub && current != null,
  });
  const snapshot = useQuery({
    queryKey: ['snapshot', draftVer],
    queryFn: () => fetchSnapshot(),
    enabled: !pub && (st.diag || railPresence.present),
  });
  const verify = useQuery({
    queryKey: ['deployment-verify', current],
    queryFn: () => verifyDeployment({ revision_id: current! }),
    enabled: !pub && current != null && draft.isEmpty(),
    refetchInterval: q => ((q.state.data?.summary.changed_targets ?? 0) > 0 ? 5_000 : false),
  });
  // 顶栏只关心仍占用 single-flight 锁的发布。与历史页分开缓存，避免每个页面每五秒
  // 下载并聚合 50 条历史；所有发布 mutation 对 ['deployments'] 的前缀失效会同时刷新两者。
  const deployments = useQuery({
    queryKey: ['deployments', 'runtime'],
    queryFn: () => fetchActiveDeployments(),
    enabled: !pub,
    refetchInterval: query => (query.state.data?.deployments.some(deployment => deployment.active) ? 5_000 : 30_000),
  });
  const activeDeploy = (deployments.data?.deployments ?? []).find(d => d.active);
  // 等待确认与执行中需要区分：含破坏性动作的波需要人工确认后才继续下发，而顶栏两种情况
  // 都只显示一个红色角标，会导致持续等待一个不会自动继续的操作。
  const awaitingDeploy = (deployments.data?.deployments ?? []).find(d => d.awaiting_confirmation);
  // 权限任务在生成 deployment 之前就可能失败，因此不能从发布列表推断这层状态。
  // 放在全局面包屑后，每个页面都能看到同一份队列读数。
  const grantAutomation = useQuery({
    queryKey: ['grant-automation'],
    queryFn: () => fetchGrantAutomationStatus(),
    enabled: !pub,
    refetchInterval: query => ((query.state.data?.pending_jobs ?? 0) > 0 ? 5_000 : 30_000),
  });

  const {
    list,
    changed,
    dirty,
    pending: artifactsPending,
    error: artifactsError,
  } = useChangedArtifacts(current, prev, { enabled: artifacts && draftDirty });
  const draftBlast = useMemo(() => blastRadius(list, changed), [list, changed]);
  const pendingTargets = dirty ? undefined : verify.data?.summary.changed_targets;
  const diagnostics = visibleDiagnostics(compile.data?.diagnostics);
  // 诊断的 location 中全部是 id，而面板需要显示名称。这两份数据在其他位置已在读取，
  // 此处只是将 id 到名称的转换集中处理（见 formatLocation）。
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), enabled: !pub && st.diag });
  const diagNames = useMemo<DiagNames>(() => {
    const nodes = new Map((nodeList.data?.nodes ?? []).map(n => [n.node_id, n.name]));
    const chains = new Map(
      (snapshot.data?.snapshot.apps ?? []).flatMap(a => a.chains.map(c => [c.id, c.name] as const)),
    );
    return { node: id => nodes.get(id), chain: id => chains.get(id) };
  }, [nodeList.data, snapshot.data]);

  const grantRuntime = compactGrantAutomation(grantAutomation.data, grantAutomation.isPending, !!grantAutomation.error);
  // 面包屑只留一段需要关注的短状态；健康且空闲时不显示常驻文案。
  // 修订号只跟随活动发布出现，避免把当前修订误读成需要处理的状态。
  const runtimeReadError = revisions.error ?? compile.error;
  const crumbRuntime: RuntimeCrumbState = runtimeReadError
    ? { text: revisions.error ? '修订状态未知' : '编译状态未知', tone: 'bad' }
    : dirty
      ? artifactsError
        ? { text: '草稿 · 影响范围未知', tone: 'bad' }
        : artifactsPending
          ? { text: '草稿 · 计算影响中', tone: 'normal' }
          : {
              text: draftBlast.size ? `草稿 · ${draftBlast.size} 台` : '草稿 · 无产物变更',
              tone: draftBlast.size ? 'hot' : 'normal',
              title: draftBlast.size ? [...draftBlast].join(', ') : undefined,
            }
      : awaitingDeploy
        ? { text: `发布 #${awaitingDeploy.id} · 待确认`, tone: 'bad' }
        : activeDeploy
          ? { text: `发布 #${activeDeploy.id} · 进行中`, tone: 'hot' }
          : verify.isPending
            ? { text: '检查中', tone: 'normal' }
            : verify.error
              ? { text: '发布状态未知', tone: 'bad' }
              : grantRuntime.tone === 'bad'
                ? grantRuntime
                : pendingTargets
                  ? { text: `待发布 ${pendingTargets} 台`, tone: 'hot' }
                  : grantRuntime;

  return (
    <div className={`forge${narrow ? ' narrow' : ''}`}>
      <div className="fg-left">
        <div className={`fg-desk${nav === 'topo' ? ' is-topo' : ''}`}>
          <TopBar
            branding={branding}
            who={session.who}
            narrow={narrow}
            nav={nav}
            summary={compile.data?.summary}
            diagnostics={diagnostics}
            diagNames={diagNames}
            diagnosticsPending={revisions.isPending || (current != null && compile.isPending)}
            diagnosticsError={runtimeReadError}
            pendingTargets={pendingTargets}
            activeDeploy={activeDeploy}
            awaitingDeploy={awaitingDeploy}
            railOpen={panel.open}
            onLogout={() => confirmDiscardChanges() && onLogout()}
          />

          {/* 手机端由主导航和浏览器历史承担定位，不再重复显示面包屑。 */}
          {!narrow && (
            <div className="fg-crumb">
              <ForgeCrumb nav={nav} />
              <span className="fg-crumb-right">
                <FleetTrafficMeter />
                {/* 公开访客无权读取发布状态和修订号，实时流量仍按节点读取权限展示。 */}
                {!pub && <RuntimeCrumbStatus state={crumbRuntime} revision={activeDeploy ? current : undefined} />}
              </span>
            </div>
          )}

          <DraftBar current={current} />

          {nav === 'topo' ? (
            <div className="fg-topo" key="topo" role="main" aria-label={`${LABEL[nav]}内容`} tabIndex={-1}>
              <LoadingBoundary fallback={<Loading variant="canvas" />} variant="canvas">
                <TopoCanvas />
              </LoadingBoundary>
            </div>
          ) : (
            <div className="fg-view" key={nav} role="main" aria-label={`${LABEL[nav]}内容`} tabIndex={-1}>
              <Work nav={nav} />
            </div>
          )}
        </div>
      </div>

      {railPresence.present && (
        <ArtifactRail
          revision={current}
          prev={prev}
          compareClean={(pendingTargets ?? 0) > 0}
          apps={snapshot.data?.snapshot.apps ?? []}
          onClose={() => artifactPanel.close()}
          motionState={railPresence.phase}
        />
      )}

      {/* 该层只渲染非 tab 的窗口（当前即顶栏的诊断窗）——功能页面本身也是 wm 中的窗口，
          一并渲染会在页面上重复显示相同内容。
          拓扑的检视此前也经由此处，现已改为台面右侧的常驻栏：浮窗会遮挡刚点击的图形区域，
          且每点击一个对象就增加一个窗口。 */}
      {nav === 'topo' && <WinLayer filter={w => !w.key.startsWith('tab:')} render={win => <Pane win={win} />} />}
    </div>
  );
}

// 主导航的按钮在宽、窄屏共用同一份 DOM。容器 class 只负责布局：宽屏是 fg-nav，
// 窄屏是可横向滚动的 fg-facebar；按钮本身始终由 fg-nv 定义，避免两套视觉再次分叉。
//
// 此处此前是屏幕底部的固定 tab bar，与顶栏两行合计占用 140px（占 844 屏高的 16.6%）。
// 两层都移到顶部后为 86px，底部空间留给草稿条。
//
// 只放 NAV 的主工作流，与宽屏顶栏一致。按钮直接复用宽屏的 fg-nv，不在这里维护
// 第二套尺寸、图标和选中态；窄屏的差异只有容器允许横向滚动。
// 曾尝试将 MORE 的四项也加入（带横向滚动），结果是右侧部分始终不可见——与收进菜单
// 的效果相同，且会产生该处有更多内容的错误预期。
function MainNav({
  className,
  faces,
  who,
  nav,
  pendingTargets,
  activeDeploy,
  awaitingDeploy,
}: {
  className: 'fg-nav' | 'fg-facebar';
  faces: Face[];
  who: Whoami;
  nav: NavKey;
  pendingTargets: number | undefined;
  activeDeploy: { id: number } | undefined;
  awaitingDeploy: { id: number } | undefined;
}) {
  return (
    <nav className={className} aria-label="主导航">
      {visible(faces, who).map(f => (
        <button
          key={f.key}
          className="fg-nv"
          aria-current={nav === f.key ? 'true' : 'false'}
          aria-label={f.label}
          title={f.label}
          onClick={() => navigate(f.key)}
        >
          {f.icon && <Icon of={f.icon} size={14} className="fg-nv-ic" />}
          <span className="fg-nv-label">{f.label}</span>
          {f.key === 'deploy' && activeDeploy && (
            <span
              className={`fg-badge ${awaitingDeploy ? 'chg' : 'err'}`}
              title={
                awaitingDeploy
                  ? `发布 #${awaitingDeploy.id} 的破坏性波次在等人确认——不点它不会自己往下发`
                  : `发布 #${activeDeploy.id} 进行中`
              }
            >
              {awaitingDeploy ? '等确认' : `#${activeDeploy.id}`}
            </span>
          )}
          {f.key === 'deploy' && !activeDeploy && (pendingTargets ?? 0) > 0 && (
            <span className="fg-badge chg">{pendingTargets}</span>
          )}
        </button>
      ))}
    </nav>
  );
}

/* 窄屏没有面包屑。此前进入下级后会多出一行「‹ 返回 + 当前层级名称」（.fg-backrow），
   现已移除：窄屏一共只有 44px 的一行头部，再加一行 40px 是为了一个动作和一个已经写在
   内容区标题里的名字。退出下级有两条现成的路——点导航行里高亮的那一格（navigate 不带
   drill 即回到该页首页，见 route.ts），以及浏览器的后退（每次下钻都 pushState）。 */

/* ══ 顶栏 ══ */

/** Logo 与字标是同一个品牌入口。navigate 不带 drill 会清掉机器详情层级并回到机器列表。 */
function BrandHome({ branding }: { branding: BrandingSettings }) {
  return (
    <button
      className="fg-home"
      type="button"
      aria-label="返回机器首页"
      title="返回机器首页"
      onClick={() => navigate('nodes')}
    >
      <BrandIcon branding={branding} />
      <span className="fg-brand">{branding.site_name}</span>
    </button>
  );
}

function TopBar({
  branding,
  who,
  narrow,
  nav,
  summary,
  diagnostics,
  diagNames,
  diagnosticsPending,
  diagnosticsError,
  pendingTargets,
  activeDeploy,
  awaitingDeploy,
  railOpen,
  onLogout,
}: {
  branding: BrandingSettings;
  who: Whoami;
  /* 窄屏使用另一套结构：品牌与页面导航并入一行，不是压缩桌面顶栏。 */
  narrow: boolean;
  nav: NavKey;
  summary: { errors: number; warnings: number; infos: number; can_publish: boolean } | undefined;
  diagnostics: Diagnostic[];
  /* 用于将 location 中的 id 转换为名称。查找不到时回退到 id，见 formatLocation。 */
  diagNames: DiagNames;
  diagnosticsPending: boolean;
  diagnosticsError: unknown;
  pendingTargets: number | undefined;
  /* 存在 active 的发布（限流锁被占用，含 halted 未收尾）时显示红色状态 */
  activeDeploy: { id: number } | undefined;
  // 含破坏性动作的波次停在等待确认状态的那条发布。它同时存在于 activeDeploy 中，
  // 此处只是改变角标的文案——红色角标显示发布编号时会被理解为正在自动推进。
  awaitingDeploy: { id: number } | undefined;
  railOpen: boolean;
  onLogout: () => void;
}) {
  const st = useForge();
  const [more, setMore] = useState(false);
  const moreButtonRef = useRef<HTMLButtonElement>(null);
  const moreMenuRef = useRef<HTMLDivElement>(null);
  const pendingAppearanceTransition = useRef<(() => void) | null>(null);
  const menuPresence = usePresence(more, 180);
  const diagPresence = usePresence(st.diag, 180);
  const themeKey = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const selectedPaletteName = PALETTES.find(option => option.key === paletteKey)?.name ?? paletteKey;
  const nextThemeTransition = theme.snapshot() === 'dark' ? 'theme-light' : 'theme-dark';
  const toggleTheme = () => runVisualTransition(() => theme.toggle(), nextThemeTransition);
  const closeMenuThenTransition = (transition: () => void) => {
    pendingAppearanceTransition.current = transition;
    setMore(false);
  };

  /* 外观切换会给整页截图。等菜单的退出动画结束并从 DOM 移除后再截图，避免旧菜单被
     烙进亮暗或色调切换的第一帧。两种外观控制必须共用这一时序。 */
  useLayoutEffect(() => {
    if (more || menuPresence.present) return;
    const transition = pendingAppearanceTransition.current;
    if (!transition) return;
    pendingAppearanceTransition.current = null;
    queueMicrotask(transition);
  }, [menuPresence.present, more]);
  /* 评审角色无法获取产物（服务端返回 403），开关一并隐藏 */
  const artifacts = can(who.role, 'artifacts');
  const account = accountOf(who);

  /* 点击其他位置时关闭菜单和气泡。两者绑定在同一个 document 监听上，避免重复实现。 */
  useEffect(() => {
    if (!more && !st.diag) return;
    const close = () => {
      setMore(false);
      forge.setDiag(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      close();
      if (more) moreButtonRef.current?.focus();
    };
    document.addEventListener('click', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', esc);
    };
  }, [more, st.diag]);

  // 三个级别分别统计，数值取自服务端的 summary 而非从列表反推：此前使用
  // `warnings = diagnostics.length - errors`，在只有两个级别时等价，引入 info 后会将提示
  // 全部计为警告——表现为服务端返回 `warnings: 0` 而角标显示「2 警」。相同的计算在
  // 概览页也有一份，两处需要同步修改（`panes/index.tsx`）。顶栏角标只统计错误和警告，
  // 提示不计入：它只列出可能被忽略的配置细节，在全局位置显示会与分级的目的相悖。
  const errors = summary?.errors ?? 0;
  const warnings = summary?.warnings ?? 0;
  const rest = visible(narrow ? [...MOBILE_MORE, ...MORE] : MORE, who);
  const deploymentMenuHint = awaitingDeploy
    ? `发布 #${awaitingDeploy.id} 等待确认`
    : activeDeploy
      ? `发布 #${activeDeploy.id} 进行中`
      : (pendingTargets ?? 0) > 0
        ? `${pendingTargets} 台待发布`
        : undefined;

  const focusMenuEdge = (edge: 'first' | 'last') => {
    requestAnimationFrame(() => {
      const items = moreMenuRef.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)');
      if (!items?.length) return;
      items[edge === 'first' ? 0 : items.length - 1]?.focus();
    });
  };

  const openMoreFromKeyboard = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    event.preventDefault();
    setMore(true);
    forge.setDiag(false);
    focusMenuEdge(event.key === 'ArrowDown' ? 'first' : 'last');
  };

  const moveWithinMore = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Tab') {
      setMore(false);
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

  /* 账户牌替代「⋯」打开同一个菜单：顶栏常驻显示当前账户。窄屏只留首字母牌，名称与角色
     在菜单头和读屏名称里。 */
  const accountButton = (
    <button
      ref={moreButtonRef}
      type="button"
      className={`fg-who${narrow ? ' compact' : ''}${account.guest ? ' guest' : ''}`}
      title={`${account.name} · ${account.role}`}
      aria-label={`${account.name}，${account.role}：账户与更多功能`}
      aria-haspopup="menu"
      aria-controls="forge-more-menu"
      aria-expanded={more}
      onKeyDown={openMoreFromKeyboard}
      onClick={e => {
        e.stopPropagation();
        setMore(v => !v);
        forge.setDiag(false);
      }}
    >
      <span className="fg-who-plate" aria-hidden="true">
        {account.initial}
      </span>
      {!narrow && <span className="fg-who-name">{account.name}</span>}
      {!narrow && <Icon of="chevronDown" size={12} className="fg-who-caret" />}
    </button>
  );

  const diagPop = diagPresence.present && (
    <div
      className="fg-pop"
      data-motion-state={diagPresence.phase}
      aria-hidden={!st.diag || undefined}
      inert={!st.diag}
      onClick={e => e.stopPropagation()}
    >
      {diagnosticsError ? (
        <ErrorBox error={diagnosticsError} />
      ) : diagnosticsPending ? (
        <Loading variant="table" />
      ) : (
        <DiagTable diagnostics={diagnostics} names={diagNames} />
      )}
    </div>
  );

  const menu = menuPresence.present && (
    <div
      id="forge-more-menu"
      ref={moreMenuRef}
      className="fg-menu nav-menu"
      role="menu"
      aria-label="账户与更多功能"
      data-motion-state={menuPresence.phase}
      aria-hidden={!more || undefined}
      inert={!more}
      onClick={() => setMore(false)}
      onKeyDown={moveWithinMore}
    >
      {/* 菜单头重复账户牌的读屏名称，只作视觉呈现；读屏从账户牌读到账户名与角色。 */}
      <div className={`fg-menu-id${account.guest ? ' guest' : ''}`} aria-hidden="true">
        <span className="fg-who-plate">{account.initial}</span>
        <span className="fg-menu-id-text">
          <span className="fg-menu-id-name">{account.name}</span>
          <span className="fg-menu-id-role">{account.role}</span>
        </span>
      </div>
      <hr />
      {rest.map(f => (
        <button
          key={f.key}
          type="button"
          role="menuitem"
          className="fg-menu-item"
          aria-current={nav === f.key ? 'page' : undefined}
          onClick={() => navigate(f.key)}
        >
          {f.icon && <Icon of={f.icon} size={14} className="fg-menu-icon" />}
          <span className="fg-menu-copy">
            {f.label}
            {f.key === 'deploy' && deploymentMenuHint && <small>{deploymentMenuHint}</small>}
          </span>
          {nav === f.key && <Icon of="check" size={13} className="fg-menu-check" />}
        </button>
      ))}
      {/* 产物在窄屏下是全屏覆盖层，不是随手查看的内容，因此从导航行收入菜单。 */}
      {narrow && artifacts && (
        <button type="button" role="menuitem" className="fg-menu-item" onClick={() => artifactPanel.toggle()}>
          <Icon of="artifactFolder" size={14} className="fg-menu-icon" />
          <span className="fg-menu-copy">
            产物<small>这一版编译出了什么</small>
          </span>
        </button>
      )}
      {/* 页面入口与即时外观控制分组。明暗模式使用明确的二选一，色调单独一行并显示当前名称；
          两行与菜单项同一层级，不再套一层框。选择任一外观后立即关闭菜单。 */}
      {(rest.length > 0 || (narrow && artifacts)) && <hr />}
      <div className="fg-appearance" role="group" aria-label="外观" onClick={e => e.stopPropagation()}>
        <div className="fg-appearance-row">
          <span className="fg-appearance-label">主题</span>
          <div className="fg-theme-switch" role="group" aria-label="明暗模式">
            <button
              type="button"
              role="menuitemradio"
              className="fg-theme-option"
              aria-label="使用亮色模式"
              aria-checked={themeKey === 'light'}
              onClick={() => {
                if (themeKey === 'light') return setMore(false);
                closeMenuThenTransition(toggleTheme);
              }}
            >
              <Icon of="sun" size={12} className="fg-theme-option-icon" />
              亮色
            </button>
            <button
              type="button"
              role="menuitemradio"
              className="fg-theme-option"
              aria-label="使用暗色模式"
              aria-checked={themeKey === 'dark'}
              onClick={() => {
                if (themeKey === 'dark') return setMore(false);
                closeMenuThenTransition(toggleTheme);
              }}
            >
              <Icon of="moon" size={12} className="fg-theme-option-icon" />
              暗色
            </button>
          </div>
        </div>
        <div className="fg-appearance-row">
          <span className="fg-appearance-label">
            色调<small>{selectedPaletteName}</small>
          </span>
          <div className="fg-tone-list" role="group" aria-label="界面色调">
            {PALETTES.map(option => (
              <button
                key={option.key}
                type="button"
                role="menuitemradio"
                className="fg-accdot"
                title={`${option.name}：${option.description}`}
                aria-label={`使用${option.name}色调：${option.description}`}
                aria-checked={paletteKey === option.key}
                onClick={event => {
                  if (paletteKey === option.key) return setMore(false);
                  const origin = motionOriginFor(event.currentTarget, event.clientX, event.clientY);
                  closeMenuThenTransition(() =>
                    runVisualTransition(() => palette.set(option.key), 'appearance', origin),
                  );
                }}
              >
                <span className="fg-accdot-swatch" style={{ backgroundColor: option.action }} />
              </button>
            ))}
          </div>
        </div>
      </div>
      <hr />
      {/* 修改密码对所有角色开放，因此不与上面按角色过滤的页面放在一起。
          公开账户除外：它是免密的共用身份，为其设置密码会导致所有人无法登录。 */}
      {!isPublic(who) && (
        <button
          type="button"
          role="menuitem"
          className="fg-menu-item"
          aria-current={nav === 'password' ? 'page' : undefined}
          onClick={() => navigate('password')}
        >
          <Icon of="security" size={14} className="fg-menu-icon" />
          <span className="fg-menu-copy">改密码</span>
          {nav === 'password' && <Icon of="check" size={13} className="fg-menu-check" />}
        </button>
      )}
      {/* 公开访客从这里进入登录；已有身份从这里退出。账户名与角色已在菜单头，这一行只写动作。 */}
      <button type="button" role="menuitem" className="fg-menu-item fg-account-action" onClick={onLogout}>
        <Icon of={isPublic(who) ? 'access' : 'outbound'} size={14} className="fg-menu-icon" />
        <span className="fg-menu-copy">{isPublic(who) ? '登录' : '退出登录'}</span>
      </button>
    </div>
  );

  if (narrow) {
    /* 窄屏只有这一行：品牌 + 三个高频页面 + 诊断 + 账户牌。隧道、发布和用量收入账户牌的
       菜单，发布状态作为菜单项说明显示。下钻不再增加第二行，理由见上方 `.fg-backrow` 的说明。 */
    return (
      <div className="fg-top fg-navrow">
        <BrandHome branding={branding} />
        <MainNav
          className="fg-facebar"
          faces={MOBILE_NAV}
          who={who}
          nav={nav}
          pendingTargets={pendingTargets}
          activeDeploy={activeDeploy}
          awaitingDeploy={awaitingDeploy}
        />
        {/* 与宽屏的处理一致：诊断属于发布流程的读数，公开访客不显示。 */}
        {!isVisitor(who) && (
          <div className="fg-menuwrap">
            <button
              className="fg-ico"
              aria-expanded={st.diag}
              aria-label="诊断"
              title="诊断"
              onClick={e => {
                e.stopPropagation();
                forge.toggleDiag();
                setMore(false);
              }}
            >
              <Icon of="diag" size={13} className="fg-tgl-ic" />
              {(diagnosticsError || errors > 0 || warnings > 0) && (
                <i className={`fg-dot${diagnosticsError || errors ? ' err' : ''}`} />
              )}
            </button>
            {diagPop}
          </div>
        )}

        <div className="fg-menuwrap">
          {accountButton}
          {menu}
        </div>
      </div>
    );
  }

  return (
    <div className="fg-top">
      <BrandHome branding={branding} />
      <span className="fg-vr" />

      <MainNav
        className="fg-nav"
        faces={NAV}
        who={who}
        nav={nav}
        pendingTargets={pendingTargets}
        activeDeploy={activeDeploy}
        awaitingDeploy={awaitingDeploy}
      />

      <span className="sp" />

      {/* 产物与诊断只显示图标，名称放在悬停提示和读屏文字里。
          产物按钮只控制展开和收起。是否有待发布内容由「发布」导航表示；否则历史 diff
          的标记会被理解为发布未完成。
          读不了产物的角色看到的是禁用而不是消失：按角色隐藏时，顶栏在不同身份下少一个
          控件，而少掉的那个是这套外壳里唯一的产物入口。 */}
      <button
        type="button"
        className="fg-tgl"
        aria-pressed={railOpen}
        disabled={!artifacts}
        title={artifacts ? '显示 / 隐藏产物栏' : '当前身份无权查看产物'}
        onClick={() => artifactPanel.toggle()}
      >
        <Icon of="artifactFolder" size={15} className="fg-tgl-ic" />
        <span className="fg-tgl-label">产物</span>
      </button>

      {/* 诊断不对公开访客显示：它表示该版本的编译结果和可发布性，属于编辑到发布流程的
          读数。修订号不在顶栏重复——面包屑行（fg-crumb-right）已承担这项读数。 */}
      {!isVisitor(who) && (
        <div className="fg-menuwrap">
          <button
            type="button"
            className="fg-tgl"
            aria-expanded={st.diag}
            title="诊断"
            onClick={e => {
              e.stopPropagation();
              forge.toggleDiag();
              setMore(false);
            }}
          >
            <Icon of="diag" size={15} className="fg-tgl-ic" />
            <span className="fg-tgl-label">诊断</span>
            {(diagnosticsError || errors > 0 || warnings > 0) && (
              <span className={`fg-badge${diagnosticsError || errors ? ' err' : ''}`}>
                {diagnosticsError ? '!' : errors || warnings}
              </span>
            )}
          </button>
          {diagPop}
        </div>
      )}

      <span className="fg-vr" />
      <div className="fg-menuwrap">
        {accountButton}
        {menu}
      </div>
    </div>
  );
}

/* ══ 工作区 ══ */

// 顶部面包屑。第一段是当前所在的页面（机器、线路等），其后是面板内的下钻层级。
// *
// * 下钻状态保存在 `win.data.drill` 中，其结构由面板自行定义，因此此处不解析它，
// * 只读取面板写入的 `crumb`；点击某一段时将该段携带的 `drill` 原样写回，面板会返回该层级。
// * 点击第一段时清除 `drill`——各面板使用 `?? { p: 'list' }` 作为默认值，清除后回到列表。
function ForgeCrumb({ nav }: { nav: NavKey }) {
  const snap = useSyncExternalStore(wm.subscribe, wm.snapshot);
  const win = snap.wins.find(w => w.key === `tab:${nav}`);
  const segs = (win?.data.crumb as CrumbSeg[] | undefined) ?? [];
  const goto = (drill?: unknown) => returnTo(nav, drill as Loc['drill']);

  return (
    <>
      {segs.length === 0 ? (
        <span className="cur">{LABEL[nav]}</span>
      ) : (
        <button type="button" className="fg-crumb-link" onClick={() => goto()}>
          {LABEL[nav]}
        </button>
      )}
      {segs.map((seg, i) => (
        <Fragment key={i}>
          <span className="sep">/</span>
          {i === segs.length - 1 || seg.drill === undefined ? (
            <span className="cur">{seg.label}</span>
          ) : (
            <button type="button" className="fg-crumb-link" onClick={() => goto(seg.drill)}>
              {seg.label}
            </button>
          )}
        </Fragment>
      ))}
    </>
  );
}

function Work({ nav }: { nav: NavKey }) {
  const snap = useSyncExternalStore(wm.subscribe, wm.snapshot);
  const win = snap.wins.find(w => w.key === `tab:${nav}`);

  // 判断依据是窗口是否存在而非窗口本身：窗口被清除时（换人后重新 init、旧布局失效）
  // 可以自动恢复，不会停留在下方的 Loading 状态；而窗口内容变化不应触发重新打开。
  // 已打开时不执行任何操作——窗内导航状态（向导的当前步骤、下钻到的机器）保持不变。
  const hasWin = !!win;
  useEffect(() => {
    if (nav === 'topo' || hasWin) return;
    wm.open(`tab:${nav}`, LABEL[nav]);
  }, [nav, hasWin]);

  if (!win) {
    return null;
  }
  // 页面根节点自己决定是列表卡片还是连续详情纸。工作区只负责滚动，不再先画一层通用纸：
  // 否则卡片页会多出一个无意义的表面，初次读取时也会先闪出这张空纸。
  return <Pane win={win} bare={nav === 'nodes' || nav === 'users'} />;
}
