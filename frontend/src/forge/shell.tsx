// 编译台 v2：一条顶栏、一块工作区、一根贯通到顶的产物栏。
//
// 相对 v1（左树 / 中栏 / 右产物 三栏），改动有四项：
//
// 一、顶栏只覆盖左列。右侧产物栏从 y=0 贯通到底，与浏览器顶边对齐。
// 二、移除左侧模型树，导航移入顶栏。在当前规模下该树只增加成本：查看机器数量
//     由零次点击变为两次。选路改由各页面自身的列表进入——该能力页面本身已具备，
//     树只是重复实现了一遍。
// 三、诊断由常驻面板改为顶栏气泡。它的使用方式是查看、进入、关闭。
// 四、内容位于网格台面上的一张纸内。
//
// 各面板本身未作改动：写入路径仍只有表单一条，纳管向导、发布波次、授权矩阵
// 都是原有组件。
//
// 旧外壳（#bar / #winlayer / #artpanel）同样未改动，将 app.tsx 的 SHELL 改回
// 'workbench' 即可切换回去——本仓库没有版本控制，回退路径需保留在代码中。

import { Fragment, useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchDeployments,
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
import { LinksPane } from '../panes/links';
import { TopoCanvas } from '../topo/canvas';
import { Loading } from '../ui/bits';
import { useNarrow } from '../ui/viewport';
import { artifactPanel } from '../ui/artifact-panel';
import { DiagTable } from '../ui/diag-table';
import { WinLayer } from '../ui/windows';
import { wm, type CrumbSeg } from '../wm/store';
import { draft } from '../draft';
import { DraftBar } from './draft-bar';
import { ArtifactRail, blastRadius, useChangedArtifacts } from './artifacts';
import { navigate, startRouting } from './route';
import { can, isPublic, isVisitor } from '../session';
import { forge, useForge, type NavKey } from './state';
import { theme } from './theme';
import { palette, PALETTES } from './palette';
import { Icon, type IconName } from '../ui/icons';
import { BrandIcon } from '../ui/branding';
import { compactGrantAutomation, RuntimeCrumbStatus, type RuntimeCrumbState } from '../ui/grant-automation';

interface Face {
  key: NavKey;
  label: string;
  /* 顶栏导航的图标。收入「⋯」的项没有：菜单行是文字列表，图标在那里没有定位作用。 */
  icon?: IconName;
  /* 默认对所有角色可见。按角色隐藏入口属于体验优化，不构成安全边界。 */
  roles?: AdminRole[];
}

// 顶栏只显示当前主工作流。外部出站已经在规则的「转发给」中管理，独立隧道页暂时
// 保留作兼容入口但不再占一个导航位。
const NAV: Face[] = [
  { key: 'nodes', label: '机器', icon: 'nodes' },
  { key: 'chains', label: '线路', icon: 'chains' },
  { key: 'users', label: '用户', icon: 'users' },
  { key: 'deploy', label: '发布', icon: 'deploy', roles: ['editor', 'publisher', 'tenant-admin', 'system-admin'] },
  { key: 'usage', label: '用量', icon: 'usage' },
];

/* 手机端只把三项高频配置入口留在顶栏，腾出的宽度用于恢复按钮文字。发布和用量仍使用
   同一份 Face 定义，只是移动到更多菜单，避免两套角色权限和名称逐渐分叉。 */
const MOBILE_NAV = NAV.filter(f => f.key === 'nodes' || f.key === 'chains' || f.key === 'users');
const MOBILE_MORE = NAV.filter(f => f.key === 'deploy' || f.key === 'usage');

// 收入「⋯」的项：都是低频访问的页面，占用顶栏位置的收益较低。
// 窄屏同理：该行只放 NAV 的主工作流，这些低频页面仍从「⋯」进入。
const MORE: Face[] = [
  { key: 'settings', label: '设置', roles: ['editor', 'publisher', 'tenant-admin', 'system-admin'] },
  { key: 'tenants', label: '租户' },
  { key: 'topo', label: '拓扑' },
  { key: 'links', label: '链路与 MTU' },
];

/* 不进入页面列表，但需要标题：面包屑和窗口名都读取 LABEL。 */
const OFF_NAV: Face[] = [{ key: 'password', label: '改密码' }];

const LABEL: Record<NavKey, string> = Object.fromEntries(
  [...NAV, ...MORE, ...OFF_NAV].map(f => [f.key, f.label]),
) as Record<NavKey, string>;

const visible = (faces: Face[], who: Whoami) =>
  faces.filter(f => !f.roles || f.roles.includes(who.role));

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
  // 草稿版本进入该层的查询键，同时统一失效所有页面的读取缓存。
  // 仅依靠查询键不够：各页面使用 `['snapshot']`，键中不含草稿版本，草稿变化后
  // 它们仍会命中缓存——表现为修改后草稿条已出现但表格内容未更新。
  // 订阅集中在此而非分散在各写入点：草稿可能从任意位置写入（api.ts 中的写函数、
  // 连线操作、冒烟脚本），失效逻辑只应有一处。
  const draftVer = useSyncExternalStore(draft.subscribe, draft.version);
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        qc.invalidateQueries({ queryKey: ['snapshot'] });
        qc.invalidateQueries({ queryKey: ['compile'] });
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
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
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
    enabled: current != null,
  });
  const snapshot = useQuery({ queryKey: ['snapshot', draftVer], queryFn: () => fetchSnapshot() });
  const verify = useQuery({
    queryKey: ['deployment-verify', current],
    queryFn: () => verifyDeployment({ revision_id: current! }),
    enabled: !pub && current != null && draft.isEmpty(),
    refetchInterval: q => ((q.state.data?.summary.changed_targets ?? 0) > 0 ? 5_000 : false),
  });
  // 发布列表全局轮询：任意页面下顶栏都需要能显示发布中状态，不能只在发布页打开时
  // 才获知有发布在执行。发布页也读取该 key，tanstack 共享缓存不会重复请求。
  // active 表示限流锁被占用（deployment.rs），包含 halted 未收尾的情况。
  const deployments = useQuery({
    queryKey: ['deployments'],
    queryFn: () => fetchDeployments(),
    enabled: !pub,
    refetchInterval: 5_000,
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
    refetchInterval: 5_000,
  });

  const { list, changed, dirty } = useChangedArtifacts(current, prev);
  const draftBlast = useMemo(() => blastRadius(list, changed), [list, changed]);
  const pendingTargets = dirty ? undefined : verify.data?.summary.changed_targets;
  const diagnostics = visibleDiagnostics(compile.data?.diagnostics);
  // 诊断的 location 中全部是 id，而面板需要显示名称。这两份数据在其他位置已在读取，
  // 此处只是将 id 到名称的转换集中处理（见 formatLocation）。
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const diagNames = useMemo<DiagNames>(() => {
    const nodes = new Map((nodeList.data?.nodes ?? []).map(n => [n.node_id, n.name]));
    const chains = new Map(
      (snapshot.data?.snapshot.apps ?? []).flatMap(a => a.chains.map(c => [c.id, c.name] as const)),
    );
    return { node: id => nodes.get(id), chain: id => chains.get(id) };
  }, [nodeList.data, snapshot.data]);

  const grantRuntime = compactGrantAutomation(grantAutomation.data, grantAutomation.isPending, !!grantAutomation.error);
  // 面包屑只留一段短状态：需要人工介入和失败优先，安静时才显示队列健康。
  // 修订号由相邻的 Rn 只显示一次，避免“已收敛到修订 n · 修订 n”。
  const crumbRuntime: RuntimeCrumbState = dirty
    ? {
        text: draftBlast.size ? `草稿 · ${draftBlast.size} 台` : '草稿 · 无产物变更',
        tone: draftBlast.size ? 'hot' : 'normal',
        title: draftBlast.size ? [...draftBlast].join(', ') : undefined,
      }
    : verify.isPending
      ? { text: '检查中', tone: 'normal' }
      : verify.error
        ? { text: '发布状态未知', tone: 'bad' }
        : awaitingDeploy
          ? { text: `发布 #${awaitingDeploy.id} · 待确认`, tone: 'bad' }
          : grantRuntime.tone === 'bad'
            ? grantRuntime
            : pendingTargets
              ? { text: `待发布 ${pendingTargets} 台`, tone: 'hot' }
              : grantRuntime;

  return (
    <div className={`forge${narrow ? ' narrow' : ''}`}>
      <div className="fg-left">
        <TopBar
          branding={branding}
          who={session.who}
          narrow={narrow}
          nav={nav}
          summary={compile.data?.summary}
          diagnostics={diagnostics}
          diagNames={diagNames}
          pendingTargets={pendingTargets}
          activeDeploy={activeDeploy}
          awaitingDeploy={awaitingDeploy}
          railOpen={panel.open}
          onLogout={onLogout}
        />

        {/* 窄屏不显示该行，也不给它任何替代形态：当前位置由导航行里高亮的那一格加内容区
            自己的标题表示，发布状态读数由窄屏「发布」按钮的角标承担。 */}
        {!narrow && (
          <div className="fg-crumb">
            <ForgeCrumb nav={nav} />
            {/* 公开访客不显示该行右侧的全部读数：发布状态和修订号属于同一类信息，
                而这两个查询在该身份下无权访问——保留会始终停留在检查中的状态。 */}
            {!pub && (
              <span className="fg-crumb-right">
                <RuntimeCrumbStatus state={crumbRuntime} revision={current} />
              </span>
            )}
          </div>
        )}

        <DraftBar current={current} />

        {nav === 'topo' ? (
          <div className="fg-topo">
            <TopoCanvas />
          </div>
        ) : (
          <div className="fg-desk">
            <Work nav={nav} />
          </div>
        )}
      </div>

      {panel.open && artifacts && (
        <ArtifactRail
          revision={current}
          prev={prev}
          compareClean={(pendingTargets ?? 0) > 0}
          apps={snapshot.data?.snapshot.apps ?? []}
          onClose={() => artifactPanel.close()}
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
// 曾尝试将 MORE 的四项也加入（带横向滚动），结果是右侧部分始终不可见——与收入「⋯」
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
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  /* 评审角色无法获取产物（服务端返回 403），开关一并隐藏 */
  const artifacts = can(who.role, 'artifacts');

  /* 点击其他位置时关闭菜单和气泡。两者绑定在同一个 document 监听上，避免重复实现。 */
  useEffect(() => {
    if (!more && !st.diag) return;
    const close = () => {
      setMore(false);
      forge.setDiag(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
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
  // 提示不计入：它表示无法判定的事实，在全局位置显示会与分级的目的相悖。
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

  const diagPop = st.diag && (
    <div className="fg-pop" onClick={e => e.stopPropagation()}>
      <DiagTable diagnostics={diagnostics} names={diagNames} />
    </div>
  );

  const menu = more && (
    <div className="fg-menu" onClick={() => setMore(false)}>
      {rest.map(f => (
        <button key={f.key} onClick={() => navigate(f.key)}>
          {f.label}
          {f.key === 'deploy' && deploymentMenuHint && <small>{deploymentMenuHint}</small>}
        </button>
      ))}
      {/* 分隔线用于区分页面项和设置项。上方没有任何项时（公开访客在 MORE 中没有可见页面），
          该分隔线上方没有内容，会成为菜单顶部的一条无意义的横线。 */}
      {rest.length > 0 && <hr />}
      {/* 产物在窄屏下是全屏覆盖层，不是随手查看的内容，因此从导航行收入菜单。 */}
      {narrow && artifacts && (
        <button onClick={() => artifactPanel.toggle()}>
          产物<small>这一版编译出了什么</small>
        </button>
      )}
      <button onClick={() => theme.toggle()}>
        切换亮 / 暗<small>默认暗色</small>
      </button>
      {/* 调色盘是即时预览项而不是跳转项：点击不关闭菜单（stopPropagation），
          可以连续试色。选中态由 aria-pressed 的圆环表示。 */}
      <div className="fg-accrow" onClick={e => e.stopPropagation()}>
        <span className="t">配色</span>
        {PALETTES.map(option => (
          <button
            key={option.key}
            className="fg-accdot"
            title={`${option.name}：${option.description}`}
            aria-label={`配色 ${option.name}：${option.description}`}
            aria-pressed={paletteKey === option.key}
            style={{ backgroundColor: option.action }}
            onClick={() => palette.set(option.key)}
          />
        ))}
      </div>
      {/* 修改密码对所有角色开放，因此不与上面按角色过滤的页面放在一起。
          公开账户除外：它是免密的共用身份，为其设置密码会导致所有人无法登录。 */}
      {!isPublic(who) && (
        <button onClick={() => navigate('password')}>
          改密码<small>改自己的登录密码</small>
        </button>
      )}
      {/* 公开访客的「退出」即登录入口：该页面上没有其他位置可以返回登录表单。 */}
      <button onClick={onLogout}>
        {isPublic(who) ? '登录' : '退出'}
        <small>
          {isPublic(who) ? (
            '现在是公开访客，登录换成你自己的身份'
          ) : (
            <>
              {who.operator_id} · {who.role} · scope {who.tenant_scope ?? '全局'}
            </>
          )}
        </small>
      </button>
    </div>
  );

  if (narrow) {
    /* 窄屏只有这一行：品牌 + 三个高频页面 + 诊断 + ⋯。发布和用量收入更多菜单，
       发布状态作为菜单项说明显示。下钻不再增加第二行，理由见上方 `.fg-backrow` 的说明。 */
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
              {(errors > 0 || warnings > 0) && <i className={`fg-dot${errors ? ' err' : ''}`} />}
            </button>
            {diagPop}
          </div>
        )}

        <div className="fg-menuwrap">
          <button
            className="fg-ico"
            title="更多"
            aria-label="更多"
            aria-expanded={more}
            onClick={e => {
              e.stopPropagation();
              setMore(v => !v);
              forge.setDiag(false);
            }}
          >
            ⋯
          </button>
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

      {/* 产物按钮只控制展开和收起。是否有待发布内容由「发布」导航表示；否则历史 diff
          的标记会被理解为发布未完成。
          读不了产物的角色看到的是禁用而不是消失：按角色隐藏时，顶栏在不同身份下少一个
          控件，而少掉的那个是这套外壳里唯一的产物入口。 */}
      <button
        className="fg-tgl"
        aria-pressed={railOpen}
        disabled={!artifacts}
        title="显示 / 隐藏产物栏"
        onClick={() => artifactPanel.toggle()}
      >
        <Icon of="artifacts" size={13} className="fg-tgl-ic" />
        产物
      </button>

      {/* 诊断不对公开访客显示：它表示该版本的编译结果和可发布性，属于编辑到发布流程的
          读数。修订号不在顶栏重复——面包屑行（fg-crumb-right）已承担这项读数。 */}
      {!isVisitor(who) && (
        <div className="fg-menuwrap">
          <button
            className="fg-tgl"
            aria-expanded={st.diag}
            title="诊断"
            onClick={e => {
              e.stopPropagation();
              forge.toggleDiag();
              setMore(false);
            }}
          >
            <Icon of="diag" size={13} className="fg-tgl-ic" />
            诊断
            {(errors > 0 || warnings > 0) && (
              <span className={`fg-badge${errors ? ' err' : ''}`}>{errors || warnings}</span>
            )}
          </button>
          {diagPop}
        </div>
      )}

      <div className="fg-menuwrap">
        <button
          className="btn fg-more"
          title="更多"
          aria-label="更多"
          onClick={e => {
            e.stopPropagation();
            setMore(v => !v);
            forge.setDiag(false);
          }}
        >
          ⋯
        </button>
        {menu}
      </div>
    </div>
  );
}

/* ══ 工作区 ══ */

// 导航键到已有功能页面的对应关系。两侧使用同一套 key，因此不需要映射表：
// `links` 是后增加的页面，没有对应的旧功能窗，直接渲染。
// 顶部面包屑。第一段是当前所在的页面（机器、线路等），其后是面板内的下钻层级。
// *
// * 下钻状态保存在 `win.data.drill` 中，其结构由面板自行定义，因此此处不解析它，
// * 只读取面板写入的 `crumb`；点击某一段时将该段携带的 `drill` 原样写回，面板会返回该层级。
// * 点击第一段时清除 `drill`——各面板使用 `?? { p: 'list' }` 作为默认值，清除后回到列表。
function ForgeCrumb({ nav }: { nav: NavKey }) {
  const snap = useSyncExternalStore(wm.subscribe, wm.snapshot);
  const win = snap.wins.find(w => w.key === `tab:${nav}`);
  const segs = (win?.data.crumb as CrumbSeg[] | undefined) ?? [];
  // 返回到第 `keep` 段（0 表示顶层）。`crumb` 需要同步截断：只修改 drill 时，
  // 面板已返回列表而面包屑仍显示原有层级，表现为点击无响应。
  const goto = (drill: unknown, keep: number) => {
    if (!win) return;
    wm.setData(win.id, { ...win.data, drill, crumb: segs.slice(0, keep) });
  };

  return (
    <>
      {segs.length === 0 ? (
        <span className="cur">{LABEL[nav]}</span>
      ) : (
        <a onClick={() => goto(undefined, 0)}>{LABEL[nav]}</a>
      )}
      {segs.map((seg, i) => (
        <Fragment key={i}>
          <span className="sep">/</span>
          {i === segs.length - 1 || seg.drill === undefined ? (
            <span className="cur">{seg.label}</span>
          ) : (
            <a onClick={() => goto(seg.drill, i + 1)}>{seg.label}</a>
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
    if (nav === 'links' || nav === 'topo' || hasWin) return;
    wm.open(`tab:${nav}`, LABEL[nav]);
  }, [nav, hasWin]);

  if (nav === 'links')
    return (
      <div className="fg-sheet">
        <LinksPane />
      </div>
    );
  if (!win) return <Loading />;
  // 这两个页面自行分页：管控面板和详细面板各自是一张纸（fg-sheet），
  // 不再由外壳包裹一层纸并在其中嵌套 panel 卡片——与 d-desk 的布局方式一致。
  if (nav === 'nodes' || nav === 'users') return <Pane win={win} bare />;
  return (
    <div className="fg-sheet">
      <Pane win={win} />
    </div>
  );
}
