import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  applyDraft,
  checkpointOnboarding,
  completeOnboarding,
  createDeployment,
  fetchDeployment,
  fetchDeployments,
  fetchNodes,
  fetchOnboarding,
  fetchSnapshot,
  type ConsoleSnapshot,
  type DeploymentDetail,
  type OnboardingState,
} from '../api';
import { draft, type DraftEntry } from '../draft';
import { navigate } from '../forge/route';
import { useForge } from '../forge/state';
import { nodeInitializationView } from '../node-initialization';
import { useSession } from '../session';
import { useAgentLiveness } from '../ui/agent-alive';
import { ErrorBox } from '../ui/bits';
import { randomKey } from '../ui/platform';
import { wm } from '../wm/store';
import type { Drill } from './nodes';
import { isValidSlug } from './ports';
import { SubscriptionViewer, useUserGrantProbe } from './subscription';

// 首次上线清单。
//
// 清单不拥有业务页面：它常驻右下角、跟随页面切换，写出已完成步骤的结果，当前步骤只给一句话和一个
// 操作。用户、发布、拨测在清单里就地完成；纳管与建链仍在各自的生产页面上完成，进入这些页面时清单
// 收成胶囊，不遮挡表单。所有进度都来自服务端检查点与真实的机器、修订、变更单和拨测任务。

interface CreatedModel {
  appId: string;
  chainId: string;
  ingressId: string;
}

interface ModelReceipt extends CreatedModel {
  tenantId: string;
  userId: string;
  revisionId: number;
}

type GuidePhase = 'machine' | 'agent' | 'chain' | 'user' | 'publish' | 'activate' | 'verify' | 'complete';
type StepState = 'done' | 'current' | 'next';
type Tone = 'idle' | 'busy' | 'warn' | 'bad';

const STEP_LABELS = ['机器', '线路', '用户', '发布', '拨测'] as const;
/** 变更单在清单里的四段：与发布详情的阶段不同，这里只回答「走到哪了」。 */
const DEPLOY_STAGES = ['创建变更单', '下发', '应用', '生效'] as const;

let reopenVersion = 0;
const reopenListeners = new Set<() => void>();

/** Reopen the current first-run checklist from the account menu without turning it into a route. */
export function openOnboardingGuide() {
  reopenVersion += 1;
  for (const listener of reopenListeners) listener();
}

const subscribeReopen = (listener: () => void) => {
  reopenListeners.add(listener);
  return () => reopenListeners.delete(listener);
};
const reopenSnapshot = () => reopenVersion;

function pendingCreatedModel(entries: readonly DraftEntry[]): CreatedModel | null {
  let found: CreatedModel | null = null;
  for (const entry of entries) {
    if (entry.op.op !== 'create_ingress') continue;
    found = {
      appId: entry.op.app_id,
      chainId: entry.op.ingress.chain_id,
      ingressId: entry.op.ingress.id,
    };
  }
  return found;
}

/** 建链页「加入草稿」之后、修订提交之前，线路的名称只在草稿里。 */
function draftChainName(entries: readonly DraftEntry[], chainId: string | undefined): string | null {
  if (!chainId) return null;
  for (const entry of entries) {
    const op = entry.op;
    if ((op.op === 'create_chain' || op.op === 'upsert_chain') && op.chain.id === chainId) return op.chain.name;
  }
  return null;
}

function committedChainName(snapshot: ConsoleSnapshot | undefined, appId: string | null, chainId: string | null) {
  if (!snapshot || !appId || !chainId) return null;
  const app = snapshot.snapshot.apps.find(item => item.id === appId);
  return app?.chains.find(item => item.id === chainId)?.name ?? null;
}

function receiptKey(operatorId: string) {
  return `brocade.first-run.model-receipt.${operatorId}`;
}

function readReceipt(operatorId: string): ModelReceipt | null {
  try {
    const raw = localStorage.getItem(receiptKey(operatorId));
    return raw ? (JSON.parse(raw) as ModelReceipt) : null;
  } catch {
    return null;
  }
}

/** 当前步骤的下标；全部完成时等于步数。 */
function phaseStep(phase: GuidePhase): number {
  if (phase === 'machine' || phase === 'agent') return 0;
  if (phase === 'chain') return 1;
  if (phase === 'user') return 2;
  if (phase === 'publish' || phase === 'activate') return 3;
  if (phase === 'verify') return 4;
  return STEP_LABELS.length;
}

/**
 * 变更单走到第几段（下标指向进行中的一段，4 表示已生效）。机器领取后即视为已下发；
 * 目标全部成功、等待激活时停在「生效」。
 */
function deploymentStage(detail: DeploymentDetail | undefined): number {
  if (!detail) return 0;
  if (detail.activation_status === 'activated') return DEPLOY_STAGES.length;
  const targets = detail.targets.filter(target => target.status !== 'skipped');
  if (targets.length > 0 && targets.every(target => target.status === 'succeeded')) return 3;
  if (targets.some(target => target.dispatched_at != null || target.status !== 'pending')) return 2;
  return 1;
}

