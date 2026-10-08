import {
  Fragment,
  useEffect,
  useId,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react';
import { useMutation, useQuery, useQueryClient, type UseQueryResult } from '@tanstack/react-query';
import {
  discardPendingChanges,
  cancelDeployment,
  cancelRollbackDeployment,
  confirmWave,
  createDeployment,
  createRollback,
  fetchArtifactContent,
  fetchDeployment,
  fetchDeployments,
  fetchRevisions,
  haltDeployment,
  isolateDeploymentTarget,
  planDeployment,
  retryTarget,
  verifyDeployment,
  type ArtifactIndexEntry,
  type DeploymentListItem,
  type DeploymentTargetDetail,
  type NodeAgentStateItem,
  type PlannedAction,
  type PlannedTarget,
  type RevisionList,
  type RevisionListItem,
} from '../api';
import { draft } from '../draft';
import { AgentReleaseTab, agentBadge, useAgentRelease } from './agent-release';
import { XrayReleaseTab, useXrayRelease, xrayBadge } from './xray-release';
import { FlagRun, ReleaseLedger, stamp } from './deploy-cockpit';
import { entryId, useRevisionDiff } from '../forge/artifacts';
import { artifactFile, artifactFmt, countChanges, diffLines, highlight } from '../forge/diff';
import { can, useSession } from '../session';
import {
  Ago,
  Confirm,
  Empty,
  ErrorBox,
  Loading,
  STATUS_TEXT,
  SegmentedControl,
  Status,
  type LoadingVariant,
} from '../ui/bits';
import { Icon, PanelTitle, type IconName } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { randomKey } from '../ui/platform';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { navigate, returnTo } from '../forge/route';

type Drill =
  | { p: 'list' }
  // key 是本次预览的幂等键：进入预览时生成一次，重复点击「创建」返回同一条 deployment，
  // 不会因重复点击创建出两条。每次重新生成则失去幂等性。
  | { p: 'plan'; revision?: number; key?: string }
  | { p: 'detail'; id: number };

/* 动作决定是否具有破坏性，破坏性决定波次划分 */
const ACTION_LABEL: Record<PlannedAction, string> = {
  'apply-phantun': '更新 Phantun',
  'apply-hy2-port-hop': '更新端口跳跃',
  'sync-grants': '同步授权',
  'apply-wire-guard': '更新 WireGuard',
  'apply-xray': '更新 Xray',
  'disable-phantun': '停用 Phantun',
  'disable-hy2-port-hop': '停用端口跳跃',
  'disable-wire-guard': '停用 WireGuard',
  'disable-xray': '停用 Xray',
};

const ACTION_NOTE: Record<PlannedAction, string> = {
  'apply-phantun': '同步 fake TCP 封装进程',
  'apply-hy2-port-hop': '只调整 nft，进程不停',
  'sync-grants': '热更新访问名单，连接不中断',
  'apply-wire-guard': '同步隧道配置，已有链路不中断',
  'apply-xray': '重写配置并重启，当前连接会中断',
  'disable-phantun': '依赖 fake TCP 的链路会中断',
  'disable-hy2-port-hop': '客户端将只能连接固定落点端口',
  'disable-wire-guard': '经过该隧道的链路会中断',
  'disable-xray': '该机器上的所有连接会中断',
};

type DeploymentStage = 'config' | 'verify' | 'rollout' | 'stop';

interface DeploymentStep<T> {
  wave: number;
  targets: T[];
  stage: DeploymentStage;
  title: string;
  step: number;
  steps: number;
  disruptive: boolean;
  needsConfirmation: boolean;
}

/**
 * `wave` is the persisted execution contract, not product language. Turn it into the three
 * operator-facing stages without changing the IDs sent back to the confirmation endpoint.
 */
function deploymentSteps<T extends { wave: number; disruptive: boolean }>(
  targets: T[],
  actions: (target: T) => readonly string[],
): DeploymentStep<T>[] {
  const waves = [...new Set(targets.map(target => target.wave))].sort((a, b) => a - b);
  const targetsIn = (wave: number) => targets.filter(target => target.wave === wave);
  const xrayWaves = waves.filter(wave => targetsIn(wave).some(target => actions(target).includes('apply-xray')));
  const firstXray = xrayWaves[0] ?? -1;
  const verificationWave = xrayWaves.length > 1 && targetsIn(firstXray).length === 1 ? firstXray : undefined;
  const stopWaves = waves.filter(wave => {
    if (wave === 0) return false;
    return targetsIn(wave).every(target => {
      const names = actions(target);
      return !names.includes('apply-xray') && (names.includes('disable-xray') || names.includes('disable-wire-guard'));
    });
  });
  const rolloutWaves = waves.filter(wave => wave !== 0 && wave !== verificationWave && !stopWaves.includes(wave));

  return waves.map(wave => {
    const inWave = targetsIn(wave);
    const stage: DeploymentStage =
      wave === 0 ? 'config' : wave === verificationWave ? 'verify' : stopWaves.includes(wave) ? 'stop' : 'rollout';
    const stageWaves = stage === 'stop' ? stopWaves : stage === 'rollout' ? rolloutWaves : [wave];
    return {
      wave,
      targets: inWave,
      stage,
      title:
        stage === 'config'
          ? '更新机器配置'
          : stage === 'verify'
            ? '发布验证'
            : stage === 'stop'
              ? '停用服务'
              : '全量发布',
      step: stageWaves.indexOf(wave) + 1,
      steps: stageWaves.length,
      disruptive: inWave.some(target => target.disruptive),
      needsConfirmation: inWave.some(
        target =>
          target.disruptive &&
          (wave > 1 || actions(target).includes('disable-xray') || actions(target).includes('disable-wire-guard')),
      ),
    };
  });
}

const stageCount = <T,>(steps: DeploymentStep<T>[]) => new Set(steps.map(step => step.stage)).size;

const confirmationSummary = <T,>(steps: DeploymentStep<T>[]) => {
  const gated = steps.filter(step => step.needsConfirmation);
  const rollout = gated.filter(step => step.stage === 'rollout').length;
  return [
    ...new Set(gated.filter(step => step.stage !== 'rollout').map(step => step.title)),
    ...(rollout ? [`全量发布的 ${rollout} 个步骤`] : []),
  ].join('、');
};

function deploymentStepAction<T extends { node_id: string }>(
  step: DeploymentStep<T>,
  nameOf: (nodeId: string) => string,
) {
  if (step.stage !== 'rollout') return `开始${step.title}`;
  const names = step.targets.map(target => nameOf(target.node_id));
  return names.length === 1 ? `更新${names[0]}` : `更新这 ${names.length} 台机器`;
}

// 失败处理建议：deployment target 的 error 是 agent 侧的自由文本（服务端没有结构化
// 错误码，deployment.rs 原样透传 agent 的 report.error），只能按子串匹配推断。
// 顺序即优先级，第一条匹配的生效；均不匹配时使用兜底建议「先查看日志」。
//
// 由于是推断，表述需要限定在观察层面：先复述报错内容，再给出常见原因。
// 使用断言式表述的代价是按其排查一个不存在的问题，返回后发现报错没有变化——
// 此后该提示不再被信任，后续匹配正确的条目也失去作用。
const FAILURE_GUIDES: { re: RegExp; guide: string }[] = [
  {
    re: /Exec format error|cannot execute|\(os error 8\)/,
    guide:
      'agent 二进制在这台上跑不起来，多半是架构不符。控制面内嵌 x86_64 和 aarch64 两种，先在这台上看 uname -m；两种都不是才需要自己编一个，用 --agent-bin-url 重跑 install.sh。',
  },
  {
    re: /desired request failed: HTTP 4|probe targets request failed: HTTP 4|link probe failed: HTTP 4/,
    guide:
      '控制面拒了这台的请求。最常见是 node token 失效或权限不够——到机器页「重签 token」再重试；不是的话，agent 日志里有那条请求的完整响应。',
  },
  {
    re: /HTTP 5\d\d/,
    guide: '控制面内部错误。等一会儿重试；反复失败查控制面日志。',
  },
  {
    re: /connection refused|timed out|timeout|temporary failure/,
    guide: '这台机器和控制面之间没接通。查它的出网、控制面在不在，以及安装时配的 BROCADE_AGENT_SERVER。',
  },
  {
    re: /permission denied|Operation not permitted/,
    guide:
      'agent 动不了系统。最常见是装的时候没以 root 跑 install.sh（--apply linux 要求 root）；也可能是容器或内核策略挡住了这一步。',
  },
  {
    re: /不在这台机器上/,
    guide:
      '这台要用伪 TCP，而 agent 说 phantun 不在这台机器上。重跑 install.sh 补装，或让控制面配好 BROCADE_PHANTUN_SERVER_URL / BROCADE_PHANTUN_CLIENT_URL 再重试。',
  },
  {
    re: /健康检查有/,
    guide:
      '收敛后自检没过。上机器看 journalctl -u brocade-agent -n 50，确认具体哪一项没通过（端口没监听、xray 没起来、wg 握手失败）。',
  },
];

const guideFor = (error: string) =>
  FAILURE_GUIDES.find(g => g.re.test(error))?.guide ??
  /* 均不匹配时使用兜底建议：agent 的报错是自由文本，给出错误的推断比不推断更差
   （deployment.md 中没有错误码） */
  '上机器看 journalctl -u brocade-agent -n 50，找到报错前的现场；修好后逐台 retry。';

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「发布」）由外壳补全。 */
const crumbOf = (d: Drill): CrumbSeg[] => {
  switch (d.p) {
    case 'list':
      return [];
    case 'plan':
      return [{ label: d.revision == null ? '计划预览' : `计划预览 · 修订 ${d.revision}` }];
    case 'detail':
      return [{ label: `#${d.id}` }];
  }
};

export function DeployPane({ win }: { win: Win }) {
  const { who } = useSession();
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => navigate('deploy', d);
  useCrumb(win, crumbOf(drill));

  if (drill.p === 'plan') {
    return <PlanRoute win={win} drill={drill} go={go} />;
  }
  // key 按 deployment 确定：Detail 中的 ask / confirmedWave 是该条发布的状态，
  // 切换发布（如回滚跳转到新工单）时应重置，不能带入下一条。
  if (drill.p === 'detail') return <Detail key={drill.id} id={drill.id} go={go} />;

  // 发布页照机器详情页分页签（配置 / Agent / Xray），每个页签照用量页：左侧读数栏，右侧列表。
  return <DeployOverview go={go} editable={can(who.role, 'system')} />;
}

function PlanRoute({ win, drill, go }: { win: Win; drill: Extract<Drill, { p: 'plan' }>; go: (d: Drill) => void }) {
  const [generatedKey] = useState(() => drill.key ?? randomKey());
  useEffect(() => {
    // A render can be abandoned under concurrent React. Only publish the generated idempotency key
    // after commit; StrictMode may repeat this effect, but both writes carry the same key.
    if (!drill.key) wm.setData(win.id, { ...win.data, drill: { ...drill, key: generatedKey } });
  }, [drill, generatedKey, win.data, win.id]);

  if (!drill.key) return <Loading variant="plan" />;
  return <PlanPreview revision={drill.revision} idempotencyKey={drill.key} go={go} />;
}

// 协议中的取值是 config / grants，界面按发起方式表述：变更单由人工发起，需要关注分波和确认；
// 自动化授权单由权限操作或配额执行自动发起，只增删运行时的名单。
const KIND_LABEL = { all: '全部', config: '变更单', grants: '自动化授权单' } as const;
const HISTORY_PREVIEW_COUNT = 6;

// 发布列表通常由服务端按 id 倒序返回，但基线判定不能依赖调用方排序。回滚也会产生更大的
// 修订号，因此比较的是 deployment id（实际发生顺序），返回该次发布引用的修订。
export function latestSuccessfulRevision(items: DeploymentListItem[]): number | null {
  const latest = items.reduce<DeploymentListItem | null>(
    (best, item) => (item.status === 'succeeded' && (best === null || item.id > best.id) ? item : best),
    null,
  );
  return latest?.revision_id ?? null;
}

function RevisionTrail({ revisions, base }: { revisions: RevisionListItem[]; base: number | null }) {
  if (revisions.length === 0 && base == null) return null;
  return (
    <ol className="cg-revs" aria-label="待发布修订">
      {revisions.map(revision => (
        <li key={revision.id}>
          <span className="dot" />
          <span className="rev">R{revision.id}</span>
          <span className="msg">{revision.note || '未填写修订说明'}</span>
          <span className="by">
            {revision.author || '系统'} · <When at={revision.created_at} />
          </span>
        </li>
      ))}
      {base != null && (
        <li className="base">
          <span className="dot" />
          <span className="rev">R{base}</span>
          <span className="msg">已发布的运行基线</span>
          <span className="by" />
        </li>
      )}
    </ol>
  );
}

/* ══ 发布流水 ═════════════════════════════════════════════════════════════════
   配置页签右侧的一条时间轴：未发布的修订 → 进行中的变更单 → 最近的已发布单据。
   稿件：mockups/deploy-redesign.html 的方案 B，稿件里的私有前缀在这里统一写作 cgf-。 */

type FlowTone = 'ok' | 'warn' | 'err' | 'run' | 'idle';

// 结果图标落在时间轴的圆点位置。成功是常态，只画图标不写字；失败、取消、待补偿、
// 等确认才在右侧出状态文字。
const TONE_ICON: Record<FlowTone, IconName> = {
  ok: 'check',
  warn: 'clock',
  err: 'close',
  run: 'clock',
  idle: 'dash',
};

function deploymentTone(item: DeploymentListItem): FlowTone {
  if (item.failed_targets > 0 || item.status === 'halted') return 'err';
  if (item.awaiting_confirmation || item.settlement_status === 'debt') return 'warn';
  if (item.status === 'running' || item.status === 'planned') return 'run';
  if (item.activation_status === 'activated' || item.status === 'succeeded') return 'ok';
  return 'idle';
}

// 已生效且已收敛的单据不写状态文字——它是这张表里最常见的一行，写出来只是把同一句话
// 重复几十遍。其余取值（待生效、等确认、待补偿、失败、取消）都需要读者看到。
const settledRecord = (item: DeploymentListItem) =>
  item.activation_status === 'activated' && item.settlement_status === 'converged';

function FlowRow({
  tone,
  icon,
  at,
  muted = false,
  title,
  tags,
  state,
  meta,
  hint,
  onOpen,
  focusKey,
}: {
  tone: FlowTone;
  icon?: IconName;
  at: string;
  muted?: boolean;
  title: ReactNode;
  tags?: ReactNode;
  state?: ReactNode;
  meta: ReactNode;
  hint?: string;
  onOpen?: () => void;
  focusKey?: string;
}) {
  return (
    <li
      className={`cgf-row ${tone}${muted ? ' muted' : ''}`}
      role={onOpen ? 'button' : undefined}
      tabIndex={onOpen ? 0 : undefined}
      data-route-focus={focusKey}
      title={hint}
      onClick={onOpen}
      onKeyDown={event => {
        if (!onOpen || event.target !== event.currentTarget) return;
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          onOpen();
        }
      }}
    >
      <When at={at} className="cgf-time" />
      <span className="cgf-node">
        <Icon of={icon ?? TONE_ICON[tone]} size={11} />
      </span>
      <span className="cgf-main">
        <b>{title}</b>
        {tags}
      </span>
      {state ? <span className={`cgf-state ${tone}`}>{state}</span> : <span />}
      <span className="cgf-meta">{meta}</span>
    </li>
  );
}