const CHECK = (
  <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
    <path
      d="M3.5 8.4 6.6 11.4 12.6 4.8"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

function Chevron({ up }: { up: boolean }) {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path
        d={up ? 'M4 10 8 6 12 10' : 'M4 6 8 10 12 6'}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** 完成为实心对勾；进行中为强调色圈，等待外部事件时转动；需要处理时为叹号。 */
function StepMark({ state, tone, index }: { state: StepState; tone: Tone; index: number }) {
  if (state === 'done') return <span className="onb-mark done">{CHECK}</span>;
  if (state === 'current' && (tone === 'bad' || tone === 'warn'))
    return <span className={`onb-mark current ${tone}`}>!</span>;
  if (state === 'current' && tone === 'busy')
    return (
      <span className="onb-mark current busy">
        <i />
      </span>
    );
  return <span className={`onb-mark ${state}`}>{index + 1}</span>;
}

function ProgressRing({ value, total }: { value: number; total: number }) {
  const radius = 8.5;
  const length = 2 * Math.PI * radius;
  return (
    <svg className="onb-ring" width="22" height="22" viewBox="0 0 22 22" aria-hidden="true">
      <circle cx="11" cy="11" r={radius} />
      {value > 0 && (
        <circle
          className="onb-ring-value"
          cx="11"
          cy="11"
          r={radius}
          strokeDasharray={`${(length * value) / total} ${length}`}
          transform="rotate(-90 11 11)"
        />
      )}
    </svg>
  );
}

const Spinner = () => <span className="onb-spin" aria-hidden="true" />;

/**
 * 纳管、安装、建链页的页脚和发布计划的操作条都吸附在视口底边，主按钮在右侧。两者重叠时把清单抬到
 * 操作条之上；页脚仍在文档流中、离底边较远时不移动。
 */
function useDockLift(dock: RefObject<HTMLDivElement | null>, location: string): number {
  const [lift, setLift] = useState(0);
  useLayoutEffect(() => {
    let frame = 0;
    let footers: HTMLElement[] | null = null;
    const measure = () => {
      frame = 0;
      const height = window.innerHeight;
      const style = getComputedStyle(dock.current ?? document.documentElement);
      const gap = parseFloat(style.getPropertyValue('--onb-gap'));
      const safeBottom = parseFloat(style.getPropertyValue('--safe-bottom')) || 0;
      const base = Math.max(Number.isFinite(gap) ? gap : 20, safeBottom);
      const band = (dock.current?.offsetHeight ?? 0) + base;
      let next = 0;
      for (const footer of footers ?? []) {
        const rect = footer.getBoundingClientRect();
        if (rect.height === 0 || rect.top >= height - base || rect.bottom <= height - band) continue;
        next = Math.max(next, height - rect.top + 8 - base);
      }
      setLift(Math.max(0, Math.round(next)));
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };
    // 页面内容读取完成、字体就绪、清单展开或收起都会改变两者的位置关系，所以连同页脚一起观察尺寸。
    const resize = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => collect());
    const view = document.querySelector('.fg-view');
    const collect = () => {
      const next = Array.from(document.querySelectorAll<HTMLElement>('.fg-view .pv-foot, .fg-view .cg-foot'));
      const known = footers;
      if (!known || next.length !== known.length || next.some((footer, position) => footer !== known[position])) {
        footers = next;
        resize?.disconnect();
        if (view) resize?.observe(view);
        if (dock.current) resize?.observe(dock.current);
        for (const footer of next) {
          resize?.observe(footer);
          const paper = footer.closest('.fg-sheet');
          if (paper) resize?.observe(paper);
        }
      }
      schedule();
    };
    collect();
    // 懒加载会在清单挂载后才插入页脚；纸面的入场 transform 也只改变位置，不触发尺寸观察。
    // 分别在 DOM 更新和动画结束后重测，避免把入场的 6px 位移保留为最终抬升量。
    const mutations = new MutationObserver(collect);
    if (view) mutations.observe(view, { childList: true, subtree: true });
    const settledEvents = ['animationend', 'animationcancel', 'transitionend', 'transitioncancel'] as const;
    for (const event of settledEvents) view?.addEventListener(event, schedule, true);
    window.addEventListener('scroll', schedule, true);
    window.addEventListener('resize', schedule);
    return () => {
      cancelAnimationFrame(frame);
      mutations.disconnect();
      resize?.disconnect();
      for (const event of settledEvents) view?.removeEventListener(event, schedule, true);
      window.removeEventListener('scroll', schedule, true);
      window.removeEventListener('resize', schedule);
    };
  }, [dock, location]);
  return lift;
}

/** 右下角的容器：负责抬升，并在显示期间给工作区底部留出胶囊的高度，页面滚到底时内容不被盖住。 */
function Dock({ location, expanded, children }: { location: string; expanded: boolean; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const lift = useDockLift(ref, `${location}|${expanded ? 'card' : 'pill'}`);
  useEffect(() => {
    document.documentElement.dataset.onboarding = 'dock';
    return () => {
      delete document.documentElement.dataset.onboarding;
    };
  }, []);
  return (
    <div
      ref={ref}
      className="onb-dock"
      style={{ '--onb-lift': `${lift}px` } as CSSProperties}
      data-state={expanded ? 'card' : 'pill'}
    >
      {children}
    </div>
  );
}

function StepRow({
  index,
  state,
  tone,
  label,
  fact,
  children,
}: {
  index: number;
  state: StepState;
  tone: Tone;
  label: string;
  fact?: string | null;
  children?: ReactNode;
}) {
  return (
    <li className={`onb-step ${state}`} aria-current={state === 'current' ? 'step' : undefined}>
      <StepMark state={state} tone={tone} index={index} />
      <span className="onb-label">{label}</span>
      {state === 'done' && fact && <span className="onb-fact">{fact}</span>}
      {state === 'current' && children && (
        <div className="onb-body" aria-live="polite">
          {children}
        </div>
      )}
    </li>
  );
}

export function OnboardingGuide() {
  const qc = useQueryClient();
  const { initial, who } = useSession();
  const { nav } = useForge();
  const windows = useSyncExternalStore(wm.subscribe, wm.snapshot);
  const nodeDrill = windows.wins.find(win => win.key === 'tab:nodes')?.data.drill as Drill | undefined;
  const deployDrill = windows.wins.find(win => win.key === 'tab:deploy')?.data.drill as { p?: string } | undefined;
  const entries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const reopen = useSyncExternalStore(subscribeReopen, reopenSnapshot, reopenSnapshot);
  const seenReopen = useRef(reopen);
  // 已完成的安装登录时保持安静，只能从账户菜单重新打开。
  const [dismissed, setDismissed] = useState(initial.onboarding === 'complete');
  const [receipt, setReceipt] = useState<ModelReceipt | null>(() => readReceipt(who.operator_id));
  const [viewer, setViewer] = useState(false);
  const [blocking, setBlocking] = useState(false);
  const [openedExplicitly, setOpenedExplicitly] = useState(false);
  const [collapsedPhase, setCollapsedPhase] = useState<string | null>(null);
  const [createdDeployment, setCreatedDeployment] = useState<{
    revisionId: number;
    deploymentId: number;
  } | null>(null);
  const location = `${nav}:${nav === 'nodes' ? JSON.stringify(nodeDrill ?? null) : nav === 'deploy' ? (deployDrill?.p ?? '') : ''}`;

  useEffect(() => {
    if (seenReopen.current === reopen) return;
    seenReopen.current = reopen;
    setDismissed(false);
    setOpenedExplicitly(true);
    setCollapsedPhase(null);
  }, [reopen]);
  // 显式展开只对当前页面有效：换到别的页面后重新按「是否在该步骤的页面上」决定收起与展开。
  const seenLocation = useRef(location);
  useEffect(() => {
    if (seenLocation.current === location) return;
    seenLocation.current = location;
    setOpenedExplicitly(false);
  }, [location]);

  const onboarding = useQuery({
    queryKey: ['onboarding'],
    queryFn: fetchOnboarding,
    refetchInterval: query => (query.state.data?.status === 'active' ? 5_000 : false),
  });
  const state = onboarding.data;
  const nodes = useQuery({
    queryKey: ['nodes'],
    queryFn: () => fetchNodes(),
    enabled: state?.status === 'active',
    refetchInterval: state?.status === 'active' ? 3_000 : false,
  });
  const rootNodes = (nodes.data?.nodes ?? []).filter(node => node.tenant_id === state?.root_tenant_id);
  const node = (nodes.data?.nodes ?? []).find(item => item.node_id === state?.node_id);
  const liveness = useAgentLiveness(node);
  // Existing installations have no initialization intent. Once the first business model has
  // been committed, its ordinary release can also resolve a manual initialization conflict.
  const awaitingInitialization =
    state?.target_revision_id == null &&
    !receipt &&
    node?.initialization != null &&
    node.initialization.state !== 'succeeded';
  // 线路名称：提交前在草稿里，提交后在快照里（外壳已持有同一份快照缓存）。
  const snapshot = useQuery({
    queryKey: ['snapshot'],
    queryFn: () => fetchSnapshot(),
    enabled: state?.chain_id != null,
  });

  const checkpointNode = useMutation({
    mutationFn: (nodeId: string) => checkpointOnboarding({ step: 'node', node_id: nodeId }),
    onSuccess: value => qc.setQueryData(['onboarding'], value),
  });
  const autoNode = useRef<string | null>(null);
  useEffect(() => {
    if (state?.status !== 'active' || state.node_id || rootNodes.length !== 1) return;
    const nodeId = rootNodes[0].node_id;
    if (autoNode.current === nodeId) return;
    autoNode.current = nodeId;
    checkpointNode.mutate(nodeId);
  }, [checkpointNode, rootNodes, state?.node_id, state?.status]);

  const deployments = useQuery({
    queryKey: ['deployments', 'config'],
    queryFn: () => fetchDeployments('config'),
    enabled: state?.status === 'active' && state.target_revision_id != null,
    refetchInterval: state?.status === 'active' && state.target_revision_id != null ? 3_000 : false,
  });
  const matchingDeployment = deployments.data?.deployments.find(
    deployment =>
      deployment.revision_id === state?.target_revision_id &&
      deployment.status !== 'failed' &&
      deployment.status !== 'canceled' &&
      deployment.id !== state?.deployment_id,
  );
  const checkpointDeployment = useMutation({
    mutationFn: (deploymentId: number) => checkpointOnboarding({ step: 'deployment', deployment_id: deploymentId }),
    onSuccess: value => qc.setQueryData(['onboarding'], value),
  });
  // 创建结果先在本页跟踪：失败单不能写入检查点，也不能因此丢失失败详情。
  // 变更单编号随创建递增；服务端关联到更新的替代单后，由新的检查点接管跟踪。
  const checkpointedDeploymentId = state?.deployment_id ?? null;
  const createdDeploymentId =
    createdDeployment &&
    createdDeployment.revisionId === state?.target_revision_id &&
    (checkpointedDeploymentId == null || checkpointedDeploymentId <= createdDeployment.deploymentId)
      ? createdDeployment.deploymentId
      : null;
  const selectedDeploymentId = createdDeploymentId ?? checkpointedDeploymentId;
  const deployment = useQuery({
    queryKey: ['deployment', selectedDeploymentId],
    queryFn: () => fetchDeployment(selectedDeploymentId!),
    enabled: state?.status === 'active' && selectedDeploymentId != null,
    refetchInterval: query => (query.state.data?.activation_status === 'activated' ? false : 3_000),
  });
  const selectedDeploymentStopped = deployment.data?.status === 'failed' || deployment.data?.status === 'canceled';
  const checkpointCandidateId =
    createdDeploymentId != null && !selectedDeploymentStopped
      ? deployment.data
        ? createdDeploymentId
        : null
      : matchingDeployment?.id;
  const autoDeployment = useRef<number | null>(null);
  useEffect(() => {
    if (state?.status !== 'active' || state.target_revision_id == null) return;
    if (createdDeploymentId == null && state.deployment_id != null && !selectedDeploymentStopped) return;
    if (checkpointCandidateId == null || checkpointCandidateId === state.deployment_id) return;
    if (autoDeployment.current === checkpointCandidateId) return;
    autoDeployment.current = checkpointCandidateId;
    checkpointDeployment.mutate(checkpointCandidateId);
  }, [
    checkpointDeployment,
    checkpointCandidateId,
    createdDeploymentId,
    selectedDeploymentStopped,
    state?.deployment_id,
    state?.status,
    state?.target_revision_id,
  ]);

  // 就地发布与发布计划页的「创建变更单」是同一个接口。幂等键在请求成功前保留：响应丢失后重试仍落到
  // 同一张变更单；成功后清空，失败单之后的「重新发布」才会创建新单。
  const publishKey = useRef<string | null>(null);
  const publish = useMutation({
    mutationFn: (revision: number) => {
      publishKey.current ??= randomKey();
      return createDeployment({ revision_id: revision, idempotency_key: publishKey.current });
    },
    onSuccess: (result, revisionId) => {
      publishKey.current = null;
      setCreatedDeployment({
        revisionId,
        deploymentId: result.deployment_id,
      });
      void qc.invalidateQueries({ queryKey: ['deployments'] });
      void qc.invalidateQueries({ queryKey: ['deployment-verify'] });
    },
  });

  const complete = useMutation({
    mutationFn: ({ probeId, itemId }: { probeId: string; itemId: string }) => completeOnboarding(probeId, itemId),
    onSuccess: value => {
      setViewer(false);
      qc.setQueryData(['onboarding'], value);
    },
  });

  if (onboarding.isPending || dismissed) return null;
  if (!onboarding.error && (!state || state.status === 'not-applicable')) return null;

  const created = pendingCreatedModel(entries);
  const phase: GuidePhase | 'error' = onboarding.error
    ? 'error'
    : state!.status === 'complete'
      ? 'complete'
      : !state!.node_id
        ? 'machine'
        : liveness?.state !== 'online' || awaitingInitialization
          ? 'agent'
          : state!.target_revision_id == null
            ? created || receipt
              ? 'user'
              : 'chain'
            : selectedDeploymentId == null
              ? 'publish'
              : selectedDeploymentId === state!.deployment_id && deployment.data?.activation_status === 'activated'
                ? 'verify'
                : 'activate';

  const onProvisionSurface = nav === 'nodes' && (nodeDrill?.p === 'provision' || nodeDrill?.p === 'install');
  const onChainWizard = nav === 'nodes' && nodeDrill?.p === 'chain';
  const onPlan = nav === 'deploy' && deployDrill?.p === 'plan';
  const onStepPage =
    ((phase === 'machine' || phase === 'agent') && onProvisionSurface) ||
    (phase === 'chain' && onChainWizard) ||
    (phase === 'publish' && onPlan);
  const expanded = openedExplicitly || (!onStepPage && collapsedPhase !== phase);
  const collapse = () => {
    setOpenedExplicitly(false);
    setCollapsedPhase(phase);
  };
  const expand = () => {
    setOpenedExplicitly(true);
    setCollapsedPhase(null);
  };

  const index = phase === 'error' ? 0 : phaseStep(phase);
  const nodeLabel = node?.name || state?.node_id || '';
  let title = '首次上线';
  let tone: Tone = 'idle';
  let hint: string | null = null;
  let body: ReactNode = null;

  if (phase === 'error') {
    title = '无法读取首次上线进度';
    tone = 'bad';
    body = (
      <>
        <ErrorBox error={onboarding.error} />
        <div className="onb-actions">
          <button type="button" className="btn sm" onClick={() => void onboarding.refetch()}>
            重新读取
          </button>
        </div>
      </>
    );
  } else if (phase === 'machine') {
    title = rootNodes.length > 1 ? '选择第一台机器' : '纳管第一台机器';
    // 纳管成功后跳到安装页、检查点尚未写入的片刻也属于这一步，那时不再提示填写表单。
    hint = nodeDrill?.p === 'provision' ? '在本页填写机器 ID 并纳管' : null;
    if (nodes.error) {
      tone = 'bad';
      body = <ErrorBox error={nodes.error} />;
    } else if (checkpointNode.error) {
      tone = 'bad';
      body = (
        <>
          <ErrorBox error={checkpointNode.error} />
          {rootNodes.length === 1 && (
            <div className="onb-actions">
              <button
                type="button"
                className="btn sm primary"
                onClick={() => checkpointNode.mutate(rootNodes[0].node_id)}
              >
                重试记录机器
              </button>
            </div>
          )}
        </>
      );
    } else if (checkpointNode.isPending || rootNodes.length === 1) {
      tone = 'busy';
      body = (
        <p className="onb-note">
          <Spinner />
          正在记录 {rootNodes[0]?.name || rootNodes[0]?.node_id || '这台机器'}
        </p>
      );
    } else if (rootNodes.length > 1) {
      body = (
        <>
          <p className="onb-note">检测到 {rootNodes.length} 台机器，选一台继续。</p>
          <div className="onb-picks">
            {rootNodes.map(item => (
              <button
                type="button"
                className="onb-pick"
                key={item.node_id}
                onClick={() => checkpointNode.mutate(item.node_id)}
              >
                <b>{item.name || item.node_id}</b>
                <span className="mono">{item.node_id}</span>
              </button>
            ))}
          </div>
        </>
      );
    } else if (!nodes.isPending) {
      body = (
        <div className="onb-actions">
          <button type="button" className="btn primary" onClick={() => navigate('nodes', { p: 'provision', step: 1 })}>
            纳管机器
          </button>
        </div>
      );
    }
  } else if (phase === 'agent') {
    // 回到创建时留下的安装页，才能继续看到只返回一次的安装命令。
    const installation =
      nodeDrill?.p === 'install' && nodeDrill.node === state!.node_id
        ? nodeDrill
        : { p: 'install' as const, node: state!.node_id!, step: 2 };
    const onInstallation = nav === 'nodes' && installation === nodeDrill;
    const initializing = liveness?.state === 'online' && awaitingInitialization;
    const initialization = nodeInitializationView(node?.initialization);
    let note: ReactNode;
    if (initializing) {
      title = initialization.title;
      tone = initialization.tone === 'warn' ? (node?.initialization?.state === 'failed' ? 'bad' : 'warn') : 'busy';
      note =
        initialization.tone === 'warn' ? (
          <p className={`onb-note ${tone}`}>{initialization.description}</p>
        ) : (
          <p className="onb-note">
            <Spinner />
            服务端正在自动发布初始配置
          </p>
        );
    } else if (liveness?.state === 'polling') {
      title = '等待 Agent 上线';
      tone = liveness.onceOnline ? 'warn' : 'busy';
      note = liveness.onceOnline ? (
        <p className="onb-note warn">机器曾经上线，当前没有心跳</p>
      ) : (
        <p className="onb-note">
          <Spinner />
          token 已兑换，等待心跳
        </p>
      );
    } else {
      title = '在机器上执行安装命令';
      tone = 'busy';
      note = (
        <p className="onb-note">
          <Spinner />
          {nodeLabel} 已登记，等待执行安装命令
        </p>
      );
    }
    hint = !initializing && liveness?.state !== 'polling' ? '在机器上执行本页的命令' : title;
    body = (
      <>
        {note}
        {checkpointNode.error && <ErrorBox error={checkpointNode.error} />}
        {!onInstallation && (
          <div className="onb-actions">
            <button type="button" className="btn sm" onClick={() => navigate('nodes', installation)}>
              {initializing ? '查看初始化进度' : '查看安装命令'}
            </button>
          </div>
        )}
      </>
    );
  } else if (phase === 'chain') {
    title = '创建第一条线路';
    hint = '在本页选好协议后「加入草稿」';
    body = (
      <>
        <p className="onb-note">入口和出口默认都是 {nodeLabel}。</p>
        <div className="onb-actions">
          <button
            type="button"
            className="btn primary"
            onClick={() => navigate('nodes', { p: 'chain', id: state!.node_id! })}
          >
            创建线路
          </button>
        </div>
      </>
    );
  } else if (phase === 'user') {
    title = '创建第一个用户';
    body = (
      <FirstUserForm
        state={state!}
        created={created ?? receipt!}
        initialReceipt={receipt}
        onBusyChange={setBlocking}
        onReceipt={setReceipt}
        onCommitted={value => {
          setReceipt(null);
          qc.setQueryData(['onboarding'], value);
        }}
      />
    );
  } else if (phase === 'publish') {
    const revision = state!.target_revision_id!;
    title = '发布配置';
    hint = '核对后「创建变更单」';
    if (deployments.error || checkpointDeployment.error) {
      tone = 'bad';
      body = (
        <>
          <ErrorBox error={deployments.error ?? checkpointDeployment.error} />
          {matchingDeployment && checkpointDeployment.error && (
            <div className="onb-actions">
              <button
                type="button"
                className="btn sm primary"
                onClick={() => checkpointDeployment.mutate(matchingDeployment.id)}
              >
                重试关联变更单
              </button>
            </div>
          )}
        </>
      );
    } else if (checkpointDeployment.isPending || matchingDeployment) {
      tone = 'busy';
      body = (
        <p className="onb-note">
          <Spinner />
          正在关联变更单 #{matchingDeployment?.id}
        </p>
      );
    } else {
      body = (
        <>
          <p className="onb-note">
            修订 R{revision} · 发布到 {nodeLabel}
          </p>
          {publish.error && <ErrorBox error={publish.error} />}
          <div className="onb-actions">
            <button
              type="button"
              className="btn primary"
              disabled={publish.isPending}
              onClick={() => publish.mutate(revision)}
            >
              {publish.isPending ? '发布中…' : '发布'}
            </button>
            <button type="button" className="onb-link" onClick={() => navigate('deploy', { p: 'plan', revision })}>
              核对计划
            </button>
          </div>
        </>
      );
    }
  } else if (phase === 'activate') {
    const id = selectedDeploymentId!;
    const row = deployments.data?.deployments.find(item => item.id === id);
    const detail = deployment.data;
    const stopped = selectedDeploymentStopped;
    const checkpointError = state!.deployment_id !== id && !stopped ? checkpointDeployment.error : null;
    const attention =
      !stopped &&
      (row?.awaiting_confirmation === true || detail?.status === 'halted' || detail?.activation_status === 'rejected');
    const at = stopped ? Math.min(deploymentStage(detail), 3) : deploymentStage(detail);
    const failure = detail?.targets.find(target => target.error)?.error ?? null;
    title = stopped ? '发布没有完成' : attention ? '变更单需要处理' : '配置上线中';
    tone = stopped || checkpointError ? 'bad' : attention ? 'warn' : 'busy';
    const openDeployment = () => navigate('deploy', { p: 'detail', id });
    body = deployment.error ? (
      <ErrorBox error={deployment.error} />
    ) : (
      <>
        <p className={`onb-note${stopped ? ' bad' : attention ? ' warn' : ''}`}>
          {!stopped && !attention && <Spinner />}
          变更单 #{id}
          {stopped
            ? ` 已停止${failure ? `：${failure}` : ''}`
            : row?.awaiting_confirmation
              ? ' 等待人工确认'
              : detail?.status === 'halted'
                ? ' 已熔断'
                : detail?.activation_status === 'rejected'
                  ? ' 未能生效'
                  : detail?.activation_status === 'activated'
                    ? ' 已生效，正在记录首次上线进度'
                    : ` · ${DEPLOY_STAGES[Math.min(at, DEPLOY_STAGES.length - 1)]}中`}
        </p>
        <div className="onb-stages" aria-hidden="true">
          {DEPLOY_STAGES.map((label, position) => (
            <i
              key={label}
              className={
                position < at ? 'done' : position === at ? (stopped ? 'bad' : attention ? 'warn' : 'current') : ''
              }
            />
          ))}
        </div>
        {publish.error && <ErrorBox error={publish.error} />}
        {checkpointError && <ErrorBox error={checkpointError} />}
        <div className="onb-actions">
          {checkpointError && (
            <button type="button" className="btn primary" onClick={() => checkpointDeployment.mutate(id)}>
              重试关联变更单
            </button>
          )}
          {stopped && (
            <button
              type="button"
              className="btn primary"
              disabled={publish.isPending}
              onClick={() => publish.mutate(state!.target_revision_id!)}
            >
              {publish.isPending ? '发布中…' : '重新发布'}
            </button>
          )}
          {attention ? (
            <button type="button" className="btn primary" onClick={openDeployment}>
              查看变更单
            </button>
          ) : (
            <button type="button" className="onb-link" onClick={openDeployment}>
              查看变更单
            </button>
          )}
        </div>
      </>
    );
  } else if (phase === 'verify') {
    title = '端到端拨测';
  } else {
    title = '第一条线路已经可用';
  }

  const chainName =
    draftChainName(entries, created?.chainId) ??
    committedChainName(snapshot.data, state?.app_id ?? null, state?.chain_id ?? null) ??
    state?.chain_id ??
    created?.chainId ??
    receipt?.chainId ??
    null;
  const facts = [
    state?.node_id ? `${state.node_id}${liveness ? ` · ${liveness.state === 'online' ? '在线' : '离线'}` : ''}` : null,
    chainName,
    state?.user_id ?? receipt?.userId ?? null,
    state?.deployment_id != null ? `#${state.deployment_id} 已生效` : null,
    '已通过',
  ];
  const isComplete = phase === 'complete';

  const steps = STEP_LABELS.map((label, position) => {
    const stepState: StepState = position < index ? 'done' : position === index ? 'current' : 'next';
    if (stepState === 'current' && phase === 'verify')
      return (
        <ProbeRow
          key={label}
          index={position}
          state={state!}
          nodeLabel={nodeLabel}
          completing={complete.isPending}
          completeError={complete.error}
          onPassed={result => complete.mutate(result)}
          onOpenViewer={() => setViewer(true)}
        />
      );
    return (
      <StepRow
        key={label}
        index={position}
        state={stepState}
        tone={stepState === 'current' ? tone : 'idle'}
        label={stepState === 'current' ? title : label}
        fact={facts[position]}
      >
        {body}
      </StepRow>
    );
  });

  return (
    <>
      <Dock location={location} expanded={expanded}>
        {/* 收起只隐藏清单，保留拨测订阅、完成回写和表单输入。 */}
        <section className="onb-card" aria-label="首次上线" hidden={!expanded}>
          <header className="onb-head">
            <h2>{isComplete ? '第一条线路已经可用' : '首次上线'}</h2>
            {!isComplete && phase !== 'error' && (
              <span className="onb-count">
                {index} / {STEP_LABELS.length}
              </span>
            )}
            <button
              type="button"
              className="onb-fold"
              aria-label="收起首次上线清单"
              disabled={blocking}
              onClick={collapse}
            >
              <Chevron up={false} />
            </button>
          </header>
          {phase === 'error' ? (
            <div className="onb-error">{body}</div>
          ) : (
            <ol className="onb-steps" aria-label="首次上线步骤">
              {steps}
            </ol>
          )}
          {isComplete && (
            <footer className="onb-foot">
              {state?.user_id && (
                <button
                  type="button"
                  className="btn"
                  onClick={() => navigate('users', { p: 'user', id: state.user_id! })}
                >
                  查看订阅
                </button>
              )}
              <button type="button" className="btn primary" onClick={() => setDismissed(true)}>
                完成
              </button>
            </footer>
          )}
        </section>
        {!expanded && (
          <button type="button" className="onb-pill" aria-expanded="false" onClick={expand}>
            {isComplete ? (
              <span className="onb-mark done">{CHECK}</span>
            ) : (
              <ProgressRing value={index} total={STEP_LABELS.length} />
            )}
            <span className="onb-pill-copy">
              <b>{isComplete ? '首次上线已完成' : '首次上线'}</b>
              <span>
                {tone === 'busy' && <Spinner />}
                {onStepPage && hint ? hint : title}
              </span>
            </span>
            <Chevron up />
          </button>
        )}
      </Dock>
      {viewer && state?.user_tenant_id && state.user_id && (
        <SubscriptionViewer
          tenant={state.user_tenant_id}
          user={state.user_id}
          kind="uri"
          onProbePassed={result => complete.mutate(result)}
          onClose={() => setViewer(false)}
        />
      )}
    </>
  );
}

/**
 * 拨测一步：只拨首次上线那条入口在当前 Serving 计划里的条目；任一项通过即作为完成证据提交。
 * 与订阅弹窗共享同一轮任务，弹窗里发起的拨测在这里同样生效。
 */
function ProbeRow({
  index,
  state,
  nodeLabel,
  completing,
  completeError,
  onPassed,
  onOpenViewer,
}: {
  index: number;
  state: OnboardingState;
  nodeLabel: string;
  completing: boolean;
  completeError: unknown;
  onPassed: (result: { probeId: string; itemId: string }) => void;
  onOpenViewer: () => void;
}) {
  const probe = useUserGrantProbe(state.user_tenant_id!, state.user_id!);
  const targets = probe.items.filter(
    item => item.app_id === state.app_id && item.chain_id === state.chain_id && item.ingress_id === state.ingress_id,
  );
  const passed = targets.find(item => item.status === 'passed');
  const running = probe.busy || targets.some(item => item.status === 'waiting' || item.status === 'running');
  const failed =
    !running &&
    !passed &&
    targets.some(item => item.status === 'failed' || item.status === 'canceled') &&
    probe.job != null;
  const failure = targets.find(item => item.status === 'failed')?.detail ?? probe.job?.message ?? null;
  const reported = useRef<string | null>(null);
  useEffect(() => {
    if (!probe.job || !passed) return;
    const key = `${probe.job.id}:${passed.id}`;
    if (reported.current === key) return;
    reported.current = key;
    onPassed({ probeId: probe.job.id, itemId: passed.id });
  }, [onPassed, passed, probe.job]);

  const start = () => {
    reported.current = null;
    probe.start(targets.map(item => item.id));
  };
  const viewerLink = (
    <button type="button" className="onb-link" onClick={onOpenViewer}>
      查看节点地址
    </button>
  );
  let title = '端到端拨测';
  let tone: Tone = 'idle';
  let body: ReactNode;
  if (probe.pending) {
    tone = 'busy';
    body = (
      <p className="onb-note">
        <Spinner />
        读取拨测计划
      </p>
    );
  } else if (probe.unavailable) {
    title = '拨测不可用';
    tone = 'warn';
    body = (
      <>
        <p className="onb-note warn">{probe.unavailable}</p>
        <div className="onb-actions">{viewerLink}</div>
      </>
    );
  } else if (probe.error && !running) {
    tone = 'bad';
    body = (
      <>
        <ErrorBox error={probe.error} />
        <div className="onb-actions">{viewerLink}</div>
      </>
    );
  } else if (passed && completeError) {
    title = '拨测结果未能记录';
    tone = 'bad';
    body = (
      <>
        <ErrorBox error={completeError} />
        <div className="onb-actions">
          <button type="button" className="btn primary" onClick={start}>
            重新拨测
          </button>
        </div>
      </>
    );
  } else if (passed) {
    title = '拨测通过';
    tone = 'busy';
    body = (
      <p className="onb-note">
        <Spinner />
        {completing ? '正在记录拨测结果' : '拨测通过'}
      </p>
    );
  } else if (running) {
    title = '拨测中';
    tone = 'busy';
    body = (
      <p className="onb-note">
        <Spinner />
        使用 {state.user_id} 的授权连接 {nodeLabel}
      </p>
    );
  } else if (targets.length === 0) {
    tone = 'warn';
    body = (
      <>
        <p className="onb-note warn">拨测计划里还没有这条线路的入口</p>
        <div className="onb-actions">{viewerLink}</div>
      </>
    );
  } else if (failed) {
    title = '拨测未通过';
    tone = 'bad';
    body = (
      <>
        <p className="onb-note bad">{failure ?? '没有一项通过'}</p>
        <div className="onb-actions">
          <button type="button" className="btn primary" onClick={start}>
            重试
          </button>
          {viewerLink}
        </div>
      </>
    );
  } else {
    body = (
      <>
        <p className="onb-note">使用 {state.user_id} 的授权从控制台拨测入口。</p>
        <div className="onb-actions">
          <button type="button" className="btn primary" disabled={probe.busy} onClick={start}>
            拨测
          </button>
        </div>
      </>
    );
  }
  return (
    <StepRow index={index} state="current" tone={tone} label={title}>
      {body}
    </StepRow>
  );
}

function FirstUserForm({
  state,
  created,
  initialReceipt,
  onBusyChange,
  onReceipt,
  onCommitted,
}: {
  state: OnboardingState;
  created: CreatedModel;
  initialReceipt: ModelReceipt | null;
  onBusyChange: (busy: boolean) => void;
  onReceipt: (value: ModelReceipt) => void;
  onCommitted: (value: OnboardingState) => void;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  const [userId, setUserId] = useState(initialReceipt?.userId ?? '');
  const tenantId = state.root_tenant_id ?? '';
  const commit = useMutation({
    mutationFn: async () => {
      let receipt = initialReceipt;
      if (!receipt) {
        draft.push({ op: 'create_user', user: { tenant_id: tenantId, id: userId.trim() } });
        draft.push({
          op: 'upsert_grant',
          grant: {
            app_id: created.appId,
            tenant_id: tenantId,
            user_id: userId.trim(),
            ingress_id: created.ingressId,
            enabled: true,
          },
        });
        const batch = draft.beginSubmission();
        let succeeded = false;
        try {
          const result = await applyDraft(
            batch.entries.map(entry => entry.op),
            '首次上线：创建第一条链与第一个用户',
          );
          succeeded = true;
          receipt = {
            ...created,
            tenantId,
            userId: userId.trim(),
            revisionId: result.revision_id,
          };
          onReceipt(receipt);
          try {
            localStorage.setItem(receiptKey(who.operator_id), JSON.stringify(receipt));
          } catch {
            // The current tab can still finish; storage only provides crash recovery.
          }
        } finally {
          draft.finishSubmission(batch, succeeded);
        }
      }
      const checked = await checkpointOnboarding({
        step: 'model',
        app_id: receipt.appId,
        chain_id: receipt.chainId,
        ingress_id: receipt.ingressId,
        user_tenant_id: receipt.tenantId,
        user_id: receipt.userId,
        target_revision_id: receipt.revisionId,
      });
      try {
        localStorage.removeItem(receiptKey(who.operator_id));
      } catch {
        // A stale receipt is ignored after the server checkpoint exists.
      }
      return checked;
    },
    onSuccess: value => {
      void qc.invalidateQueries({ queryKey: ['snapshot'] });
      void qc.invalidateQueries({ queryKey: ['users'] });
      void qc.invalidateQueries({ queryKey: ['revisions'] });
      onCommitted(value);
    },
  });
  useEffect(() => {
    onBusyChange(commit.isPending);
    return () => onBusyChange(false);
  }, [commit.isPending, onBusyChange]);

  const trimmed = userId.trim();
  const valid = !!tenantId && isValidSlug(trimmed);
  return (
    <form
      className="onb-user"
      onSubmit={event => {
        event.preventDefault();
        if (valid && !commit.isPending) commit.mutate();
      }}
    >
      <div className="onb-inline">
        <input
          className="f mono"
          value={userId}
          aria-label="用户 ID"
          aria-invalid={!!trimmed && !isValidSlug(trimmed)}
          disabled={!!initialReceipt || commit.isPending}
          placeholder="用户 ID，如 alice"
          spellCheck={false}
          autoCapitalize="none"
          autoComplete="off"
          onChange={event => setUserId(event.target.value)}
        />
        <button className="btn primary" type="submit" disabled={!valid || commit.isPending}>
          {commit.isPending ? '提交中…' : initialReceipt ? '重试确认' : '创建并提交'}
        </button>
      </div>
      {!!trimmed && !isValidSlug(trimmed) && <p className="onb-note bad">只能使用 a-z 0-9 . _ -，最长 32 个字符</p>}
      {initialReceipt && <p className="onb-note">修订已写入，点「重试确认」完成记录</p>}
      {commit.error && <ErrorBox error={commit.error} />}
    </form>
  );
}