// 流内分组：未发布 / 进行中 / 已发布。第一格空出时刻与图标两列，标题与记录正文对齐。
function FlowGroup({
  label,
  tone,
  meta,
  action,
}: {
  label: string;
  tone?: 'warn';
  meta: ReactNode;
  action?: ReactNode;
}) {
  return (
    <li className="cgf-group">
      <span />
      <b className={tone}>{label}</b>
      <span className="cgf-gmeta">{meta}</span>
      {action}
    </li>
  );
}

/* ══ 发布页：页签 + 驾驶舱 ═════════════════════════════════════════════════════
   外层照机器详情页：页头是身份（图标板 + 状态灯 + 「发布」+ 线上/当前修订）│ 页签 │ 当前页的主操作。
   每个页签照用量页：左侧 300px 读数栏，右侧列表。配置页的列表是发布流水，Agent / Xray 是逐台机器表。
   稿件：mockups/deploy-cockpit.html。 */

type DeployTab = 'config' | 'agent' | 'xray';

const DEPLOY_TABS: { key: DeployTab; label: string; icon: IconName }[] = [
  { key: 'config', label: '配置', icon: 'config' },
  { key: 'agent', label: 'Agent', icon: 'agent' },
  { key: 'xray', label: 'Xray', icon: 'xray' },
];

type TabBadge = { tone: 'run' | 'gold'; text: string } | null;

function DeployOverview({ go, editable }: { go: (d: Drill) => void; editable: boolean }) {
  // 页签是一次浏览中的位置，不进地址栏（与机器详情页相同）。
  const [tab, setTab] = useState<DeployTab>('config');
  const [editing, setEditing] = useState<'agent' | 'xray' | null>(null);
  const tabIdBase = useId();
  useSyncExternalStore(draft.subscribe, draft.version);

  // 顶栏的发布状态也读 ['deployments']，这里与它共用同一查询键。
  const all = useQuery({ queryKey: ['deployments'], queryFn: () => fetchDeployments() });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  // 服务端拒绝不涉及任何机器的发布，因此此处同样应提前拦截。
  // 查询键与 ForgeShell 的 verify 一致，共享缓存不产生额外请求。
  const verify = useQuery({
    queryKey: ['deployment-verify', current],
    queryFn: () => verifyDeployment({ revision_id: current! }),
    enabled: current != null,
  });
  const agent = useAgentRelease();
  const xray = useXrayRelease();

  // 页头的角标与状态灯读全部三页的数据：任一组还在加载就挂载，会先画出「没有待处理」再改口。
  if (
    all.isPending ||
    revisions.isPending ||
    agent.releases.isPending ||
    agent.nodes.isPending ||
    xray.releases.isPending
  )
    return <Loading variant="deploy" />;
  // 历史与当前修订共同决定配置页的按钮是否可用。缺一项时继续渲染会把“未知”误当成
  // “没有活动发布”或“已收敛”。
  if (all.error || revisions.error) return <ErrorBox error={all.error ?? revisions.error} />;

  const deployments = all.data.deployments;
  const activeConfig = deployments.find(d => d.active && d.kind === 'config');
  const publishedBase = latestSuccessfulRevision(deployments.filter(item => item.kind === 'config'));
  const coveredRevision = activeConfig?.revision_id ?? publishedBase;
  const pendingRevisions = revisions.data.revisions.filter(
    revision =>
      revision.has_snapshot &&
      revision.status !== 'aborted' &&
      revision.id > (coveredRevision ?? 0) &&
      revision.id <= revisions.data.current_revision,
  );
  const draftDirty = !draft.isEmpty();
  const fleet = agent.nodes.data?.nodes ?? [];

  const liveDeployments = deployments.filter(d => d.active);
  const failing = liveDeployments.some(d => d.failed_targets > 0 || d.status === 'halted');
  const attention =
    pendingRevisions.length > 0 ||
    draftDirty ||
    liveDeployments.some(d => d.awaiting_confirmation) ||
    agent.counts.behind > 0 ||
    xray.counts.behind > 0;
  const running = liveDeployments.length > 0 || !!agent.active || !!xray.active;
  const lamp =
    failing || agent.active?.status === 'halted' || xray.active?.status === 'halted'
      ? { tone: 'err', why: '有发布失败或已暂停，需要处理' }
      : attention
        ? { tone: 'warn', why: '有待处理的发布' }
        : running
          ? { tone: 'run', why: '发布进行中' }
          : { tone: 'ok', why: '配置与软件版本均已收敛' };
  const badges: Record<DeployTab, TabBadge> = {
    config: pendingRevisions.length ? { tone: 'gold', text: String(pendingRevisions.length) } : null,
    agent: agentBadge(agent),
    xray: xrayBadge(xray),
  };

  const stopEditing = () => {
    agent.setSelected([]);
    agent.create.reset();
    xray.setSelected([]);
    xray.create.reset();
    setEditing(null);
  };
  const startEditing = (which: 'agent' | 'xray') => {
    stopEditing();
    setEditing(which);
  };
  const tabId = (key: DeployTab) => `${tabIdBase}-${key}-tab`;
  const panelId = `${tabIdBase}-panel`;
  const selectTab = (next: DeployTab) => {
    if (next === tab) return;
    stopEditing();
    setTab(next);
  };
  const moveTab = (event: ReactKeyboardEvent<HTMLButtonElement>, from: DeployTab) => {
    const index = DEPLOY_TABS.findIndex(item => item.key === from);
    let next: number | null = null;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % DEPLOY_TABS.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp')
      next = (index - 1 + DEPLOY_TABS.length) % DEPLOY_TABS.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = DEPLOY_TABS.length - 1;
    if (next === null) return;
    event.preventDefault();
    const key = DEPLOY_TABS[next].key;
    selectTab(key);
    window.requestAnimationFrame(() => document.getElementById(tabId(key))?.focus());
  };

  const noPermission = editable ? undefined : '需要系统管理员权限';
  const action =
    tab === 'config' ? (
      <PlanButton
        pending={current == null || verify.isPending || !!verify.error}
        changed={verify.data?.summary.changed_targets}
        onClick={() => go({ p: 'plan', key: randomKey() })}
      />
    ) : tab === 'agent' && agent.active ? (
      <button
        className="btn danger"
        type="button"
        disabled={!editable || agent.busy}
        title={noPermission}
        onClick={() => agent.setAskCancel(true)}
      >
        停止发布
      </button>
    ) : tab === 'agent' ? (
      editing === 'agent' || !agent.ready ? null : (
        <button
          className={agent.counts.behind ? 'btn primary' : 'btn'}
          type="button"
          disabled={!editable}
          title={noPermission}
          onClick={() => startEditing('agent')}
        >
          升级 Agent
        </button>
      )
    ) : xray.active ? (
      <button
        className="btn danger"
        type="button"
        disabled={!editable || xray.busy}
        title={noPermission}
        onClick={() => xray.setAskCancel(true)}
      >
        停止发布
      </button>
    ) : editing === 'xray' || !xray.ready ? null : (
      <button
        className={xray.counts.behind ? 'btn primary' : 'btn'}
        type="button"
        disabled={!editable}
        title={noPermission}
        onClick={() => startEditing('xray')}
      >
        升级 Xray
      </button>
    );
  // 页签内的取消自己放弃改动；批准成功后表单已按服务端结果重新对齐，这里只退出编辑。
  const setEditingFor = (which: 'agent' | 'xray') => (on: boolean) => setEditing(on ? which : null);

  return (
    <div className="nd-sheet nd-page cgc-page">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head">
          <div className="nd-page-identity">
            <span className="cg-plate">
              <Icon of="deploy" size={18} />
              <span className={`cg-lamp ${lamp.tone}`} role="img" aria-label={lamp.why} title={lamp.why} />
            </span>
            <div className="nd-ident-text">
              <div className="nd-ident-row">
                <h1 className="nd-id nd-name">发布</h1>
              </div>
              <span className="nd-ident-meta">
                线上 {publishedBase == null ? '—' : `R${publishedBase}`} · 当前 {current == null ? '—' : `R${current}`}
                {agent.nodes.data ? ` · ${fleet.length} 台机器` : ''}
              </span>
            </div>
          </div>
          <div className="nd-tabs" role="tablist" aria-label="发布页签">
            <div className="nd-tabs-seg">
              {DEPLOY_TABS.map(item => {
                const badge = badges[item.key];
                return (
                  <button
                    key={item.key}
                    type="button"
                    id={tabId(item.key)}
                    role="tab"
                    aria-selected={tab === item.key}
                    aria-controls={panelId}
                    tabIndex={tab === item.key ? 0 : -1}
                    onKeyDown={event => moveTab(event, item.key)}
                    onClick={() => selectTab(item.key)}
                  >
                    <Icon of={item.icon} size={14} className="nd-tab-ic" />
                    {item.label}
                    {/* 金色数字是待处理数，主题色是正在自行升级的进度。零时不画。 */}
                    {badge && (
                      <span className={`nd-tab-badge ${badge.tone === 'run' ? 'cgc-run' : 'gold'}`}>{badge.text}</span>
                    )}
                  </button>
                );
              })}
            </div>
          </div>
          {action && (
            <div className="nd-tools">
              <div className="nd-acts">{action}</div>
            </div>
          )}
        </header>
        <div className="nd-paper-body">
          <div className="usage-cockpit cgc-cockpit" role="tabpanel" id={panelId} aria-labelledby={tabId(tab)}>
            {tab === 'config' ? (
              <ConfigTab
                go={go}
                deployments={deployments}
                revisions={revisions.data}
                verify={verify}
                fleet={fleet}
                pendingRevisions={pendingRevisions}
                publishedBase={publishedBase}
                draftDirty={draftDirty}
              />
            ) : tab === 'agent' ? (
              <AgentReleaseTab
                agent={agent}
                editable={editable}
                editing={editing === 'agent'}
                onEditingChange={setEditingFor('agent')}
              />
            ) : (
              <XrayReleaseTab
                xray={xray}
                editable={editable}
                editing={editing === 'xray'}
                onEditingChange={setEditingFor('xray')}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function ConfigTab({
  go,
  deployments,
  revisions,
  verify,
  fleet,
  pendingRevisions,
  publishedBase,
  draftDirty,
}: {
  go: (d: Drill) => void;
  deployments: DeploymentListItem[];
  revisions: RevisionList;
  verify: UseQueryResult<Awaited<ReturnType<typeof verifyDeployment>>>;
  fleet: NodeAgentStateItem[];
  pendingRevisions: RevisionListItem[];
  publishedBase: number | null;
  draftDirty: boolean;
}) {
  const [kind, setKind] = useState<'all' | 'config' | 'grants'>('all');
  const [historyExpanded, setHistoryExpanded] = useState(false);
  // 不筛选时与页头共用 ['deployments']。筛选时先用已有记录在本地筛出同类占位，
  // 服务端结果返回后替换，切换类型时流水不会整块消失。
  const list = useQuery({
    queryKey: kind === 'all' ? ['deployments'] : ['deployments', kind],
    queryFn: () => fetchDeployments(kind === 'all' ? undefined : kind),
    placeholderData: kind === 'all' ? undefined : { deployments: deployments.filter(item => item.kind === kind) },
  });

  const current = revisions.current_revision;
  const targets = verify.data?.targets;
  const changed = targets?.filter(target => target.status !== 'skipped');
  const disruptive = changed?.filter(target => target.disruptive);
  const nodeById = new Map(fleet.map(node => [node.node_id, node]));
  const latestRevision = revisions.revisions.find(revision => revision.id === current);
  const lastConfig = deployments
    .filter(item => item.kind === 'config' && !item.active)
    .reduce<DeploymentListItem | null>((best, item) => (best === null || item.id > best.id ? item : best), null);

  // 进行中的单据单独成组，历史里不再重复它。默认只给出最近几条，
  // 完整账本在操作者主动展开后才出现。
  const actives = deployments.filter(d => d.active);
  const finished = (list.data?.deployments ?? []).filter(item => !item.active);
  const visibleItems = historyExpanded ? finished : finished.slice(0, HISTORY_PREVIEW_COUNT);
  const hiddenItems = finished.length - visibleItems.length;

  return (
    <>
      <ReleaseLedger
        label="待发布修订"
        value={pendingRevisions.length}
        unit="个"
        period={`线上 ${publishedBase == null ? '—' : `R${publishedBase}`} → 当前 R${current}${
          draftDirty ? ' · 草稿未提交，不在预览中' : ''
        }`}
        parts={[
          { tone: 'warn', label: '待变更', count: changed?.length ?? null },
          { tone: 'ok', label: '与当前修订一致', count: targets && changed ? targets.length - changed.length : null },
        ]}
        facts={[
          [
            '会中断连接',
            disruptive?.length ? (
              <>
                <FlagRun nodes={disruptive.map(target => nodeById.get(target.node_id))} />
                <em className="cgc-em">{disruptive.length} 台</em>
              </>
            ) : (
              '—'
            ),
          ],
          [
            '待变更机器',
            changed?.length ? <FlagRun nodes={changed.map(target => nodeById.get(target.node_id))} /> : '—',
          ],
          ['最近修订', latestRevision ? `R${latestRevision.id} · ${stamp(latestRevision.created_at)}` : '—'],
          ['上次发布', lastConfig ? `#${lastConfig.id} · ${stamp(lastConfig.created_at)}` : '—'],
        ]}
      />
      <section className="cgc-main">
        {verify.error && <ErrorBox error={verify.error} />}
        <div className="cgf-well" aria-busy={list.isPlaceholderData || undefined}>
          <ol className="cgf-list">
            {kind !== 'grants' && pendingRevisions.length > 0 && (
              <>
                <FlowGroup label="待发布" tone="warn" meta={`${pendingRevisions.length} 个修订`} />
                {pendingRevisions.map(revision => (
                  <FlowRow
                    key={`revision-${revision.id}`}
                    tone="idle"
                    muted
                    at={revision.created_at}
                    title={revision.note || '未填写修订说明'}
                    meta={`R${revision.id} · ${revision.author || '系统'}`}
                  />
                ))}
              </>
            )}

            {actives.map(item => (
              <LiveDeployment key={`live-${item.id}`} item={item} go={go} />
            ))}

            <FlowGroup
              label="发布记录"
              meta={finished.length === 0 ? '还没有发布记录' : `最近 ${visibleItems.length} 条`}
              action={
                <SegmentedControl
                  className="cgf-kind"
                  ariaLabel="按类型筛选"
                  value={kind}
                  options={(['all', 'config', 'grants'] as const).map(key => ({ value: key, label: KIND_LABEL[key] }))}
                  onChange={key => {
                    setKind(key);
                    setHistoryExpanded(false);
                  }}
                />
              }
            />
            {list.error ? (
              <li className="cgf-empty">
                <ErrorBox error={list.error} />
              </li>
            ) : finished.length === 0 ? (
              <li className="cgf-empty">
                <Empty>
                  {kind === 'grants'
                    ? '还没有自动化授权单。修改授权、停用或启用用户、轮换 UUID，以及额度自动调整都会落在这里。'
                    : kind === 'config'
                      ? '还没有变更单。'
                      : '还没有发布记录。'}
                </Empty>
              </li>
            ) : (
              visibleItems.map((item, index) => {
                const day = dayKey(item.created_at);
                const newDay = index === 0 || day !== dayKey(visibleItems[index - 1].created_at);
                return (
                  <Fragment key={item.id}>
                    {newDay && (
                      <li className="cgf-day">
                        <span>{day}</span>
                      </li>
                    )}
                    <RecordRow item={item} items={finished} go={go} />
                  </Fragment>
                );
              })
            )}
          </ol>

          {finished.length > HISTORY_PREVIEW_COUNT && (
            <div className="cgf-more">
              <button
                className="btn"
                type="button"
                aria-expanded={historyExpanded}
                onClick={() => setHistoryExpanded(expanded => !expanded)}
              >
                {historyExpanded ? `收起到最近 ${HISTORY_PREVIEW_COUNT} 条` : `查看其余 ${hiddenItems} 条`}
              </button>
            </div>
          )}
        </div>
      </section>
    </>
  );
}

// 单据一行。同一修订发布过多次时标出次序：列表按时间倒序排列，因此向后统计（更早的记录）。
function RecordRow({
  item,
  items,
  go,
}: {
  item: DeploymentListItem;
  items: DeploymentListItem[];
  go: (d: Drill) => void;
}) {
  const tries = items.filter(x => x.revision_id === item.revision_id && x.kind === item.kind);
  const nth = tries.length > 1 ? tries.length - tries.indexOf(item) : 0;
  const tone = deploymentTone(item);
  return (
    <FlowRow
      tone={tone}
      at={item.created_at}
      muted={item.kind === 'grants'}
      focusKey={`deployment:${item.id}`}
      hint={item.awaiting_confirmation ? `${item.status}：发布步骤在等人确认` : item.status}
      title={item.note ?? `修订 ${item.revision_id}`}
      tags={
        <>
          {item.kind === 'grants' && <em className="cgf-tag">自动化授权</em>}
          {item.rollback_of_deployment_id != null && (
            <em className="cgf-tag warn">回滚到 #{item.rollback_of_deployment_id}</em>
          )}
          {item.sync_of_deployment_id != null && <em className="cgf-tag warn">补推 #{item.sync_of_deployment_id}</em>}
        </>
      }
      state={settledRecord(item) ? undefined : <DeployStatus item={item} />}
      meta={
        <>
          #{item.id} · R{item.revision_id}
          {nth > 1 ? ` · 第 ${nth} 次` : ''} · {item.changed_targets} 台{item.actor ? ` · ${item.actor}` : ''}
        </>
      }
      onOpen={() => go({ p: 'detail', id: item.id })}
    />
  );
}

// 存在草稿时不禁用：verify 查询的是已提交的修订，此时结果为 0 属于正常，
// 而预览页的「不是当前修订」提示比禁用按钮表达得更明确。
function PlanButton({
  pending,
  changed,
  onClick,
}: {
  pending: boolean;
  changed: number | undefined;
  onClick: () => void;
}) {
  useSyncExternalStore(draft.subscribe, draft.version);
  const dirty = !draft.isEmpty();
  const nothing = !dirty && changed === 0;
  return (
    <button
      className={`btn${nothing ? '' : ' primary'}`}
      disabled={pending || nothing}
      title={nothing ? '所有机器的产物都已经是当前修订的样子' : dirty ? '预览的是已提交的修订，不含草稿' : ''}
      onClick={onClick}
    >
      {nothing ? '无待发布变更' : '审阅变更'}
    </button>
  );
}

// 用一条进度条替代原有的「变更/总数 · 失败 · 波次」三列。
// 该条表示本次发布的目标构成，而非实时进度——列表接口只返回总数、变更数、跳过数、
// 失败数，不包含已完成数量，因此不绘制推断出的进度。
// 当前执行到哪一步由左侧的状态标签表示（running 状态带动画）。
// 本次发布涉及哪些机器不在列表中显示。
// 此处此前有一条按 `changed/total` 填充的进度条，后改为「6 台中改 1 台」加点阵——
// 两个版本都在列表中占用一整列，而它们表示的是细节，不是浏览该表时需要的信息：
// 本页需要确定的是哪一条发布、是否成功、发布时间。机器数量、跳过和失败的逐台分布
// 都在详情页中。

// 状态的显示文案。含失败的取消需要与普通取消区分：前者是人工中止，
// 后者是推送失败后的收尾，排查时属于两种不同情况。
function DeployStatus({ item }: { item: DeploymentListItem }) {
  // 「等确认」的优先级高于「推送中」：该发布确实处于 running 状态，但停止推进的原因不是
  // 机器响应慢，而是缺少人工确认。显示为「推送中」会导致继续等待，而它不会自动继续。
  const text =
    item.activation_status === 'activated' && item.settlement_status !== 'converged'
      ? `已生效 · ${item.debt_targets} 台待补偿`
      : item.activation_status === 'activated'
        ? '已生效'
        : item.status === 'succeeded' && item.activation_status === 'waiting'
          ? '执行完成 · 待生效'
          : item.awaiting_confirmation
            ? '等确认'
            : item.status === 'canceled' && item.failed_targets > 0
              ? '失败后取消'
              : (STATUS_TEXT[item.status] ?? item.status);
  return text;
}

// 时间只占一行。绝对时间不再单独显示为一行小字：那会使每行高度增加一档，而该表的作用
// 在于一屏可浏览的记录数量。日期由上方按天分组的组标题表示，同一天内的具体时刻
// 写入 title（由 `Ago` 提供）——排查时悬停即可查看。
function When({ at, className = 'cgo-when' }: { at: string; className?: string }) {
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  const d = new Date(t);
  const pad = (n: number) => String(n).padStart(2, '0');
  return (
    <span className={className} title={Number.isNaN(t) ? at : d.toLocaleString()}>
      {Number.isNaN(t) ? at : `${pad(d.getHours())}:${pad(d.getMinutes())}`}
    </span>
  );
}

// 按天分组的组标题。今天 / 昨天 / 具体日期——连续列出时，
// 「25 分钟前」和「4 小时前」之间是否跨天无法判断。
function dayKey(at: string): string {
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(t)) return '';
  const d = new Date(t);
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  const days = Math.floor((midnight.getTime() - new Date(d).setHours(0, 0, 0, 0)) / 86_400_000);
  if (days <= 0) return '今天';
  if (days === 1) return '昨天';
  const pad = (n: number) => String(n).padStart(2, '0');
  return d.getFullYear() === new Date().getFullYear()
    ? `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
    : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

// 进行中的变更单：流水里单独一组——组头是这张单据与下一步操作，其下是分阶段步骤条。
// 步骤条按 wave 分段，一段收敛后才推送下一段；需要人工确认的那一段停在这里等按钮。
function LiveDeployment({ item, go }: { item: DeploymentListItem; go: (d: Drill) => void }) {
  const { who } = useSession();
  const nameOf = useNodeNames();
  const qc = useQueryClient();
  const [ask, setAsk] = useState(false);
  const detail = useQuery({
    queryKey: ['deployment', item.id, who.role === 'system-admin'],
    queryFn: () => fetchDeployment(item.id, '', who.role === 'system-admin'),
    enabled: item.kind === 'config' && Boolean(item.active),
  });
  const confirm = useMutation({
    mutationFn: (wave: number) => confirmWave(item.id, wave),
    onSuccess: () => {
      setAsk(false);
      qc.invalidateQueries({ queryKey: ['deployment', item.id] });
      qc.invalidateQueries({ queryKey: ['deployments'] });
    },
  });
  // 隔离的机器不参与本次发布（详情页把它们单列一段），步骤条按实际会执行的机器分段，
  // 否则含隔离机器的那一段永远不会变成「已完成」。
  const targets =
    detail.data?.targets.filter(target => target.status !== 'skipped' && target.status !== 'deferred') ?? [];
  const steps = deploymentSteps(targets, actionsOf);
  const live = targets.filter(target => LIVE_TARGET.has(target.status));
  const openWave = live.length ? Math.min(...live.map(target => target.wave)) : null;
  const openStep = steps.find(step => step.wave === openWave);
  const action = openStep ? deploymentStepAction(openStep, nameOf) : '继续发布';
  const kindLabel = item.kind === 'grants' ? '自动化授权单' : '变更单';
  return (
    <>
      <FlowGroup
        label="进行中"
        tone="warn"
        meta={
          <>
            {kindLabel} #{item.id}
            {item.note ? ` · ${item.note}` : ''} · R{item.base_revision_id ?? '—'} → R{item.revision_id} ·{' '}
            {item.changed_targets} 台{item.actor ? ` · ${item.actor}` : ''}
          </>
        }
        action={
          item.awaiting_confirmation && openStep ? (
            <button
              className="btn primary"
              type="button"
              disabled={!can(who.role, 'publish') || confirm.isPending}
              onClick={() => setAsk(true)}
            >
              {action}
            </button>
          ) : (
            <button className="btn" type="button" onClick={() => go({ p: 'detail', id: item.id })}>
              看详情
            </button>
          )
        }
      />
      {steps.length > 0 && (
        <li className="cgf-steps">
          <ol className="cgf-track" aria-label="发布步骤">
            {steps.map((step, index) => {
              const done = step.targets.every(target => target.status === 'succeeded');
              const open = step.wave === openWave;
              const tone = done ? 'ok' : !open ? '' : item.awaiting_confirmation ? 'wait' : 'run';
              const names = step.targets.map(target => nameOf(target.node_id));
              return (
                <li key={step.wave} className={tone}>
                  <span className="bar" />
                  <span className="name">
                    {done ? <Icon of="check" size={11} /> : open ? <Icon of="clock" size={11} /> : <em>{index + 1}</em>}
                    {step.steps > 1 ? `${step.title} ${step.step}/${step.steps}` : step.title}
                  </span>
                  <span className="target">
                    {names.length > 2 ? `${names.length} 台机器` : names.join('、')}
                    {open && item.awaiting_confirmation ? ' · 等待确认' : ''}
                  </span>
                </li>
              );
            })}
          </ol>
        </li>
      )}
      {ask && openStep && (
        <Confirm
          title={`${action}？`}
          body={<WaveSummary targets={openStep.targets} nameOf={nameOf} />}
          confirmLabel="确认并开始"
          onConfirm={() => confirm.mutate(openStep.wave)}
          onCancel={() => setAsk(false)}
        />
      )}
    </>
  );
}

function PlanPreview({
  revision,
  idempotencyKey,
  go,
}: {
  revision?: number;
  idempotencyKey: string;
  go: (d: Drill) => void;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  /* picked 只在重新预览当前修订时设置：用户操作优先于从路由传入的 revision 参数 */
  const [picked, setPicked] = useState<number | undefined>(undefined);
  const [note, setNote] = useState('');
  const target = picked ?? revision ?? revisions.data?.current_revision;
  const stale = revisions.data && target !== revisions.data.current_revision;

  const plan = useQuery({
    queryKey: ['plan', target],
    queryFn: () => planDeployment(target!),
    enabled: !!target,
    retry: false,
  });
  // Artifact indexes are part of the plan's initial review surface. Start them alongside the plan
  // request and keep the real plan root unmounted until both the target and its baseline are known.
  const artifactDiff = useRevisionDiff(target, plan.data?.base_revision_id);

  // 撤销该版本需要检查是否已有发布引用它——服务端会拒绝，但按钮本身不应显示：
  // 点击后必然报错的按钮比不提供该按钮更容易造成困惑。查询键与发布列表相同，
  // 命中缓存。
  const deployments = useQuery({ queryKey: ['deployments'], queryFn: () => fetchDeployments() });
  const [askAbort, setAskAbort] = useState(false);
  const abort = useMutation({
    mutationFn: (rev: number) => discardPendingChanges(rev),
    onSuccess: () => {
      // 模型整体回退一个版本，所有由模型派生的缓存都需要重新获取：编译视图、快照、
      // 机器列表（退役状态可能在该版本中修改）、以及 verify 的汇总结果——发布页的按钮
      // 依据它变为无可发布内容。
      for (const key of [
        ['revisions'],
        ['compile'],
        ['snapshot'],
        ['nodes'],
        ['deployments'],
        ['deployment-verify'],
        ['plan'],
      ])
        qc.invalidateQueries({ queryKey: key });
      /* 返回列表：预览的修订已被撤销，停留在本页显示的是一份已不存在的计划。 */
      returnTo('deploy');
    },
  });
  // 基线为最近一次成功发布对应的修订。按发布 id 排序（即时间顺序），不按修订号排序：
  // 回滚发布的修订号高于它回滚的那些，按修订号排序会将基线定位到更早的模型。
  const publishedBase = latestSuccessfulRevision(deployments.data?.deployments ?? []);
  /* 该区间内存在仍然有效的发布时不能丢弃——服务端使用同一判定。 */
  const blockedByDeployment =
    publishedBase != null &&
    target != null &&
    (deployments.data?.deployments ?? []).some(
      d => d.revision_id > publishedBase && d.revision_id <= target && d.status !== 'canceled',
    );
  const canAbort =
    target != null &&
    !stale &&
    publishedBase != null &&
    publishedBase < target &&
    !blockedByDeployment &&
    can(who.role, 'system');

  const create = useMutation({
    mutationFn: () =>
      // 空备注不发送，让服务端继续生成「修订内容 · 机器数」；只有操作者实际填写时才覆盖。
      createDeployment({
        revision_id: target!,
        idempotency_key: idempotencyKey,
        ...(note.trim() ? { note: note.trim() } : {}),
      }),
    onSuccess: res => {
      qc.invalidateQueries({ queryKey: ['deployments'] });
      qc.invalidateQueries({ queryKey: ['deployment-verify'] });
      go({ p: 'detail', id: res.deployment_id });
    },
  });

  if (revisions.error || deployments.error || plan.error || artifactDiff.error) {
    return <ErrorBox error={revisions.error ?? deployments.error ?? plan.error ?? artifactDiff.error} />;
  }
  const artifactsPending =
    target != null && (artifactDiff.pending || (plan.data?.base_revision_id != null && !artifactDiff.known));
  if (revisions.isPending || deployments.isPending || (target != null && plan.isPending) || artifactsPending)
    return <Loading variant="plan" />;
  if (!target) return <Empty>还没有可预览的修订。</Empty>;
  if (!plan.data) return <Loading variant="plan" />;

  const data = plan.data;
  const actingTargets = data.targets.filter(t => t.status !== 'skipped');
  const activeTargets = data.targets.filter(t => t.status === 'pending');
  const steps = deploymentSteps(activeTargets, target => target.actions);
  const stages = stageCount(steps);
  const confirmations = confirmationSummary(steps);
  const baseline = data.base_revision_id;
  const includedRevisions = revisions.data.revisions.filter(
    item => item.id > (baseline ?? 0) && item.id <= target && item.status !== 'aborted',
  );
  const defaultNote = `${includedRevisions.at(0)?.note || `修订 ${target}`} · ${data.summary.changed_targets} 台`;

  return (
    <div className="nd-sheet nd-page cg-page">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head cg-head">
          <div className="nd-page-identity">
            <span className="cg-plate">
              <Icon of="deploy" size={18} />
            </span>
            <div className="nd-ident-text">
              <div className="nd-ident-row">
                <h1 className="nd-id nd-name">创建变更单</h1>
                <span className={`st ${stale ? 'st-warn' : 'st-pending'}`}>{stale ? '预览已过期' : '预览'}</span>
              </div>
              <span className="nd-ident-meta">
                {baseline == null ? '空白环境' : `R${baseline}`} → R{target} · {includedRevisions.length} 个修订
              </span>
            </div>
          </div>
          <ol className="cg-steps" aria-label="发布流程">
            <li className="current">
              <i>1</i>
              <span>审阅计划</span>
            </li>
            <li>
              <i>2</i>
              <span>执行发布</span>
            </li>
            <li>
              <i>3</i>
              <span>生效</span>
            </li>
          </ol>
        </header>

        <div className="nd-paper-body cg-body">
          <main className="cg-main">
            {data.warnings.length > 0 && (
              <section className="panel config-panel cg-sec cg-warning-section">
                <header>
                  <PanelTitle of="warn">发布前提醒</PanelTitle>
                  <span className="cg-meta">{data.warnings.length} 项</span>
                </header>
                <div className="cg-warning-list">
                  {data.warnings.map((warning, index) => (
                    <article key={`${warning.code}:${warning.location}:${index}`}>
                      <span className="cg-warning-code">{warning.code}</span>
                      <div>
                        <b>{warning.location}</b>
                        <p>{warning.message}</p>
                      </div>
                    </article>
                  ))}
                </div>
              </section>
            )}

            <section className="panel config-panel cg-sec">
              <header>
                <PanelTitle of="deploy">执行计划</PanelTitle>
                <span className="cg-meta">
                  <span>
                    <b>{activeTargets.length}</b> 台 · <b>{stages}</b> 个发布阶段
                    {confirmations ? ` · ${steps.filter(step => step.needsConfirmation).length} 步需确认` : ''}
                  </span>
                </span>
              </header>
              <div className="cgr-notice">
                <span className="cg-lamp idle" />
                <span>
                  <b>创建时，系统会再次检查当前配置。</b>如果配置已更新，本次创建将取消，请重新预览。
                </span>
              </div>
              <PlanTargets targets={data.targets} />
            </section>

            <details className="panel config-panel cg-sec cg-disclosure cg-artifact-review">
              <summary>
                <PanelTitle of="artifacts">{baseline === target ? '运行状态产物' : '产物差异'}</PanelTitle>
                <span className="cg-meta">{baseline == null ? '全部新建' : `R${baseline} → R${target}`}</span>
                <span className="cgf-disclosure-state" aria-hidden="true">
                  <span>展开</span>
                  <span>收起</span>
                </span>
              </summary>
              <ArtifactChanges revision={target} base={baseline} targets={actingTargets} loadingVariant="plan" />
            </details>

            {(create.error || abort.error) && <ErrorBox error={create.error ?? abort.error} />}
          </main>

          <aside className="cg-aside" aria-label="变更单摘要">
            <section className="panel config-panel cg-sec">
              <header>
                <PanelTitle of="settings">摘要</PanelTitle>
              </header>
              <dl className="cg-kv">
                <dt>变更机器</dt>
                <dd>{data.summary.changed_targets} 台</dd>
                <dt>中断连接</dt>
                <dd className={data.summary.disruptive_targets ? 'warn' : 'dim'}>
                  {data.summary.disruptive_targets ? `${data.summary.disruptive_targets} 台` : '无'}
                </dd>
                <dt>需确认</dt>
                <dd className={confirmations ? 'warn' : 'dim'}>{confirmations || '无'}</dd>
                {data.summary.deferred_targets > 0 && (
                  <>
                    <dt>上线后补偿</dt>
                    <dd>{data.summary.deferred_targets} 台</dd>
                  </>
                )}
              </dl>
              <details className="cg-summary-revisions">
                <summary>查看包含的 {includedRevisions.length} 个修订</summary>
                <RevisionTrail revisions={includedRevisions} base={baseline} />
              </details>
            </section>
          </aside>
        </div>

        <footer className={`cg-foot ${stale ? 'is-stale' : ''}`} aria-label="创建发布单">
          <div className="cg-foot-state" aria-live="polite">
            <span className={`cg-lamp ${stale ? 'err' : 'ok'}`} />
            <span>
              <b>{stale ? `当前配置已更新到 R${revisions.data.current_revision}` : '创建变更单'}</b>
              <small>
                {stale
                  ? `这份预览仍是 R${target}，请重新生成计划。`
                  : `R${target} 为当前修订 · 共 ${stages} 个发布阶段 · ${confirmations ? `${confirmations}开始前需要确认` : '全程自动执行'}`}
              </small>
            </span>
          </div>
          <label className="cg-note">
            <span>备注</span>
            <input
              className="f"
              value={note}
              placeholder={defaultNote}
              disabled={!!stale}
              onChange={event => setNote(event.target.value)}
            />
          </label>
          {stale && (
            <button className="btn" onClick={() => setPicked(revisions.data.current_revision)}>
              重新预览
            </button>
          )}
          {canAbort && (
            <button className="btn danger" disabled={abort.isPending} onClick={() => setAskAbort(true)}>
              {abort.isPending ? '撤销中…' : '撤销计划'}
            </button>
          )}
          <button
            className="btn primary"
            disabled={!can(who.role, 'publish') || create.isPending || data.summary.changed_targets === 0 || !!stale}
            title={stale ? '预览已经过期' : data.summary.changed_targets === 0 ? '没有需要更新的机器' : ''}
            onClick={() => create.mutate()}
          >
            {create.isPending ? '创建中…' : '创建变更单'}
          </button>
        </footer>
      </div>

      {askAbort && target != null && publishedBase != null && (
        <Confirm
          title="撤销这次计划？"
          body={
            <>
              丢弃<b>上面这份 diff 的全部内容</b>——从已发布的修订 <b className="mono">{publishedBase}</b> 到现在的{' '}
              <b className="mono">{target}</b> 之间 <b>{target - publishedBase}</b>{' '}
              版改动，一次全撤。不是只撤最后一次提交。
              <br />
              这些改动一次都没发下去过，机器上跑的仍是修订 {publishedBase} 的产物——
              所以撤它不动任何机器，模型退回去就已经收敛，待发的变更归零。
              <br />
              撤完会记一个新修订号（内容等于修订 {publishedBase}），被丢掉的那些在历史里 标成 aborted。
            </>
          }
          confirmLabel="撤销计划"
          onConfirm={() => {
            setAskAbort(false);
            abort.mutate(target);
          }}
          onCancel={() => setAskAbort(false)}
        />
      )}
    </div>
  );
}

function PlanTargets({ targets }: { targets: PlannedTarget[] }) {
  const nameOf = useNodeNames();
  const activeTargets = targets.filter(target => target.status === 'pending');
  const deferredTargets = targets.filter(target => target.status === 'deferred');
  const skippedTargets = targets.filter(target => target.status === 'skipped');
  if (activeTargets.length === 0 && deferredTargets.length === 0 && skippedTargets.length === 0)
    return <Empty>这次没有目标：所有机器的产物都没变。</Empty>;
  const steps = deploymentSteps(activeTargets, target => target.actions);
  const waveOf = new Map(activeTargets.map(target => [target.node_id, target.wave]));
  const groups = steps.reduce<{ stage: DeploymentStage; title: string; steps: DeploymentStep<PlannedTarget>[] }[]>(
    (result, step) => {
      const previous = result.at(-1);
      if (previous?.stage === step.stage) previous.steps.push(step);
      else result.push({ stage: step.stage, title: step.title, steps: [step] });
      return result;
    },
    [],
  );

  const targetRows = (stepTargets: PlannedTarget[]) => (
    <>
      {stepTargets.map(target => (
        <div className="cgr-row" key={target.node_id}>
          <span className={`cg-lamp ${target.status === 'deferred' ? 'defer' : target.disruptive ? 'warn' : 'ok'}`} />
          <span className="cgo-main">
            <b title={target.node_id}>{nameOf(target.node_id)}</b>
            <small>
              {[target.node_id, ...target.actions.map(action => ACTION_LABEL[action])].join(' · ')}
              {(target.prerequisites ?? []).map(node => {
                const sameStep = waveOf.get(node) === target.wave;
                return (
                  <em
                    key={`wait:${node}`}
                    className="dep"
                    title={sameStep ? `与 ${node} 协同切换` : `等待 ${node} 收敛后再执行`}
                  >
                    {' · '}
                    {sameStep ? '协同切换' : '先等'} {nameOf(node)}
                  </em>
                );
              })}
            </small>
          </span>
          <span className={`cgr-effect ${target.disruptive ? 'risk' : ''}`}>
            {target.status === 'deferred'
              ? '机器已隔离，恢复服务后补发'
              : target.actions.map(action => ACTION_NOTE[action]).join('；')}
          </span>
        </div>
      ))}
    </>
  );

  const facts = (groupSteps: DeploymentStep<PlannedTarget>[]) => {
    const count = groupSteps.reduce((total, step) => total + step.targets.length, 0);
    const disruptive = groupSteps.some(step => step.disruptive);
    const gated = groupSteps.filter(step => step.needsConfirmation).length;
    return (
      <span className="cgr-facts">
        {count} 台 · {disruptive ? <em className="warn">中断连接</em> : '连接保持'} ·{' '}
        {gated ? <em className="warn">{gated > 1 ? `${gated} 步需确认` : '需确认'}</em> : '自动下发'}
      </span>
    );
  };

  return (
    <div className="cgr-waves">
      {groups.map(group => {
        const label =
          group.stage === 'config'
            ? '自动执行'
            : group.stage === 'verify'
              ? '先更新 1 台'
              : group.stage === 'rollout'
                ? '按依赖顺序更新其余机器'
                : '';
        if (group.stage === 'rollout' && group.steps.length > 1) {
          return (
            <div className="cgr-group cgr-stage" key={`${group.stage}:${group.steps[0].wave}`}>
              <div className="cgr-head">
                <b>{group.title}</b>
                <span className="lbl">{label}</span>
                {facts(group.steps)}
              </div>
              <div className="cgr-stage-steps">
                {group.steps.map(step => (
                  <div className="cgr-stage-step" key={step.wave}>
                    <div className="cgr-step-head">
                      <b>
                        步骤 {step.step}/{step.steps}
                      </b>
                      {facts([step])}
                    </div>
                    {targetRows(step.targets)}
                  </div>
                ))}
              </div>
            </div>
          );
        }
        const step = group.steps[0];
        return (
          <div className="cgr-group" key={`${group.stage}:${step.wave}`}>
            <div className="cgr-head">
              <b>{group.title}</b>
              {label && <span className="lbl">{label}</span>}
              {facts(group.steps)}
            </div>
            {targetRows(step.targets)}
          </div>
        );
      })}
      {deferredTargets.length > 0 && (
        <div className="cgr-group">
          <div className="cgr-head">
            <b>隔离待补偿</b>
            <span className="cgr-facts">{deferredTargets.length} 台 · 不参与本次发布</span>
          </div>
          {targetRows(deferredTargets)}
        </div>
      )}
      {skippedTargets.length > 0 && (
        <div className="cgr-group">
          <div className="cgr-head">
            <b>产物未变</b>
            <span className="cgr-facts">{skippedTargets.length} 台 · 跳过</span>
          </div>
          <div className="cgr-names">{skippedTargets.map(target => nameOf(target.node_id)).join(' · ')}</div>
        </div>
      )}
    </div>
  );
}

// 该发布对产物的修改内容。
// 产物是模型快照的纯函数，分别编译两个 revision 即可逐行比较，服务端无需额外计算。
// 基线是 base_revision_id——同类上一次成功推送的版本，而非上一个修订：
// 期间提交但未发布的修订不属于本次发布。
export function ArtifactChanges({
  revision,
  base,
  targets,
  loadingVariant = 'table',
}: {
  revision: number;
  base: number | null;
  // 详情页传入 DeploymentTargetDetail[]，预览页传入 PlannedTarget[]——只需要 node_id。
  // 本次发布的目标机器由调用方确定，此处只负责比较产物。
  targets: { node_id: string }[];
  loadingVariant?: LoadingVariant;
}) {
  const nameOf = useNodeNames();
  const { list, changed, known, pending, error } = useRevisionDiff(revision, base);

  if (error) return <ErrorBox error={error} />;
  if (pending || (base != null && !known)) return <Loading variant={loadingVariant} />;

  // 按机器筛选而非按产物类型筛选：这些机器上与基线不同的全部列出，包括 grants.json-rpc。
  // 它是运行时的名单，两类发布都可能包含——重启 xray 后需要重新加载该名单。
  const mine = new Set(targets.map(t => t.node_id));
  const nodeChanges = list.filter(
    a => a.target_kind === 'node' && mine.has(a.target_id) && (base == null || changed.has(entryId(a))),
  );
  // 订阅不通过发布下发，但发生变化时需要提示。按用户数统计而非文件数：
  // 一个用户对应 clash 和 uri 两份，显示为「2 份变更」会被理解为涉及两个用户。
  const subscriptionChanges = list.filter(a => a.target_kind === 'user' && (base == null || changed.has(entryId(a))));
  const usersWithSubscriptionChanges = [...new Set(subscriptionChanges.map(a => a.target_id))];
  const nodesWithChanges = [...new Set(nodeChanges.map(a => a.target_id))];
  const groups: ArtifactBrowserGroup[] = [
    ...nodesWithChanges.map(node => ({
      key: `node:${node}`,
      name: nameOf(node),
      entries: nodeChanges.filter(entry => entry.target_id === node),
      changeLabel: base == null ? '新建' : '变更',
    })),
    ...usersWithSubscriptionChanges.map(userKey => ({
      key: `user:${userKey}`,
      name: `用户 ${userKey.split(':').at(-1)}`,
      entries: subscriptionChanges.filter(entry => entry.target_id === userKey),
      changeLabel: base == null ? '新建' : '变更',
    })),
  ];

  return (
    <>
      {nodeChanges.length === 0 && (
        <div className="cgr-notice">
          <span className="cg-lamp idle" />
          <span>
            {base === revision && targets.length > 0
              ? '证书等运行状态已经变化，需要重新下发；创建后可在发布详情中查看实际文件记录'
              : base == null
                ? '第一次发布没有可新建的机器产物'
                : `与修订 ${base} 相比，上方 ${targets.length} 台机器的产物没有变化`}
            。
          </span>
        </div>
      )}
      {usersWithSubscriptionChanges.length > 0 && (
        <div className="cgr-notice">
          <span className="cg-lamp ok" />
          <span>
            用户订阅{base == null ? '新建' : '变更'} · {usersWithSubscriptionChanges.length} 人
          </span>
        </div>
      )}
      {groups.length > 0 && <RevisionArtifactBrowser groups={groups} revision={revision} base={base} />}
    </>
  );
}

type RecordedArtifact = { state?: string; sha256?: string; content?: string };
type ArtifactBrowserGroup = {
  key: string;
  name: string;
  entries: ArtifactIndexEntry[];
  changeLabel: string;
};

function RevisionArtifactBrowser({
  groups,
  revision,
  base,
}: {
  groups: ArtifactBrowserGroup[];
  revision: number;
  base: number | null;
}) {
  const entries = groups.flatMap(group => group.entries.map(entry => ({ group, entry })));
  const [picked, setPicked] = useState(() => (entries[0] ? entryId(entries[0].entry) : ''));
  const selected = entries.find(item => entryId(item.entry) === picked) ?? entries[0];
  if (!selected) return null;
  return (
    <div className="cg-files">
      <div className="cg-tree" aria-label="产物文件">
        {groups.map(group => (
          <div key={group.key}>
            <div className="cg-tree-group">
              <b>{group.name}</b>
              <span>
                {group.entries.length} 份{group.changeLabel}
              </span>
            </div>
            {group.entries.map(entry => (
              <button
                className="cg-tree-file"
                type="button"
                key={entryId(entry)}
                aria-current={entryId(selected.entry) === entryId(entry)}
                onClick={() => setPicked(entryId(entry))}
              >
                <span className="nm">{artifactFile(entry.artifact_kind)}</span>
                <span className="cg-fstate">{base == null ? '新增' : '修改'}</span>
              </button>
            ))}
          </div>
        ))}
      </div>
      <FileDiff entry={selected.entry} groupName={selected.group.name} revision={revision} base={base} />
    </div>
  );
}

function artifactRecord(value: unknown, key: string): RecordedArtifact | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const artifact = (value as Record<string, unknown>)[key];
  return artifact && typeof artifact === 'object' ? (artifact as RecordedArtifact) : undefined;
}

interface RecordedGrantClient {
  email: string;
  uuid: string;
  flow: string | null;
}

interface RecordedGrantChange extends RecordedGrantClient {
  key: string;
  tag: string;
  change: 'added' | 'removed' | 'updated';
}

/** Read only the fields needed for a safe historical comparison. UUID participates in equality
 * but is never rendered: it is a credential, whereas email and inbound tag are operational labels. */
function recordedGrantClients(value: unknown): Map<string, RecordedGrantClient & { tag: string }> | null {
  if (!value || typeof value !== 'object') return null;
  const grants = (value as Record<string, unknown>).grants;
  if (!grants || typeof grants !== 'object') return null;
  const state = (grants as Record<string, unknown>).state;
  if (state === 'disabled') return new Map();
  if (state !== 'present') return null;
  const inbounds = (grants as Record<string, unknown>).inbounds;
  if (!Array.isArray(inbounds)) return null;

  const clients = new Map<string, RecordedGrantClient & { tag: string }>();
  for (const inbound of inbounds) {
    if (!inbound || typeof inbound !== 'object') return null;
    const tag = (inbound as Record<string, unknown>).tag;
    const entries = (inbound as Record<string, unknown>).clients;
    if (typeof tag !== 'string' || !Array.isArray(entries)) return null;
    for (const client of entries) {
      if (!client || typeof client !== 'object') return null;
      const row = client as Record<string, unknown>;
      if (typeof row.email !== 'string' || typeof row.uuid !== 'string') return null;
      const flow = typeof row.flow === 'string' ? row.flow : null;
      clients.set(`${tag}\u0000${row.email}`, { tag, email: row.email, uuid: row.uuid, flow });
    }
  }
  return clients;
}

function hasRecordedAction(value: unknown, action: string): boolean {
  if (!value || typeof value !== 'object') return false;
  const actions = (value as Record<string, unknown>).actions;
  return Array.isArray(actions) && actions.includes(action);
}

function recordedGrantChanges(target: DeploymentTargetDetail): RecordedGrantChange[] | null {
  if (!hasRecordedAction(target.desired_structure, 'sync-grants')) return null;
  const before = recordedGrantClients(target.observed_before);
  const after = recordedGrantClients(target.observed_after);
  if (!before || !after) return [];

  return [...new Set([...before.keys(), ...after.keys()])]
    .sort((left, right) => left.localeCompare(right))
    .flatMap<RecordedGrantChange>(key => {
      const previous = before.get(key);
      const current = after.get(key);
      if (!previous && current) return [{ ...current, key, change: 'added' as const }];
      if (previous && !current) return [{ ...previous, key, change: 'removed' as const }];
      if (previous && current && (previous.uuid !== current.uuid || previous.flow !== current.flow)) {
        return [{ ...current, key, change: 'updated' as const }];
      }
      return [];
    });
}

function RecordedGrantChanges({ groups }: { groups: { key: string; name: string; changes: RecordedGrantChange[] }[] }) {
  return (
    <div className="cgr-waves cg-grant-changes" aria-label="运行时授权变更">
      {groups.map(group => (
        <section className="cgr-group" key={group.key}>
          <div className="cgr-head">
            <b>{group.name}</b>
            <span className="cgr-facts">{group.changes.length} 项授权变更</span>
          </div>
          {group.changes.map(change => {
            const label = change.change === 'added' ? '新增' : change.change === 'removed' ? '移除' : '更新';
            const tone = change.change === 'added' ? 'ok' : change.change === 'removed' ? 'err' : 'warn';
            return (
              <div className="cgr-row" key={change.key}>
                <span className={`cg-lamp ${tone}`} />
                <span className="cgo-main">
                  <b>{change.email}</b>
                  <small>{change.tag}</small>
                </span>
                <span className={`cgr-effect ${tone}`}>{label}</span>
              </div>
            );
          })}
        </section>
      ))}
    </div>
  );
}

/** 同修订也可能因证书等运行状态产生不同产物，历史详情必须使用发布时保存的内容。 */
export function RecordedArtifactChanges({ targets }: { targets: DeploymentTargetDetail[] }) {
  const nameOf = useNodeNames();
  const groups = targets.map(target => ({
    key: target.node_id,
    name: nameOf(target.node_id),
    grantChanges: recordedGrantChanges(target),
    files: ['phantun', 'wireguard', 'xray', 'hy2_port_hop'].flatMap(kind => {
      const after = artifactRecord(target.desired_structure, kind);
      const before = artifactRecord(target.observed_before, kind);
      if (!after || after.state === 'unmanaged') return [];
      if (before && after.state === 'present' && before.state === 'present' && after.sha256 === before.sha256)
        return [];
      if (before?.state === 'absent' && after.state === 'disabled') return [];
      return [{ key: `${target.node_id}:${kind}`, kind, before, after }];
    }),
  }));
  const files = groups.flatMap(group => group.files.map(file => ({ group, file })));
  const grantGroups = groups.flatMap(group =>
    group.grantChanges && group.grantChanges.length > 0
      ? [{ key: group.key, name: group.name, changes: group.grantChanges }]
      : [],
  );
  const hasGrantSync = groups.some(group => group.grantChanges !== null);
  const [picked, setPicked] = useState(() => files[0]?.file.key ?? '');
  const selected = files.find(item => item.file.key === picked) ?? files[0];

  return (
    <>
      {selected && (
        <div className="cg-files is-compact">
          <div className="cg-tree" aria-label="发布时保存的产物文件">
            {groups.map(group => (
              <div key={group.key}>
                <div className="cg-tree-group">
                  <b>{group.name}</b>
                  <span>{group.files.length} 份变更</span>
                </div>
                {group.files.map(file => (
                  <button
                    className="cg-tree-file"
                    type="button"
                    key={file.key}
                    aria-current={selected.file.key === file.key}
                    onClick={() => setPicked(file.key)}
                  >
                    <span className="nm">{artifactFile(file.kind)}</span>
                    <span
                      className={`cg-fstate ${file.before?.state === 'absent' ? 'new' : file.after.state === 'disabled' ? 'warn' : ''}`}
                    >
                      {file.before?.state === 'absent' ? '新增' : file.after.state === 'disabled' ? '停用' : '修改'}
                    </span>
                  </button>
                ))}
              </div>
            ))}
          </div>
          <RecordedFileDiff
            groupName={selected.group.name}
            kind={selected.file.kind}
            before={selected.file.before}
            after={selected.file.after}
          />
        </div>
      )}
      {grantGroups.length > 0 && <RecordedGrantChanges groups={grantGroups} />}
      {!selected && grantGroups.length === 0 && (
        <div className="cgr-notice">
          <span className="cg-lamp idle" />
          <span>
            {hasGrantSync
              ? '授权名单已同步，但机器没有提供可比较的执行前后记录。'
              : '没有文件内容变化；本次执行的重应用操作见上方动作记录。'}
          </span>
        </div>
      )}
    </>
  );
}

function RecordedFileDiff({
  groupName,
  kind,
  before,
  after,
}: {
  groupName: string;
  kind: string;
  before: RecordedArtifact | undefined;
  after: RecordedArtifact;
}) {
  const beforeText = before?.state === 'absent' ? '' : before?.content;
  const afterText = after.state === 'disabled' ? '' : after.content;
  if (beforeText === undefined || afterText === undefined) {
    return (
      <div className="cg-viewer">
        <ArtifactViewerHead groupName={groupName} kind={kind} state="不可比较" />
        <div className="note cg-artifact-unavailable">
          {before === undefined
            ? '执行前状态尚未记录，暂不能比较。'
            : '记录中的原文不可用或当前账号无权查看，暂不能显示逐行差异。'}
          <div>
            执行前：<code>{before?.sha256 ?? before?.state ?? '未知'}</code>
          </div>
          <div>
            本次目标：<code>{after.sha256 ?? after.state}</code>
          </div>
        </div>
      </div>
    );
  }
  const ops =
    before?.state === 'absent'
      ? afterText.split('\n').map((s, i) => ({ t: '+' as const, n: i + 1, s }))
      : after.state === 'disabled'
        ? beforeText.split('\n').map(s => ({ t: '-' as const, n: null, s }))
        : diffLines(beforeText, afterText);
  const counts = countChanges(ops);
  return (
    <div className="cg-viewer">
      <ArtifactViewerHead
        groupName={groupName}
        kind={kind}
        state={before?.state === 'absent' ? '新增' : after.state === 'disabled' ? '停用' : '修改'}
        counts={counts}
      />
      <div className="fg-code cg-diff">
        <table>
          <tbody>
            {collapseContext(ops, 3).map((row, i) =>
              row === null ? (
                <tr key={i} className="gap">
                  <td className="ln">⋯</td>
                  <td className="src" />
                </tr>
              ) : (
                <tr key={i} className={row.t === '+' ? 'add' : row.t === '-' ? 'del' : undefined}>
                  <td className="ln">{row.n ?? ''}</td>
                  <td
                    className="src"
                    dangerouslySetInnerHTML={{ __html: highlight(row.s, artifactFmt(kind)) || '&nbsp;' }}
                  />
                </tr>
              ),
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ArtifactViewerHead({
  groupName,
  kind,
  state,
  counts,
}: {
  groupName: string;
  kind: string;
  state: string;
  counts?: { add: number; del: number };
}) {
  return (
    <div className="cg-viewer-head">
      <span className="path">
        {groupName} / <b>{artifactFile(kind)}</b>
      </span>
      <span className="sp" />
      {counts && (
        <span className="fg-delta">
          <span className="add">+{counts.add}</span> <span className="del">−{counts.del}</span>
        </span>
      )}
      <span className="cg-fstate">{state}</span>
    </div>
  );
}

function FileDiff({
  entry,
  groupName,
  revision,
  base,
}: {
  entry: ArtifactIndexEntry;
  groupName: string;
  revision: number;
  base: number | null;
}) {
  const key = [entry.target_kind, entry.target_id, entry.artifact_kind] as const;
  const here = useQuery({
    queryKey: ['artifact', revision, ...key],
    queryFn: () => fetchArtifactContent(...key, revision),
  });
  const there = useQuery({
    queryKey: ['artifact', base, ...key],
    queryFn: () => fetchArtifactContent(...key, base ?? undefined),
    enabled: base != null,
  });

  const fmt = artifactFmt(entry.artifact_kind);
  if (here.isPending || (base != null && there.isPending))
    return (
      <div className="cg-viewer">
        <ArtifactViewerHead groupName={groupName} kind={entry.artifact_kind} state={base == null ? '新增' : '修改'} />
        <Loading variant="code" />
      </div>
    );
  if (here.error || (base != null && there.error))
    return (
      <div className="cg-viewer">
        <ArtifactViewerHead groupName={groupName} kind={entry.artifact_kind} state={base == null ? '新增' : '修改'} />
        <ErrorBox error={here.error ?? there.error} />
      </div>
    );

  const text = here.data.content ?? '';
  const before = there.data?.content ?? '';
  // `diffLines('', text)` 会把空串当成一行删除；首次发布应只有真正的新建行。
  const ops =
    base == null ? text.split('\n').map((s, i) => ({ t: '+' as const, n: i + 1, s })) : diffLines(before, text);
  const counts = countChanges(ops);
  /* 产物有数百行，全部展开会使改动内容难以定位。 */
  const shown = base == null ? ops : collapseContext(ops, 3);

  return (
    <div className="cg-viewer">
      <ArtifactViewerHead
        groupName={groupName}
        kind={entry.artifact_kind}
        state={base == null ? '新增' : '修改'}
        counts={counts}
      />
      <div className="fg-code cg-diff">
        <table>
          <tbody>
            {shown.map((row, i) =>
              row === null ? (
                <tr key={`gap-${i}`} className="gap">
                  <td className="ln">⋯</td>
                  <td className="src" />
                </tr>
              ) : (
                <tr key={i} className={row.t === '+' ? 'add' : row.t === '-' ? 'del' : undefined}>
                  <td className="ln">{row.n ?? ''}</td>
                  <td className="src" dangerouslySetInnerHTML={{ __html: highlight(row.s, fmt) || '&nbsp;' }} />
                </tr>
              ),
            )}
          </tbody>
        </table>
      </div>
      {here.data.redacted && (
        <div className="fg-afoot">
          <span className="st st-warn">已打码</span>
          <span className="note">私钥原文仅 system-admin 可见。</span>
        </div>
      )}
    </div>
  );
}

/** 将未改动的长段落折叠为一个省略行，改动行前后各保留 `pad` 行。`null` 表示被折叠的段落。 */
function collapseContext<T extends { t: ' ' | '-' | '+' }>(ops: T[], pad: number): (T | null)[] {
  const keep = new Set<number>();
  ops.forEach((op, i) => {
    if (op.t === ' ') return;
    for (let j = Math.max(0, i - pad); j <= Math.min(ops.length - 1, i + pad); j++) keep.add(j);
  });
  const out: (T | null)[] = [];
  let gap = false;
  ops.forEach((op, i) => {
    if (keep.has(i)) {
      out.push(op);
      gap = false;
    } else if (!gap) {
      out.push(null);
      gap = true;
    }
  });
  return out;
}

const OPEN_STATES = new Set(['planned', 'running', 'dispatched', 'converging']);
// 熔断只停止波次推进，不释放限流锁（active 仍为 TRUE），因此熔断后仍需支持取消——
// 取消是将进行中的 target 置为 canceled 并释放锁的唯一方式
// （brocade-store 允许 planned/running/halted 状态执行取消）。
const CANCELABLE = new Set([...OPEN_STATES, 'halted']);
/* target 尚未进入终态表示该波仍处于打开状态（与服务端 current_wave 的判定一致） */
const LIVE_TARGET = new Set(['pending', 'dispatched', 'converging']);

const actionsOf = (t: DeploymentTargetDetail): string[] => {
  const actions = (t.desired_structure as { actions?: unknown } | null)?.actions;
  return Array.isArray(actions) ? (actions as string[]) : [];
};

/* 待确认的危险操作。确认框渲染在页面底部，同时只显示一个。 */
type Ask =
  | { kind: 'halt' }
  | { kind: 'cancel' }
  | { kind: 'cancelRollback' }
  | { kind: 'rollback' }
  | { kind: 'wave'; wave: number };

const ISOLATABLE_TARGET = new Set(['pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty']);

function DeploymentStages({
  steps,
  openWave,
  halted,
  publisher,
  system,
  retryPending,
  retryingNode,
  onRetry,
  onIsolate,
}: {
  steps: DeploymentStep<DeploymentTargetDetail>[];
  openWave: number | null;
  halted: boolean;
  publisher: boolean;
  system: boolean;
  retryPending: boolean;
  retryingNode: string | undefined;
  onRetry: (nodeId: string) => void;
  onIsolate: (target: DeploymentTargetDetail) => void;
}) {
  const groups = steps.reduce<
    { stage: DeploymentStage; title: string; steps: DeploymentStep<DeploymentTargetDetail>[] }[]
  >((result, step) => {
    const previous = result.at(-1);
    if (previous?.stage === step.stage) previous.steps.push(step);
    else result.push({ stage: step.stage, title: step.title, steps: [step] });
    return result;
  }, []);

  const stateOf = (step: DeploymentStep<DeploymentTargetDetail>) => {
    const isOpen = openWave === step.wave;
    const canConfirm = isOpen && !halted && step.needsConfirmation;
    const queued = openWave !== null && step.wave > openWave;
    const succeeded = step.targets.every(target => target.status === 'succeeded');
    const failed = step.targets.some(target => target.status.startsWith('failed'));
    return {
      isOpen,
      tone: failed ? 'err' : canConfirm ? 'warn' : succeeded ? 'ok' : '',
      label: succeeded
        ? '已完成'
        : failed
          ? '执行失败'
          : canConfirm
            ? '等待确认'
            : isOpen && !halted
              ? '进行中 · 等待 Agent'
              : queued
                ? '等待前一步完成'
                : halted
                  ? '已停止'
                  : '等待执行',
    };
  };
  const targetRows = (step: DeploymentStep<DeploymentTargetDetail>) => (
    <>
      {step.targets.map(target => (
        <TargetRow
          key={target.node_id}
          t={target}
          publisher={publisher}
          system={system}
          retryPending={retryPending}
          retrying={retryPending && retryingNode === target.node_id}
          onRetry={() => onRetry(target.node_id)}
          onIsolate={() => onIsolate(target)}
        />
      ))}
    </>
  );
  const stageDescription = (stage: DeploymentStage) =>
    stage === 'config'
      ? '自动下发，不中断现有连接'
      : stage === 'verify'
        ? '先验证一台机器，再继续扩大范围'
        : stage === 'stop'
          ? '停止服务前需要明确确认'
          : '按监听依赖顺序更新其余机器';

  return (
    <div className="cgr-waves">
      {groups.map(group => {
        const single = group.steps.length === 1 ? stateOf(group.steps[0]) : null;
        const singleClass =
          single?.tone === 'err' ? 'is-fail' : single?.tone === 'warn' ? 'is-open' : single?.isOpen ? 'is-run' : '';
        return (
          <section className={`cgr-group cgr-stage ${singleClass}`} key={`${group.stage}:${group.steps[0].wave}`}>
            <div className="cgr-head">
              <b>{group.title}</b>
              <span className="lbl">{stageDescription(group.stage)}</span>
              <span className={`cgr-state ${single?.tone ?? ''}`}>
                {single?.label ?? `${group.steps.length} 个步骤`}
              </span>
            </div>
            {group.steps.length === 1 ? (
              targetRows(group.steps[0])
            ) : (
              <div className="cgr-stage-steps">
                {group.steps.map(step => {
                  const state = stateOf(step);
                  const stateClass =
                    state.tone === 'err' ? 'is-fail' : state.tone === 'warn' ? 'is-open' : state.isOpen ? 'is-run' : '';
                  return (
                    <section className={`cgr-stage-step ${stateClass}`} key={step.wave}>
                      <div className="cgr-step-head">
                        <b>
                          步骤 {step.step}/{step.steps}
                        </b>
                        <span className={`cgr-state ${state.tone}`}>{state.label}</span>
                      </div>
                      {targetRows(step)}
                    </section>
                  );
                })}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
}

function Detail({ id, go }: { id: number; go: (d: Drill) => void }) {
  const nameOf = useNodeNames();
  const { who } = useSession();
  const qc = useQueryClient();
  const [ask, setAsk] = useState<Ask | null>(null);
  // 已成功确认的波次。confirm 只记录确认状态，不关闭波次
  // （deployment.rs::confirm_deployment_wave），target 需要等 agent 拉取后才离开 live 集合，
  // openWave 不会立即前移——不记录该状态时，按钮在请求成功后到状态更新之间仍可点击，
  // 同一个波会被确认两次。
  const [confirmedWave, setConfirmedWave] = useState<number | null>(null);
  const detail = useQuery({
    queryKey: ['deployment', id, who.role === 'system-admin'],
    queryFn: () => fetchDeployment(id, '', who.role === 'system-admin'),
    /* agent 采用拉取模型并异步回报，前端通过轮询获取进度 */
    refetchInterval: q =>
      OPEN_STATES.has((q.state.data?.status ?? '') as string) || q.state.data?.settlement_status === 'debt'
        ? 3_000
        : false,
  });

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['deployment', id] });
    qc.invalidateQueries({ queryKey: ['deployments'] });
    qc.invalidateQueries({ queryKey: ['deployment-verify'] });
  };
  const confirm = useMutation({
    mutationFn: (wave: number) => confirmWave(id, wave),
    onSuccess: (_res, wave) => {
      setConfirmedWave(wave);
      refresh();
    },
  });
  const halt = useMutation({ mutationFn: () => haltDeployment(id), onSuccess: refresh });
  const cancel = useMutation({
    mutationFn: () => cancelDeployment(id),
    onSuccess: () => {
      refresh();
    },
  });
  const cancelRollback = useMutation({
    mutationFn: () => cancelRollbackDeployment(id),
    onSuccess: res => {
      refresh();
      if (res.rollback_deployment_id) go({ p: 'detail', id: res.rollback_deployment_id });
    },
  });
  const rollback = useMutation({
    mutationFn: () =>
      createRollback({
        target_deployment_id: id,
        idempotency_key: randomKey(),
        note: `restore model snapshot from deployment #${id}`,
      }),
    onSuccess: res => {
      refresh();
      go({ p: 'detail', id: res.deployment_id });
    },
  });
  const retry = useMutation({ mutationFn: (node: string) => retryTarget(id, node), onSuccess: refresh });
  const isolate = useMutation({
    mutationFn: (target: DeploymentTargetDetail) =>
      isolateDeploymentTarget(id, target.node_id, {
        expected_target_status: target.status,
        acknowledge_uncertain: target.status !== 'pending',
      }),
    onSuccess: () => {
      refresh();
      qc.invalidateQueries({ queryKey: ['nodes'] });
    },
  });

  if (detail.isPending) return <Loading variant="deployment" />;
  if (detail.error) return <ErrorBox error={detail.error} />;

  const d = detail.data;
  // 只列出实际有操作的机器。十余行 skipped 会使实际执行的行难以定位，
  // 而本次发布涉及哪些机器正是详情页的主要内容。总数在列表页的 changed/total 中。
  const acting = d.targets.filter(t => t.status !== 'skipped');
  const steps = deploymentSteps(acting, actionsOf);
  const publisher = can(who.role, 'publish');
  const dirty = d.targets.filter(t => t.status === 'failed-dirty');
  const failed = d.targets.filter(t => t.status.startsWith('failed'));
  const live = d.targets.filter(t => LIVE_TARGET.has(t.status));
  const openWave = live.length ? Math.min(...live.map(t => t.wave)) : null;
  const openStep = steps.find(step => step.wave === openWave);
  const openAction = openStep ? deploymentStepAction(openStep, nameOf) : null;
  const askedStep = ask?.kind === 'wave' ? steps.find(step => step.wave === ask.wave) : undefined;
  const finishedTargets = acting.filter(target => target.status === 'succeeded').length;
  const finishedSteps = steps.filter(step => step.targets.every(target => target.status === 'succeeded')).length;
  const disruptiveTargets = acting.filter(target => target.disruptive).length;
  const executionDone = d.status === 'succeeded';
  const effective = d.activation_status === 'activated';
  const detailTone =
    d.status === 'halted' || failed.length > 0
      ? 'err'
      : effective
        ? 'ok'
        : openStep?.needsConfirmation
          ? 'warn'
          : OPEN_STATES.has(d.status)
            ? 'run'
            : 'idle';

  return (
    <>
      <div className="nd-sheet nd-page cg-page cg-detail">
        <div className="fg-sheet nd-paper">
          <header className="nd-page-head cg-head">
            <div className="nd-page-identity">
              <span className="cg-plate">
                <Icon of="deploy" size={18} />
                <span className={`cg-lamp ${detailTone}`} />
              </span>
              <div className="nd-ident-text">
                <div className="nd-ident-row">
                  <h1 className="nd-id nd-name">变更单 #{d.id}</h1>
                  <Status value={d.status} />
                </div>
                <span className="nd-ident-meta">
                  {d.note || '未填写备注'} · {d.base_revision_id == null ? '空白环境' : `R${d.base_revision_id}`} → R
                  {d.revision_id}
                </span>
              </div>
            </div>
            <ol className="cg-steps" aria-label="发布流程">
              <li className="done">
                <i>1</i>
                <span>审阅计划</span>
              </li>
              <li className={executionDone ? 'done' : 'current'}>
                <i>2</i>
                <span>
                  执行发布
                  <small>
                    {finishedSteps}/{steps.length} 步
                  </small>
                </span>
              </li>
              <li className={effective ? 'done' : executionDone ? 'current' : ''}>
                <i>3</i>
                <span>生效</span>
              </li>
            </ol>
            <div className="nd-acts">
              {openStep?.needsConfirmation && openWave === openStep.wave && d.status !== 'halted' && (
                <button
                  className="btn primary"
                  disabled={!publisher || confirm.isPending || confirmedWave === openStep.wave}
                  onClick={() => setAsk({ kind: 'wave', wave: openStep.wave })}
                >
                  {confirmedWave === openStep.wave ? '已确认' : openAction}
                </button>
              )}
              {OPEN_STATES.has(d.status) && (
                <button
                  className="btn danger"
                  disabled={!publisher || halt.isPending}
                  onClick={() => setAsk({ kind: 'halt' })}
                >
                  熔断
                </button>
              )}
              {(CANCELABLE.has(d.status) || d.status === 'succeeded') && (
                <details className="cg-menu-wrap">
                  <summary className="btn" aria-label="更多操作">
                    <Icon of="more" size={16} />
                  </summary>
                  <div className="cg-menu" role="menu">
                    {CANCELABLE.has(d.status) && (
                      <button type="button" role="menuitem" onClick={() => setAsk({ kind: 'cancel' })}>
                        <b>取消变更单</b>
                        <span>停止本次发布，已完成的机器保持新配置</span>
                      </button>
                    )}
                    {CANCELABLE.has(d.status) && (
                      <button type="button" role="menuitem" onClick={() => setAsk({ kind: 'cancelRollback' })}>
                        <b>取消并回滚</b>
                        <span>停止本单并强制同步到运行基线</span>
                      </button>
                    )}
                    {d.status === 'succeeded' && (
                      <button type="button" role="menuitem" onClick={() => setAsk({ kind: 'rollback' })}>
                        <b>恢复到这次快照</b>
                        <span>恢复这次发布的模型并创建同步单</span>
                      </button>
                    )}
                  </div>
                </details>
              )}
            </div>
          </header>

          <div className="nd-paper-body cg-body">
            <main className="cg-main">
              {d.status === 'halted' && (
                <div className="cgr-notice err">
                  <span className="cg-lamp err" />
                  <span>
                    <b>发布已熔断。</b>
                    {dirty.length > 0
                      ? `${dirty.map(target => nameOf(target.node_id)).join('、')} 的现场状态无法确认。`
                      : failed.length > 0
                        ? `${failed.map(target => nameOf(target.node_id)).join('、')} 收敛失败，后续发布已停止。`
                        : '人工停止了后续发布，已经成功的机器保持现状。'}
                    {failed.length > 0 ? '修复机器后可以逐台重试；' : ''}也可以取消这张变更单后重新发布。
                  </span>
                </div>
              )}

              <section className="panel config-panel cg-sec">
                <header>
                  <PanelTitle of="deploy">执行进度</PanelTitle>
                  <span className="cg-meta">
                    {finishedTargets} / {acting.length} 台完成
                  </span>
                </header>
                <DeploymentStages
                  steps={steps}
                  openWave={openWave}
                  halted={d.status === 'halted'}
                  publisher={publisher}
                  system={can(who.role, 'system') && !isolate.isPending}
                  retryPending={retry.isPending}
                  retryingNode={retry.variables}
                  onRetry={nodeId => retry.mutate(nodeId)}
                  onIsolate={target => isolate.mutate(target)}
                />
              </section>

              <details className="panel config-panel cg-sec cg-disclosure cg-artifact-review">
                <summary>
                  <PanelTitle of="artifacts">产物记录</PanelTitle>
                  <span className="cg-meta">
                    {d.base_revision_id == null ? '空白环境' : `R${d.base_revision_id}`} → R{d.revision_id} ·{' '}
                    {acting.length} 台机器
                  </span>
                  <span className="cgf-disclosure-state" aria-hidden="true">
                    <span>展开</span>
                    <span>收起</span>
                  </span>
                </summary>
                <RecordedArtifactChanges targets={acting} />
              </details>

              {(confirm.error ||
                halt.error ||
                cancel.error ||
                cancelRollback.error ||
                rollback.error ||
                retry.error ||
                isolate.error) && (
                <ErrorBox
                  error={
                    confirm.error ??
                    halt.error ??
                    cancel.error ??
                    cancelRollback.error ??
                    rollback.error ??
                    retry.error ??
                    isolate.error
                  }
                />
              )}
            </main>

            <aside className="cg-aside" aria-label="变更单信息">
              <section className="panel config-panel cg-sec">
                <header>
                  <PanelTitle of="deploy">单据</PanelTitle>
                </header>
                <dl className="cg-kv">
                  <dt>状态</dt>
                  <dd>{STATUS_TEXT[d.status] ?? d.status}</dd>
                  <dt>运行基线</dt>
                  <dd>{d.base_revision_id == null ? '空白环境' : `R${d.base_revision_id}`}</dd>
                  <dt>发布目标</dt>
                  <dd className="act">R{d.revision_id}</dd>
                  <dt>影响机器</dt>
                  <dd>{acting.length} 台</dd>
                  <dt>会中断连接</dt>
                  <dd className={disruptiveTargets ? 'warn' : 'dim'}>
                    {disruptiveTargets ? `${disruptiveTargets} 台` : '无'}
                  </dd>
                  <dt>发起人</dt>
                  <dd>{d.actor ?? '—'}</dd>
                  <dt>创建时间</dt>
                  <dd>
                    <Ago at={d.created_at} />
                  </dd>
                </dl>
                <div className="chipline cg-detail-statuses">
                  {effective && d.settlement_status === 'debt' && (
                    <span className="st st-gold">已生效 · {d.debt_targets} 台待补偿</span>
                  )}
                  {effective && d.settlement_status === 'converged' && <span className="st st-succeeded">已生效</span>}
                  {executionDone && !effective && <span className="st st-gold">执行完成 · 待生效</span>}
                  {d.settlement_status === 'uncertain' && <span className="st st-warn">补偿状态待确认</span>}
                  {d.rollback_of_deployment_id && (
                    <span className="st st-warn">回滚到 #{d.rollback_of_deployment_id}</span>
                  )}
                  {d.sync_of_deployment_id && <span className="st st-gold">补推 #{d.sync_of_deployment_id}</span>}
                </div>
              </section>
            </aside>
          </div>
        </div>
      </div>

      {/* 危险操作确认。发布步骤确认附带中断影响摘要；回滚类操作需要输入「回滚」才能执行。 */}
      {ask?.kind === 'halt' && (
        <Confirm
          title="熔断这次发布？"
          body={<>停住后续发布，已成功的机器保持成功。要彻底收尾再点「取消」。</>}
          confirmLabel="熔断"
          onConfirm={() => {
            setAsk(null);
            halt.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'cancel' && (
        <Confirm
          title="取消这次发布？"
          body={
            <>
              只停止这次发布：在途 target 转 canceled。已经 dispatched / converging 但没回报的， 现场会被标成
              dirty——那台机器现在什么状态没人知道。
            </>
          }
          confirmLabel="取消发布"
          onConfirm={() => {
            setAsk(null);
            cancel.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'cancelRollback' && (
        <Confirm
          title="取消并回滚？"
          body={
            <>
              system-admin 操作。取消当前发布，并强制覆盖成它之前最近一次成功发布的快照，
              创建强制同步工单——机器会按旧快照收敛。
            </>
          }
          confirmLabel="取消并回滚"
          requireWord="回滚"
          onConfirm={() => {
            setAsk(null);
            cancelRollback.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'rollback' && (
        <Confirm
          title="恢复到这次快照？"
          body={<>system-admin 操作。恢复这次发布对应的模型快照，并创建强制同步工单—— 机器会回到发布前的样子。</>}
          confirmLabel="恢复快照"
          requireWord="回滚"
          onConfirm={() => {
            setAsk(null);
            rollback.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'wave' && (
        <Confirm
          title={`${askedStep ? deploymentStepAction(askedStep, nameOf) : '继续发布'}？`}
          body={<WaveSummary targets={d.targets.filter(t => t.wave === ask.wave)} nameOf={nameOf} />}
          confirmLabel="确认并开始"
          onConfirm={() => {
            setAsk(null);
            confirm.mutate(ask.wave);
          }}
          onCancel={() => setAsk(null)}
        />
      )}
    </>
  );
}

// 确认波次前先展示该波的中断影响。数据全部在 Detail 中，只是平时需要自行读完表格——
// 确认弹窗中集中呈现。
/* 该波需要确认的是影响范围和是否中断，而非逐台核对。
   因此按动作聚合而非逐台列出——一波九十台时该表有九十行，
   逐行读完仍无法得出中断的机器数量，需要自行统计。
   聚合后可直接得出该结果。
   逐台明细在页面的波次表中已有，关闭该框即可查看。 */
function WaveSummary({ targets, nameOf }: { targets: DeploymentTargetDetail[]; nameOf: (id: string) => string }) {
  const actionCount = targets.reduce((n, t) => n + actionsOf(t).length, 0);
  const disruptive = targets.filter(t => t.disruptive);

  // 动作到机器数的映射。按机器数排序，影响最大的排在前面（重写 xray 的机器数通常最多）。
  const byAction = new Map<string, number>();
  for (const t of targets) for (const a of actionsOf(t)) byAction.set(a, (byAction.get(a) ?? 0) + 1);
  const actions = [...byAction.entries()].sort((a, b) => b[1] - a[1]);

  // 只显示部分机器名。显示若干个用于确认是否为预期的机器范围，而非用于逐一核对。
  const SHOW = 6;
  const shown = targets.slice(0, SHOW).map(t => nameOf(t.node_id));
  const rest = targets.length - shown.length;

  return (
    <>
      <p>
        这一步 <b>{targets.length}</b> 台机器、{actionCount} 个动作。
        {disruptive.length > 0 ? (
          <>
            其中 <b>{disruptive.length}</b> 台是破坏性的 —— 会断连接，确认后立刻执行。
          </>
        ) : (
          '不会中断连接，机器可以并行更新。'
        )}
      </p>
      <table className="cg-sum">
        <tbody>
          {actions.map(([a, n]) => (
            <tr key={a}>
              <td>
                <span className="cg-act">{(ACTION_LABEL as Record<string, string>)[a] ?? a}</span>{' '}
                <b className="mono">×{n}</b>
              </td>
              <td>{(ACTION_NOTE as Record<string, string>)[a]}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="cg-dim">
        {shown.join('、')}
        {rest > 0 && ` 等 ${targets.length} 台`}
        {rest > 0 && <>（逐台明细在下面的执行表里）</>}
      </p>
    </>
  );
}

function TargetRow({
  t,
  publisher,
  system,
  retryPending,
  retrying,
  onRetry,
  onIsolate,
}: {
  t: DeploymentTargetDetail;
  publisher: boolean;
  system: boolean;
  retryPending: boolean;
  retrying: boolean;
  onRetry: () => void;
  onIsolate: () => void;
}) {
  const nameOf = useNodeNames();
  const guide = t.error ? guideFor(t.error) : undefined;
  const tone = t.status.startsWith('failed')
    ? 'err'
    : t.status === 'succeeded'
      ? 'ok'
      : LIVE_TARGET.has(t.status)
        ? 'run'
        : 'idle';
  const canRetry = t.status.startsWith('failed');
  const canIsolate = ISOLATABLE_TARGET.has(t.status);
  return (
    <div className="cgr-row is-run">
      <span className={`cg-lamp ${tone}`} />
      <span className="cgo-main">
        <b title={t.node_id}>{nameOf(t.node_id)}</b>
        <small>
          {[t.node_id, ...actionsOf(t).map(action => (ACTION_LABEL as Record<string, string>)[action] ?? action)].join(
            ' · ',
          )}
        </small>
      </span>
      <span className={`cgo-state ${tone}`}>{STATUS_TEXT[t.status] ?? t.status}</span>
      <span className="cgo-when">{t.dispatched_at ? <Ago at={t.dispatched_at} /> : '—'}</span>
      {(t.error || canRetry || canIsolate) && (
        <div className="cgr-fail">
          {t.error && <code>{t.error}</code>}
          {t.error && guide && (
            <p>
              <span>处理方式</span>
              {guide}
              {t.status === 'failed-dirty' ? ' 这台机器当前状态未知，请先登录确认。' : ''}
            </p>
          )}
          {(canRetry || canIsolate) && (
            <div className="ops">
              {canRetry && (
                <button className="btn" disabled={!publisher || retryPending} onClick={onRetry}>
                  {retrying ? '重试中…' : '重试'}
                </button>
              )}
              {canIsolate && (
                <button className="btn danger" disabled={!system} onClick={onIsolate}>
                  隔离
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
