import { draft } from '../draft';
import {
  Fragment,
  lazy,
  Suspense,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import {
  abandonNode,
  fetchAgentLogPolicy,
  fetchArtifactContentView,
  fetchCompileView,
  fetchDeployments,
  fetchLinkHealth,
  fetchLinkMtu,
  fetchNodes,
  fetchRevisions,
  fetchTenants,
  fetchUsageNodeSeries,
  fetchUsageNodeSeriesRange,
  fetchNodeLoadOverview,
  fetchNodeTraffic,
  fetchCerts,
  fetchNodeNicListWindows,
  fetchLatestNodePingProbes,
  fetchNodePublicIpHistory,
  fetchNodePingProbe,
  fetchNodePingProbeRange,
  isolateNode,
  monthBytes,
  issueNodeToken,
  provisionNode,
  removeRetiredNodes,
  restoreNodeService,
  saveNodeLogPolicy,
  saveNodeTraffic,
  setNodeCertGroup,
  setNodeStatus,
  setWireGuardLinkDisabled,
  updateNode,
  verifyDeployment,
  MIN_AGENT_PROTOCOL_VERSION,
  type AgentLogLimits,
  type AgentLogLimitOverrides,
  type AgentLogPolicyNode,
  type AgentLogPolicyView,
  type Dns,
  type NodeConnection,
  type DomainStrategy,
  type GroupCertificate,
  type LinkHealthItem,
  type NodeAgentStateItem,
  type NodeCertificateState,
  type NodeLoadView,
  type NodeNicSample,
  type NodeNicView,
  type NodePingProbeLatestView,
  type NodePingProbeView,
  PING_PROBE_FAMILIES,
  type PingProbeFamily,
  type PingProbeKind,
  type PingProbePoint,
  type PingProbeTargetSeries,
  type NodePublicIpEvent,
  type NodeTrafficCycleKind,
  type NodeTrafficItem,
  type NodeTrafficView,
  type ProvisionNodeResult,
  type UsageNodeSeries,
} from '../api';
import { fetchPreviewStatus } from '../preview/api';
import { PreviewProvision, type PreviewWizDrill } from '../preview/provision';
import { can, isVisitor, useSession } from '../session';
import { Ago, Confirm, Empty, EmptyState, ErrorBox, Loading, SegmentedControl, SegSwitch } from '../ui/bits';
import { FieldLoading, PanelLoading } from '../ui/loading';
import { Icon, ListIcon, PanelTitle, type IconName } from '../ui/icons';
import { bytes } from '../ui/format';
import { CopyButton } from '../ui/copy-button';
import { firstProvisionError, provisionFormErrors } from '../provision-form';
import { WizardCard, WizardField, WizardFooter, WizardPaper, WizardPaperHeader } from '../ui/wizard-paper';
import { useNarrow } from '../ui/viewport';
import { useNow } from '../ui/clock';
import { useAgentLiveness } from '../ui/agent-alive';
import {
  PING_FAMILY_LABEL,
  pingLatencyMs,
  pingLossStats,
  pingLossText,
  pingLossTone,
  pingSampleLost,
  pingSampleText,
  pingSkipReason,
  pingSkipReasonShort,
  pingSkipReasonText,
  worstPingLossTone,
  type PingLossTone,
} from '../ui/ping-probe';
import { type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { RegionFlag } from '../ui/region-flag';
import { navigate, returnTo } from '../forge/route';
import { cancelVisualTransition } from '../ui/motion';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import { OBSERVE_MS_UNIT, OBSERVE_SERIES_COLOR_VARS } from '../ui/observe-chart';
import { dur, iso, throughputAxis } from './telemetry-format';
import { LOG_MAX_MIB, LOG_MIN_MIB, validLogMib } from '../ui/log-policy';
import { isForwardTargetInChain } from './rule-graph';
import { chainSpine, fetchSnapshot, type SnapshotChain, type SnapshotIngress, type SnapshotStep } from '../api';
import { MuxObservationCard } from '../mux-observation';
import { VpngateObservationCard } from '../vpngate-observation';
import { NodeRealtimeProvider } from '../node-realtime';
import { ReverseHealthCard } from '../reverse-health';
import type { AppIr } from '../topo/model';

const ChainWizard = lazy(() => import('./chain-wizard').then(module => ({ default: module.ChainWizard })));
const ChainRulesPanel = lazy(() => import('./chains').then(module => ({ default: module.ChainRulesPanel })));
const LazyMachineEgressDnsRules = lazy(() =>
  import('./rules').then(module => ({ default: module.MachineEgressDnsRules })),
);
const RuleDraftScope = lazy(() => import('./rules').then(module => ({ default: module.RuleDraftScope })));
const importTelemetry = () => import('./telemetry');
const importNodeObservationCharts = () => import('./node-observation-charts');
const LazyThroughputChart = lazy(() => importTelemetry().then(module => ({ default: module.ThroughputChart })));

type TelemetryModule = Awaited<ReturnType<typeof importTelemetry>>;
type NodeObservationChartsModule = Awaited<ReturnType<typeof importNodeObservationCharts>>;
type NodeObservationModules = {
  LoadCard: TelemetryModule['LoadCard'];
  ThroughputChart: TelemetryModule['ThroughputChart'];
  PingLatencyChart: NodeObservationChartsModule['PingLatencyChart'];
  ObservationChartLoading: NodeObservationChartsModule['ObservationChartLoading'];
};
type NodeObservationModuleState =
  { status: 'pending' } | { status: 'ready'; modules: NodeObservationModules } | { status: 'error'; error: unknown };

let loadedNodeObservationModules: NodeObservationModules | null = null;
let nodeObservationModulesPromise: Promise<NodeObservationModules> | null = null;

function loadNodeObservationModules(): Promise<NodeObservationModules> {
  if (loadedNodeObservationModules) return Promise.resolve(loadedNodeObservationModules);
  nodeObservationModulesPromise ??= Promise.all([importTelemetry(), importNodeObservationCharts()]).then(
    ([telemetry, charts]) => {
      loadedNodeObservationModules = {
        LoadCard: telemetry.LoadCard,
        ThroughputChart: telemetry.ThroughputChart,
        PingLatencyChart: charts.PingLatencyChart,
        ObservationChartLoading: charts.ObservationChartLoading,
      };
      return loadedNodeObservationModules;
    },
  );
  return nodeObservationModulesPromise;
}

function useNodeObservationModules(): NodeObservationModuleState {
  const [state, setState] = useState<NodeObservationModuleState>(() =>
    loadedNodeObservationModules ? { status: 'ready', modules: loadedNodeObservationModules } : { status: 'pending' },
  );

  useEffect(() => {
    if (state.status !== 'pending') return;
    let active = true;
    loadNodeObservationModules().then(
      modules => {
        if (active) setState({ status: 'ready', modules });
      },
      error => {
        if (active) setState({ status: 'error', error });
      },
    );
    return () => {
      active = false;
    };
  }, [state.status]);

  return state;
}

// 纳管分两个阶段，中间是一次不可逆的写库：
// `provision` 是填写信息（此时机器尚未创建），`install` 是为已创建的机器安装 agent。
// 因此 install 的标识是 node_id——它会进入地址栏，切换后返回仍可定位；
// `result` 只是创建流程返回的一次性响应，重新进入后不再存在（见 ProvisionInstall）。
export type Drill =
  | { p: 'list' }
  | { p: 'node'; id: string; tab?: 'config' }
  | { p: 'provision'; step: number }
  | { p: 'install'; node: string; step: number; result?: ProvisionNodeResult }
  | { p: 'chain'; id: string };

/* 向导的两个阶段共用同一套步骤条，此处收敛组件签名 */
export type WizDrill = Extract<Drill, { p: 'provision' } | { p: 'install' }>;

const PROVISION_FORM_DEFAULTS = {
  id: '',
  name: '',
  public_ipv4: '',
  public_ipv6: '',
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  wg_listen_port: '51820',
  api_port: '10085',
  overlay: true,
  egress_allowed: true,
  dns: 'system',
  domain_strategy: 'use_ip' as DomainStrategy,
  cert_label_id: '',
};

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「机器」）由外壳补全。
   段的标签用机器名而非 node_id——名称是日常识别依据；drill 仍按 id 跳转。 */
const crumbOf = (d: Drill, nameOf: (id: string) => string): CrumbSeg[] => {
  switch (d.p) {
    case 'list':
      return [];
    case 'node':
      return [{ label: nameOf(d.id) }];
    case 'provision':
      return [{ label: '纳管向导' }];
    case 'install':
      return [{ label: nameOf(d.node), drill: { p: 'node', id: d.node } }, { label: '纳管向导' }];
    case 'chain':
      return [{ label: nameOf(d.id), drill: { p: 'node', id: d.id } }, { label: '建链向导' }];
  }
};

export function NodesPane({ win, bare = false }: { win: Win; bare?: boolean }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => navigate('nodes', d);
  // 面包屑用机器名。nodes 查询在列表页已拉取，通常命中缓存；名称缺失或未加载时回退到 id。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const nameOf = (id: string) => nodes.data?.nodes.find(n => n.node_id === id)?.name || id;
  useCrumb(win, crumbOf(drill, nameOf));

  if (drill.p === 'list') return <NodeList go={go} sheeted={bare} />;

  const body =
    drill.p === 'provision' || drill.p === 'install' ? (
      <ProvisionGate drill={drill} go={go} />
    ) : drill.p === 'chain' ? (
      <ChainStep id={drill.id} />
    ) : (
      <NodeDetail id={drill.id} initialTab={drill.tab} go={go} sheeted={bare} />
    );

  // Provision and chain wizards own their full-width paper through WizardPaper. Wrapping them in
  // another fg-sheet here creates a second rounded surface around the real page. NodeDetail also
  // owns its sheet when `sheeted` is true, so every drill-down can return its page directly.
  return body;
}

function ProvisionGate({ drill, go }: { drill: WizDrill; go: (d: Drill) => void }) {
  const preview = useQuery({
    queryKey: ['preview-status'],
    queryFn: fetchPreviewStatus,
    retry: false,
  });
  if (preview.isPending) return <Loading variant="form" />;
  if (preview.error) return <ErrorBox error={preview.error} />;
  if (preview.data.enabled) {
    return <PreviewProvision drill={drill as PreviewWizDrill} go={go} status={preview.data} />;
  }
  return <Provision drill={drill} go={go} />;
}

/* 向导需要该机器的完整信息（租户、名称），从 nodes 列表中获取 */
function ChainStep({ id }: { id: string }) {
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const n = nodes.data?.nodes.find(x => x.node_id === id);
  if (nodes.isPending) return <Loading variant="form" />;
  if (!n) return <ErrorBox error={new Error(`没有这台机器：${id}`)} />;
  // 标题与项目页的入口一致（chains.tsx 的 NewChain）：同一个向导的两个入口应使用相同
  // 的外框，副标题中改为说明从哪台机器进入。创建后不跳转到发布页——向导的四个步骤
  // 全部写入草稿（`draft.push`），未提交则没有修订，发布页只会显示「已收敛」，
  // 跳转到该页会与实际操作不符。后续操作是顶栏草稿条上的「提交」。
  return (
    <Suspense fallback={<Loading variant="form" />}>
      <ChainWizard node={n} onDone={() => returnTo('nodes', { p: 'node', id })} />
    </Suspense>
  );
}

// 服务端返回的 applied 结构如下（brocade-store::console 的 node_agent_state_from_row）：
// { phantun:{state,sha256}, wireguard:{state,sha256}, xray:{state,sha256},
//   hy2_port_hop:{state,sha256}, grants:{state}, source_deployment_id, observed_at }
// 其中不含修订号。修订号需用 source_deployment_id 在发布记录中反查。
interface AppliedState {
  phantun?: { state?: string | null; sha256?: string | null };
  wireguard?: { state?: string | null; sha256?: string | null };
  xray?: { state?: string | null; sha256?: string | null };
  hy2_port_hop?: { state?: string | null; sha256?: string | null };
  grants?: { state?: string | null };
  source_deployment_id?: number | null;
  observed_at?: string | null;
}

const appliedOf = (node: NodeAgentStateItem): AppliedState | null => (node.applied as AppliedState | null) ?? null;

const STATE_CLASS: Record<string, string> = {
  present: 'st-succeeded',
  disabled: 'st-skipped',
  unmanaged: 'st-pending',
  unknown: 'st-pending',
  dirty: 'st-halted',
};

/* 本版本应下发到机器上的全部产物。需要列全，不能只列常用的两件：控制面判定「待发布」
   依据的就是这些状态，遗漏任何一件时，该机器持续处于待发布状态的原因在控制台上
   无法查明——端口跳跃刚上线时即是如此，库中记录为 unknown，界面上没有任何显示。 */
type ArtifactKind = 'phantun' | 'wireguard' | 'xray' | 'hy2_port_hop';

/** 简称。这些值排在同一行内，使用全称会导致该行换行。
 *
 *  跳转一项带上 hy2 前缀：只写「跳转」与链路上的「转发」只差一个字，而后者是另一件事
 *  （中转端口，走 TCP，位于 xray 配置中）。此项表示的是 Hysteria 2 的 UDP 端口区间
 *  对应的 nft 规则。 */
const ARTIFACT_LABEL: Record<ArtifactKind, string> = {
  phantun: 'PHANTUN',
  wireguard: 'WG',
  xray: 'XRAY',
  hy2_port_hop: 'HY2 端口跳跃',
};

const ARTIFACT_KINDS = Object.keys(ARTIFACT_LABEL) as ArtifactKind[];

function artifactState(node: NodeAgentStateItem, kind: ArtifactKind): string {
  return appliedOf(node)?.[kind]?.state ?? 'unknown';
}

/** 产物状态片。片上写产物名而不是状态词：五件产物排成一条状态带后，五个「ON」并列
 *  无法分辨说的是哪一件；状态由图标和颜色承担，名字才是这一片的标识。
 *  图标不是颜色的重复——ON 与 DIFF 此前只由绿/红区分，形状让它们不依赖颜色也能分辨。 */
function StateChip({ state, label }: { state: string; label: string }) {
  const glyph = STATE_ICON[state];
  return (
    <span className={`st nd-rt-chip ${STATE_CLASS[state] ?? ''}`} title={`${label} ${STATE_TITLE[state] ?? state}`}>
      {glyph && <Icon of={glyph} size={9} className="nd-rt-chip-ic" />}
      {label}
    </span>
  );
}

const STATE_ICON: Record<string, IconName | undefined> = {
  present: 'check',
  disabled: 'dash',
  dirty: 'neq',
};

/* 状态词此前显示在片上（ON / OFF / N/A / ? / DIFF），产物名在左侧的标签列。
   五件产物合并成一条状态带后标签列不再存在，片上改写产物名，状态词移入 title。
   未纳管与未知没有对应图标：二者都表示「读不到这一项」，给一个形状会读成一种结果。 */
const STATE_TITLE: Record<string, string> = {
  present: '已应用',
  disabled: '已关闭',
  unmanaged: '未纳管',
  unknown: '未知',
  dirty: '已变化',
};

/* ── 字段的两种排列方式。选用依据见 styles.css，此处只提供结构。 ── */

/* ⑥ 规格网格：字段数量多、值较短、彼此平级 */

/** 一个字段格。第三个元素是跨栏数——网格是 8 栏，长值（CPU 型号、发行版、内核）
    占一栏放不下。跨栏数在窄容器下由 styles.css 按断点收窄，不在此处判断容器宽度。
    使用字面量而非数字，是为了在调用处能直接看出这是跨栏而不是别的计数。 */
type CellSpan = 'w2' | 'w3' | 'w5';
type CellItem = [string, ReactNode] | [string, ReactNode, CellSpan];

function Cells({ items }: { items: CellItem[] }) {
  return (
    <dl className="nd-rt-cells">
      {items.map(([k, v, span]) => (
        <div className={span ? `nd-rt-c ${span}` : 'nd-rt-c'} key={k}>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

/** 一条横带：一个数据来源。三条带的来源互不相同——AGENT 是 agent 轮询时写入的状态，
    HOST 来自负载上报，CONFIG 是收敛观测的结果——因此每条带自带时效读数，
    合并成一张卡后不能只在卡头写一个时间。 */
function Band({ name, icon, meta, children }: { name: string; icon: IconName; meta: ReactNode; children: ReactNode }) {
  return (
    <section className="nd-rt-band">
      <div className="nd-rt-bl">
        <b>
          <Icon of={icon} size={12} className="nd-rt-bl-ic" />
          {name}
        </b>
        <i>{meta}</i>
      </div>
      {children}
    </section>
  );
}

/** 异常读数前的标记。判定结果此前只由 `.hot` 的金色表达，而 styles.css 中没有裸
    `.hot` 的规则（只有 .plate.hot 这类作用域版本），三处判定实际渲染为普通文字。
    颜色之外再给一个形状，异常项在一片读数里才可定位。 */
function Hot({ children }: { children: ReactNode }) {
  return (
    <span className="hot">
      <Icon of="warn" size={10} className="nd-rt-hot-ic" />
      {children}
    </span>
  );
}

/* ⑤ 双列表的一行。注解通过标签的 title 提供，不附加在值之后——值列只放数据 */
function Row({ k, hint, children }: { k: string; hint?: string; children: ReactNode }) {
  return (
    <div className="row">
      <span className={hint ? 'k q' : 'k'} title={hint}>
        {k}
      </span>
      <span className="v">{children}</span>
    </div>
  );
}

/** 该地址上 agent 实际使用的出口地址。
 *
 * 置于地址输入框之后而非单开一列：配置中填写的是外部拨入使用的地址，agent 上报的
 * 是出网时的源地址，两者不一致说明地址填写错误或机器 IP 已变更——这类错误只在
 * 实际拨入时才会暴露。分置两处时需要主动进行比对才能发现。
 *
 * 经 NAT 时不做判定：此时出口地址本就与对外可拨的地址不同。 */
function EgressMirror({
  configured,
  observed,
  nat,
}: {
  configured: string | null;
  observed: string | null;
  nat: boolean;
}) {
  if (!observed) return null;
  if (nat) return <span className="egress mono">出口 {observed}</span>;
  if (configured && configured === observed) return <span className="egress">出口一致</span>;
  return (
    <span className="egress mono warn" title="配置里写的地址跟机器实际出口不是一个">
      出口 {observed}
    </span>
  );
}

function PublicIpEventRow({ event }: { event: NodePublicIpEvent }) {
  const observedAt = new Date(event.observed_at);
  const day = observedAt.toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' });
  const clock = observedAt.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
  const countryMoved =
    event.family === 'v4' &&
    event.previous_country_code &&
    event.current_country_code &&
    event.previous_country_code !== event.current_country_code;
  const facts = [event.family, event.family === 'v4' ? event.current_country_code : null, '公网观测']
    .filter(Boolean)
    .join(' · ');

  return (
    <div className={`rvh-ev${countryMoved ? ' warn' : ''}`}>
      <time>
        {day}
        <u>{clock}</u>
      </time>
      <div>
        <b className="mono">
          {event.previous_ip ?? '—'} → {event.current_ip}
        </b>
        <span className="rvh-arrow">{facts}</span>
        <i>{event.event_kind === 'first_observed' ? '首次确认' : countryMoved ? '换址 · 换国' : '换址'}</i>
      </div>
    </div>
  );
}

function usePublicIpHistory(nodeId: string) {
  return useQuery({
    queryKey: ['node-public-ip-history', nodeId, 14],
    queryFn: () => fetchNodePublicIpHistory(nodeId, 14),
    refetchInterval: 30_000,
  });
}

/** 地址变化本身作为历史事实记录，但 AGENT 带只用一个指标占位；首次观测不是变化，
 * 因此不进入计数。详情仍保留发生时间、地址族和前后地址。 */
function PublicIpHistoryDetails({
  nodeId,
  history,
  lastSeen,
}: {
  nodeId: string;
  history: ReturnType<typeof usePublicIpHistory>;
  lastSeen: string | undefined;
}) {
  const changes = history.data?.events.filter(event => event.event_kind === 'changed') ?? [];

  return (
    <div id={`public-ip-history-${nodeId}`} className="nd-public-ip-detail">
      <div className="rvh-log nd-public-ip-history">
        {history.isPending && <Loading />}
        {history.error && <ErrorBox error={history.error} />}
        {history.data &&
          (changes.length === 0 ? (
            <Empty>最近 {history.data.visible_days} 天没有公网 IP 变更</Empty>
          ) : (
            changes.map(event => <PublicIpEventRow event={event} key={event.id} />)
          ))}
        <p className="nd-public-ip-foot">
          {lastSeen ? (
            <>
              最近观测 <Ago at={lastSeen} />
            </>
          ) : (
            '等待首次观测'
          )}
          {history.data && (
            <>
              {' '}
              · 默认窗口 {history.data.visible_days} 天 · 最多追溯 {history.data.retention_days} 天
            </>
          )}
        </p>
      </div>
    </div>
  );
}

// NAT 各占一行（地址一行、其 NAT 一行）。置于地址右侧时它是整行视觉权重最高的元素，
// 高于该行的主要内容即地址；独占一行后左侧标签可明确标出是哪条 IP 的 NAT——
// IPv4 和 IPv6 各有独立开关，相邻排列时容易误操作。
// 二选一开关：两格，选中的一格反白。
// 替换了原先的圆角开关（`.switch`）——圆角开关是移动端系统控件样式，圆角胶囊加圆形滑块
// 在本页的直角、细线、等宽字中是唯一的圆角元素，且高度只有 20px，
// 与同行的输入框（33px）、按钮（31px）明显不齐。
//
// 更重要的是关闭状态下无法确定其含义：一个灰色的开关，需要先根据「该行标签是
// IPv4 NAT」推断灰色表示不经 NAT。两格都有文字标签则不需要该推断。

interface BurstObservatoryInfo {
  subjects: string[];
}

const obj = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null;

const strList = (value: unknown) =>
  Array.isArray(value) ? value.filter((x): x is string => typeof x === 'string') : [];

// 只取 subjectSelector：面板当前需要从产物中获取的唯一信息是该跳是否在探测清单中。
// 探测参数和 balancer 相关项此前也会解析并显示，展示移除后解析一并移除。
function parseBurstObservatory(content: string | null | undefined): BurstObservatoryInfo | null {
  if (!content) return null;
  let root: unknown;
  try {
    root = JSON.parse(content);
  } catch {
    return null;
  }

  const burst = obj(obj(root)?.burstObservatory);
  if (!burst) return null;

  return { subjects: strList(burst.subjectSelector) };
}

const hopSubject = (hop: Pick<LinkHealthItem, 'chain_id' | 'peer_node_id'>) =>
  `out:${hop.chain_id}>${hop.peer_node_id}`;

const parseHopSubject = (subject: string) => {
  const rest = subject.startsWith('out:') ? subject.slice(4) : subject;
  const [chainId, peerNodeId] = rest.split('>');
  if (!chainId || !peerNodeId) return null;
  return { chainId, peerNodeId };
};

/** 列表排序用的标签，与卡片上 `<b>` 渲染的是同一个值。 */
const nodeLabel = (n: NodeAgentStateItem) => n.name || n.node_id;

/* `numeric` 是必须的：字典序下 `tokyo-iij-10` 会排在 `tokyo-iij-2` 前面，机器名普遍带编号。
   指定 zh 让中文名走拼音序而不是码点序（代价是中日韩字符整体排在拉丁字母之前，
   中英混名的机器会聚到一头）。Collator 建一次复用，不要每次渲染新建。 */
const NAME_ORDER = new Intl.Collator('zh', { numeric: true, sensitivity: 'base' });

/** Only terminal lifecycle states have already lost their agent credential and teardown work. */
export const nodeRemovalReady = (node: Pick<NodeAgentStateItem, 'retired_at' | 'lifecycle_phase'>) =>
  node.retired_at !== null && (node.lifecycle_phase === 'retired' || node.lifecycle_phase === 'abandoned');

export const nodeListSnapshotFresh = (dataUpdatedAt: number, now = Date.now()) =>
  dataUpdatedAt > 0 && now - dataUpdatedAt <= 60_000;

/* 机器在链中的角色。`chains` 是它参与的链数量（不是其所属项目的链数量）。 */
// `chainUseOf` 返回的 role 是拼接后的字符串（如「入口 / 中转」），区分主干和非主干
// 两套表述（主干中转 / 中转、主干末跳），另有「规则节点」。列表只需要两端：
// 流量从哪里进入、从哪里出网。中间的角色（中转、规则节点）在本屏内不影响判断——
// 机器只要在链中，不是入口就是中间环节，中间环节不需要标注。

function NodeList({ go, sheeted = false }: { go: (d: Drill) => void; sheeted?: boolean }) {
  const { who } = useSession();
  const qc = useQueryClient();
  const nodes = useQuery({
    queryKey: ['nodes'],
    queryFn: () => fetchNodes(),
    refetchInterval: 10_000,
    refetchOnMount: 'always',
  });
  // 卡片底部只显示物理网卡累计。XRAY 业务累计仍可在用量页和机器详情的
  // XRAY 曲线中查看，不在这张用于扫视机器状态的卡片上建立第二个并列数字。
  const traffic = useQuery({
    queryKey: ['node-traffic'],
    queryFn: fetchNodeTraffic,
    enabled: nodes.isSuccess,
    refetchInterval: 30_000,
    retry: false,
  });
  // 各机器的近期负载。列表用它绘制 NIC 曲线，并决定状态灯的颜色与 title。
  // 与上面的查询一样独立：查询失败时列表正常显示，状态点回退为只反映活性。
  const load = useQuery({
    queryKey: ['node-nic-list', 'windows', LIST_NIC_WINDOWS],
    queryFn: () => fetchNodeNicListWindows(LIST_NIC_WINDOWS),
    enabled: nodes.isSuccess,
    refetchInterval: 30_000,
    retry: false,
  });
  const loadOf = useMemo(() => new Map((load.data?.nodes ?? []).map(n => [n.node_id, n])), [load.data]);
  const pingProbe = useQuery({
    queryKey: ['ping-probe-nodes', 'latest'],
    queryFn: () => fetchLatestNodePingProbes(),
    enabled: nodes.isSuccess,
    refetchInterval: query => Math.max(query.state.data?.interval_secs ?? 10, 10) * 1_000,
    retry: false,
  });
  const pingProbeOf = useMemo(
    () => new Map((pingProbe.data?.nodes ?? []).map(node => [node.node_id, node])),
    [pingProbe.data],
  );
  // 各机器在链中的角色。窄屏下第二行显示的即是该信息——扫视列表时需要的是
  // 该机器是入口还是出口、参与了几条链，而不是它的公网地址。
  // 使用详情页的 `chainUseOf`（同一套判定，两处不会给出不同角色），
  // 数据来自外壳已在拉取的 `['snapshot']`，不新增请求。
  // 使用 `null` 而非空 Map：该机器未加入任何链与数据尚未就绪是两种状态。
  // 批量退役。行内不常驻删除和退役控件——列表上一次误触即导致一台机器停止服务，
  // 因此入口是显式的「多选」：不启用多选时，本页只有导航操作。
  //
  // 这几个 hook 必须位于下面两条 early return 之前：放在 `list` 附近可读性更好，但
  // 那样它们只在数据就绪的那次渲染中被调用，hook 数量前后不一致会导致 React 报错（#310）。
  const [selectionMode, setSelectionMode] = useState<'retire' | 'remove' | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [removalNotice, setRemovalNotice] = useState<string | null>(null);
  const togglePick = (id: string) =>
    setPicked(prev => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const exitSelect = () => {
    setSelectionMode(null);
    setPicked(new Set());
  };
  const beginSelect = (mode: 'retire' | 'remove') => {
    setSelectionMode(mode);
    setPicked(new Set());
  };
  const retireAll = useMutation({
    // 名单在提交时重新计算，不使用渲染时的快照：从勾选到点击按钮之间列表会自动刷新
    // （10 秒一次），期间若有机器已在别处退役，此处不应重复提交。
    mutationFn: async () => {
      const live = (nodes.data?.nodes ?? []).filter(n => picked.has(n.node_id) && !n.retired_at);
      for (const n of live) await setNodeStatus(n.node_id, 'retired');
    },
    onSuccess: () => {
      exitSelect();
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
    },
  });
  const removeNodes = useMutation({
    mutationFn: (nodeIds: string[]) => removeRetiredNodes(nodeIds),
    onSuccess: result => {
      exitSelect();
      setRemovalNotice(
        `已移除 ${result.removed_nodes.length} 台机器，并清理 ${result.removed_chains.length} 条关联线路。`,
      );
      for (const key of [
        ['nodes'],
        ['snapshot'],
        ['revisions'],
        ['compile'],
        ['deployments'],
        ['usage-node-series'],
        ['node-load-list'],
        ['node-nic-list'],
        ['ping-probe-nodes'],
        ['link-health'],
        ['link-mtu'],
        ['certs'],
        ['agent-log-policy'],
        ['node-traffic'],
      ]) {
        qc.invalidateQueries({ queryKey: key });
      }
    },
  });

  const section = sheeted ? 'fg-sheet' : 'panel';
  if (nodes.isPending) return <Loading variant="nodes" sheeted={sheeted} />;
  if (nodes.error) return <ErrorBox error={nodes.error} />;

  // 已退役的排到末尾，其余按卡片上显示的那个标签排。
  //
  // 服务端给的是 `ORDER BY n.id`（brocade-store/src/console.rs 的 node_agent_state_sql），
  // 而卡片显示的是 `name || node_id`——id 是纳管时填的 slug，之后改名不会动它，于是列表
  // 按一个不显示的字段排序、展示另一个字段，改过名的机器位置就没有道理。
  //
  // 排序键仍然只有这两个维度。不引入在线/掉线，也不引入流量：那两项都会随轮询变化，
  // 列表每隔十几秒重排一次，鼠标伸过去的那台已经挪走了。名字不会自己变，所以稳定。
  const list = [...nodes.data.nodes].sort(
    (a, b) =>
      Number(!!a.retired_at) - Number(!!b.retired_at) ||
      NAME_ORDER.compare(nodeLabel(a), nodeLabel(b)) ||
      // 同名机器（`name` 没有唯一约束）之间仍需一个确定的顺序，否则两次渲染可能不同。
      NAME_ORDER.compare(a.node_id, b.node_id),
  );
  const live = list.filter(n => !n.retired_at);
  const retired = list.filter(n => !!n.retired_at);
  // React Query deliberately reuses the last successful list on route changes. Names and layout
  // remain useful, but an old last_poll_at must not briefly turn every lamp red before the forced
  // mount refresh completes.
  const nodeSnapshotFresh = nodeListSnapshotFresh(nodes.dataUpdatedAt);
  const isolatedNodeIds = new Set(list.filter(n => n.operationally_isolated).map(n => n.node_id));
  const removable = retired.filter(nodeRemovalReady);
  // 已退役的不计入：对其再次提交退役是不产生任何变更的草稿操作，
  // 而计入后会使操作者认为本次退役了 N 台。
  const pickedLive = [...picked].filter(id => list.some(n => n.node_id === id && !n.retired_at));
  const pickedRetired = [...picked].filter(id => removable.some(n => n.node_id === id));
  const confirmRemoval = (ids: string[]) => {
    const targets = [...new Set(ids)].sort();
    if (targets.length === 0) return;
    const names = targets.map(id => nodeLabel(list.find(node => node.node_id === id)!));
    const confirmed = window.confirm(
      `确定永久移除 ${targets.length} 台退役机器吗？\n\n${names.join('、')}\n\n` +
        '所有包含这些机器的线路会一并删除，机器的运行记录、发布目标和用量明细也会清理。此操作不能撤销。',
    );
    if (!confirmed) return;
    setRemovalNotice(null);
    removeNodes.mutate(targets);
  };
  const trafficOf = new Map((traffic.data?.nodes ?? []).map(item => [item.node_id, item]));
  const renderHeader = () => (
    <header>
      <ListIcon of="nodes" />
      <h4>机器</h4>
      <span className="sp" />
      {selectionMode === 'retire' ? (
        <>
          <button className="btn" onClick={exitSelect}>
            取消
          </button>
          {/* 退役直接创建停用发布，不进入浏览器草稿。 */}
          <button
            className="btn danger"
            disabled={pickedLive.length === 0 || retireAll.isPending}
            title={`退役选中的 ${pickedLive.length} 台：保留记录，并为 Agent 创建完整停用发布`}
            onClick={() => retireAll.mutate()}
          >
            {retireAll.isPending ? '提交中…' : `退役下线${pickedLive.length > 0 ? ` ${pickedLive.length}` : ''}`}
          </button>
        </>
      ) : (
        <>
          <button
            className="btn"
            disabled={!can(who.role, 'system') || selectionMode !== null}
            onClick={() => beginSelect('retire')}
          >
            多选
          </button>
          <button
            className="btn primary"
            disabled={!can(who.role, 'system')}
            onClick={() => go({ p: 'provision', step: 1 })}
          >
            ＋ 纳管机器
          </button>
        </>
      )}
    </header>
  );
  const renderCards = (items: NodeAgentStateItem[], purpose: 'retire' | 'remove') => (
    <div className="ncards">
      {items.map(n => (
        <NodeCard
          key={n.node_id}
          node={n}
          snapshotFresh={nodeSnapshotFresh}
          isolatedNodeIds={isolatedNodeIds}
          traffic={trafficOf.get(n.node_id)}
          load={loadOf.get(n.node_id)}
          pingProbe={pingProbeOf.get(n.node_id)}
          pingProbeIntervalSecs={pingProbe.data?.interval_secs ?? 60}
          pingProbeReady={pingProbe.isSuccess}
          trafficPending={traffic.isPending}
          selecting={selectionMode === purpose}
          selectable={purpose === 'retire' ? !n.retired_at : nodeRemovalReady(n)}
          checked={picked.has(n.node_id)}
          onPick={() => togglePick(n.node_id)}
          onPrepare={() => void prefetchNodeDetailData(qc, n.node_id, !isVisitor(who))}
          go={go}
        />
      ))}
    </div>
  );
  const emptyMachines = (
    <EmptyState
      icon="nodes"
      title="还没有机器"
      action={
        <button
          className="btn primary"
          disabled={!can(who.role, 'system')}
          onClick={() => go({ p: 'provision', step: 1 })}
        >
          纳管第一台机器
        </button>
      }
    >
      先登记机器身份与网络信息，再在目标机器上安装 Agent。
    </EmptyState>
  );

  if (sheeted) {
    return (
      <div className="cardpage node-cardpage">
        <section className="panel titled node-list-panel">
          {renderHeader()}
          {retireAll.error && <ErrorBox error={retireAll.error} />}
          {removalNotice && <div className="callout">{removalNotice}</div>}
          {list.length === 0 ? (
            emptyMachines
          ) : live.length === 0 ? (
            <Empty>没有在用机器。</Empty>
          ) : (
            renderCards(live, 'retire')
          )}
        </section>
        {retired.length > 0 && (
          <section className="panel titled node-retired-panel">
            <header>
              <PanelTitle of="nodes">退役机器</PanelTitle>
              <span className="hint">
                {retired.length} 台{removable.length !== retired.length ? ` · ${removable.length} 台可移除` : ''}
              </span>
              <span className="sp" />
              {selectionMode === 'remove' ? (
                <>
                  <button className="btn" type="button" disabled={removeNodes.isPending} onClick={exitSelect}>
                    取消
                  </button>
                  <button
                    className="btn danger node-remove-button"
                    type="button"
                    disabled={pickedRetired.length === 0 || removeNodes.isPending}
                    onClick={() => confirmRemoval(pickedRetired)}
                  >
                    <Icon of="trash" size={13} className="node-remove-button-icon" />
                    {removeNodes.isPending
                      ? '移除中…'
                      : `移除选中${pickedRetired.length > 0 ? ` ${pickedRetired.length}` : ''}`}
                  </button>
                </>
              ) : (
                <>
                  <button
                    className="btn"
                    type="button"
                    disabled={
                      !can(who.role, 'system') ||
                      removable.length === 0 ||
                      selectionMode !== null ||
                      removeNodes.isPending
                    }
                    onClick={() => beginSelect('remove')}
                  >
                    多选
                  </button>
                  <button
                    className="btn danger node-remove-button"
                    type="button"
                    disabled={
                      !can(who.role, 'system') ||
                      removable.length === 0 ||
                      selectionMode !== null ||
                      removeNodes.isPending
                    }
                    onClick={() => confirmRemoval(removable.map(node => node.node_id))}
                  >
                    <Icon of="trash" size={13} className="node-remove-button-icon" />
                    {removeNodes.isPending ? '移除中…' : '移除全部'}
                  </button>
                </>
              )}
            </header>
            {removeNodes.error && <ErrorBox error={removeNodes.error} />}
            {renderCards(retired, 'remove')}
          </section>
        )}
      </div>
    );
  }

  return (
    <div className={section}>
      {renderHeader()}
      {retireAll.error && <ErrorBox error={retireAll.error} />}
      {removeNodes.error && <ErrorBox error={removeNodes.error} />}
      {removalNotice && <div className="callout">{removalNotice}</div>}
      {list.length === 0 ? emptyMachines : renderCards(list, 'retire')}
    </div>
  );
}

// 状态点只表示一项：agent 是否仍在拉取配置。
// 判定依据与纳管向导一致——使用 last_poll_at 而非 usage 上报，60s = agent 15s 轮询
// 加 systemd RestartSec=5 加时钟误差。灰色表示从未上报，与掉线是两种状态。
function pollTone(node: NodeAgentStateItem): 'ok' | 'bad' | 'idle' {
  if (!node.last_poll_at) return 'idle';
  const raw = node.last_poll_at;
  const t = Date.parse(raw.endsWith('Z') || raw.includes('+') ? raw : `${raw}Z`);
  if (Number.isNaN(t)) return 'idle';
  return Date.now() - t > 60_000 ? 'bad' : 'ok';
}

type NodeLampState = {
  tone: 'ok' | 'warn' | 'bad' | 'idle';
  why: string;
};

/** 节点列表和详情书签共用的状态灯。在线状态优先，失联后不再用旧上报里的
 * finding 推断当前状态；detailOnly 只是详情说明，不改变灯色。 */
function nodeLampState(
  node: NodeAgentStateItem,
  wireguardEnabled?: boolean,
  isolatedNodeIds?: ReadonlySet<string>,
): NodeLampState {
  if (node.lifecycle_phase === 'retiring') return { tone: 'warn', why: '退役中：等待停用收敛' };
  if (node.lifecycle_phase === 'retired') {
    return node.lifecycle_last_error
      ? { tone: 'warn', why: `已停用，外部资源待清理：${node.lifecycle_last_error}` }
      : { tone: 'idle', why: '已退役并确认停用' };
  }
  if (node.lifecycle_phase === 'abandoned') return { tone: 'bad', why: '强制退役：未确认远端停用' };
  if (node.operationally_isolated) {
    return {
      tone: 'warn',
      why:
        node.convergence_debt_count > 0
          ? `已隔离：${node.convergence_debt_count} 项收敛债务`
          : '已隔离：等待管理员恢复服务',
    };
  }

  const live = pollTone(node);
  if (live === 'idle') return { tone: 'idle', why: '从未上报' };
  if (live === 'bad') return { tone: 'bad', why: '失联' };

  const findings = runtimeFindings(node, wireguardEnabled, isolatedNodeIds).filter(f => !f.detailOnly);
  if (findings.length === 0) return { tone: 'ok', why: '没有要处理的' };

  return {
    tone: findings.some(f => f.tone === 'bad') ? 'bad' : 'warn',
    why: findings.map(f => f.chip).join(' · '),
  };
}

function nodeLifecycleLabel(node: NodeAgentStateItem): string | null {
  switch (node.lifecycle_phase) {
    case 'active':
      return null;
    case 'retiring':
      return '退役中';
    case 'retired':
      return node.lifecycle_last_error ? '待清理' : '已退役';
    case 'abandoned':
      return '强制退役';
  }
}

/** 卡片上的地址字段。
 *
 * 只显示一个地址：有 v4 显示 v4，否则显示 v6，两族齐全时完整值在 title 中。NAT 状态和
 * 是否另有 v6 地址都不在此显示——列表用于快速扫视，这两项需要停下阅读，应在详情页查看。
 *
 * 「仅 overlay」需要与「无地址」区分：纯 overlay 的机器不是没有地址，而是**无法直接拨入，
 * 只能通过骨干网访问**。因此显示「仅 overlay」而非留空，颜色使用中性灰而非告警色——
 * 它是一种属性，不是异常。具体的 overlay 地址不在该列表的数据中（`NodeAgentStateItem`
 * 没有该字段），需在详情页查看。 */
function NodeAddr({ node }: { node: NodeAgentStateItem }) {
  const { public_ipv4: v4, public_ipv6: v6 } = node;
  if (!v4 && !v6) {
    return (
      <span className="nc-ip none" title="没有公网地址，只存在于 overlay 中：无法直接连接，只能经骨干访问">
        仅 overlay
      </span>
    );
  }
  const main = v4 ?? (v6 as string);
  return (
    <span className="nc-ip" title={[v4, v6].filter(Boolean).join('  ')}>
      {main}
    </span>
  );
}

/** 一台机器一张卡片。
 *
 * 由行改为卡片的取舍：行的优势不在于节省空间，而在于**对齐的数字列可以纵向扫视**——
 * 二十台机器时视线可沿本月用量列直接向下。卡片放弃该能力，换取每台可显示一条趋势线，
 * 而用量上升趋势比当前用量更早反映问题。
 *
 * 卡片上只有五项：地区与状态的组合标识、名称、流量线、本月总量和 IP。移除的每一项原因相同——
 * **它们回答的是具体问题，而本页的职责是定位哪台机器有问题**：
 *   角色标签（入口/中转/末跳）  该机器在链中的位置，属于详情页
 *   异常文字                    压缩为状态点的颜色，具体项在 title 中
 *   租户                        确定后不再变化的属性
 *   内存 / 磁盘 / 连接表        正常时不携带信息，越限时会反映在状态点颜色上
 */
function NodeCard({
  node,
  snapshotFresh,
  isolatedNodeIds,
  load,
  pingProbe,
  pingProbeIntervalSecs,
  pingProbeReady,
  traffic,
  trafficPending,
  selecting,
  selectable,
  checked,
  onPick,
  onPrepare,
  go,
}: {
  node: NodeAgentStateItem;
  /** False means the cached inventory may still be rendered, but its liveness is not evidence. */
  snapshotFresh: boolean;
  isolatedNodeIds: ReadonlySet<string>;
  /** 该机器的近期负载。undefined 表示尚未读取或查询失败。 */
  load?: NodeNicView;
  /** 每个探测目标的最新一次读数；配置 TCP 目标后在卡片右下角替换 IP。 */
  pingProbe?: NodePingProbeLatestView;
  pingProbeIntervalSecs: number;
  /** 只有接口成功返回才能证明「没有配置目标」；加载中和请求失败都不能回退显示 IP。 */
  pingProbeReady: boolean;
  traffic?: NodeTrafficItem;
  trafficPending: boolean;
  selecting: boolean;
  selectable: boolean;
  checked: boolean;
  onPick: () => void;
  onPrepare: () => void;
  go: (d: Drill) => void;
}) {
  const { who } = useSession();
  const canCreate = can(who.role, 'edit');
  const retired = node.lifecycle_phase !== 'active';
  const narrow = useNarrow();
  const live = snapshotFresh ? pollTone(node) : 'idle';
  // 具体项写入 title，悬停即可在扫视列表时确认黄/红色对应的问题。
  const lamp = snapshotFresh
    ? nodeLampState(node, undefined, isolatedNodeIds)
    : ({ tone: 'idle', why: '缓存状态待确认' } satisfies NodeLampState);

  const prepareTimer = useRef(0);
  useEffect(
    () => () => {
      if (prepareTimer.current) window.clearTimeout(prepareTimer.current);
    },
    [],
  );
  const prepareNow = () => {
    if (selecting) return;
    if (prepareTimer.current) window.clearTimeout(prepareTimer.current);
    prepareTimer.current = 0;
    onPrepare();
  };
  const schedulePrepare = () => {
    if (selecting || prepareTimer.current) return;
    // A short dwell distinguishes intent from merely sweeping the pointer across a 15-card grid.
    // Keyboard focus and touch press below remain immediate because both are explicit navigation.
    prepareTimer.current = window.setTimeout(prepareNow, 120);
  };
  const cancelPrepare = () => {
    if (prepareTimer.current) window.clearTimeout(prepareTimer.current);
    prepareTimer.current = 0;
  };
  const open = () => {
    if (selecting) return selectable && onPick();
    prepareNow();
    return go({ p: 'node', id: node.node_id });
  };
  return (
    <article
      role="button"
      tabIndex={0}
      data-route-focus={`node:${node.node_id}`}
      className={`ncard tone-${lamp.tone}${retired ? ' off' : ''}${checked ? ' picked' : ''}`}
      onMouseEnter={schedulePrepare}
      onMouseLeave={cancelPrepare}
      onFocus={event => {
        if (event.target === event.currentTarget) prepareNow();
      }}
      onPointerDown={event => {
        if (event.pointerType !== 'mouse') prepareNow();
      }}
      onClick={open}
      onKeyDown={e => {
        if (e.target !== e.currentTarget) return;
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          open();
        }
      }}
    >
      <div className="nc-head">
        {/* 多选框临时插在身份信息之前，不替换状态灯。列表卡保持一条扁平的
            「状态灯 → 完整区域旗 → 名字」阅读顺序，不复用详情页的方形组合徽标。 */}
        {selecting && (
          <span className="nc-pick-slot">
            <input
              type="checkbox"
              className="nc-pick"
              checked={checked}
              disabled={!selectable}
              aria-label={`选中 ${node.name || node.node_id}`}
              onChange={onPick}
              onClick={e => e.stopPropagation()}
            />
          </span>
        )}
        <span className={`nc-status-slot${node.public_ipv4_country ? ' with-region' : ''}`}>
          <i className={`node-lamp ${lamp.tone}`} title={lamp.why} aria-label={lamp.why} />
        </span>
        {node.public_ipv4_country && (
          <span className="nc-region-flag">
            <RegionFlag code={node.public_ipv4_country} />
          </span>
        )}
        <b className={node.name ? undefined : 'mono'}>{node.name || node.node_id}</b>
        {/* 时间只在异常时着色。正常拉取的机器都显示为十几秒前，且每十秒各自变化，
            扫视列表时不携带信息——机器是否在线已由状态点表示。 */}
        <span className={`nc-ago${live === 'bad' && !retired ? ' bad' : ''}`}>
          {retired ? nodeLifecycleLabel(node) : <PollAgo at={node.last_poll_at} />}
        </span>
      </div>

      <NicWave load={load} />

      <div className="nc-foot">
        <NicMonthTotal traffic={traffic} pending={trafficPending} retired={retired} />
        <span className="sp" />
        {/* 按钮位于卡片右下角（CSS 中 position:absolute），悬停时覆盖在 IP 或 TCP 延迟之上。
            由于脱离文档流，它的显示和隐藏都不会改变右下角摘要的位置。
            「打开」已移除——整张卡片本身可点击，一屏九张各带一个按钮会形成密集的按钮排列。 */}
        <span className="nc-dock" onClick={e => e.stopPropagation()}>
          {!narrow && !retired && (
            <button className="btn" disabled={!canCreate} onClick={() => go({ p: 'chain', id: node.node_id })}>
              建一条链
            </button>
          )}
        </span>
        {!pingProbeReady || (pingProbe && pingProbe.targets.some(target => target.kind === 'tcp')) ? (
          <TcpProbeLatest view={pingProbe} intervalSecs={pingProbeIntervalSecs} pending={!pingProbeReady} />
        ) : (
          <NodeAddr node={node} />
        )}
      </div>
    </article>
  );
}

export const LOAD_RANGES = [
  { seconds: 30 * 60, label: '30m', menuLabel: '近 30 分钟', heading: '30 MINUTES' },
  { seconds: 60 * 60, label: '1h', menuLabel: '近 1 小时', heading: '1 HOUR' },
  { seconds: 6 * 60 * 60, label: '6h', menuLabel: '近 6 小时', heading: '6 HOURS' },
  { seconds: 12 * 60 * 60, label: '12h', menuLabel: '近 12 小时', heading: '12 HOURS' },
  { seconds: 24 * 60 * 60, label: '24h', menuLabel: '近 24 小时', heading: '24 HOURS' },
] as const;
export const DEFAULT_LOAD_RANGE: LoadRange = LOAD_RANGES[1];
const GRID_SECS = 30;
export interface LoadRange {
  seconds: number;
  label: string;
  menuLabel: string;
  heading: string;
  /** 存在时是固定历史区间；没有时是随当前时间移动的最近 N 秒。 */
  startUnixSecs?: number;
  endUnixSecs?: number;
}

const MAX_DETAIL_RANGE_SECS = 24 * 60 * 60;
const MIN_DETAIL_RANGE_SECS = 60;

export function loadRangeBounds(range: LoadRange): { startUnixSecs: number; endUnixSecs: number } {
  if (range.startUnixSecs != null && range.endUnixSecs != null) {
    return { startUnixSecs: range.startUnixSecs, endUnixSecs: range.endUnixSecs };
  }
  return absoluteLoadRange(range.seconds);
}

function loadRangeKey(range: LoadRange): string | number {
  return range.startUnixSecs != null && range.endUnixSecs != null
    ? `${range.startUnixSecs}-${range.endUnixSecs}`
    : range.seconds;
}

function fixedLoadRange(range: LoadRange): boolean {
  return range.startUnixSecs != null && range.endUnixSecs != null;
}

function absoluteLoadRange(seconds: number): { startUnixSecs: number; endUnixSecs: number } {
  const endUnixSecs = Math.floor(Date.now() / 1000);
  return { startUnixSecs: endUnixSecs - seconds, endUnixSecs };
}

function fetchNodeLoadInRange(nodeId: string, range: LoadRange) {
  const { startUnixSecs, endUnixSecs } = loadRangeBounds(range);
  return fetchNodeLoadOverview(nodeId, startUnixSecs, endUnixSecs);
}

function fetchUsageInRange(nodeId: string, range: LoadRange) {
  if (!fixedLoadRange(range)) return fetchUsageNodeSeries(range.seconds, nodeId);
  const { startUnixSecs, endUnixSecs } = loadRangeBounds(range);
  return fetchUsageNodeSeriesRange(startUnixSecs, endUnixSecs, nodeId);
}

function fetchPingInRange(nodeId: string, range: LoadRange, signal?: AbortSignal) {
  if (!fixedLoadRange(range)) return fetchNodePingProbe(nodeId, range.seconds, '', signal);
  const { startUnixSecs, endUnixSecs } = loadRangeBounds(range);
  return fetchNodePingProbeRange(nodeId, startUnixSecs, endUnixSecs, '', signal);
}

/** Match refresh cost to the amount of immutable history in the response. Recent ranges still
 * track the ten-second probe cadence; 24-hour charts no longer download the whole day every five
 * seconds. Fixed historical ranges never change and therefore never poll. */
export function pingRefreshMillis(range: LoadRange): number | false {
  if (fixedLoadRange(range)) return false;
  if (range.seconds <= 60 * 60) return 10_000;
  if (range.seconds <= 6 * 60 * 60) return 30_000;
  return 60_000;
}

// 时间范围切换和三个观测面板必须共用完全相同的查询定义。切换前先通过这些定义填充
// React Query 缓存，完成后再更换面板读取的 key；否则三个请求按各自返回顺序卸载/重挂
// 面板，会把同一次“切换范围”表现成三轮互不相关的加载。
const NODE_DETAIL_LIVE_STALE_MS = 10_000;

function nodeLoadRangeQuery(nodeId: string, range: LoadRange) {
  return {
    queryKey: ['node-load-history', nodeId, loadRangeKey(range)] as const,
    queryFn: () => fetchNodeLoadInRange(nodeId, range),
    staleTime: fixedLoadRange(range) ? Infinity : NODE_DETAIL_LIVE_STALE_MS,
    retry: false,
  };
}

function nodeUsageRangeQuery(nodeId: string, range: LoadRange) {
  return {
    queryKey: ['usage-node-series', nodeId, loadRangeKey(range)] as const,
    queryFn: () => fetchUsageInRange(nodeId, range),
    staleTime: fixedLoadRange(range) ? Infinity : NODE_DETAIL_LIVE_STALE_MS,
  };
}

function nodePingRangeQuery(nodeId: string, range: LoadRange) {
  const refreshMillis = pingRefreshMillis(range);
  return {
    queryKey: ['node-ping-probe', nodeId, loadRangeKey(range)] as const,
    queryFn: ({ signal }: { signal: AbortSignal }) => fetchPingInRange(nodeId, range, signal),
    staleTime: refreshMillis || Infinity,
    refetchInterval: refreshMillis,
    retry: false,
  };
}

/** Warm only the machine the operator has expressed intent to open.
 *
 * The rolling-range query keys deliberately omit the wall clock. A prefetched response can paint
 * the first frame immediately; after ten seconds it is stale and the mounted detail page fetches
 * the complete moving window again, replacing the cache with every point that arrived between
 * prefetch and navigation. If the entry has already been garbage-collected, the same full fetch
 * simply becomes the initial request. Either path cannot leave a permanent time hole. */
export async function prefetchNodeDetailData(
  queryClient: QueryClient,
  nodeId: string,
  includeDeployments: boolean,
): Promise<void> {
  const range = DEFAULT_LOAD_RANGE;
  const tasks: Promise<unknown>[] = [
    loadNodeObservationModules(),
    queryClient.prefetchQuery(nodeLoadRangeQuery(nodeId, range)),
    queryClient.prefetchQuery(nodeUsageRangeQuery(nodeId, range)),
    queryClient.prefetchQuery(nodePingRangeQuery(nodeId, range)),
    queryClient.prefetchQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot(), staleTime: 30_000 }),
  ];
  if (includeDeployments) {
    tasks.push(
      queryClient.prefetchQuery({
        queryKey: ['deployments'],
        queryFn: () => fetchDeployments(),
        staleTime: 30_000,
      }),
    );
  }
  await Promise.allSettled(tasks);
}

export function ObserveLinkControl({ value, onChange }: { value: boolean; onChange: (value: boolean) => void }) {
  return (
    <label
      className="switch observe-link-switch"
      title={value ? '已同步同组图表的时间位置与 Tooltip' : '各图表独立显示 Tooltip'}
    >
      <span className="switch-label">同组图表联动</span>
      <input
        type="checkbox"
        role="switch"
        aria-label="同组图表联动"
        aria-checked={value}
        checked={value}
        onChange={event => onChange(event.target.checked)}
      />
      <span className="switch-ui" aria-hidden="true">
        <span />
      </span>
    </label>
  );
}

export function ObserveRangeControl({
  value,
  onChange,
  pending = false,
}: {
  value: LoadRange;
  onChange: (value: LoadRange) => void;
  pending?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const rootRef = useRef<HTMLSpanElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);

  const localInput = (unixSecs: number) => {
    const date = new Date(unixSecs * 1000);
    const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
    return local.toISOString().slice(0, 16);
  };
  const resetAbsoluteFields = () => {
    const bounds = loadRangeBounds(value);
    setFrom(localInput(bounds.startUnixSecs));
    setTo(localInput(bounds.endUnixSecs));
  };
  const openMenu = () => {
    if (!open) resetAbsoluteFields();
    setOpen(current => !current);
  };

  const fromSecs = Math.floor(new Date(from).getTime() / 1000);
  const toSecs = Math.floor(new Date(to).getTime() / 1000);
  const span = toSecs - fromSecs;
  const absoluteError =
    !Number.isFinite(fromSecs) || !Number.isFinite(toSecs)
      ? '请选择完整的起止时间'
      : span < MIN_DETAIL_RANGE_SECS
        ? '时间范围至少 1 分钟'
        : span > MAX_DETAIL_RANGE_SECS
          ? '时间范围最多 24 小时'
          : null;

  const shortDateTime = (unixSecs: number) =>
    new Date(unixSecs * 1000).toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
    });

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!rootRef.current?.contains(target) && !menuRef.current?.contains(target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      setOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener('pointerdown', outside);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('keydown', escape);
    };
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return;
    const position = () => {
      const menu = menuRef.current;
      if (!menu) return;
      if (window.innerWidth <= 520) {
        menu.style.removeProperty('top');
        menu.style.removeProperty('left');
        return;
      }
      const trigger = triggerRef.current?.getBoundingClientRect();
      if (!trigger) return;
      const margin = 10;
      const gap = 5;
      const left = Math.min(
        window.innerWidth - menu.offsetWidth - margin,
        Math.max(margin, trigger.right - menu.offsetWidth),
      );
      const below = trigger.bottom + gap;
      const top =
        below + menu.offsetHeight <= window.innerHeight - margin
          ? below
          : Math.max(margin, trigger.top - menu.offsetHeight - gap);
      menu.style.top = `${top}px`;
      menu.style.left = `${left}px`;
    };
    position();
    window.addEventListener('resize', position);
    window.addEventListener('scroll', position, true);
    return () => {
      window.removeEventListener('resize', position);
      window.removeEventListener('scroll', position, true);
    };
  }, [open]);

  const focusOption = (index: number) => {
    const count = LOAD_RANGES.length;
    optionRefs.current[(index + count) % count]?.focus();
  };
  const openFromKeyboard = (index: number) => {
    setOpen(true);
    window.requestAnimationFrame(() => focusOption(index));
  };

  return (
    <span ref={rootRef} className={`observe-range${open ? ' open' : ''}`}>
      <button
        ref={triggerRef}
        type="button"
        className="btn observe-range-trigger"
        aria-label={`观测时间范围：${value.menuLabel}`}
        aria-busy={pending}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={openMenu}
        onKeyDown={event => {
          const selected = Math.max(
            0,
            LOAD_RANGES.findIndex(option => !fixedLoadRange(value) && option.seconds === value.seconds),
          );
          if (event.key === 'ArrowDown') {
            event.preventDefault();
            openFromKeyboard(selected);
          } else if (event.key === 'ArrowUp') {
            event.preventDefault();
            openFromKeyboard(selected);
          }
        }}
      >
        <Icon of="calendar" size={14} className="nd-tool-icon observe-range-clock" />
        <span>{value.menuLabel}</span>
      </button>
      {open &&
        createPortal(
          <div ref={menuRef} className="observe-range-menu" role="dialog" aria-label="观测时间范围">
            <div
              className="observe-range-presets"
              role="listbox"
              aria-label="快速范围"
              onKeyDown={event => {
                const current = optionRefs.current.indexOf(document.activeElement as HTMLButtonElement);
                if (event.key === 'ArrowDown') {
                  event.preventDefault();
                  focusOption(current + 1);
                } else if (event.key === 'ArrowUp') {
                  event.preventDefault();
                  focusOption(current - 1);
                } else if (event.key === 'Home') {
                  event.preventDefault();
                  focusOption(0);
                } else if (event.key === 'End') {
                  event.preventDefault();
                  focusOption(LOAD_RANGES.length - 1);
                }
              }}
            >
              <b>快速范围</b>
              {LOAD_RANGES.map((option, index) => (
                <button
                  key={option.seconds}
                  ref={element => {
                    optionRefs.current[index] = element;
                  }}
                  type="button"
                  role="option"
                  aria-selected={!fixedLoadRange(value) && value.seconds === option.seconds}
                  onClick={() => {
                    onChange(option);
                    setOpen(false);
                    triggerRef.current?.focus();
                  }}
                >
                  {option.menuLabel}
                  <span className="observe-range-check" aria-hidden="true">
                    ✓
                  </span>
                </button>
              ))}
            </div>
            <div className="observe-range-absolute">
              <b>自定义范围</b>
              <label>
                <span>从</span>
                <input
                  aria-label="观测开始时间"
                  type="datetime-local"
                  value={from}
                  onChange={event => setFrom(event.target.value)}
                />
              </label>
              <label>
                <span>到</span>
                <input
                  aria-label="观测结束时间"
                  type="datetime-local"
                  value={to}
                  onChange={event => setTo(event.target.value)}
                />
              </label>
              <span className={`observe-range-error${absoluteError ? ' bad' : ''}`}>
                {absoluteError ?? '固定区间，最长 24 小时'}
              </span>
              <button
                type="button"
                className="observe-range-apply"
                disabled={absoluteError !== null}
                onClick={() => {
                  const menuLabel = `${shortDateTime(fromSecs)} → ${shortDateTime(toSecs)}`;
                  onChange({
                    seconds: span,
                    label: 'custom',
                    menuLabel,
                    heading: 'CUSTOM RANGE',
                    startUnixSecs: fromSecs,
                    endUnixSecs: toSecs,
                  });
                  setOpen(false);
                  triggerRef.current?.focus();
                }}
              >
                应用时间范围
              </button>
            </div>
          </div>,
          document.body,
        )}
    </span>
  );
}

// 列表保留最近 24 个实际样本。窗口查询没有固定墙上时间边界，曲线按首尾可绘制样本铺满；
// 这里表达的是停机前最后一段趋势，在线/离线由机器卡已有的独立活性状态表达。
const LIST_NIC_WINDOWS = 24;
const LIST_NIC_WINDOW_SECS = 30;
// 上一版的最低尺度是 1 Mbit/s。改为每窗口字节数后做等价换算，避免只因换单位就改变曲线高度。
const LIST_NIC_MIN_CEILING_BYTES = (1_000_000 * LIST_NIC_WINDOW_SECS) / 8;
// usage 查询仍需要一个近期窗口参数，但列表现在只读其中的本月累计字段。
/** 将采样点连接为平滑曲线，且**保证曲线不超出相邻两点之间的取值范围**。
 *
 * 使用 Fritsch–Carlson 单调三次插值，而非更常见的 Catmull-Rom。原因是过冲：Catmull-Rom
 * 的控制点由前后邻点的斜率决定，遇到陡升陡降时会超出数据范围——单个峰值旁的谷底可达
 * y=27.4（画布高度只有 26），流量起始格的峰顶可达 y=-0.8。超出部分被 SVG viewport 裁剪，
 * 表现为部分波峰波谷不可见，而这些正是流量变化最剧烈、最需要显示的时段。
 *
 * 单调插值的做法是先按邻段斜率计算每点的切线，再限制切线大小（Fritsch–Carlson 的 3 倍上限），
 * 使每一段保持单调——单调意味着段内极值只出现在两个端点，从而不会过冲。平坦段
 * （两点取值相同）的切线为零，因此贴底的零基线是水平的，不会隆起。 */
function smoothPath(xs: number[], ys: number[]): string {
  const n = xs.length;
  if (n < 2) return `M${xs[0]?.toFixed(1) ?? 0},${ys[0]?.toFixed(1) ?? 0}`;
  const slope = new Array<number>(n - 1);
  for (let i = 0; i < n - 1; i++) slope[i] = (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]);
  const m = new Array<number>(n);
  m[0] = slope[0];
  m[n - 1] = slope[n - 2];
  for (let i = 1; i < n - 1; i++) {
    // 升降转折点处切线取 0：否则该点会被切线带出取值范围。
    m[i] = slope[i - 1] * slope[i] <= 0 ? 0 : (slope[i - 1] + slope[i]) / 2;
  }
  for (let i = 0; i < n - 1; i++) {
    if (slope[i] === 0) {
      m[i] = 0;
      m[i + 1] = 0;
      continue;
    }
    const a = m[i] / slope[i];
    const b = m[i + 1] / slope[i];
    const s = Math.hypot(a, b);
    if (s > 3) {
      m[i] = (3 / s) * a * slope[i];
      m[i + 1] = (3 / s) * b * slope[i];
    }
  }
  let d = `M${xs[0].toFixed(1)},${ys[0].toFixed(1)}`;
  for (let i = 0; i < n - 1; i++) {
    const h = (xs[i + 1] - xs[i]) / 3;
    const c1x = xs[i] + h;
    const c1y = ys[i] + m[i] * h;
    const c2x = xs[i + 1] - h;
    const c2y = ys[i + 1] - m[i + 1] * h;
    d += ` C${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ${xs[i + 1].toFixed(1)},${ys[i + 1].toFixed(1)}`;
  }
  return d;
}

/** 机器卡的 NIC 面积图。RX + TX 取自 agent 按 30 秒窗口计算的 `/proc/net/dev`
 * 平均速率，再按该样本的实际窗口时长积分为字节/窗口。它包含 xray、WireGuard 封装及系统
 * 其他网络流量。下方「本月」仍是计费用量，两者的口径由各自标签明确区分。
 *
 * Y 轴从 0 到 `max(3.75 MB / 30s 窗口, 本机近期峰值)`，不再拿全页最忙的机器当公共上限。
 * 该下限与原来的 1 Mbit/s 等价，避免将系统心跳放大成满幅波峰。卡片只表达该机自身的流量趋势，
 * 极值圆点悬停显示的是字节/窗口，不是 bit/s。`has_gap` 样本的速率不可比，直接断线，
 * 不补 0（补 0 会把「无法测量」说成「实际没有流量」）。 */
function nodeCardLatency(value: number | null): string {
  return value == null ? '—' : String(Math.round(value));
}

/** 机器卡右下角：每个 TCP 目标一个读数，取 IPv4，只填了 IPv6 的目标取 IPv6。最新一轮无响应的
 * 主读数是灰白色「—」。另一族（IPv6）的结果不占卡片空间，两族读数都保留在 title 里。 */
export function TcpProbeLatest({
  view,
  intervalSecs,
  pending = false,
}: {
  view?: NodePingProbeLatestView;
  intervalSecs: number;
  pending?: boolean;
}) {
  const now = useNow();
  if (pending && !view) return null;
  if (!view || view.targets.length === 0) return null;
  const entries = view.targets
    .filter(target => target.kind === 'tcp')
    .map(target => ({ target, primary: (target.ipv4 ?? target.ipv6)?.latest ?? null }));
  if (entries.length === 0) return null;
  const staleAfterSecs = Math.max(intervalSecs * 2, 30);
  const expired = (sample: PingProbePoint) => now / 1000 - sample.probed_at_unix_secs > staleAfterSecs;
  const latestOf = (target: (typeof entries)[number]['target']) =>
    PING_PROBE_FAMILIES.flatMap(family => {
      const latest = target[family]?.latest;
      return latest ? [latest] : [];
    });
  const stale = entries.some(({ target }) => latestOf(target).some(expired));
  const primaryValue = (sample: PingProbePoint | null) => (sample && sample.attempted ? pingLatencyMs(sample) : null);
  const title = entries
    .map(({ target }) => {
      const readings = PING_PROBE_FAMILIES.flatMap(family => {
        const series = target[family];
        if (!series) return [];
        const label = PING_FAMILY_LABEL[family];
        const reason = series.latest && !series.latest.attempted ? series.latest.skip_reason : undefined;
        return [
          reason
            ? `${label} 未探测（${pingSkipReasonText(reason, family)}）`
            : `${label} ${pingSampleText(series.latest ?? undefined)}`,
        ];
      });
      const newest = latestOf(target).reduce<PingProbePoint | null>(
        (found, sample) => (found && found.probed_at_unix_secs >= sample.probed_at_unix_secs ? found : sample),
        null,
      );
      const sampledAt = newest ? new Date(newest.probed_at_unix_secs * 1000).toLocaleString('zh-CN') : '尚无样本';
      return `${target.name}：${readings.join(' · ')} · 最新样本 ${sampledAt}${newest && expired(newest) ? ' · 已过期' : ''}`;
    })
    .join('\n');
  return (
    <span className={`nc-tcp-latest${stale ? ' stale' : ''}`} title={title}>
      <span className="values">
        {entries.slice(0, 3).map(({ target, primary }, index) => (
          <Fragment key={`${target.name}-${index}`}>
            {index > 0 && ' / '}
            {pingSampleLost(primary ?? undefined) ? <i className="lost">—</i> : nodeCardLatency(primaryValue(primary))}
          </Fragment>
        ))}
      </span>
      {entries.some(({ primary }) => primaryValue(primary) != null) && <em>ms</em>}
    </span>
  );
}

export function NicWave({ load }: { load?: { series: NodeNicSample[] } }) {
  const samples = (load?.series ?? []).slice(-LIST_NIC_WINDOWS);
  const label = 'NIC · 30 秒 / 窗口';
  const windowBytes = (sample: NodeNicSample) => {
    const seconds = sample.window_end_unix_secs - sample.window_start_unix_secs;
    return ((sample.nic_rx_bps + sample.nic_tx_bps) * seconds) / 8;
  };
  const valid = samples
    .map((sample, index) => ({ sample, index, value: windowBytes(sample) }))
    .filter(point => !point.sample.has_gap && Number.isFinite(point.value) && point.value >= 0);
  if (valid.length === 0) {
    return (
      <div className="history-plot node-nic-plot empty">
        <span className="plot-label">{label} · 尚无样本</span>
      </div>
    );
  }

  const W = 100;
  const H = 36;
  // 窗口查询返回的是最近 N 条记录，不是固定时间区间。首尾无效窗口没有可绘制内容，继续
  // 为它们留槽只会让卡片边缘出现空白；内部 gap 仍按索引占位并断线，保留真实缺口。
  const firstDrawableIndex = valid[0].index;
  const lastDrawableIndex = valid[valid.length - 1].index;
  const drawableSpan = lastDrawableIndex - firstDrawableIndex;
  const xOf = (index: number) => (drawableSpan === 0 ? W / 2 : ((index - firstDrawableIndex) * W) / drawableSpan);
  const max = valid.reduce((top, point) => (point.value > top ? point.value : top), 0);
  const min = valid.reduce((bottom, point) => (point.value < bottom ? point.value : bottom), valid[0].value);
  const ceiling = Math.max(LIST_NIC_MIN_CEILING_BYTES, max);
  const yOf = (value: number) => 4 + (1 - value / ceiling) * 21;

  // 按 has_gap 分段后分别绘制，避免平滑线跨过不可比样本。
  const segments: Array<Array<{ x: number; y: number }>> = [];
  let segment: Array<{ x: number; y: number }> = [];
  for (let index = 0; index < samples.length; index += 1) {
    const sample = samples[index];
    const value = windowBytes(sample);
    if (sample.has_gap || !Number.isFinite(value) || value < 0) {
      if (segment.length > 0) segments.push(segment);
      segment = [];
      continue;
    }
    segment.push({ x: xOf(index), y: yOf(value) });
  }
  if (segment.length > 0) segments.push(segment);

  const maxPoint = valid.reduce((picked, point) => (point.value > picked.value ? point : picked));
  const minPoint = valid.reduce((picked, point) => (point.value <= picked.value ? point : picked));
  const marker = (kind: '最高' | '最低' | 'NIC', point: (typeof valid)[number], position: 'max' | 'min') => {
    const x = xOf(point.index);
    const y = yOf(point.value);
    // tooltip 宽约占小图的三分之一，14% 才贴边会在 268px 卡片上溢出约 10px。
    const edge = x < 22 ? ' edge-left' : x > 78 ? ' edge-right' : '';
    // 方向看实际 y，不看「最高/最低」语义。Y 轴有最低上限时，最高点也可能落在下半部。
    const tipSide = y / H <= 0.5 ? ' tip-below' : ' tip-above';
    return (
      <button
        type="button"
        className={`extreme-point ${position}${edge}${tipSide}`}
        style={{ left: `${x}%`, top: `${(y / H) * 100}%` }}
        aria-label={`${kind === 'NIC' ? '' : kind} NIC 传输量 ${bytes(point.value)} 每窗口`}
        onClick={event => event.stopPropagation()}
      >
        <span className="plot-tip">
          <span>{kind}</span>
          <b>{bytes(point.value)}</b>
          <em>/窗口</em>
        </span>
      </button>
    );
  };
  const aria = `${label}，最高 ${bytes(max)}，最低 ${bytes(min)}`;
  return (
    <div className="history-plot node-nic-plot">
      <span className="plot-graph">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={aria}>
          {segments.map((points, index) => {
            const d = smoothPath(
              points.map(point => point.x),
              points.map(point => point.y),
            );
            return (
              <Fragment key={index}>
                <path className="area" d={`${d} L${points[points.length - 1].x},${H} L${points[0].x},${H} Z`} />
                <path className="line" d={d} />
              </Fragment>
            );
          })}
        </svg>
        {max === min ? marker('NIC', maxPoint, 'max') : marker('最高', maxPoint, 'max')}
        {max !== min && marker('最低', minPoint, 'min')}
      </span>
      <span className="plot-label">{label}</span>
    </div>
  );
}

const EXACT_BYTE_UNITS: ReadonlyArray<{ name: string; bytes: bigint }> = [
  { name: 'EiB', bytes: 1024n ** 6n },
  { name: 'PiB', bytes: 1024n ** 5n },
  { name: 'TiB', bytes: 1024n ** 4n },
  { name: 'GiB', bytes: 1024n ** 3n },
  { name: 'MiB', bytes: 1024n ** 2n },
  { name: 'KiB', bytes: 1024n },
];

/** Format a decimal-string counter without first rounding it through a JS number. */
export function exactBytes(value: string): string {
  let amount: bigint;
  try {
    amount = BigInt(value);
  } catch {
    return '—';
  }
  const unit = EXACT_BYTE_UNITS.find(candidate => amount >= candidate.bytes);
  if (!unit) return `${amount} B`;
  const hundredths = (amount * 100n + unit.bytes / 2n) / unit.bytes;
  const whole = hundredths / 100n;
  const fraction = (hundredths % 100n).toString().padStart(2, '0');
  return `${whole}.${fraction} ${unit.name}`;
}

function trafficGapReason(reason: string | null): string {
  switch (reason) {
    case 'machine-reboot':
      return '机器重启';
    case 'interface-changed':
      return '默认网卡变化';
    case 'counter-regressed':
      return '网卡计数回退';
    case 'meter-replaced':
      return 'Agent 流量状态已更换';
    case 'agent-discontinuity':
      return 'Agent 检测到计数中断';
    case 'report-gap':
      return '控制面跨日失联';
    default:
      return '采集从本期中途开始';
  }
}

function NicMonthTotal({
  traffic,
  pending,
  retired,
}: {
  traffic?: NodeTrafficItem;
  pending: boolean;
  retired: boolean;
}) {
  const measured =
    traffic && (traffic.tracking_started_at_unix_secs !== null || traffic.calibrated_at_unix_secs !== null);
  if (!measured) {
    return (
      <span className="lst-sum void" title={retired ? '已退役' : pending ? '读取中' : 'Agent 尚未上报网卡累计'}>
        —<small>本月</small>
      </span>
    );
  }
  const period = `${new Date(traffic.period_start_unix_secs * 1000).toISOString().slice(0, 10)} 至 ${new Date(
    traffic.period_end_unix_secs * 1000,
  )
    .toISOString()
    .slice(0, 10)}（UTC）`;
  return (
    <span
      className="lst-sum"
      title={`${period}${
        traffic.has_gap ? `\n${trafficGapReason(traffic.last_gap_reason)}造成采集缺口，请校准确认总量` : ''
      }`}
    >
      {exactBytes(traffic.total_bytes)}
      <small>本月</small>
    </span>
  );
}

// 尾部数字位只有一行宽度，「14 秒前」这类完整表述放不下，也没有必要——
// 单位压缩为一个字母，绝对时刻仍在 title 中。
//
// 使用 useNow 而非 Date.now()：该值表示距上次上报的时长，每秒都在变化。
// 只在重渲染时计算时，它会随列表 5 秒一次的 refetch 更新——每五秒跳变一次、
// 中间静止，表现为界面无响应，而该字段正是用于判断 agent 是否在线的。
function PollAgo({ at }: { at: string | null }) {
  const now = useNow();
  if (!at) return <>—</>;
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(t)) return <>—</>;
  const s = Math.max(0, Math.round((now - t) / 1000));
  const text =
    s < 60
      ? `${s}s`
      : s < 3600
        ? `${Math.floor(s / 60)}m`
        : s < 86400
          ? `${Math.floor(s / 3600)}h`
          : `${Math.floor(s / 86400)}d`;
  return <span title={at}>{text}</span>;
}

// 该机器发出的中继跳当前是否连通。
// 判定依据是 outbound 计数器的增量：产物中的 observatory 每 10 秒通过每个转发出口探测一次，
// 因此该跳连通时 downlink 会持续增长；增量为零即表示不通。
//
// 与上面的「已应用」属于两类事实，因此分开显示：
// 「已应用」表示配置是否已下发到该机器，此处表示链路是否连通。配置全部正常而用户无法连接，
// 正是因为此前只有前者。
// 链路统计单独提取：观测页顶部的状态条使用这份数据（RELAY STATUS 表已移除）。
function useHopStats(nodeId: string) {
  const health = useQuery({
    queryKey: ['link-health'],
    queryFn: () => fetchLinkHealth(),
    refetchInterval: 30_000,
  });
  const artifact = useQuery({
    queryKey: ['artifact', 'view', 'burst-observatory', nodeId, 'xray'],
    queryFn: () => fetchArtifactContentView('node', nodeId, 'xray'),
    retry: false,
  });
  const observatory = parseBurstObservatory(artifact.data?.content);
  const mine = health.error ? [] : (health.data?.hops ?? []).filter(h => h.node_id === nodeId);
  const rows = new Map<
    string,
    { subject: string; chainId: string; peerNodeId: string; health: LinkHealthItem | null }
  >();
  for (const subject of observatory?.subjects ?? []) {
    const parsed = parseHopSubject(subject);
    if (!parsed) continue;
    rows.set(subject, { subject, ...parsed, health: null });
  }
  for (const hop of mine) {
    const subject = hopSubject(hop);
    rows.set(subject, {
      subject,
      chainId: hop.chain_id,
      peerNodeId: hop.peer_node_id,
      health: hop,
    });
  }
  // 产物中不存在的跳整行不显示。这类记录是 link_health 的历史残留——链已修改、节点已移出，
  // 而该表按 (node, chain, peer) 保留旧记录，agent 也不会上报该跳已不再管理。
  // 此前为其添加红色的「不在产物」标记，结果是表格持续增长并混入不存在的跳。
  // 无法获取 observatory 时（没有 xray 产物，或该次请求失败）无法判定，全部保留：
  // 显示多余记录优于因一次取数失败而清空整张表。
  const subjectSet = new Set(observatory?.subjects ?? []);
  const inArtifact = (subject: string) => !observatory || subjectSet.has(subject);
  const list = [...rows.values()].filter(r => inArtifact(r.subject));
  // 状态条的「N/M 通」使用同一口径：表中不显示的跳不应计入，
  // 否则顶部显示 3/5 而下方只能列出 3 行。
  const shown = mine.filter(h => inArtifact(hopSubject(h)));
  const loading = health.isLoading && artifact.isLoading;
  const dead = shown.filter(h => !h.alive);
  const expected = list.length;
  const reported = shown.length;
  const alive = shown.length - dead.length;
  // 窗口长度由行内移至表头：同一次上报中所有跳共用一个窗口，每格重复「/ 60s」
  // 是重复显示同一数值。取第一条上报的即可；没有任何上报时不显示秒数，
  // 此时整列本身也是「—」。
  const windowSecs = mine[0]?.window_secs ?? null;
  return { list, loading, dead, expected, reported, alive, observatory, windowSecs };
}

/** 把 usage 桶落到请求区间内的绝对 30 秒网格。无桶表示该窗口没有转发字节，按 0 处理。 */
function usageRoleTimeline(
  series: UsageNodeSeries | undefined,
  rangeStartUnixSecs: number,
  rangeEndUnixSecs: number,
): { timesUnixSecs: number[]; user: number[]; relay: number[] } {
  const slots = Math.max(1, Math.ceil((rangeEndUnixSecs - rangeStartUnixSecs) / GRID_SECS));
  const endKey = Math.floor(rangeEndUnixSecs / GRID_SECS) * GRID_SECS;
  const startKey = endKey - (slots - 1) * GRID_SECS;
  const timesUnixSecs = Array.from({ length: slots }, (_, index) => startKey + index * GRID_SECS);
  const user = new Array<number>(slots).fill(0);
  const relay = new Array<number>(slots).fill(0);
  for (const bucket of series?.buckets ?? []) {
    const key = Math.floor(Date.parse(bucket.window_end) / 1000 / GRID_SECS) * GRID_SECS;
    if (!Number.isFinite(key)) continue;
    const index = Math.round((key - startKey) / GRID_SECS);
    if (index < 0 || index >= slots) continue;
    user[index] += ((bucket.user_uplink_bytes + bucket.user_downlink_bytes) * 8) / GRID_SECS;
    relay[index] += ((bucket.relay_uplink_bytes + bucket.relay_downlink_bytes) * 8) / GRID_SECS;
  }
  return { timesUnixSecs, user, relay };
}

/* 网卡吞吐与 XRAY 承载吞吐堆进一个面板。两者同窗口、同粒度、用 group 联动十字线，但口径
 * 不同：网卡按方向（接收/发送）统计全部流量，XRAY 按角色（用户/中继）只统计 xray 转发的
 * 字节——不能合并进一张图，各自的 Y 轴与图例保持这个差异可见，所以是同卡内的两张图。
 * node-load 与 usage 两个查询的 queryKey 都与页面其它处一致，React Query 去重、不多发请求。 */
export function ThroughputPanel({
  nodeId,
  range,
  linked,
  observationModules,
}: {
  nodeId: string;
  range: LoadRange;
  linked: boolean;
  observationModules?: NodeObservationModules;
}) {
  const load = useQuery({
    ...nodeLoadRangeQuery(nodeId, range),
    refetchInterval: fixedLoadRange(range) ? false : range.seconds <= 60 * 60 ? 10_000 : 30_000,
  });
  const usage = useQuery({
    ...nodeUsageRangeQuery(nodeId, range),
    refetchInterval: fixedLoadRange(range) ? false : 30_000,
  });
  const report = load.data;
  if (!report)
    return (
      <ThroughputPanelState
        state={load.error ? 'error' : 'pending'}
        ChartLoading={observationModules?.ObservationChartLoading}
      />
    );

  const series = report.series;
  const latest = report.latest_sample ?? series[series.length - 1];
  if (series.length === 0 || !latest) {
    const emptyBlock = (title: string, icon: IconName) => (
      <div className="nd-throughput-block" key={title}>
        <div className="load-network-cap">
          <b>
            <Icon of={icon} size={13} className="chart-title-icon" />
            {title}
          </b>
        </div>
        <p className="note throughput-empty">尚无 Agent 上报样本。</p>
      </div>
    );
    return (
      <section className="chart-card nd-throughput-panel" aria-label="吞吐">
        {emptyBlock('网卡流量', 'agent')}
        {emptyBlock('XRAY 流量', 'tunnels')}
      </section>
    );
  }

  const group = linked ? `nd-tp-${nodeId}` : undefined;
  const rangeStart = report.range_start_unix_secs;
  const rangeEnd = report.range_end_unix_secs;

  // 网卡：每个点使用上报的真实结束时间。轴保持完整请求区间，因此尾部没有样本时会留白。
  const latestNic = latest.has_gap ? null : latest;
  const host = report.host;
  const nicTimes = series.map(sample => sample.window_end_unix_secs);
  const nicRx = series.map(sample => (sample.has_gap ? null : sample.nic_rx_bps));
  const nicTx = series.map(sample => (sample.has_gap ? null : sample.nic_tx_bps));
  const drops = latestNic ? latestNic.nic_rx_drop + latestNic.nic_tx_drop + latestNic.nic_err : 0;
  const nicMeta = [
    latestNic && latestNic.nic_rx_drop > 0 ? `接收丢弃 ${latestNic.nic_rx_drop.toLocaleString()}` : null,
    latestNic && latestNic.nic_tx_drop > 0 ? `发送丢弃 ${latestNic.nic_tx_drop.toLocaleString()}` : null,
    latestNic && latestNic.nic_err > 0 ? `网卡错误 ${latestNic.nic_err.toLocaleString()}` : null,
    host?.nic ?? null,
    typeof host?.nic_mtu === 'number' ? `MTU ${host.nic_mtu}` : null,
  ].filter((value): value is string => value !== null);

  // XRAY：按角色只统计 xray 转发的字节。
  const mine = usage.data?.nodes.find(n => n.node_id === nodeId);
  const { timesUnixSecs: xrayTimes, user, relay } = usageRoleTimeline(mine, rangeStart, rangeEnd);
  const monthTotal = mine ? monthBytes(mine) : 0;

  /* 每块图一个单位：标题栏写它，图例按它读数，图内的刻度和 tooltip 走同一次 throughputAxis
     的结果。峰值只算一遍，所以标题、刻度、图例三处不可能各说各话。 */
  const nicUnit = throughputAxis(
    nicRx.length > 0 ? nicRx : [latestNic?.nic_rx_bps ?? null],
    nicTx.length > 0 ? nicTx : [latestNic?.nic_tx_bps ?? null],
  ).unit;
  const xrayUnit = throughputAxis(user, relay).unit;
  const ThroughputChart = observationModules?.ThroughputChart ?? LazyThroughputChart;

  return (
    <section className="chart-card nd-throughput-panel" aria-label="吞吐">
      <div className="nd-throughput-block">
        <div className="load-network-cap">
          <b>
            <Icon of="agent" size={13} className="chart-title-icon" />
            网卡流量
          </b>
          <span className="chart-unit">({nicUnit.name})</span>
          {nicMeta.length > 0 && <span className={drops > 0 ? 'hot' : undefined}>{nicMeta.join(' · ')}</span>}
          <span>
            最后样本 <Ago at={iso(latest.window_end_unix_secs)} />
          </span>
          <footer className="load-network-legend" aria-label="网卡流量图例">
            <span className="rx">
              <i />
              接收 <b>{latestNic ? nicUnit.read(latestNic.nic_rx_bps) : '—'}</b>
            </span>
            <span className="tx">
              <i />
              发送 <b>{latestNic ? nicUnit.read(latestNic.nic_tx_bps) : '—'}</b>
            </span>
          </footer>
        </div>
        <Suspense fallback={<ObservationReading ChartLoading={observationModules?.ObservationChartLoading} />}>
          <ThroughputChart
            timesUnixSecs={nicTimes}
            rangeStartUnixSecs={rangeStart}
            rangeEndUnixSecs={rangeEnd}
            rx={nicRx}
            tx={nicTx}
            rxName="接收"
            txName="发送"
            group={group}
          />
        </Suspense>
      </div>
      {usage.data ? (
        <div className="nd-throughput-block">
          <div className="load-network-cap">
            <b>
              <Icon of="tunnels" size={13} className="chart-title-icon" />
              XRAY 流量
            </b>
            <span className="chart-unit">({xrayUnit.name})</span>
            <span>本月 {bytes(monthTotal)}</span>
            <footer className="load-network-legend" aria-label="XRAY 流量图例">
              <span className="rx">
                <i />
                用户 <b>{xrayUnit.read(user[user.length - 1])}</b>
              </span>
              <span className="tx">
                <i />
                中继 <b>{xrayUnit.read(relay[relay.length - 1])}</b>
              </span>
            </footer>
          </div>
          <Suspense fallback={<ObservationReading ChartLoading={observationModules?.ObservationChartLoading} />}>
            <ThroughputChart
              timesUnixSecs={xrayTimes}
              rangeStartUnixSecs={rangeStart}
              rangeEndUnixSecs={rangeEnd}
              rx={user}
              tx={relay}
              rxName="用户"
              txName="中继"
              group={group}
            />
          </Suspense>
        </div>
      ) : (
        <ObservationChartStateBlock
          title="XRAY 流量"
          icon="tunnels"
          state={usage.error ? 'error' : 'pending'}
          ChartLoading={observationModules?.ObservationChartLoading}
        />
      )}
    </section>
  );
}

// 该机器 wg0 的 MTU。
// 它是接口属性——每台机器一个 wg0、一个 MTU，`MTU =` 只写在 [Interface] 段，
// [Peer] 段没有该键。因此它挂在节点上，既非全局配置也非链路配置。设置页中的值是
// 未单独设置的机器使用的默认值。
//
// 建议值来自探测：agent 按对测量路径 MTU，该机器的建议值等于其所有路径中的最小值减 60。
function NodeMtuRow({ node, canEdit, onSaved }: { node: NodeAgentStateItem; canEdit: boolean; onSaved: () => void }) {
  /* 只取全局默认值当占位符。此处不再显示探测结果——建议值、最小路径通向哪台、
     几条未探出，是设置页 MTU 探测那一段的内容（它按机队列出全部链路对）；这一行只回答
     「这台机器的 wg0 用哪个 MTU」。因此也去掉了 60 秒轮询：默认值是全局设置，不是观测量。 */
  const probe = useQuery({ queryKey: ['link-mtu'], queryFn: () => fetchLinkMtu() });
  /* null 表示本次未改动，输入框显示模型值。控件常驻、改了才出工具条——与本页其余各行
     （身份、DNS、出网权限、连接策略）同一形态，不再先点一个「改」把行切进编辑态。
     留空即回退到全局默认值，全局值由占位符给出，与连接策略那几项的写法一致。 */
  const [value, setValue] = useState<string | null>(null);
  /* 当前值取自草稿快照，理由见 useDraftNode。快照尚未返回时回退到直连值，避免该行为空。 */
  const draftNode = useDraftNode(node.node_id);
  const modelMtu = draftNode ? (draftNode.mtu ?? null) : node.mtu;
  const base = modelMtu === null ? '' : String(modelMtu);
  const shown = value ?? base;
  const dirty = value !== null && shown.trim() !== base;
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的 MTU`, `node-tab:${node.node_id}:config`);
  /* 取值来源随输入框走，不随模型走：清空后这一行应当立刻显示「继承全局」，
     而不是等保存成功再改口。 */
  const ownNow = shown.trim() !== '';

  const save = useMutation({
    /* 0 表示清空并回退到全局默认值，见 updateNode 中该字段的说明。 */
    mutationFn: () => updateNode(node.node_id, { mtu: Number(shown.trim()) || 0 }),
    onSuccess: () => {
      setValue(null);
      onSaved();
    },
  });

  return (
    <Row k="MTU">
      <span className="nd-ctl-line">
        <input
          className="f mono"
          style={{ width: 100 }}
          value={shown}
          disabled={!canEdit}
          placeholder={probe.data?.default_mtu == null ? '' : String(probe.data.default_mtu)}
          aria-label="wg0 MTU"
          onChange={e => setValue(e.target.value)}
        />
        <OverrideTag own={ownNow} />
      </span>
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            修改 MTU 会重新生成 wg 配置，触发一次链路重连。
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setValue(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </Row>
  );
}

function WgListenPortRow({
  nodeId,
  currentPort,
  canEdit,
  onSaved,
}: {
  nodeId: string;
  currentPort: number | null;
  canEdit: boolean;
  onSaved: () => void;
}) {
  const qc = useQueryClient();
  /* 输入框常驻，改了才出工具条。`null` 表示本次未改动，显示模型值。
     不在 overlay 中时没有监听端口可设（currentPort 为 null），输入框禁用而不是隐藏——
     隐藏之后这一行只剩一个「—」，看不出是这台机器没有该项还是接口没返回。 */
  const [value, setValue] = useState<string | null>(null);
  const draftNode = useDraftNode(nodeId);
  const port = draftNode?.wireguard?.listen_port ?? currentPort;
  const base = port === null ? '' : String(port);
  const shown = value ?? base;
  const parsed = Number(shown.trim());
  const valid = /^\d+$/.test(shown.trim()) && Number.isInteger(parsed) && parsed >= 1 && parsed <= 65_535;
  const dirty = value !== null && shown.trim() !== base;
  useUnsavedChanges(dirty, `${nodeId} 的 WireGuard 端口`, `node-tab:${nodeId}:config`);

  const save = useMutation({
    mutationFn: () => updateNode(nodeId, { wg_listen_port: parsed }),
    onSuccess: () => {
      setValue(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      onSaved();
    },
  });

  return (
    <Row k="监听端口">
      <input
        className="f mono"
        style={{ width: 110 }}
        value={shown}
        disabled={!canEdit || currentPort === null}
        inputMode="numeric"
        min={1}
        max={65_535}
        placeholder={currentPort === null ? '未加入 overlay' : ''}
        aria-label="WireGuard UDP 监听端口"
        onChange={event => setValue(event.target.value)}
      />
      {dirty && !valid && (
        <span className="sub" style={{ color: 'var(--err)' }}>
          端口必须为 1–65535。
        </span>
      )}
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            所有对端的 wg0.conf 会随之变更，发布后链路将重新握手。
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={!valid || save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setValue(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </Row>
  );
}

// 入站传输方式：外部如何连接该机器的 WireGuard 端口。
// WireGuard 只使用 UDP。上游封禁入站 UDP 时（判定依据不是握手失败，而是探测报告——
// ICMP/TCP 连通而 UDP 不通，且主动发出的包也无响应，说明是无状态封禁），唯一的方案是
// 在两端将 UDP 封装进其他协议。phantun 只添加一层 TCP 封装，不做重传和拥塞控制，
// 因此不存在 TCP-over-TCP 在丢包时内外两层相互放大的问题。
function WgTransportRow({
  node,
  canEdit,
  onSaved,
}: {
  node: NodeAgentStateItem;
  canEdit: boolean;
  onSaved: () => void;
}) {
  // 默认使用高位、未被常见服务占用的端口。不使用 443：该端口在本机上很可能已被
  // 接入面占用，且在 443 上运行非 TLS 服务会在主动探测下暴露。
  const [staged, setStaged] = useState<{ fake: boolean; port: number } | null>(null);

  /* 这行编辑的是节点声明的入站方式，必须读取节点模型本身。编译后的 `wrap.servers`
     表示 phantun 服务实际部署在哪台机器：声明 fake TCP 的节点位于 NAT 后时，服务端会
     借到可达的对端，拿它反推会把当前节点显示为 UDP、对端显示为 Phantun。

     优先读取同一节点尚未提交的 update_node，再读草稿预览快照；两者都没有时才回退到
     agent-state。这样保存到草稿后不会弹回旧值，也不会把开关串到对端。 */
  const draftNode = useDraftNode(node.node_id);
  const snapshotTransport =
    draftNode?.wireguard && 'transport' in draftNode.wireguard ? draftNode.wireguard.transport : undefined;
  const transport = draftNode?.wg_transport ?? snapshotTransport;
  const currentFake = transport ? transport.t === 'fake_tcp' : node.wg_transport_kind === 'fake_tcp';
  const currentPort = transport ? (transport.t === 'fake_tcp' ? transport.v.port : null) : node.wg_fake_tcp_port;

  const fake = staged?.fake ?? currentFake;
  const port = staged?.port ?? currentPort ?? 39743;
  const setFake = (next: boolean) => setStaged({ fake: next, port });
  const setPort = (next: number) => setStaged({ fake, port: next });

  const save = useMutation({
    mutationFn: () =>
      updateNode(node.node_id, {
        wg_transport: fake ? { t: 'fake_tcp', v: { port } } : { t: 'udp' },
      }),
    onSuccess: () => {
      // 清除本地暂存：写入草稿后数据源回到编译视图，保留本地值会导致两个值并存。
      setStaged(null);
      onSaved();
    },
  });

  const editingReady = canEdit;

  /* 两档常驻，改了才出工具条，不再先点「改」把行切进编辑态。
     端口输入框只在选中 Phantun 时出现——直连 UDP 下它没有对应的配置项。 */
  const dirty = staged !== null && (staged.fake !== currentFake || (staged.fake && staged.port !== currentPort));
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的入站传输`, `node-tab:${node.node_id}:config`);

  return (
    <Row k="入站传输">
      <span className="nd-ctl-line">
        <SegSwitch checked={fake} disabled={!editingReady} onChange={setFake} off="直连 UDP" on="Phantun" />
        {fake && (
          <input
            className="f mono"
            style={{ width: 110 }}
            value={port}
            disabled={!editingReady}
            inputMode="numeric"
            aria-label="Phantun 伪 TCP 端口"
            onChange={e => setPort(Number(e.target.value) || 0)}
          />
        )}
      </span>
      <span className="sub">Phantun 把 WireGuard 的 UDP 伪装成 TCP，用于 UDP 被限速的线路。</span>
      {fake && <span className="sub">使用高位端口。不要使用 443：容易与接入面冲突，也容易被探测。</span>}
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            所有对端的 wg0.conf 会随之变更。属破坏性变更。
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={!editingReady || save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setStaged(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </Row>
  );
}

// wg 层的全部配置：MTU、入站连接方式、私钥归属。
// 合并为一张卡片是因为它们表示的是同一项内容——该机器的 overlay 接口配置。
// 该机器是否加入 overlay。关闭表示不生成 wg 配置、不加入任何链路，agent 会移除 wg0；
// 它仍可作为中继，只要有链在其上开启中转端口——此时使用公网地址
// （ir/system.rs 的 SystemNode）。因此该开关不等同于停用该机器。
//
// 关闭是破坏性操作，且存在一种编译器能拦截但不易预见的情况：任一跳声明走
// WireGuard 且目标是该机器时，关闭后该跳无可用路径，会报 hop.unreachable 并阻止发布。
// 因此此处先统计当前有多少跳依赖其 overlay，避免提交后才发现。
//
// 切换开关后不立即写入草稿：先保存在本地并显示影响范围，确认后再写入。
function OverlayRow({ node, canEdit, onSaved }: { node: NodeAgentStateItem; canEdit: boolean; onSaved: () => void }) {
  const qc = useQueryClient();
  const [staged, setStaged] = useState<boolean | null>(null);

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled: canEdit });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: canEdit && !!current,
  });

  // 当前值取自编译结果而非 `node.overlay`：`/nodes/agent-state` 是直连接口，不经过
  // 草稿预览，以它为数据源会导致切换开关、写入草稿后开关显示回退——改动已在草稿中
  // 而界面显示为未修改。判定依据是在系统层中且有 wg 配置：不在 overlay 中的中继机同样
  // 出现在 `system.nodes` 中，只是 `wireguard` 为 null，两者都不满足的机器不在该数组内，
  // 两种情况都视为不在 overlay 中。
  // 编译结果尚未返回时回退到模型值，避免该行为空。
  const systemNodes = (compile.data?.system as { nodes?: { id: string; wireguard?: unknown }[] } | undefined)?.nodes;
  const draftNode = useDraftNode(node.node_id);
  const currentValue =
    draftNode?.overlay ??
    (systemNodes ? systemNodes.find(n => n.id === node.node_id)?.wireguard != null : node.overlay);
  const value = staged ?? currentValue;
  const dirty = staged !== null && staged !== currentValue;
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的 Overlay`, `node-tab:${node.node_id}:config`);

  // 统计的是编译产生的 hop 而非规则表：规则中未写 dial 即表示走 overlay，主干上
  // 未写规则的那一跳也由编译器补全——两种情况都只在编译结果中可见。
  const overlayHops = ((compile.data?.apps as AppIr[] | undefined) ?? [])
    .flatMap(app => app.hops ?? [])
    .filter(hop => hop.path === 'overlay' && (hop.from === node.node_id || hop.to === node.node_id));

  const save = useMutation({
    mutationFn: () => updateNode(node.node_id, { overlay: value }),
    onSuccess: () => {
      setStaged(null);
      /* 该行的当前值来自编译视图，不重新编译则不会反映刚写入的改动 */
      qc.invalidateQueries({ queryKey: ['compile'] });
      // Runtime findings read the draft-aware snapshot so a stale userspace observation is
      // hidden as soon as WG is turned off, without waiting for commit or the next agent report.
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      onSaved();
    },
  });

  const dependencyPending = revisions.isPending || (current != null && compile.isPending);
  const dependencyError = revisions.error ?? compile.error;
  const editingReady = canEdit && !dependencyPending && !dependencyError;

  return (
    <Row k="WireGuard">
      <SegSwitch
        checked={value}
        disabled={!editingReady}
        onChange={checked => setStaged(checked)}
        off="关闭"
        on="启用"
      />
      {dependencyError && <ErrorBox error={dependencyError} />}
      {!dirty && (
        <span className="sub">
          {value
            ? '已配置 wg0 与 overlay 地址，其他机器可通过 overlay 连接它。'
            : '不生成 wg 配置。仍可作为中继：在链上为它开中转口，走公网地址。'}
        </span>
      )}
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            {value
              ? '所有 overlay 成员的 wg0.conf 都需重新生成。属破坏性变更。'
              : '本机的 wg0 会被移除，所有对端的 wg0.conf 中也不再包含它。属破坏性变更。'}
          </span>
          {!value && overlayHops.length > 0 && (
            <span className="sub" style={{ color: 'var(--err)' }}>
              当前有 {overlayHops.length} 跳经由它的 overlay（
              {[...new Set(overlayHops.map(h => `${h.chain}: ${h.from}→${h.to}`))].join('、')}
              ）。关闭后这些跳无路可达，编译会报 hop.unreachable 并阻止发布。请先将这些跳改为连接具体地址。
            </span>
          )}
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={!editingReady || save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setStaged(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </Row>
  );
}

// 该机器允许出网。
// 它是编译器为链末端补全默认动作时的判定依据：一条链到达该机器后没有下一跳，
// 且规则表未写末条规则时，允许出网则补 `Egress`，不允许则补 `Block` 并报 `step.no-egress`。
//
// 它不等同于该链从此处出网——后者取决于规则表中是否显式写有 Egress。因此开启该开关
// 不会立即使流量从该机器出网，它只表示允许；实际使流量出网的是链上的规则。
// 反之，关闭该开关会使所有以该机器为末端的链在编译时被补为 Block。
//
// 当前值取自编译结果而非 `node.egress_allowed`——原因与相邻的 overlay 开关一致：
// 直连接口不经过草稿预览，以它为数据源会导致切换并写入草稿后开关显示回退。
// 编译结果尚未返回时回退到模型值，避免该行为空。
function EgressRow({ node, canEdit, onSaved }: { node: NodeAgentStateItem; canEdit: boolean; onSaved: () => void }) {
  const qc = useQueryClient();
  const [staged, setStaged] = useState<boolean | null>(null);

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled: canEdit });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: canEdit && !!current,
  });

  // 每个 app 的 nodes 都是全量节点表（ir/routing.rs 中的 filter 只过滤退役节点），
  // 因此取任意一个 app 即可，无需合并所有 app。没有任何项目时无法读取——
  // 此时回退到模型值。
  const appNodes = (compile.data?.apps as AppIr[] | undefined)?.[0]?.nodes;
  const draftNode = useDraftNode(node.node_id);
  const currentValue =
    draftNode?.egress_allowed ??
    (appNodes
      ? (appNodes.find(n => n.id === node.node_id)?.egress_allowed ?? node.egress_allowed)
      : node.egress_allowed);
  const value = staged ?? currentValue;
  const dirty = staged !== null && staged !== currentValue;
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的出网权限`, `node-tab:${node.node_id}:config`);

  const save = useMutation({
    mutationFn: () => updateNode(node.node_id, { egress_allowed: value }),
    onSuccess: () => {
      setStaged(null);
      /* 该行的当前值来自编译视图，不重新编译则不会反映刚写入的改动 */
      qc.invalidateQueries({ queryKey: ['compile'] });
      onSaved();
    },
  });

  const dependencyPending = revisions.isPending || (current != null && compile.isPending);
  const dependencyError = revisions.error ?? compile.error;
  const editingReady = canEdit && !dependencyPending && !dependencyError;

  return (
    <Row k="出网权限">
      <SegSwitch
        checked={value}
        disabled={!editingReady}
        onChange={checked => setStaged(checked)}
        off="禁止出网"
        on="可出网"
      />
      {dependencyError && <ErrorBox error={dependencyError} />}
      {!dirty && (
        <span className="sub">
          {value
            ? '兜底规则：链最终到达这台机器时，编译器补上「可出网」。'
            : '兜底规则：链最终到达这台机器时，编译器补上「禁止出网、禁止访问互联网」。'}
        </span>
      )}
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            {value
              ? '以这台机器收尾的链将重新编译为「可出网」。'
              : '以这台机器收尾的链将编译为「禁止出网、禁止访问互联网」，阻断链上流量。'}
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={!editingReady || save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setStaged(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </Row>
  );
}

// 六档取值的说明。选项直接使用产物中的原值——该字段的使用者会查看 xray.json，
// 译为中文后反而需要用「先 IPv4 再 IPv6」反查对应的原值。代价是选项本身不够自明，
// 因此说明需要常驻显示，不能只在非默认档时出现。
const DOMAIN_STRATEGIES: { v: DomainStrategy; note: string }[] = [
  {
    v: 'use_ip',
    note: 'A 与 AAAA 同时查询，两族地址混在一起随机选用。同一个域名可能这次走 IPv4、下次走 IPv6。',
  },
  { v: 'use_ipv4', note: '只发送 A 查询，不回退。只有 AAAA 记录的域名，这台机器无法连接。' },
  { v: 'use_ipv6', note: '只发送 AAAA 查询，不回退。只有 A 记录的域名，这台机器无法连接。' },
  { v: 'use_ipv4v6', note: '先查询 A，无结果时再单独查询一次 AAAA。两次查询，纯 IPv6 域名会多一个 RTT。' },
  { v: 'use_ipv6v4', note: '先查询 AAAA，无结果时再单独查询一次 A。两次查询，纯 IPv4 域名会多一个 RTT。' },
  { v: 'as_is', note: '不解析，域名原样交给拨号器，由机器自身的 resolv.conf 决定。' },
];

// 模型中存储为 snake_case，界面显示 xray 的写法。该字段不做翻译：xray.json 中的取值
// 与此处选择的应是同一个词，否则对照产物排查时需要额外换算。
const DOMAIN_STRATEGY_LABEL: Record<DomainStrategy, string> = {
  use_ip: 'UseIP',
  use_ipv4: 'UseIPv4',
  use_ipv6: 'UseIPv6',
  use_ipv4v6: 'UseIPv4v6',
  use_ipv6v4: 'UseIPv6v4',
  as_is: 'AsIs',
};

function strategyNote(strategy: DomainStrategy, dns: Dns): string {
  const base = DOMAIN_STRATEGIES.find(s => s.v === strategy)?.note ?? '';
  // AsIs 的影响取决于相邻字段的取值：配置了实际的 DNS 服务器时该设置才会使其失效，
  // 跟随系统时两者本就一致，无需提示。编译期也会报 node.dns-bypassed，但时机过晚——
  // 选择在此处做出，影响也应在此处呈现。
  if (strategy === 'as_is' && dns.t === 'servers' && dns.v.length > 0) {
    return `${base}此处配置的 ${dns.v.join('、')} 不会被使用。`;
  }
  return base;
}

/** 将逗号分隔的输入解析为 Dns。空值或 `system` 均表示跟随系统。 */
function parseDns(raw: string): Dns {
  const value = raw.trim();
  if (value === '' || value === 'system') return { t: 'system' };
  return {
    t: 'servers',
    v: value
      .split(',')
      .map(s => s.trim())
      .filter(Boolean),
  };
}

function formatDns(dns: Dns): string {
  return dns.t === 'servers' ? dns.v.join(', ') : 'system';
}

/** 这台机器在草稿全部生效后的模型值，未取到时为 null。
 *
 * 写草稿的控件必须以它为当前值，不能读 `NodeAgentStateItem`：后者来自 `/nodes/agent-state`，
 * 是不经过草稿预览的直连接口，草稿提交之前不会变。以它为基准的控件在「保存到草稿」之后
 * 会回落到改动前的内容——草稿里确实记下了这次修改，界面却显示什么都没发生。
 * OverlayRow、WgTransportRow、ConnectionCard 三处的注释记录过同一个坑。
 *
 * 与其他页面共用 `['snapshot']` 查询键，通常命中缓存；草稿任何变动都会由 shell 集中失效
 * （见 forge/shell.tsx）。 */
function useDraftNode(nodeId: string) {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const entries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const model = snapshot.data?.snapshot.nodes?.find(row => row.id === nodeId);
  const pending = entries.find(entry => entry.op.op === 'update_node' && entry.op.node_id === nodeId)?.op;
  if (pending?.op !== 'update_node') return model ? { ...model, wg_transport: undefined } : null;
  return {
    ...model,
    ...pending.node,
    id: nodeId,
    mtu: pending.node.mtu === 0 ? null : (pending.node.mtu ?? model?.mtu),
    wireguard: pending.node.wg_listen_port == null ? model?.wireguard : { listen_port: pending.node.wg_listen_port },
  };
}

/* 配置页上所有下拉框的宽度。`select.f` 是全站唯一不带 width 声明的表单控件，不给宽度就
   按最长一条 option 撑开——同一栏里几张卡的下拉框会各自宽出不同的距离。取 200px 是因为
   域名解析那一项的选项最长（`UseIPv6v4`），它决定了这一档的下限。 */
const SELECT_FIELD = { width: 200 };

/* 机器证书组随模型草稿预览、提交，再由发布流程下发。 */
export function certificateSigningLabel(certificate: Pick<GroupCertificate, 'signing_method'>): string {
  return certificate.signing_method === 'self-signed' ? '自签证书' : "Let's Encrypt";
}

const NODE_CERTIFICATE_PATHS = {
  'public-ca': '/var/lib/brocade-agent/tls/public-ca/current.pem',
  'self-signed': {
    a: '/var/lib/brocade-agent/tls/self-signed/slot-a.pem',
    b: '/var/lib/brocade-agent/tls/self-signed/slot-b.pem',
  },
} as const;

/** 与 xray 产物和 agent 写盘约定相同的固定路径。公有 CA 只有当前证书落盘；自签使用 A/B。 */
export function certificateInstallPath(
  certificate: Pick<GroupCertificate, 'signing_method' | 'runtime_slot' | 'status'>,
): string | null {
  if (certificate.signing_method === 'public-ca') {
    return certificate.status === 'serving' ? NODE_CERTIFICATE_PATHS['public-ca'] : null;
  }
  if (certificate.runtime_slot == null || !['serving', 'ready', 'compatible'].includes(certificate.status)) {
    return null;
  }
  return NODE_CERTIFICATE_PATHS['self-signed'][certificate.runtime_slot];
}

function certificateRole(certificate: GroupCertificate): string {
  switch (certificate.status) {
    case 'serving':
      return '当前使用';
    case 'ready':
      return '备用';
    case 'compatible':
      return '保留';
    case 'pending':
      return '等待签发';
    case 'failed':
      return '签发失败';
    default:
      return '已换下';
  }
}

function certificateOrigin(origin: GroupCertificate['origin']): string {
  return origin === 'bootstrap' ? '初始化' : origin === 'spare' ? '手动添加' : '自动续期';
}

function certificateTime(value: string | null): string {
  if (!value) return '—';
  const parsed = new Date(value.replace(' ', 'T'));
  return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString('zh-CN', { hour12: false });
}

function certificateInstallState(
  certificate: Pick<GroupCertificate, 'signing_method' | 'runtime_slot' | 'status'>,
  nodeState: NodeCertificateState,
): { text: string; tone: string } {
  if (
    (certificate.signing_method === 'self-signed' && certificate.runtime_slot == null) ||
    (certificate.signing_method === 'public-ca' && certificate.status !== 'serving') ||
    !['serving', 'ready', 'compatible'].includes(certificate.status) ||
    nodeState.on_disk === 'absent'
  ) {
    return { text: '未安装', tone: 'st-warn' };
  }
  if (nodeState.on_disk === 'current') return { text: '已安装', tone: 'st-ok' };
  if (nodeState.on_disk === 'stale') return { text: '安装内容不一致', tone: 'st-warn' };
  return { text: '安装状态未知', tone: '' };
}

export function CertGroupCard({ node, canEdit }: { node: NodeAgentStateItem; canEdit: boolean }) {
  const qc = useQueryClient();
  const certs = useQuery({ queryKey: ['certs'], queryFn: () => fetchCerts(), retry: false });
  const groups = certs.data?.groups ?? [];
  const entries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const pending = entries.find(entry => entry.op.op === 'set_node_cert_group' && entry.op.node_id === node.node_id)?.op;
  const observed = certs.data?.nodes.find(row => row.node_id === node.node_id);
  const base = pending?.op === 'set_node_cert_group' ? (pending.label_id ?? '') : (observed?.label_id ?? '');
  const [selected, setSelected] = useState<string | null>(null);
  const value = selected ?? base;
  const dirty = value !== base;
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的证书组`, `node-tab:${node.node_id}:config`);
  const group = groups.find(g => g.id === value);
  const current: NodeCertificateState | undefined = group
    ? {
        node_id: node.node_id,
        label_id: group.id,
        group_name: group.name,
        certificate_name:
          group.certificates.find(c => c.status === 'serving')?.certificate_name ?? group.names.join('、'),
        on_disk: observed?.label_id === group.id ? observed.on_disk : 'absent',
        observed_at: observed?.label_id === group.id ? observed.observed_at : null,
      }
    : undefined;
  const serving = group?.certificates.find(c => c.status === 'serving');
  // issuer 是证书里的自由文本，新的自签证书会随机生成一个逼真的名称，不能拿它判断
  // 信任来源。签发方式在证书落库时已经冻结，切换设置后也不会被改写。
  const selfSigned = serving?.signing_method === 'self-signed';
  const expiry = serving?.expires_at ? new Date(serving.expires_at) : null;

  const save = useMutation({
    mutationFn: (labelId: string) => setNodeCertGroup(node.node_id, labelId || null),
    onSuccess: () => {
      setSelected(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
    },
  });

  if (certs.isPending) return null;
  if (certs.error) return <ErrorBox error={certs.error} />;

  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="certificate">证书组</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="所属组">
          <select
            className="f"
            style={SELECT_FIELD}
            value={value}
            disabled={!canEdit || save.isPending || certs.isPending}
            onChange={e => setSelected(e.target.value)}
          >
            <option value="">不关联证书</option>
            {groups.map(g => (
              <option key={g.id} value={g.id}>
                {g.name}
              </option>
            ))}
          </select>
          <span className="sub">
            {current
              ? `出示 ${current.certificate_name}${serving ? '' : '（这个组还没有签出证书）'}`
              : '未安装：这台机器上的 TLS 与 Hysteria 2 接入面会在编译时被拒绝。'}
          </span>
        </Row>
        {current && serving && (
          <>
            <Row k="签发">
              <span className={selfSigned ? 'st st-warn' : 'st st-ok'}>{certificateSigningLabel(serving)}</span>
              <span className="sub">
                {expiry && !Number.isNaN(expiry.valueOf()) ? `叶证书有效至 ${expiry.toLocaleDateString('zh-CN')}` : ''}
              </span>
            </Row>
          </>
        )}
        {current && group && (
          <div className="node-certificates" aria-label="证书完整信息">
            {group.certificates.length === 0 ? (
              <div className="node-cert-empty">
                <span className="st st-warn">未安装</span>
                <span className="sub">该证书组还没有签发任何证书。</span>
              </div>
            ) : (
              group.certificates.map(certificate => {
                const path = certificateInstallPath(certificate);
                const installed = certificateInstallState(certificate, current);
                const names = certificate.certificate_name ? [certificate.certificate_name] : group.names;
                return (
                  <section className="node-certificate" key={certificate.id}>
                    <header>
                      <code title={certificate.id}>{certificate.id}</code>
                      <span className="st">{certificateRole(certificate)}</span>
                      <span className={`st ${installed.tone}`}>{installed.text}</span>
                    </header>
                    <dl>
                      <dt>证书名称</dt>
                      <dd className="mono">{names.join('、')}</dd>
                      <dt>签发方式</dt>
                      <dd>{certificateSigningLabel(certificate)}</dd>
                      <dt>用途 / 来源</dt>
                      <dd>
                        {certificateRole(certificate)} / {certificateOrigin(certificate.origin)}
                      </dd>
                      <dt>运行槽</dt>
                      <dd>{certificate.runtime_slot?.toUpperCase() ?? '未安装'}</dd>
                      <dt>安装路径</dt>
                      <dd className="mono">{path ?? '未安装'}</dd>
                      <dt>签发者</dt>
                      <dd>{certificate.issuer ?? '—'}</dd>
                      <dt>签发时间</dt>
                      <dd>{certificateTime(certificate.issued_at)}</dd>
                      <dt>到期时间</dt>
                      <dd>{certificateTime(certificate.expires_at)}</dd>
                      <dt>SHA-256</dt>
                      <dd className="mono">{certificate.sha256 ?? '—'}</dd>
                      <dt>签发尝试</dt>
                      <dd>
                        {certificate.attempts} 次
                        {certificate.last_attempt_at ? ` · 最近 ${certificateTime(certificate.last_attempt_at)}` : ''}
                      </dd>
                      {certificate.last_error && (
                        <>
                          <dt>最后错误</dt>
                          <dd className="bad">{certificate.last_error}</dd>
                        </>
                      )}
                    </dl>
                  </section>
                );
              })
            )}
            <div className="node-cert-observed sub">
              机器上报：
              {current.observed_at ? certificateTime(current.observed_at) : '从未上报'}
            </div>
          </div>
        )}
        {save.error && <ErrorBox error={save.error} />}
        {canEdit && dirty && (
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate(value)}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setSelected(null)}>
              还原
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// DNS 卡位于身份和 WIREGUARD 之间：身份表示外部如何访问该机器，本卡表示该机器如何
// 解析外部地址，WIREGUARD 表示骨干网连接——按此顺序构成一条完整的路径。
//
// 此前这两个字段只出现在建机器向导中，更新接口一直接收 dns 但没有对应界面，
// 导致创建后无法修改。形态与 EgressRow 一致：未修改时为灰色说明，修改后显示工具条。
// 唯一的差异是说明常驻而非被状态提示替换——选项是 UseIPv4v6 这类原值，去掉说明后无法理解。
/* 导出理由同 WgCard：保存到草稿后是否保持新值，只有连着真实取数路径才测得出。 */
export function DnsCard({
  node,
  canEdit,
  onSaved,
}: {
  node: NodeAgentStateItem;
  canEdit: boolean;
  onSaved: () => void;
}) {
  const [servers, setServers] = useState<string | null>(null);
  const [strategy, setStrategy] = useState<DomainStrategy | null>(null);

  /* 当前值取自草稿快照，理由见 useDraftNode。快照尚未返回时回退到直连值。 */
  const draftNode = useDraftNode(node.node_id);
  const baseStrategy = draftNode?.domain_strategy ?? node.domain_strategy;
  const baseServers = formatDns(draftNode?.dns ?? node.dns);
  const curServers = servers ?? baseServers;
  const curStrategy = strategy ?? baseStrategy;
  const dirty = curServers !== baseServers || curStrategy !== baseStrategy;
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的 DNS`, `node-tab:${node.node_id}:config`);

  const save = useMutation({
    mutationFn: () => updateNode(node.node_id, { dns: parseDns(curServers), domain_strategy: curStrategy }),
    onSuccess: () => {
      setServers(null);
      setStrategy(null);
      onSaved();
    },
  });

  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="dns">DNS</PanelTitle>
      </header>
      <div className="fgrid one">
        <Row k="服务器">
          <input
            className="f mono"
            style={{ width: 200 }}
            value={curServers}
            disabled={!canEdit}
            placeholder="system 或 1.1.1.1,8.8.8.8"
            onChange={e => setServers(e.target.value)}
          />
          <span className="sub">留空或填 system = 跟随系统解析。</span>
        </Row>
        <Row k="域名解析">
          <select
            className="f"
            style={SELECT_FIELD}
            value={curStrategy}
            disabled={!canEdit}
            onChange={e => setStrategy(e.target.value as DomainStrategy)}
          >
            {DOMAIN_STRATEGIES.map(s => (
              <option key={s.v} value={s.v}>
                {DOMAIN_STRATEGY_LABEL[s.v]}
              </option>
            ))}
          </select>
          <span className="sub">{strategyNote(curStrategy, parseDns(curServers))}</span>
        </Row>
      </div>
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            {/* xray 不支持配置热重载（见 agent/src/main.rs 中的 “xray has no config hot reload”），
                因此该修改不是无感知的：产物重新编译后 agent 会重启 xray，连接会中断。
                代价与修改出网权限相当，该行的说明也是同样表述。 */}
            保存后本机的 xray.json 将重新编译为 {DOMAIN_STRATEGY_LABEL[curStrategy]}，agent 应用时会重启
            xray，现有连接会断开。
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button
              className="btn"
              disabled={save.isPending}
              onClick={() => {
                setServers(null);
                setStrategy(null);
              }}
            >
              还原
            </button>
          </div>
        </>
      )}
    </div>
  );
}

/* 该机器覆盖的连接策略。共四项，每项留空表示使用全局默认值。
 *
 * 与日志保留（见 LogRetentionCard）各自成卡：两者在设置页各是一段，段内按机器分列，
 * 逐台调一项要先在机队表里找到这一行，所以都收到这台机器名下；但生效方式相反——
 * 这一张写草稿、要发布一次，那一张直接落库、下一轮轮询生效，合成一张卡说不清。
 *
 * 不再默认折叠。折叠的理由是「四项全部跟随机队默认是常态，多数机器上没有内容可看」，
 * 那是这张卡只有四行输入框时成立的判断；现在每行都带一枚「继承全局 / 本机覆盖」状态片，
 * 展开状态本身就是这台机器与机队的对照表，折起来反而要点开才知道有没有差异。
 * `details` 保留，可以手动折起。
 *
 * 不包含握手超时一项，这是有意的：该值全机队使用同一取值。60 是 xray 为对齐
 * nginx 的 client_header_timeout 选择的，目的是不暴露后端服务类型；各机器分别设置
 * 会使机器之间可通过该值区分。它只在全局设置中。
 *
 * 当前值取自草稿快照（`fetchSnapshot`）而非 `node.connection`：`/nodes/agent-state` 是
 * 直连接口，不经过草稿预览，以它为数据源会导致填入数值、写入草稿后输入框显示回退为空——
 * 与 phantun 那一行的问题相同。本版本在实测中发现同样存在该问题：最初取的是编译视图中的
 * `apps[0].nodes`，而没有任何项目的控制面编译结果中不含应用层，因此又回退到了直连值。
 *
 * 快照是正确的数据源：该卡修改的是模型字段，`fetchSnapshot` 返回的正是草稿全部生效后的
 * 模型，与是否存在项目无关。占位符中的全局默认值取自同一份数据，两者不会出现新旧不一致。 */
/* 该卡片不包含任何说明文字：取值范围、术语和影响全部在设置页说明一次即可。
   此处表示的是该机器与机队默认是否一致，四个标签加四个值即为全部内容。
 *
 * 半关闭的两项直接使用 xray 的键名。两版中文表述均不适用——「只剩上行」是按键名直译，
 * 中文中没有该表述且容易被理解为上行已关闭，含义相反；「下行关闭后」含义正确但属于
 * 自造词，无法与 xray 文档对应。键名是此处唯一既准确又能与配置完全对应的名称。 */
const CONN_FIELDS = [
  // 单位写入标签而非附加在值之后。附加在值之后时，缓冲区未设置的情况会显示为
  // 「跟架构 KiB」——非数值的取值加上单位后无法读通，且需要为此单独判断是否显示。
  //
  // 「等待」两字从后两项的标签中去掉：标签栏与本页其他卡共用一条竖线（见 styles.css
  // 的 `.nd-tab-config .fgrid .k`），`DownlinkOnly 等待（秒）` 是全页唯一撑不下的标签，
  // 为它一项把整页标签栏加宽到 176px，其余各卡的输入框就都跟着右移。
  { key: 'conn_idle_secs', label: '空闲回收（秒）', pick: false },
  { key: 'buffer_size_kb', label: '转发缓冲（KiB）', pick: false },
  { key: 'uplink_only_secs', label: 'UplinkOnly（秒）', pick: true },
  { key: 'downlink_only_secs', label: 'DownlinkOnly（秒）', pick: true },
] as const;

type ConnKey = (typeof CONN_FIELDS)[number]['key'];

/** 两个选择型字段的预设档位。取值落在预设之外时由 `picks` 自动增加一档。 */
const CONN_PICKS = [1, 2, 3, 4, 5];

const EMPTY_NODE_CONNECTION: NodeConnection = {
  conn_idle_secs: null,
  uplink_only_secs: null,
  downlink_only_secs: null,
  buffer_size_kb: null,
};

function ConnectionCard({
  node,
  canEdit,
  onSaved,
}: {
  node: NodeAgentStateItem;
  canEdit: boolean;
  onSaved: () => void;
}) {
  const qc = useQueryClient();
  // null 表示本次尚未修改任何字段，当前值直接读取模型。与出网与解析一致：控件常驻，
  // 修改后才显示工具条。
  const [form, setForm] = useState<Record<ConnKey, string> | null>(null);
  const [expanded, setExpanded] = useState(false);

  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });

  const fromModel = useDraftNode(node.node_id)?.connection;
  // 快照尚未返回，或较旧的控制面不包含该字段时，视为未覆盖任何一项，而非使整张卡报错——
  // 四项均显示全局值、标题显示「跟全局一样」，与该情况下的实际状态一致。
  const mine = fromModel ?? node.connection ?? EMPTY_NODE_CONNECTION;
  const globals = snapshot.data?.snapshot.settings?.connection;

  /** 全局配置。缓冲区本身也可能为空，此时显示「跟架构」，不生成具体数值。
      快照尚未返回时显示「…」，同样不做推断。 */
  const globalText = (key: ConnKey): string => {
    if (!globals) return '…';
    const value = globals[key];
    return value == null ? '跟架构' : String(value);
  };
  const ownText = (key: ConnKey): string => {
    const value = mine[key];
    return value == null ? '' : String(value);
  };

  const cur = (key: ConnKey) => form?.[key] ?? ownText(key);
  /** 该字段当前实际生效的值。该机器已设置时为其自身取值，未设置时为全局取值。 */
  const effective = (key: ConnKey) => (cur(key).trim() === '' ? globalText(key) : cur(key));
  const isOwn = (key: ConnKey) => cur(key).trim() !== '';
  const ownCount = CONN_FIELDS.filter(f => isOwn(f.key)).length;
  const dirty = CONN_FIELDS.some(f => cur(f.key) !== ownText(f.key));
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的连接策略`, `node-tab:${node.node_id}:config`);

  const set = (key: ConnKey, value: string) =>
    setForm({
      conn_idle_secs: cur('conn_idle_secs'),
      uplink_only_secs: cur('uplink_only_secs'),
      downlink_only_secs: cur('downlink_only_secs'),
      buffer_size_kb: cur('buffer_size_kb'),
      [key]: value,
    });

  /* 该字段显示的档位。常规为 1–5，但生效值落在该范围外时需要将其加入——接口接受 0–3600，
     若通过接口设置为 7，界面上不会有任何一档选中，此时的任意点击都会修改该值。 */
  const picks = (key: ConnKey) => {
    const now = Number(effective(key));
    const list = [...CONN_PICKS];
    if (Number.isFinite(now) && !list.includes(now)) list.push(now);
    return list.sort((a, b) => a - b);
  };

  const save = useMutation({
    mutationFn: () =>
      updateNode(node.node_id, {
        // 四项一并提交：空串返回 null（该项回退到全局默认），有内容则提交数值。
        // 不能写成 `Number(x) || null`——0 在这三项上都是合法取值，会被 || 转为 null。
        connection: {
          conn_idle_secs: connValue(cur('conn_idle_secs')),
          uplink_only_secs: connValue(cur('uplink_only_secs')),
          downlink_only_secs: connValue(cur('downlink_only_secs')),
          buffer_size_kb: connValue(cur('buffer_size_kb')),
        },
      }),
    onSuccess: () => {
      setForm(null);
      /* 该卡的当前值来自草稿快照，不重新获取则不会反映刚写入的改动 */
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      onSaved();
    },
  });

  return (
    <details
      className="panel config-panel config-disclosure conn-card"
      open={expanded}
      onToggle={event => setExpanded(event.currentTarget.open)}
    >
      <summary>
        <PanelTitle of="config">连接策略</PanelTitle>
        <span className="config-disclosure-summary">{ownCount === 0 ? '与全局一致' : `本机覆盖 ${ownCount} 项`}</span>
        <span className="sp" />
        {/* 折叠状态下内部的改动不可见——`details` 不卸载子树，编辑内容仍然存在，
            但界面上无法看出已有修改。由标题说明该状态。 */}
        {dirty && <span className="st st-warn">未保存</span>}
        <span className="config-disclosure-toggle">{expanded ? '收起' : '展开'}</span>
      </summary>
      <div className="fgrid one">
        {CONN_FIELDS.map(f => (
          <Row k={f.label} key={f.key}>
            {f.pick ? (
              /* 数字组始终显示当前生效值。点数字即创建/修改本机覆盖；只在覆盖存在时
                 显示独立的「取消覆盖」，将字段恢复为 null。不额外增加模式开关，保留原有单行操作。 */
              <span className="nd-ctl-line">
                <SegmentedControl
                  value={effective(f.key)}
                  options={picks(f.key).map(value => ({ value: String(value), label: value }))}
                  disabled={!canEdit}
                  ariaLabel={`${f.label}的数值`}
                  onChange={value => set(f.key, value)}
                />
                <OverrideTag own={isOwn(f.key)} />
                {isOwn(f.key) && (
                  <button type="button" className="btn" disabled={!canEdit} onClick={() => set(f.key, '')}>
                    取消覆盖
                  </button>
                )}
              </span>
            ) : (
              <span className="nd-ctl-line">
                <input
                  className="f mono"
                  style={{ width: 96 }}
                  value={cur(f.key)}
                  disabled={!canEdit}
                  placeholder={globalText(f.key)}
                  onChange={e => set(f.key, e.target.value)}
                />
                <OverrideTag own={isOwn(f.key)} />
              </span>
            )}
          </Row>
        ))}
      </div>
      {dirty && (
        <>
          <span className="sub" style={{ color: 'var(--gold)' }}>
            {/* 运行时配置更新只增删入站/出站/路由规则，不包含 policy 块
                （见 brocade-deployment/src/hotswap.rs），因此该修改必然触发一次重启。 */}
            保存后本机的 xray.json 将重新编译，agent 应用时会重启 xray，现有连接会断开。
          </span>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setForm(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </details>
  );
}

type TrafficCalibrationUnit = 'GiB' | 'TiB';
type TrafficForm = {
  cycle_kind: NodeTrafficCycleKind;
  reset_month: string;
  reset_day: string;
  calibration: string;
  calibration_unit: TrafficCalibrationUnit;
};

const TRAFFIC_CALIBRATION_FACTORS: Record<TrafficCalibrationUnit, bigint> = {
  GiB: 1024n ** 3n,
  TiB: 1024n ** 4n,
};
const U64_MAX = 18_446_744_073_709_551_615n;

/** Convert a human-entered IEC amount to an exact decimal byte string. */
export function trafficCalibrationBytes(raw: string, unit: TrafficCalibrationUnit): string | null {
  const value = raw.trim();
  if (!/^\d+(?:\.\d{1,6})?$/.test(value)) return null;
  const [whole, fractional = ''] = value.split('.');
  const scale = 10n ** BigInt(fractional.length);
  const numerator = BigInt(whole) * scale + BigInt(fractional || '0');
  const bytesValue = (numerator * TRAFFIC_CALIBRATION_FACTORS[unit] + scale / 2n) / scale;
  return bytesValue <= U64_MAX ? bytesValue.toString() : null;
}

/** Physical default-route traffic. Policy/calibration are immediate and do not create a revision. */
export function TrafficAccountingCard({ node, canEdit }: { node: NodeAgentStateItem; canEdit: boolean }) {
  const qc = useQueryClient();
  const traffic = useQuery({ queryKey: ['node-traffic'], queryFn: fetchNodeTraffic, retry: false });
  const mine = traffic.data?.nodes.find(item => item.node_id === node.node_id) ?? null;
  const base: TrafficForm = {
    cycle_kind: mine?.cycle_kind ?? 'monthly',
    reset_month: String(mine?.reset_month ?? 1),
    reset_day: String(mine?.reset_day ?? 1),
    calibration: '',
    calibration_unit: 'GiB',
  };
  const source = JSON.stringify([node.node_id, mine?.cycle_kind, mine?.reset_month, mine?.reset_day]);
  const [syncedFrom, setSyncedFrom] = useState(source);
  const [form, setForm] = useState<TrafficForm | null>(null);
  if (source !== syncedFrom) {
    setSyncedFrom(source);
    setForm(null);
  }

  const shown = form ?? base;
  const resetDay = Number(shown.reset_day);
  const resetMonth = Number(shown.reset_month);
  const calibration = shown.calibration.trim()
    ? trafficCalibrationBytes(shown.calibration, shown.calibration_unit)
    : null;
  const invalid =
    !Number.isInteger(resetDay) ||
    resetDay < 1 ||
    resetDay > 31 ||
    (shown.cycle_kind === 'yearly' && (!Number.isInteger(resetMonth) || resetMonth < 1 || resetMonth > 12)) ||
    (shown.calibration.trim() !== '' && calibration === null);
  const policyDirty =
    shown.cycle_kind !== base.cycle_kind ||
    shown.reset_day !== base.reset_day ||
    (shown.cycle_kind === 'yearly' && shown.reset_month !== base.reset_month);
  const dirty = policyDirty || shown.calibration.trim() !== '';
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的流量统计`, `node-tab:${node.node_id}:config`);
  const set = (patch: Partial<TrafficForm>) => setForm({ ...shown, ...patch });
  const save = useMutation({
    mutationFn: () =>
      saveNodeTraffic(node.node_id, {
        cycle_kind: shown.cycle_kind,
        reset_month: shown.cycle_kind === 'yearly' ? resetMonth : null,
        reset_day: resetDay,
        calibrated_total_bytes: calibration,
      }),
    onSuccess: view => {
      setForm(null);
      qc.setQueryData<NodeTrafficView>(['node-traffic'], view);
    },
  });

  if (!mine) return null;

  const reported = mine.last_reported_at_unix_secs;
  const tracked = mine.tracking_started_at_unix_secs !== null || mine.calibrated_at_unix_secs !== null;
  const periodStart = new Date(mine.period_start_unix_secs * 1000).toISOString().slice(0, 10);
  const nextReset = new Date(mine.period_end_unix_secs * 1000).toISOString().slice(0, 10);
  return (
    <details className="panel config-panel node-traffic-accounting">
      <summary>
        <PanelTitle of="usage">流量统计</PanelTitle>
        <span className="sp" />
        {mine.has_gap && <span className="node-traffic-summary-gap">缺口</span>}
        <span className="node-traffic-summary-total mono">{tracked ? exactBytes(mine.total_bytes) : '待上报'}</span>
        <span className="node-traffic-source" title={reported === null ? undefined : `上报于 ${iso(reported)}`}>
          <span className="mono">{mine.interface ?? '待上报'}</span>
        </span>
      </summary>
      <div className="node-traffic-overview">
        <div className="node-traffic-total">
          <span>本期总量</span>
          <span className="node-traffic-value mono">{tracked ? exactBytes(mine.total_bytes) : '—'}</span>
          <span className="node-traffic-meta">
            <span className="node-traffic-period mono">
              {periodStart} → {nextReset} <small>UTC</small>
            </span>
            {mine.calibrated_at_unix_secs !== null && (
              <span className="node-traffic-calibrated">
                校准 · <Ago at={iso(mine.calibrated_at_unix_secs)} />
              </span>
            )}
          </span>
        </div>
        <dl className="node-traffic-directions" aria-label="接收与发送流量">
          <div>
            <dt>
              <i className="rx" aria-hidden="true">
                ↓
              </i>
              接收
            </dt>
            <dd className="mono">{tracked ? exactBytes(mine.rx_bytes) : '—'}</dd>
          </div>
          <div>
            <dt>
              <i className="tx" aria-hidden="true">
                ↑
              </i>
              发送
            </dt>
            <dd className="mono">{tracked ? exactBytes(mine.tx_bytes) : '—'}</dd>
          </div>
        </dl>
      </div>
      {mine.has_gap && (
        <div
          className="node-traffic-gap"
          role="status"
          title={mine.last_gap_at_unix_secs === null ? undefined : iso(mine.last_gap_at_unix_secs)}
        >
          <span aria-hidden="true">!</span>
          缺口 · {trafficGapReason(mine.last_gap_reason)}
        </div>
      )}
      <div className="fgrid one node-traffic-settings">
        <Row k="周期">
          <SegmentedControl
            value={shown.cycle_kind}
            options={[
              { value: 'monthly' as const, label: '每月' },
              { value: 'yearly' as const, label: '每年' },
            ]}
            disabled={!canEdit}
            ariaLabel="流量重置周期"
            onChange={cycle_kind => set({ cycle_kind })}
          />
        </Row>
        <Row k="重置日">
          <span className="nd-ctl-line">
            {shown.cycle_kind === 'yearly' && (
              <select
                className="f"
                style={{ width: 82 }}
                value={shown.reset_month}
                disabled={!canEdit}
                aria-label="重置月份"
                onChange={event => set({ reset_month: event.target.value })}
              >
                {Array.from({ length: 12 }, (_, index) => index + 1).map(month => (
                  <option value={month} key={month}>
                    {month} 月
                  </option>
                ))}
              </select>
            )}
            <input
              className="f mono"
              style={{ width: 72 }}
              type="number"
              min={1}
              max={31}
              value={shown.reset_day}
              disabled={!canEdit}
              aria-label="重置日"
              onChange={event => set({ reset_day: event.target.value })}
            />
          </span>
        </Row>
        <Row k="校准总量">
          <span className="nd-ctl-line">
            <input
              className="f mono"
              style={{ width: 128 }}
              inputMode="decimal"
              placeholder="留空不变"
              value={shown.calibration}
              disabled={!canEdit}
              aria-label="当前流量校准值"
              onChange={event => set({ calibration: event.target.value })}
            />
            <select
              className="f"
              style={{ width: 76 }}
              value={shown.calibration_unit}
              disabled={!canEdit}
              aria-label="校准单位"
              onChange={event => set({ calibration_unit: event.target.value as TrafficCalibrationUnit })}
            >
              <option value="GiB">GiB</option>
              <option value="TiB">TiB</option>
            </select>
          </span>
        </Row>
      </div>
      {invalid && <div className="callout err">日期需为 1–31；校准值需为非负数字，最多保留 6 位小数。</div>}
      {save.error && <ErrorBox error={save.error} />}
      {dirty && (
        <div className="toolbar config-panel-savebar">
          <span className="sp" />
          <button
            className="btn primary"
            disabled={!canEdit || invalid || save.isPending}
            onClick={() => save.mutate()}
          >
            {save.isPending ? '保存中…' : '保存'}
          </button>
          <button className="btn" disabled={save.isPending} onClick={() => setForm(null)}>
            还原
          </button>
        </div>
      )}
    </details>
  );
}

/* 日志保留。与连接策略分开成卡而不是合成一张「本机覆盖」：两者的生效方式相反——
   连接策略写草稿、要发布一次；这一项直接落库，未覆盖的机器随下一轮 agent 轮询读到全局值。
   同卡时这条差别只能靠两条小标题说明，分卡之后由卡本身承担。 */
export function LogRetentionCard({ node, canEdit }: { node: NodeAgentStateItem; canEdit: boolean }) {
  const qc = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  /* 与设置页共用查询键：那一页已经拉过时直接命中缓存，两处显示的是同一份数据。
     旧控制面没有该接口，失败时不重试也不报错——整张卡不画，页面其余部分照常。 */
  const logPolicy = useQuery({ queryKey: ['agent-log-policy'], queryFn: fetchAgentLogPolicy, retry: false });
  const mine = logPolicy.data?.nodes.find(row => row.node_id === node.node_id) ?? null;
  if (!mine) return null;
  const overrideCount = Object.values(mine.overrides).filter(value => value !== null).length;

  return (
    <details
      className="panel config-panel config-disclosure node-log-retention"
      open={expanded}
      onToggle={event => setExpanded(event.currentTarget.open)}
    >
      <summary>
        <PanelTitle of="artifacts">日志保留</PanelTitle>
        <span className="config-disclosure-summary">
          {overrideCount === 0 ? '与全局一致' : `本机覆盖 ${overrideCount} 项`}
        </span>
        <span className="sp" />
        <span className="config-disclosure-toggle">{expanded ? '收起' : '展开'}</span>
      </summary>
      <div className="fgrid one">
        <NodeLogLimitRow
          node={mine}
          global={logPolicy.data!.global}
          canEdit={canEdit}
          onSaved={view => qc.setQueryData(['agent-log-policy'], view)}
        />
      </div>
    </details>
  );
}

/** 该项取自全局默认值还是这台机器自己设置的。继承只是来源说明，使用中性标签；
    本机覆盖是需要留意的差异状态。 */
function OverrideTag({ own }: { own: boolean }) {
  return <span className={own ? 'st st-warn' : 'st'}>{own ? '本机覆盖' : '继承全局'}</span>;
}

/* Agent、XRAY 与每个 Phantun 日志项的磁盘上限，本机覆盖优先于全局。
 *
 * 与设置页机队表里的那一行（`NodeLogPolicyRow`）是同一项配置，形态按本页的表单行重排：
 * 那一行要在一张机队表里标出是哪台机器，因此带机器名和租户；此处整页都属于这台机器。
 *
 * 不写草稿、不产生修订：保存即落库，未覆盖的机器随下一轮 agent 轮询读到全局值。 */
type NodeLogKey = keyof AgentLogLimits;
type NodeLogForm = Record<NodeLogKey, string>;

const NODE_LOG_CLASSES: ReadonlyArray<{ key: NodeLogKey; label: string }> = [
  { key: 'agent_journal_mib', label: 'Agent 日志（MiB）' },
  { key: 'xray_mib', label: 'XRAY 日志（MiB）' },
  { key: 'phantun_mib', label: 'Phantun 日志（MiB）' },
];

function NodeLogLimitRow({
  node,
  global,
  canEdit,
  onSaved,
}: {
  node: AgentLogPolicyNode;
  global: AgentLogLimits;
  canEdit: boolean;
  onSaved: (view: AgentLogPolicyView) => void;
}) {
  const toForm = (overrides: AgentLogLimitOverrides): NodeLogForm => ({
    agent_journal_mib: overrides.agent_journal_mib == null ? '' : String(overrides.agent_journal_mib),
    xray_mib: overrides.xray_mib == null ? '' : String(overrides.xray_mib),
    phantun_mib: overrides.phantun_mib == null ? '' : String(overrides.phantun_mib),
  });
  const base = toForm(node.overrides);
  const [form, setForm] = useState<NodeLogForm | null>(null);
  const source = JSON.stringify([node.node_id, node.overrides]);
  const [syncedFrom, setSyncedFrom] = useState(source);
  if (source !== syncedFrom) {
    setSyncedFrom(source);
    setForm(null);
  }
  const shown = form ?? base;
  const parse = (raw: string) => (raw.trim() === '' ? null : validLogMib(raw));
  const next: AgentLogLimitOverrides = {
    agent_journal_mib: parse(shown.agent_journal_mib),
    xray_mib: parse(shown.xray_mib),
    phantun_mib: parse(shown.phantun_mib),
  };
  const invalid = NODE_LOG_CLASSES.some(item => shown[item.key].trim() !== '' && next[item.key] === null);
  const dirty = form !== null && NODE_LOG_CLASSES.some(item => shown[item.key].trim() !== base[item.key]);
  useUnsavedChanges(dirty, `${node.name} 的日志保留`, `node-tab:${node.node_id}:config`);

  const save = useMutation({
    mutationFn: () => saveNodeLogPolicy(node.node_id, next),
    onSuccess: view => {
      setForm(null);
      onSaved(view);
    },
  });

  return (
    <>
      {NODE_LOG_CLASSES.map(item => {
        const own = shown[item.key].trim() !== '';
        return (
          <Row k={item.label} key={item.key}>
            <span className="nd-ctl-line">
              <input
                className="f mono"
                style={{ width: 96 }}
                inputMode="numeric"
                aria-label={`本机 ${item.label}`}
                placeholder={String(global[item.key])}
                disabled={!canEdit || save.isPending}
                value={shown[item.key]}
                onChange={event => setForm({ ...shown, [item.key]: event.target.value })}
              />
              <OverrideTag own={own} />
            </span>
          </Row>
        );
      })}
      {invalid && (
        <span className="sub" style={{ color: 'var(--err)' }}>
          三项均需留空或填写 {LOG_MIN_MIB}–{LOG_MAX_MIB} 的整数。
        </span>
      )}
      {dirty && (
        <>
          {save.error && <ErrorBox error={save.error} />}
          <div className="toolbar">
            <span className="sp" />
            <button
              className="btn primary"
              disabled={!canEdit || invalid || save.isPending}
              onClick={() => save.mutate()}
            >
              {save.isPending ? '保存中…' : '保存'}
            </button>
            <button className="btn" disabled={save.isPending} onClick={() => setForm(null)}>
              还原
            </button>
          </div>
        </>
      )}
    </>
  );
}

/** 将输入解析为模型值。空值对应 null，表示使用全局默认；非数值同样按空值处理，
    服务端会拒绝非法输入。 */
function connValue(raw: string): number | null {
  const t = raw.trim();
  if (t === '') return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

/* 导出供测试挂载：草稿丢弃后本卡是否回到已提交状态，只有连着真实取数路径才测得出。 */
export function WgCard({
  node,
  listenPort,
  peers,
  disabledLinks,
  enabled,
  canEdit,
  onSaved,
}: {
  node: NodeAgentStateItem;
  listenPort: number | null;
  peers: NodeAgentStateItem[];
  disabledLinks: { a: string; b: string }[];
  enabled: boolean;
  canEdit: boolean;
  onSaved: () => void;
}) {
  return (
    <div className="panel config-panel">
      <header>
        <PanelTitle of="tunnels">WIREGUARD</PanelTitle>
      </header>
      <div className="fgrid one">
        <OverlayRow node={node} canEdit={canEdit} onSaved={onSaved} />
        {/* 关闭时下面四行不画：不生成 wg 配置，监听端口、MTU、入站封装、禁用组合都不会
            进入产物，改它们没有任何效果。取值来自草稿快照（与运行时判定同一个数据源，
            见 NodeDetail 的 wireguardEnabled），因此是「已写入草稿的关闭」才收起——
            开关刚拨到关闭、尚未保存时四行仍在，那时改动还没生效。 */}
        {enabled && (
          <>
            <WgListenPortRow nodeId={node.node_id} currentPort={listenPort} canEdit={canEdit} onSaved={onSaved} />
            <NodeMtuRow node={node} canEdit={canEdit} onSaved={onSaved} />
            <WgTransportRow node={node} canEdit={canEdit} onSaved={onSaved} />
            <WgDisabledLinksRow
              node={node}
              peers={peers}
              disabledLinks={disabledLinks}
              canEdit={canEdit}
              onSaved={onSaved}
            />
          </>
        )}
      </div>
    </div>
  );
}

function WgDisabledLinksRow({
  node,
  peers,
  disabledLinks,
  canEdit,
  onSaved,
}: {
  node: NodeAgentStateItem;
  peers: NodeAgentStateItem[];
  disabledLinks: { a: string; b: string }[];
  canEdit: boolean;
  onSaved: () => void;
}) {
  const qc = useQueryClient();
  const [selected, setSelected] = useState('');
  const disabledPeers = disabledLinks
    .flatMap(link => (link.a === node.node_id ? [link.b] : link.b === node.node_id ? [link.a] : []))
    .sort();
  const peerById = new Map(peers.map(peer => [peer.node_id, peer]));
  const candidates = peers
    .filter(
      peer =>
        peer.node_id !== node.node_id &&
        peer.overlay &&
        peer.lifecycle_phase === 'active' &&
        !disabledPeers.includes(peer.node_id),
    )
    .sort((a, b) => a.node_id.localeCompare(b.node_id));

  const change = useMutation({
    mutationFn: ({ peer, disabled }: { peer: string; disabled: boolean }) =>
      setWireGuardLinkDisabled(node.node_id, peer, disabled),
    onSuccess: () => {
      setSelected('');
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      onSaved();
    },
  });

  return (
    <Row k="禁 Peer 组合">
      {disabledPeers.length === 0 ? (
        <span className="wg-peer-empty">暂无禁用组合</span>
      ) : (
        <span className="wg-peer-list" role="list" aria-label="已禁用的 Peer 组合">
          {disabledPeers.map(peer => (
            <span key={peer} className="wg-peer-item" role="listitem">
              <span className="wg-peer-name">
                <b>{peerById.get(peer)?.name || peer}</b>
                {peerById.get(peer)?.name && <code>{peer}</code>}
              </span>
              <button
                className="btn"
                disabled={!canEdit || change.isPending}
                onClick={() => change.mutate({ peer, disabled: false })}
              >
                恢复
              </button>
            </span>
          ))}
        </span>
      )}
      <span className="wg-peer-picker">
        <select
          className="f"
          value={selected}
          disabled={!canEdit || !node.overlay || change.isPending || candidates.length === 0}
          aria-label="选择要加入禁用组合的 WireGuard Peer"
          onChange={event => setSelected(event.target.value)}
        >
          <option value="">选择 Peer…</option>
          {/* 有名字时不再把 id 附在括号里重复一遍：`香港 DMIT AS3 Pro (dmit-hkg-as3-pro-lokmachau)`
              里 id 占了一半宽度，而上面的已禁用列表本来就是「名字 + 小字 id」两行式，
              加进去之后那一行仍然会写明是哪台。没有名字的机器仍然显示 id。 */}
          {candidates.map(peer => (
            <option key={peer.node_id} value={peer.node_id}>
              {peer.name || peer.node_id}
            </option>
          ))}
        </select>
        <button
          className="btn"
          disabled={!canEdit || change.isPending || selected === ''}
          onClick={() => change.mutate({ peer: selected, disabled: true })}
        >
          加入禁用
        </button>
      </span>
      <span className="sub">发布后双方都不再配置该 peer；依赖它的业务跳会阻止发布。</span>
      {change.error && <ErrorBox error={change.error} />}
    </Row>
  );
}

// ── 运行时对账的判定 ──────────────────────────────────────────────
// 这四项与产物对账不同：产物对账两侧都有期望值，不一致时重新下发；而版本由发行版决定、
// `.dat` 由 xray 自行下载、状态巡检由 agent 自行修复、丢失的用量记录无法补回。
// 它们不能通过重新发布解决，因此每条判定都需要写明成因、影响和处理方式，
// 而不是只标记为异常。

type Finding = {
  tone: 'warn' | 'bad';
  chip: string;
  text: ReactNode;
  /** 只在详情页显示，不进入列表。列表用于定位存在问题的机器，而部分条目是说明而非问题——
      例如「运行时观测尚未到达」，它解释了相邻字段显示为「—」的原因，
      但机器本身没有异常。不加区分时，刚纳管、尚未轮到上报的机器会在列表中显示警告标记，
      即把正常机器显示为异常，其后果与把异常机器显示为正常同样严重。 */
  detailOnly?: boolean;
};

/** 从 `Xray 26.7.28 (…)` 中提取 xray 版本号，无法提取时返回 null，不做推断。 */
function xrayVer(raw: string | null | undefined): [number, number] | null {
  const m = raw?.match(/(\d+)\.(\d+)\./);
  return m ? [Number(m[1]), Number(m[2])] : null;
}

/** `geodata` 自动更新合入主线的版本。低于该版本的 xray 会忽略该段配置且不报错。 */
const XRAY_GEODATA_MIN: [number, number] = [26, 4];

function spoolDropCounts(spool: NonNullable<NodeAgentStateItem['spool_backlog']>) {
  const usage = Math.max(0, spool.usage_dropped ?? 0);
  const observation = Math.max(0, spool.observation_dropped ?? 0);
  return {
    usage,
    observation,
    unclassified: Math.max(0, spool.dropped - usage - observation),
  };
}

export function runtimeFindings(
  node: NodeAgentStateItem,
  wireguardEnabled?: boolean,
  isolatedNodeIds?: ReadonlySet<string>,
): Finding[] {
  const out: Finding[] = [];
  const v = node.runtime_versions;

  const xv = xrayVer(v?.xray);
  const xrayTooOld =
    xv !== null && (xv[0] < XRAY_GEODATA_MIN[0] || (xv[0] === XRAY_GEODATA_MIN[0] && xv[1] < XRAY_GEODATA_MIN[1]));
  if (xrayTooOld) {
    out.push({
      tone: 'bad',
      // `detailOnly`：版本号在列表中不构成可操作信息——「xray 25.9」既不说明问题所在
      // 也不说明处理方式，仍需进入详情页阅读说明。因此它只在详情页显示，
      // 不在列表中占用标记位。
      detailOnly: true,
      chip: `xray ${xv![0]}.${xv![1]}`,
      text: (
        <>
          xray 低于 <b>26.4</b>：<code>geodata</code> 自动更新 2026-04-25 才进主线，这一版会
          <b>静默忽略</b>那段配置——规则库不会更新，而配置里写着每天更新。 升级 xray 才能修，重新发布没用。
        </>
      ),
    });
  }

  const wgEnabled = wireguardEnabled ?? (node.overlay && !node.retired_at);
  const wgAppliedState = appliedOf(node)?.wireguard?.state;
  if (wgEnabled && wgAppliedState !== 'disabled' && v?.wg_backend === 'userspace') {
    out.push({
      tone: 'warn',
      chip: '用户态',
      text: (
        <>
          内核里没有 wireguard 模块，<code>wg-quick</code> 回落到 wireguard-go。
          <code>wg show</code> 的输出跟内核态一模一样，吞吐差一个数量级。
        </>
      ),
    });
  }

  const wg = node.wireguard_health;
  if (wgEnabled && wgAppliedState !== 'disabled' && wg) {
    if (!wg.enabled) {
      out.push({
        tone: 'bad',
        chip: 'WG 未运行',
        text: <>模型要求加入 overlay，但 agent 报告 WireGuard 巡检未启用。</>,
      });
    } else if (wg.error) {
      out.push({
        tone: 'bad',
        chip: 'WG 巡检失败',
        text: <>agent 无法读取 WireGuard 运行态：{wg.error}</>,
      });
    } else {
      // 隔离已经是对该节点不可达的业务处置。原始健康数据仍保存在控制面供诊断，
      // 界面不再在每台健康节点上重复展示指向该隔离节点的同一条断链 finding。
      const visiblePeers = wg.peers.filter(peer => !isolatedNodeIds?.has(peer.peer_node_id));
      const down = visiblePeers.filter(peer => peer.status === 'down');
      if (down.length > 0) {
        out.push({
          tone: 'warn',
          chip: `WG 断链 ${down.length}`,
          text: (
            <>
              {down.map(peer => (
                <span key={peer.peer_node_id}>
                  <code>{peer.peer_node_id}</code>：{peer.detail ?? 'peer 无法通过 overlay 到达'}
                  <br />
                </span>
              ))}
            </>
          ),
        });
      }
      const unknown = visiblePeers.filter(peer => peer.status === 'unknown');
      if (unknown.length > 0) {
        out.push({
          tone: 'warn',
          chip: `WG 待判定 ${unknown.length}`,
          text: (
            <>
              {unknown.map(peer => (
                <span key={peer.peer_node_id}>
                  <code>{peer.peer_node_id}</code>：{peer.detail ?? '无法执行可达性探测'}
                  <br />
                </span>
              ))}
            </>
          ),
        });
      }
    }
  }

  const spool = node.spool_backlog;
  if (spool) {
    const dropped = spoolDropCounts(spool);
    if (dropped.usage > 0) {
      out.push({
        tone: 'bad',
        chip: `丢了 ${dropped.usage.toLocaleString()} 条用量`,
        text: (
          <>
            用量上报已被永久丢弃，对应时段的流量明细<b>找不回来</b>。先检查控制面连通性和用量接口错误。
          </>
        ),
      });
    }
    if (dropped.observation > 0) {
      out.push({
        tone: 'warn',
        chip: `丢了 ${dropped.observation.toLocaleString()} 条收敛结果`,
        text: <>Agent 已执行但结果被控制面永久拒绝；发布可能缺少执行证据，但这不代表流量丢失。</>,
      });
    }
    if (dropped.unclassified > 0) {
      out.push({
        tone: 'warn',
        chip: `丢了 ${dropped.unclassified.toLocaleString()} 条未分类上报`,
        text: <>旧 Agent 只记录总数，无法还原是用量还是收敛结果；升级后新丢弃会分类记录。</>,
      });
    }
  }

  const usage = node.usage_last_result;
  if (usage && usage.rejected_counters > 0) {
    out.push({
      tone: 'bad',
      chip: `拒收 ${usage.rejected_counters} 项用量`,
      text: <>最近一轮出现乱序、同一 Xray 代次内计数倒退，或标签不属于这台机器；异常项没有入账。</>,
    });
  }
  if ((usage?.growing_unknown_counters ?? 0) > 0) {
    out.push({
      tone: 'warn',
      chip: `未归属流量 ${usage!.growing_unknown_counters} 项`,
      text: <>未知计数与上一轮相比仍在增长，流量没有入账。检查 Xray 中正在运行的用户与当前权限是否一致。</>,
    });
  }
  // gap_samples 表示 Xray 重启或用量代次切换：字节已经正常入账，只是该窗口不能计算
  // 可比速率。曲线仍按 has_gap 断开，避免画出错误速率；它不是机器故障，不进入健康告警。

  const lr = node.last_local_reconcile;
  if (lr?.error) {
    out.push({
      tone: 'bad',
      chip: '自修失败',
      text: <>状态巡检修不动：{lr.error}</>,
    });
  }

  /* 该条需要最后添加：它说明上面各项显示为「—」的原因。 */
  if (!node.runtime_versions && node.agent_version) {
    out.push({
      tone: 'warn',
      detailOnly: true,
      // 构建号截断显示：完整 sha 有 64 位，完整显示会导致该行超出宽度。
      chip: `agent ${node.agent_version.replace(/^brocade-agent\//, '').slice(0, 12)}`,
      text: (
        <>
          还没收到这台的运行时观测：可能是刚纳管、还没轮到（周期 30 分钟），也可能是 Agent 协议尚未恢复。
          所以那几处是「—」不是 0。
          {/* 「等待下次发布」的表述是错误的——发布只包含配置（wireguard.conf / xray.json / grants），
              不包含 agent 二进制。但升级 agent 同样无需登录机器：agent 每轮都会请求一次
              `/agent/v1/agent-release`（brocade-agent/src/selfupdate.rs），控制面按其上报的
              arch 下发内嵌的对应版本，安装后退出并由 systemd 用新二进制重新拉起。
              登录机器重跑 install.sh 是备用方案，用于已无法访问控制面的机器。 */}
          <br />
          要升级 agent 去<b>「发布 → agent 更新」</b>发一版、范围盖上这台—— 批准后它自己换、自己重启，不用上机。
        </>
      ),
    });
  }
  return out;
}

/** 成因说明。位置和语气与 USAGE 卡下方的采集缺口说明一致。 */
function Findings({ list }: { list: Finding[] }) {
  if (list.length === 0) return null;
  return (
    <>
      {list.map((f, i) => (
        <p key={i} className={`rt-dx${f.tone === 'bad' ? ' bad' : ''}`}>
          {f.text}
        </p>
      ))}
    </>
  );
}

/** agent 的标识：其二进制自身的 sha256，不是版本号。
 *
 * 有两种来源和两种格式：poll 的 User-Agent 是 `brocade-agent/<sha>`，runtime 上报是裸 sha。
 * 两者都提取出 sha 后显示前 12 位，完整值写入 title 以便完整比对。 */
function agentIdent(raw: string | null | undefined) {
  if (!raw) return <span className="dim">—</span>;
  const bare = raw.replace(/^brocade-agent\//, '');
  if (bare === 'unknown')
    return (
      <span className="hot" title="agent 读不了 /proc/self/exe；这台机器不会自更新">
        读不出
      </span>
    );
  if (/^[0-9a-f]{64}$/.test(bare))
    return (
      <span className="mono" title={bare}>
        {bare.slice(0, 12)}
      </span>
    );
  return (
    <span className="hot" title={`无效的 Agent 构建标识：${bare}`}>
      标识无效
    </span>
  );
}

function agentProtocol(version: number | null) {
  if (version !== null && version >= MIN_AGENT_PROTOCOL_VERSION) return <>v{version}</>;
  return <Hot>{version === null ? '未上报' : `v${version} · 不兼容`} · 等待救援更新</Hot>;
}

function openvpnExtensionStatus(node: NodeAgentStateItem) {
  const installed = Boolean(node.runtime_versions?.openvpn?.trim());
  return <StateChip state={installed ? 'present' : 'disabled'} label="OPENVPN" />;
}

type ObservationReadState = 'pending' | 'error';

type ObservationChartLoadingComponent = NodeObservationModules['ObservationChartLoading'];

function ObservationReading({
  state = 'pending',
  ChartLoading,
}: {
  state?: ObservationReadState;
  ChartLoading?: ObservationChartLoadingComponent;
}) {
  if (state === 'pending' && ChartLoading) return <ChartLoading />;
  return (
    <div className={`node-observation-reading ${state}`}>{state === 'pending' ? <PanelLoading /> : '加载失败'}</div>
  );
}

function ObservationKpisState({ state = 'pending' }: { state?: ObservationReadState }) {
  return (
    <div
      className="kpi-band node-observation-kpis-state"
      aria-label={state === 'pending' ? '负载加载中' : '负载加载失败'}
    >
      {['CPU', '内存', '磁盘', '连接表', 'Load 1m', '已运行'].map(label => (
        <div className="kpi" key={label}>
          <span className="kpi-l">{label}</span>
          <span className={`kpi-v node-observation-reading-value ${state}`}>
            {state === 'pending' ? <FieldLoading announce={false} /> : '加载失败'}
          </span>
        </div>
      ))}
    </div>
  );
}

function ObservationChartStateBlock({
  title,
  icon,
  state = 'pending',
  ChartLoading,
}: {
  title: string;
  icon: IconName;
  state?: ObservationReadState;
  ChartLoading?: ObservationChartLoadingComponent;
}) {
  return (
    <div className="nd-throughput-block">
      <div className="load-network-cap">
        <b>
          <Icon of={icon} size={13} className="chart-title-icon" />
          {title}
        </b>
      </div>
      <ObservationReading state={state} ChartLoading={ChartLoading} />
    </div>
  );
}

function ThroughputPanelState({
  state = 'pending',
  ChartLoading,
}: {
  state?: ObservationReadState;
  ChartLoading?: ObservationChartLoadingComponent;
}) {
  return (
    <section className="chart-card nd-throughput-panel" aria-label="吞吐">
      <ObservationChartStateBlock title="网卡流量" icon="agent" state={state} ChartLoading={ChartLoading} />
      <ObservationChartStateBlock title="XRAY 流量" icon="tunnels" state={state} ChartLoading={ChartLoading} />
    </section>
  );
}

function PingProbePanelState({
  state = 'pending',
  ChartLoading,
}: {
  state?: ObservationReadState;
  ChartLoading?: ObservationChartLoadingComponent;
}) {
  return (
    <section className="chart-card ping-probe-panel" aria-label="Ping">
      <ObservationChartStateBlock title="ICMP PING" icon="diag" state={state} ChartLoading={ChartLoading} />
      <ObservationChartStateBlock title="TCP PING" icon="diag" state={state} ChartLoading={ChartLoading} />
    </section>
  );
}

/** 该机器的当前负载。自行获取数据，与 UsageCard 结构相同——详情页不统一管理各卡的数据。
 *
 * 获取失败时整张卡不渲染（`retry: false` 且不提示）：负载查询失败不是该机器本身的
 * 健康结论，不应在其详情页显示成机器错误。 */
function LoadCardFor({
  nodeId,
  range,
  linked,
  observationModules,
}: {
  nodeId: string;
  range: LoadRange;
  linked: boolean;
  observationModules: NodeObservationModules;
}) {
  /* 10s 而不是与上报窗口相同的 30s。窗口每 30 秒关一次（agent 的
     `SUBS_PER_WINDOW × SUB_INTERVAL_SECS`），前端也每 30 秒拉一次时两者不同相：
     最坏情况拿到的是刚过期 30 秒的窗口，再等 30 秒才拉下一次，端到端能到 60 秒。
     按 10s 拉，窗口一关最多 10 秒就被取到——这一段是纯等待，缩掉不损失任何东西。
     真正的分辨率仍是 30 秒，那要改 agent 的窗口，且用量窗口得一起动（两者故意对齐，
     遥测页把流量柱和 CPU 线叠在同一根时间轴上就靠这个）。 */
  const load = useQuery({
    ...nodeLoadRangeQuery(nodeId, range),
    // 长范围通常有约 2,880 个深度窗口，而原数据本身每 30 秒才增加一点。
    // 30m/1h 保留 10s 的低延迟；6h 以上按数据分辨率拉取，避免重复传输同一大段历史。
    refetchInterval: fixedLoadRange(range) ? false : range.seconds <= 60 * 60 ? 10_000 : 30_000,
  });
  if (!load.data) return <ObservationKpisState state={load.error ? 'error' : 'pending'} />;
  // 吞吐两图（网卡 / XRAY）已移到 ThroughputPanel，与 Ping 面板并列；本卡只留生命体征与历史。
  const LoadCard = observationModules.LoadCard;
  return (
    <LoadCard
      report={load.data}
      historyLabel={range.heading}
      linked={linked}
      metricRangeKey={loadRangeKey(range)}
      metricLive={!fixedLoadRange(range)}
    />
  );
}

const PING_SERIES_CSS = OBSERVE_SERIES_COLOR_VARS;
const PING_LEGEND_LIMIT = 3;
type PingProtocol = PingProbeKind;

/** A target of one block (ICMP or TCP). Its color is its position in the block, so it keeps the
 * same color whichever family the panel shows. */
interface PingBlockTarget {
  target: PingProbeTargetSeries;
  color: number;
}

interface PingFamilyState {
  /** At least one target has an endpoint of this family. */
  configured: boolean;
  /** Every sample of this family in the range was skipped, at least one for lack of a route. */
  noRoute: boolean;
  /** The worst loss of any target of this family in the range. */
  tone: PingLossTone | null;
}

function pingFamilyStates(view: NodePingProbeView): Record<PingProbeFamily, PingFamilyState> {
  const state = (family: PingProbeFamily): PingFamilyState => {
    const series = view.targets.flatMap(target => target[family] ?? []);
    const samples = series.flatMap(entry => entry.samples);
    return {
      configured: series.length > 0,
      noRoute:
        samples.length > 0 &&
        samples.every(sample => !sample.attempted) &&
        samples.some(sample => sample.skip_reason === 'no_route'),
      tone: worstPingLossTone(series.map(entry => pingLossTone(pingLossStats(entry.samples)))),
    };
  };
  return { ipv4: state('ipv4'), ipv6: state('ipv6') };
}

const pingFamilyUsable = (state: PingFamilyState) => state.configured && !state.noRoute;

function PingLegendItem({
  target,
  color,
  family,
}: PingBlockTarget & {
  family: PingProbeFamily;
}) {
  const series = target[family]!;
  const loss = pingLossStats(series.samples);
  const lossText = pingLossText(loss.percent);
  const reason = loss.attempted === 0 ? pingSkipReason(series.samples) : null;
  const tone = pingLossTone(loss);
  const latest = series.samples.at(-1);
  const sampledAt = latest ? new Date(latest.probed_at_unix_secs * 1000).toLocaleString('zh-CN') : '尚无样本';
  const skipped = series.samples.length - loss.attempted;
  const title =
    `${PING_FAMILY_LABEL[family]} ${series.address}\n所选时段丢包率：${lossText}（丢包 ${loss.lost} / 已探测 ${loss.attempted}）` +
    `\n未探测：${skipped}${reason ? `（${pingSkipReasonText(reason, family)}）` : ''}\n最新样本：${sampledAt}`;
  return (
    <span title={title}>
      <i style={{ background: `var(${PING_SERIES_CSS[color % PING_SERIES_CSS.length]})` }} />
      <span className="ping-probe-name">{target.name}</span>
      <b
        className={loss.attempted === 0 ? 'gap' : tone ? `loss ${tone}` : undefined}
        aria-label={
          reason ? `${target.name} 未探测：${pingSkipReasonText(reason, family)}` : `${target.name} 丢包率 ${lossText}`
        }
      >
        {reason ? pingSkipReasonShort(reason, family) : lossText}
      </b>
    </span>
  );
}

/** The shown family's legend: the first targets by block position, and「+N」for the rest. 「+N」
 * takes the loss color of the targets it folds, which the legend would otherwise hide. */
function PingProbeLegend({ targets, family }: { targets: PingBlockTarget[]; family: PingProbeFamily }) {
  const entries = targets.filter(({ target }) => target[family]);
  const shown = entries.slice(0, PING_LEGEND_LIMIT);
  const hidden = entries.slice(PING_LEGEND_LIMIT).map(({ target }) => ({
    name: target.name,
    loss: pingLossStats(target[family]!.samples),
  }));
  const lossy = hidden.filter(entry => entry.loss.lost > 0);
  const moreTone = worstPingLossTone(lossy.map(entry => pingLossTone(entry.loss)));
  const moreTitle =
    lossy.length > 0
      ? `未列出的目标有丢包：\n${lossy.map(entry => `${entry.name} ${pingLossText(entry.loss.percent)}`).join('\n')}`
      : `未列出：${hidden.map(entry => entry.name).join('、')}`;
  return (
    <footer className="load-network-legend ping-probe-legend" aria-label="Ping 图例">
      {shown.map(entry => (
        <PingLegendItem key={entry.color} {...entry} family={family} />
      ))}
      {hidden.length > 0 && (
        <span className={moreTone ? `ping-probe-more ${moreTone}` : 'ping-probe-more'} title={moreTitle}>
          +{hidden.length}
        </span>
      )}
    </footer>
  );
}

/** The panel's family switch, at its top-right corner in the first block's title bar: the whole
 * panel shows one family. A family with no configured target, or that the machine has no route for,
 * cannot be chosen. A dot on the other family says it lost packets in the range. */
function PingFamilySwitch({
  family,
  states,
  onChange,
}: {
  family: PingProbeFamily;
  states: Record<PingProbeFamily, PingFamilyState>;
  onChange: (family: PingProbeFamily) => void;
}) {
  const choose = (next: PingProbeFamily, focus?: HTMLElement | null) => {
    if (next === family || !pingFamilyUsable(states[next])) return;
    onChange(next);
    focus?.querySelector<HTMLButtonElement>(`[data-family="${next}"]`)?.focus();
  };
  return (
    <div
      className="ping-family-switch"
      role="radiogroup"
      aria-label="Ping 地址族"
      data-family={family}
      onKeyDown={event => {
        if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
        event.preventDefault();
        choose(family === 'ipv4' ? 'ipv6' : 'ipv4', event.currentTarget);
      }}
    >
      <span className="ping-family-switch-thumb" aria-hidden="true" />
      {PING_PROBE_FAMILIES.map(option => {
        const state = states[option];
        const label = PING_FAMILY_LABEL[option];
        const blocked = state.noRoute
          ? `机器没有 ${label} 路由，${label} 未探测`
          : state.configured
            ? null
            : `没有填写 ${label} 地址的目标`;
        const dot = blocked === null && option !== family ? state.tone : null;
        const loss = dot === 'down' ? '（有目标完全无响应）' : dot === 'partial' ? '（有丢包）' : '';
        return (
          <button
            key={option}
            type="button"
            role="radio"
            data-family={option}
            aria-checked={option === family}
            tabIndex={option === family ? 0 : -1}
            disabled={blocked !== null}
            data-dot={dot ?? undefined}
            title={blocked ?? `Ping 面板显示 ${label}${loss}`}
            onClick={() => choose(option)}
          >
            {label}
          </button>
        );
      })}
    </div>
  );
}

/** The second block ends its title bar with an empty slot as wide as the switch above it, so both
 * legends end at the same edge. */
function usePingSwitchSpacer(panelRef: RefObject<HTMLElement | null>) {
  useLayoutEffect(() => {
    const switcher = panelRef.current?.querySelector<HTMLElement>('.ping-family-switch');
    const spacer = panelRef.current?.querySelector<HTMLElement>('.ping-family-switch-spacer');
    if (!switcher || !spacer) return;
    const match = () => {
      spacer.style.width = `${switcher.getBoundingClientRect().width}px`;
    };
    match();
    // The button labels settle once the web font arrives.
    const observer = new ResizeObserver(match);
    observer.observe(switcher);
    return () => observer.disconnect();
  });
}

/** Fade the block body in after a family switch, as one panel-wide change rather than two charts
 * redrawing on their own. */
function useFamilySwitchFade(bodyRef: RefObject<HTMLElement | null>, family: PingProbeFamily) {
  const shown = useRef(family);
  useLayoutEffect(() => {
    if (shown.current === family) return;
    shown.current = family;
    const body = bodyRef.current;
    if (!body?.animate || window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return;
    body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 260, easing: 'ease' });
  }, [bodyRef, family]);
}

function PingProbeBlock({
  targets,
  protocol,
  family,
  corner,
  bounds,
  group,
  loading,
  observationModules,
}: {
  targets: PingBlockTarget[];
  protocol: PingProtocol;
  family: PingProbeFamily;
  /** The family switch in the first block, its width-matching slot in the second. */
  corner: ReactNode;
  bounds: { startUnixSecs: number; endUnixSecs: number };
  group?: string;
  loading: boolean;
  observationModules: NodeObservationModules;
}) {
  const lines = useMemo(
    () =>
      targets.flatMap(({ target, color }) => {
        const series = target[family];
        return series ? [{ name: target.name, color, samples: series.samples }] : [];
      }),
    [targets, family],
  );
  const bodyRef = useRef<HTMLDivElement>(null);
  useFamilySwitchFade(bodyRef, family);
  const label = `${protocol.toUpperCase()} PING`;
  const hasSamples = lines.some(line => line.samples.length > 0);
  const PingLatencyChart = observationModules.PingLatencyChart;
  return (
    <div className="ping-probe-block" aria-label={label}>
      <div className="load-network-cap">
        <b>
          <Icon of="diag" size={13} className="chart-title-icon" />
          {label}
        </b>
        {/* 量纲跟着图走：没画图时刻度也不存在，标题栏就不该挂一个单位。
            时延轴永远是毫秒（见 observeMsUnit），所以这里不必反算值轴。 */}
        {hasSamples && <span className="chart-unit">({OBSERVE_MS_UNIT})</span>}
        {targets.length > 0 && <PingProbeLegend targets={targets} family={family} />}
        {corner}
      </div>
      <div className="ping-probe-body" ref={bodyRef}>
        {loading ? (
          <ObservationReading ChartLoading={observationModules.ObservationChartLoading} />
        ) : targets.length === 0 ? (
          <p className="note ping-probe-empty">尚未在设置中配置 {protocol.toUpperCase()} 探测目标。</p>
        ) : lines.length === 0 ? (
          <p className="note ping-probe-empty">
            没有填写 {PING_FAMILY_LABEL[family]} 地址的 {protocol.toUpperCase()} 目标。
          </p>
        ) : hasSamples ? (
          <Suspense fallback={<ObservationReading ChartLoading={observationModules.ObservationChartLoading} />}>
            <PingLatencyChart lines={lines} family={family} bounds={bounds} group={group} />
          </Suspense>
        ) : (
          <p className="note ping-probe-empty">Ping 落点已配置，但 Agent 尚未上报样本。</p>
        )}
      </div>
    </div>
  );
}

export function PingProbePanel({
  nodeId,
  range,
  linked,
  observationModules,
}: {
  nodeId: string;
  range: LoadRange;
  linked: boolean;
  observationModules: NodeObservationModules;
}) {
  const probe = useQuery({
    ...nodePingRangeQuery(nodeId, range),
  });
  const [chosenFamily, setChosenFamily] = useState<PingProbeFamily>('ipv4');
  const panelRef = useRef<HTMLElement>(null);
  usePingSwitchSpacer(panelRef);
  const bounds = useMemo(() => {
    if (range.startUnixSecs != null && range.endUnixSecs != null) {
      return { startUnixSecs: range.startUnixSecs, endUnixSecs: range.endUnixSecs };
    }
    // Advance the live axis with Ping refreshes, not unrelated parent renders.
    const endUnixSecs = Math.floor(probe.dataUpdatedAt / 1000);
    return { startUnixSecs: endUnixSecs - range.seconds, endUnixSecs };
  }, [range.startUnixSecs, range.endUnixSecs, range.seconds, probe.dataUpdatedAt]);
  const view = useMemo(() => probe.data ?? { node_id: nodeId, targets: [] }, [probe.data, nodeId]);
  const states = useMemo(() => pingFamilyStates(view), [view]);
  const blocks = useMemo(() => {
    const of = (protocol: PingProtocol) =>
      view.targets.filter(target => target.kind === protocol).map((target, color) => ({ target, color }));
    return { icmp: of('icmp'), tcp: of('tcp') };
  }, [view]);
  if (probe.error && !probe.data)
    return <PingProbePanelState state="error" ChartLoading={observationModules.ObservationChartLoading} />;
  const other = chosenFamily === 'ipv4' ? 'ipv6' : 'ipv4';
  // The chosen family stays chosen while it has data to show; otherwise the panel shows the other.
  const family = pingFamilyUsable(states[chosenFamily]) || !pingFamilyUsable(states[other]) ? chosenFamily : other;
  const switcher =
    probe.isPending || view.targets.length === 0 ? null : (
      <PingFamilySwitch family={family} states={states} onChange={setChosenFamily} />
    );
  const group = linked ? `nd-ping-${nodeId}` : undefined;
  return (
    <section className="chart-card ping-probe-panel" aria-label="Ping" ref={panelRef}>
      {(['icmp', 'tcp'] as const).map((protocol, index) => (
        <PingProbeBlock
          key={protocol}
          targets={blocks[protocol]}
          protocol={protocol}
          family={family}
          corner={
            switcher && (index === 0 ? switcher : <span className="ping-family-switch-spacer" aria-hidden="true" />)
          }
          bounds={bounds}
          group={group}
          loading={probe.isPending}
          observationModules={observationModules}
        />
      ))}
    </section>
  );
}

/** 该机器本身：内核与平台、容量分母（LOAD/KPI 百分比的基数）、内核参数上限。
    组件版本不在这里；过旧或回落用户态等需要处理的结论由 CONFIGURATIONS 的 findings 呈现。 */
export function HostCard({ load, pending = false }: { load: NodeLoadView | undefined; pending?: boolean }) {
  const host = load?.host ?? null;
  const skew = load?.clock_skew_secs ?? null;
  /* CPU / 发行版 / 内核各自的第二项事实（核数、虚拟化、架构）单独成段：它们不是单位，
     而是同一字段的另一个取值，降一档色阶后主值在一列读数里仍然是最先被读到的。
     旧 Agent 没有 cpu_model 时仍显示核数。 */
  const cpu = host?.cpu_model || '';
  const cores = host && host.cores > 0 ? `${host.cores} 核` : '';
  const meta =
    load?.reported_at_unix_secs != null ? (
      <>
        <Ago at={iso(load.reported_at_unix_secs)} /> 上报
      </>
    ) : (
      '—'
    );
  if (!host)
    return (
      <Band name="HOST" icon="nodes" meta={meta}>
        <p className="note nd-rt-empty">{pending ? <FieldLoading /> : '还没有主机信息上报。'}</p>
      </Band>
    );
  const cpuSummary = [cpu, cores].filter(Boolean).join(' · ');
  const osSummary = [host.os_pretty, host.virt].filter(Boolean).join(' · ');
  const kernelSummary = [host.kernel, host.arch].filter(Boolean).join(' · ');
  const bufferSummary =
    host.rmem_max > 0 || host.wmem_max > 0 ? `${bytes(host.rmem_max)} / ${bytes(host.wmem_max)}` : '';
  return (
    <Band name="HOST" icon="nodes" meta={meta}>
      <Cells
        items={[
          [
            'CPU',
            cpuSummary ? (
              <span title={cpuSummary}>
                {cpu || cores}
                {cpu && cores && <span className="nd-rt-q">{cores}</span>}
              </span>
            ) : (
              <span className="dim">—</span>
            ),
            'w3',
          ],
          ['内存', host.mem_total_bytes > 0 ? bytes(host.mem_total_bytes) : <span className="dim">—</span>],
          ['磁盘', host.disk_total_bytes > 0 ? bytes(host.disk_total_bytes) : <span className="dim">—</span>],
          [
            '发行版',
            osSummary ? (
              <span title={osSummary}>
                {host.os_pretty || host.virt}
                {host.os_pretty && host.virt && <span className="nd-rt-q">{host.virt}</span>}
              </span>
            ) : (
              <span className="dim">—</span>
            ),
            'w3',
          ],
          [
            '内核',
            kernelSummary ? (
              <span title={kernelSummary}>
                {host.kernel || host.arch}
                {host.kernel && host.arch && <span className="nd-rt-q">{host.arch}</span>}
              </span>
            ) : (
              <span className="dim">—</span>
            ),
            'w3',
          ],
          [
            '时钟偏移',
            skew !== null ? (
              /* 接收上报时测得的 agent 钟减控制面钟。TLS 依赖对时；±5 秒是传输耗时的
                 噪声上限，超过即标出。 */
              <span title="agent 时钟减控制面时钟，接收上报时测得">
                {Math.abs(skew) >= 5 ? (
                  <Hot>
                    {skew >= 0 ? '+' : '-'}
                    {Math.abs(skew)} 秒
                  </Hot>
                ) : (
                  <>
                    {skew >= 0 ? '+' : '-'}
                    {Math.abs(skew)} 秒
                  </>
                )}
              </span>
            ) : (
              <span className="dim">—</span>
            ),
          ],
          /* 这两项安装器不管（install.sh 只调拥塞与连接表），机器上是什么就显示什么。
             不用 .inherit 降色阶：取值来源恒定为「非纳管」，对一个从不变的答案做视觉区分
             不携带信息。 */
          [
            '收发缓冲',
            bufferSummary ? <span title={bufferSummary}>{bufferSummary}</span> : <span className="dim">—</span>,
            'w2',
          ],
          ['监听队列', host.somaxconn > 0 ? host.somaxconn.toLocaleString() : <span className="dim">—</span>],
          [
            '连接表上限',
            host.conntrack_max !== null ? (
              host.conntrack_max.toLocaleString()
            ) : (
              // 未加载 nf_conntrack 不是故障，而是该机器未配置 NAT。显示「—」会被理解为读取失败。
              <span className="dim">没开</span>
            ),
          ],
        ]}
      />
    </Band>
  );
}

/** 规则库。不属于产物——控制面不下发它，由 xray 按 cron 自行下载，
    因此此处只呈现观测到的事实（更新时间、大小、来源），判定交给上面的 findings。 */
function AgentCard({
  node,
  agentStartedAt,
}: {
  node: NodeAgentStateItem;
  /** agent 进程的启动时刻（来自负载上报的进程表）。null 表示没有该读数 */
  agentStartedAt: number | null;
}) {
  // 已运行 = 当前时刻减启动时刻，每秒都在变——与 PollAgo 同理用 useNow 而非 Date.now()。
  const now = useNow();
  const [publicIpHistoryOpen, setPublicIpHistoryOpen] = useState(false);
  const publicIpHistory = usePublicIpHistory(node.node_id);
  const publicIpChangeCount =
    publicIpHistory.data?.events.filter(event => event.event_kind === 'changed').length ?? null;
  const publicIpLastSeen = [node.observed_public_ipv4?.last_seen_at, node.observed_public_ipv6?.last_seen_at]
    .filter((value): value is string => Boolean(value))
    .sort((a, b) => Date.parse(b) - Date.parse(a))[0];
  return (
    /* 「上次来拉」放在带名下方的时效位：它表示这条带的数据有多新，与 HOST 的上报时间、
       CONFIG 的观察时间同一性质，三者在同一列上才能横向比对。 */
    <Band
      name="AGENT"
      icon="agent"
      meta={
        <>
          <Ago at={node.last_poll_at} /> 来拉
        </>
      }
    >
      <Cells
        items={[
          ['上次用量', <Ago at={node.last_usage_report_at} />],
          [
            '用量明目',
            node.usage_last_result ? (
              <>
                {node.usage_last_result.accepted_readings}
                <span className="nd-rt-u">项</span>
              </>
            ) : (
              <span className="dim">—</span>
            ),
          ],
          // agent 进程自身的运行时长：崩溃循环的机器上该值恒为几分钟，与带名下的
          // 「6 秒前来拉」并排即可区分「连不上」与「一直在重启」。
          [
            '已运行',
            agentStartedAt !== null ? (
              dur(Math.max(0, Math.floor(now / 1000) - agentStartedAt))
            ) : (
              <span className="dim">—</span>
            ),
          ],
          // 「构建」不等同于「版本」：该字段是二进制自身 sha256 的前 12 位。发布 agent 时
          // 需要比对的正是它——控制面上已批准的构建号与该机器实际运行的构建号。
          ['构建', agentIdent(node.agent_version)],
          ['协议', agentProtocol(node.agent_protocol_version)],
          [
            '上报积压',
            node.spool_backlog ? (
              <>
                {node.spool_backlog.observation + node.spool_backlog.usage}
                <span className="nd-rt-u">条</span>
                <span className="dim"> · 丢弃 </span>
                {node.spool_backlog.dropped > 0 ? (
                  <Hot>{node.spool_backlog.dropped.toLocaleString()}</Hot>
                ) : (
                  node.spool_backlog.dropped.toLocaleString()
                )}
              </>
            ) : (
              <span className="dim">—</span>
            ),
          ],
          // token 字段已移除：凭据只在签发时出现一次，此处只能显示前几位，既无法核对
          // 也无法复制。需要确认的两项信息都在其他位置——连接状态见带名下的时效读数，
          // 更换凭据使用标题栏的「重签 token」。
          // 「状态巡检」指 agent 侧的本地对账（brocade-agent/src/main.rs），在控制面返回
          // 204 时执行：机器配置发生偏移后由 agent 自行修复，该字段是其执行结果。
          [
            '状态巡检',
            node.last_local_reconcile ? (
              node.last_local_reconcile.actions.length > 0 ? (
                <>
                  <Ago at={new Date(node.last_local_reconcile.at * 1000).toISOString()} />
                  <span className="dim" title={node.last_local_reconcile.actions.join('、')}>
                    {' '}
                    · 重放了 {node.last_local_reconcile.actions.join('、')}
                  </span>
                </>
              ) : (
                <span className="dim">正常</span>
              )
            ) : (
              <span className="dim">—</span>
            ),
          ],
          [
            'IP 变更记录',
            <button
              type="button"
              className="nd-public-ip-metric"
              aria-expanded={publicIpHistoryOpen}
              aria-controls={`public-ip-history-${node.node_id}`}
              onClick={() => setPublicIpHistoryOpen(open => !open)}
            >
              {publicIpHistory.error ? (
                <span className="hot">读取失败</span>
              ) : publicIpChangeCount === null ? (
                <span className="dim">—</span>
              ) : (
                <>
                  {publicIpChangeCount}
                  <span className="nd-rt-u">条</span>
                </>
              )}
            </button>,
          ],
        ]}
      />
      {publicIpHistoryOpen && (
        <PublicIpHistoryDetails nodeId={node.node_id} history={publicIpHistory} lastSeen={publicIpLastSeen} />
      )}
    </Band>
  );
}

export function AppliedCard({
  node,
  revisionOf,
}: {
  node: NodeAgentStateItem;
  revisionOf: (d: number) => number | undefined;
}) {
  const a = appliedOf(node);
  const openvpn = openvpnExtensionStatus(node);
  const rev = a?.source_deployment_id != null ? revisionOf(a.source_deployment_id) : undefined;
  const meta = a ? (
    <>
      <Ago at={a.observed_at ?? null} /> 观察
    </>
  ) : (
    '—'
  );
  if (!a)
    return (
      <Band name="CONFIG" icon="artifacts" meta={meta}>
        <Cells
          items={[
            ['状态', <span className="note">还没收敛过。</span>, 'w3'],
            ['扩展应用', openvpn],
          ]}
        />
      </Band>
    );
  return (
    <Band name="CONFIG" icon="artifacts" meta={meta}>
      {/* 逐项状态合成一条状态带，不再逐件占一格：五件产物此前各占一格，而每格里只有一个
          三字符的状态词，一格的其余宽度是空的。状态带上每片自带产物名，见 StateChip。
          需要列全：它也是发布前判定待发布的依据，unknown 一律判定为需要操作。 */}
      <Cells
        items={[
          [
            '来自',
            <>
              {a.source_deployment_id != null ? `#${a.source_deployment_id}` : '—'}
              {rev !== undefined && <span className="nd-rt-q">{`修订 ${rev}`}</span>}
            </>,
          ],
          [
            '产物',
            <span className="nd-rt-chips">
              {ARTIFACT_KINDS.map(kind => (
                <StateChip key={kind} state={artifactState(node, kind)} label={ARTIFACT_LABEL[kind]} />
              ))}
              <StateChip state={a.grants?.state ?? 'unknown'} label="授权同步" />
            </span>,
            'w3',
          ],
          ['扩展应用', openvpn],
        ]}
      />
      {/* 此处原有 xray 和 wg 的 sha256，已移除：这条带表示的是收敛来源和各产物状态，
          而 sha256 用于与产物面板逐字节核对，二者用途不同；两个 64 位值还会占满一行。
          需要查看指纹时使用产物面板，那里有完整值。 */}
    </Band>
  );
}

/** 三条带合成一张整宽卡。
 *
 * 此前是并排的三张卡：卡宽约 380px，标签列占 72px，而值多为 4–12 个字符（约 90px），
 * 每行右侧约 210px 空置，24 行即整块的空白来源。并排还使三卡各自计算标签列宽
 * （`grid-template-columns: max-content …`），值列起点落在三个不同的 x；三卡行数
 * 8 / 9 / 7，等高拉伸后剩余空白全部堆在最短的那张下缘。
 *
 * 合并后字段改为标签在值上方的单元格、8 栏对齐，栏数随容器宽度递减（见 styles.css）。
 * 三个数据来源改由横带左侧的边栏区分，每条带自带时效读数。 */
function RuntimeCard({
  node,
  load,
  loadPending,
  agentStartedAt,
  revisionOf,
  wireguardEnabled,
  isolatedNodeIds,
  children,
}: {
  node: NodeAgentStateItem;
  load: NodeLoadView | undefined;
  loadPending: boolean;
  agentStartedAt: number | null;
  revisionOf: (d: number) => number | undefined;
  wireguardEnabled: boolean;
  isolatedNodeIds: ReadonlySet<string>;
  children?: ReactNode;
}) {
  return (
    <div className="panel nd-runtime">
      <header>
        <PanelTitle of="observe">运行状态</PanelTitle>
      </header>
      <AgentCard node={node} agentStartedAt={agentStartedAt} />
      {/* HOST 的数据来自负载上报（host facts 是其中的低频段），与节点状态是两个通道。 */}
      <HostCard load={load} pending={loadPending} />
      <AppliedCard node={node} revisionOf={revisionOf} />
      {/* 判定统一挂在卡底。此前 AGENT 卡按 chip 前缀筛一份、CONFIGURATIONS 卡筛
          `chip !== '自修失败'` 再来一份，两个集合相交——「丢了 …」这类同时命中两个条件，
          在同一屏上渲染两次。合并后不再需要筛选，各条判定只出现一次。
          放在条件分支外：从未收敛过的机器同样需要这些提示。 */}
      <Findings list={runtimeFindings(node, wireguardEnabled, isolatedNodeIds)} />
      {/* 验证结果仍留在卡内：页头的「验证接入」调用同一接口，不再产生另一块重复结果。 */}
      {children}
    </div>
  );
}

interface DirectedChainEdge {
  from: string;
  to: string;
}

interface NodeChainUse {
  appId: string;
  appLabel: string;
  chain: SnapshotChain;
  spine: string[];
  ingress: SnapshotIngress | null;
  steps: SnapshotStep[];
  ownStep: SnapshotStep | null;
  role: string;
  isForwardTarget: boolean;
}

function chainEdges(steps: SnapshotStep[]): DirectedChainEdge[] {
  const edges: DirectedChainEdge[] = [];

  // 边只来自显式规则。编译器不补全主干默认边——上游未写转发时，下游从链头不可达。
  for (const step of steps) {
    for (const rule of step.rules) {
      if (rule.a.t === 'forward' && rule.a.to) {
        edges.push({ from: step.node, to: rule.a.to });
      }
    }
  }

  return edges;
}

// 从该机器沿转发边（reverse 时逆向）可达的全部机器，不含自身。
//
// 只判断是否存在：该链是否出现在这台机器的详情页、它是否算中转，判定依据都是
// 该集合是否为空。此前它还包含跳数和边的类型，用于「所有上游 / 所有下游」两列标签；
// 该展示已移除（readonly 和 editor 现在使用同一棵规则树），跳数和类型不再有使用方——
// 规则树通过缩进表示层级，比「2 跳」这类标签更准确。
function reachable(args: { start: string; edges: DirectedChainEdge[]; reverse?: boolean }): Set<string> {
  const { start, edges, reverse = false } = args;
  const adjacency = new Map<string, string[]>();
  for (const edge of edges) {
    const from = reverse ? edge.to : edge.from;
    const to = reverse ? edge.from : edge.to;
    const list = adjacency.get(from) ?? [];
    list.push(to);
    adjacency.set(from, list);
  }

  /* 图中存在环是常态（分叉后汇合），因此入队前先记录 seen，否则遍历不会终止 */
  const seen = new Set<string>();
  const queue = [start];
  for (let i = 0; i < queue.length; i += 1) {
    for (const next of adjacency.get(queue[i]) ?? []) {
      if (next === start || seen.has(next)) continue;
      seen.add(next);
      queue.push(next);
    }
  }
  return seen;
}

function chainUseOf(args: {
  appId: string;
  appLabel: string;
  chain: SnapshotChain;
  /* 主干路径（由 api.ts 的 chainSpine 派生），只用于角色判定和排序 */
  spine: string[];
  ingress: SnapshotIngress | null;
  steps: SnapshotStep[];
  nodeId: string;
}): NodeChainUse | null {
  const { appId, appLabel, chain, spine, ingress, steps, nodeId } = args;
  const stepByNode = new Map(steps.map(step => [step.node, step]));
  const ownStep = stepByNode.get(nodeId) ?? null;
  const spineIndex = spine.indexOf(nodeId);
  const edges = chainEdges(steps);
  const hostsIngress = ingress?.node === nodeId;

  const upstreams = reachable({ start: nodeId, edges, reverse: true });
  const downstreams = reachable({ start: nodeId, edges });
  if (spineIndex < 0 && !ownStep && upstreams.size === 0 && !hostsIngress) return null;

  const roles: string[] = [];
  if (hostsIngress || spineIndex === 0) roles.push('入口节点');
  else if (spineIndex > 0 && spineIndex === spine.length - 1) roles.push('出口节点');
  else if (spineIndex > 0) roles.push('中转节点');
  if (ownStep && (downstreams.size > 0 || ownStep.rules.some(rule => rule.a.t === 'reuse_listener')) && spineIndex < 0)
    roles.push('中转节点');
  if (ownStep && roles.length === 0) roles.push('规则节点');

  return {
    appId,
    appLabel,
    chain,
    spine,
    ingress,
    steps,
    ownStep,
    role: roles.join(' / ') || '链内节点',
    isForwardTarget: isForwardTargetInChain({ nodeId, steps }),
  };
}

function NodeChainRuleTree({
  nodeId,
  use,
  nodes,
  readOnly = false,
  settingsReadable = true,
}: {
  nodeId: string;
  use: NodeChainUse;
  nodes: NodeAgentStateItem[];
  readOnly?: boolean;
  settingsReadable?: boolean;
}) {
  return (
    <div className="node-chain-use">
      <ChainRulesPanel
        appId={use.appId}
        chain={use.chain}
        spine={use.spine}
        steps={use.steps}
        nodes={nodes}
        selected={nodeId}
        showHeader={false}
        rootLabel={use.chain.name || use.chain.id}
        rootLabelTitle={`${use.appId} / ${use.chain.id}`}
        readOnly={readOnly}
        settingsReadable={settingsReadable}
        loadingFallback={null}
      />
    </div>
  );
}

function MachineEgressDnsRules({
  nodeId,
  nodeName,
  readOnly,
  showHeader,
  snapshotReady,
  snapshotError,
}: {
  nodeId: string;
  nodeName: string;
  readOnly: boolean;
  showHeader?: boolean;
  snapshotReady: boolean;
  snapshotError?: unknown;
}) {
  if (!snapshotReady) return <NodeRuleCardState kind="dns" error={snapshotError} />;
  return (
    <Suspense fallback={<NodeRuleCardState kind="dns" />}>
      <LazyMachineEgressDnsRules nodeId={nodeId} nodeName={nodeName} readOnly={readOnly} showHeader={showHeader} />
    </Suspense>
  );
}

function NodeChainsSection({
  id,
  inChains,
  nodes,
  canEdit,
  canCreate,
  settingsReadable,
  go,
  snapshotReady,
  snapshotError,
}: {
  id: string;
  inChains: NodeChainUse[];
  nodes: NodeAgentStateItem[];
  canEdit: boolean;
  canCreate: boolean;
  settingsReadable: boolean;
  go: (d: Drill) => void;
  snapshotReady: boolean;
  snapshotError?: unknown;
}) {
  if (!snapshotReady) return <NodeRuleCardState kind="chains" error={snapshotError} />;
  return (
    <Suspense fallback={<NodeRuleCardState kind="chains" />}>
      <header>
        <PanelTitle of="chains">链路规则</PanelTitle>
        <span className="rule-sheet-meta">{inChains.length} 条</span>
        <span className="sp" />
        <button className="btn" disabled={!canCreate} title="以当前机器作为入口" onClick={() => go({ p: 'chain', id })}>
          添加新链
        </button>
      </header>

      {inChains.length === 0 ? null : canEdit ? (
        // 一台机器可能属于多条链，每条链又递归展开多台——若每张规则表各带一个
        // 「保存到草稿」，本屏会出现七八个。统一收敛为整段末尾的一个。
        <RuleDraftScope hint="改动落进草稿，顶栏按「提交」才写进库。" guardScope={`node-tab:${id}:chains`}>
          {inChains.map(use => (
            <NodeChainRuleTree
              key={`${use.appId}/${use.chain.id}`}
              nodeId={id}
              use={use}
              nodes={nodes}
              settingsReadable={settingsReadable}
            />
          ))}
        </RuleDraftScope>
      ) : (
        // 只读角色看到的是同一棵树，与链详情页的结构一致（chains.tsx 的只读分支）。
        // 此处此前使用另一种呈现方式——主干路径加上下游两列标签——导致同一台机器的同一条链，
        // readonly 和 editor 看到的是两种形式，而它们描述的是同一份配置：该机器上写了
        // 哪些规则、转发到何处，这些只有规则树能表达，也正是进入本节需要查看的内容。
        // 禁用由 RuleEditor 的 readOnly 控制。
        //
        // 保存条（RuleDraftScope）只在可编辑时包裹：它是一个「保存到草稿」按钮，
        // 只读时始终处于禁用状态。
        <>
          {inChains.map(use => (
            <NodeChainRuleTree
              key={`${use.appId}/${use.chain.id}`}
              nodeId={id}
              use={use}
              nodes={nodes}
              readOnly
              settingsReadable={settingsReadable}
            />
          ))}
        </>
      )}
    </Suspense>
  );
}

// 「身份」卡中三个输入框使用统一宽度：名称、公网 IPv4、公网 IPv6。统一宽度是为了使
// 右侧的竖线和 NAT 开关位于同一垂直线上——按内容各自设置宽度时（v6 长于 v4、名称更短），
// 三行的右边缘会呈阶梯状，而该差异不表达任何信息。
// 完整的 IPv6 字面量宽于该宽度，此时输入框内横向滚动，不为极端情况扩展整列宽度。

const IDENT_FIELD = { width: 220 };

/** 详情页的三页。观测和配置是原来的两栏，规则原先是页面最下方的一块通栏。 */
type NodeTab = 'observed' | 'config' | 'chains';

function NodeDetailLayout({
  sheeted,
  node,
  lamp,
  toolbar,
  children,
}: {
  sheeted: boolean;
  node: NodeAgentStateItem;
  lamp: NodeLampState;
  toolbar: ReactNode;
  children: ReactNode;
}) {
  /* 标题下的第二行只放公网 IP：名称/地区由标题行与旗板承担，id 不在这里重复。 */
  const identIp = node.public_ipv4 ?? node.public_ipv6 ?? null;
  const pageHeader = (
    <header className="nd-page-head">
      <div className="nd-page-identity">
        {/* 区域旗装进带框圆角板，状态灯落在右下角，合成一个紧凑的 identity 对象。
            无区域（geoip 无国别）时回退为裸灯，底板没有内容就不画。 */}
        {node.public_ipv4_country ? (
          <span className="nd-idplate">
            {/* clip 层裁切铺满的旗；角灯放在 clip 外，探出板角不被裁。 */}
            <span className="nd-idplate-clip">
              <RegionFlag code={node.public_ipv4_country} square />
            </span>
            <i className={`node-lamp ${lamp.tone}`} title={lamp.why} aria-label={lamp.why} />
          </span>
        ) : (
          <span className="nd-status-slot">
            <i className={`node-lamp ${lamp.tone}`} title={lamp.why} aria-label={lamp.why} />
          </span>
        )}
        <div className="nd-ident-text">
          <div className="nd-ident-row">
            <h1 className={node.name ? 'nd-id nd-name' : 'nd-id'}>{node.name || node.node_id}</h1>
            {nodeLifecycleLabel(node) && (
              <span className={`st ${node.lifecycle_phase === 'abandoned' ? 'st-failed-dirty' : 'st-halted'}`}>
                {nodeLifecycleLabel(node)}
              </span>
            )}
            {node.operationally_isolated && <span className="st st-gold">已隔离</span>}
          </div>
          {identIp && <span className="nd-ident-meta">{identIp}</span>}
        </div>
      </div>
      {toolbar}
    </header>
  );

  return (
    <div className={sheeted ? 'nd-sheet nd-page nd-node-detail' : 'nd-page nd-node-detail'}>
      {sheeted ? (
        <div className="fg-sheet nd-paper">
          {pageHeader}
          <div className="nd-paper-body">{children}</div>
        </div>
      ) : (
        <>
          {pageHeader}
          {children}
        </>
      )}
    </div>
  );
}

function NodeDetailTabState({
  tab,
  tabId,
  panelId,
  error,
}: {
  tab: NodeTab;
  tabId: string;
  panelId: string;
  error?: unknown;
}) {
  return (
    <section
      id={panelId}
      className={`nd-tab-panel node-detail-tab-state node-detail-tab-state-${tab}`}
      role="tabpanel"
      aria-labelledby={tabId}
      aria-busy={!error}
      tabIndex={0}
    >
      {error ? <ErrorBox error={error} /> : <ObservationReading />}
    </section>
  );
}

function NodeRuleCardState({ kind, error }: { kind: 'dns' | 'chains'; error?: unknown }) {
  return (
    <>
      <header>
        <PanelTitle of={kind}>{kind === 'dns' ? 'DNS 解析策略' : '链路规则'}</PanelTitle>
        <span className="rule-sheet-meta">{error ? '加载失败' : '加载中'}</span>
      </header>
      <div className="node-rule-card-state">{error ? <ErrorBox error={error} /> : <PanelLoading />}</div>
    </>
  );
}

function NodeDetail({
  id,
  go,
  sheeted = false,
  initialTab = 'observed',
}: {
  id: string;
  go: (d: Drill) => void;
  sheeted?: boolean;
  initialTab?: NodeTab;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  // 详情页的状态灯同样依赖 last_poll_at。列表卸载后不再有它的 10 秒轮询；若这里仅命中
  // 缓存，负载查询触发重绘时会拿旧时间与当前时间比较，停留 60 秒后误报「失联」。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), refetchInterval: 10_000 });
  // 访客看不到发布记录，且详情页只在判断退役工单时需要它；不要发一个必然 403 的请求。
  const deployments = useQuery({
    queryKey: ['deployments'],
    queryFn: () => fetchDeployments(),
    enabled: !isVisitor(who),
  });
  const revisionOf = (d: number) => deployments.data?.deployments.find(x => x.id === d)?.revision_id;
  const n = nodes.data?.nodes.find(x => x.node_id === id);
  /* 反向隧道卡显示的是对端机器，名称与本页其他位置同源。 */
  const nodeNameOf = (other: string) => nodes.data?.nodes.find(x => x.node_id === other)?.name || other;
  /* 签发的 node token 同样只显示一次 */
  const [issued, setIssued] = useState<{ token: string; install_command: string } | null>(null);
  const [confirmIsolationFor, setConfirmIsolationFor] = useState<string | null>(null);

  // 身份表单始终可编辑，不再设置「改名称 / 公网 IP」开关。
  // 取消编辑模式不会导致误改：不保存则不生效，且「保存到草稿」只在有修改时出现。
  const [form, setForm] = useState<{
    name: string;
    public_ipv4: string;
    public_ipv6: string;
    public_ipv4_nat: boolean;
    public_ipv6_nat: boolean;
  } | null>(null);
  /* 基准取自草稿快照，理由见 useDraftNode：以 `/nodes/agent-state` 为基准时，
     「保存到草稿」之后 setForm(null) 会让五个输入框一齐回落到改动前的内容。
     快照里没有这台机器（尚未返回、或较旧的控制面）时回退到直连值。 */
  const draftNode = useDraftNode(id);
  const base = {
    name: draftNode?.name ?? n?.name ?? '',
    public_ipv4: (draftNode ? draftNode.public_ipv4 : n?.public_ipv4) ?? '',
    public_ipv6: (draftNode ? draftNode.public_ipv6 : n?.public_ipv6) ?? '',
    public_ipv4_nat: draftNode?.public_ipv4_nat ?? n?.public_ipv4_nat ?? false,
    public_ipv6_nat: draftNode?.public_ipv6_nat ?? n?.public_ipv6_nat ?? false,
  };
  const cur = form ?? base;
  const dirty =
    form !== null &&
    (form.name !== base.name ||
      form.public_ipv4 !== base.public_ipv4 ||
      form.public_ipv6 !== base.public_ipv6 ||
      form.public_ipv4_nat !== base.public_ipv4_nat ||
      form.public_ipv6_nat !== base.public_ipv6_nat);
  useUnsavedChanges(dirty, `${base.name || id} 的机器身份`, `node-route:${id}`);
  useUnsavedChanges(issued !== null, `${base.name || id} 的一次性 node token`, `node-route:${id}`);

  const refresh = () => qc.invalidateQueries({ queryKey: ['nodes'] });
  const save = useMutation({
    /* public_ipv4 与 hop_endpoint 遵循同一约定：空串表示清空，null 表示不修改该字段 */
    mutationFn: () =>
      updateNode(id, {
        name: cur.name.trim(),
        public_ipv4: cur.public_ipv4.trim(),
        public_ipv6: cur.public_ipv6.trim(),
        public_ipv4_nat: cur.public_ipv4_nat,
        public_ipv6_nat: cur.public_ipv6_nat,
      }),
    onSuccess: () => {
      setForm(null);
      refresh();
      qc.invalidateQueries({ queryKey: ['revisions'] });
    },
  });
  const issue = useMutation({
    mutationFn: () => issueNodeToken(id),
    onSuccess: r => {
      setIssued({ token: r.token, install_command: r.install_command });
      refresh();
    },
  });
  /* 该机器被哪些项目使用 */
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });

  const retire = useMutation({
    mutationFn: (status: 'active' | 'retired') => setNodeStatus(id, status),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['deployments'] });
    },
  });
  const abandon = useMutation({
    mutationFn: () => abandonNode(id, true),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['deployments'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
    },
  });
  const serviceRestore = useMutation({
    mutationFn: () => restoreNodeService(id),
    onSuccess: () => {
      refresh();
      qc.invalidateQueries({ queryKey: ['deployments'] });
    },
  });
  const isolate = useMutation({
    mutationFn: () => isolateNode(id, true),
    onSuccess: () => {
      setConfirmIsolationFor(null);
      refresh();
      qc.invalidateQueries({ queryKey: ['deployments'] });
    },
  });

  /* 验证接入：执行一次不做任何修改的空收敛，确认该机器是否已与当前模型一致 */
  const verify = useMutation({ mutationFn: () => verifyDeployment({ node_id: id }) });

  /* ── 页签角标的两个来源 ──
     分页之后另外两页不在屏幕上，「那边有事」只能由页签自己说出来，否则在配置页改地址时
     不会知道观测页刚出了 finding。两处都复用已有的查询键，不引入新的取数：
     `['node-load-history', id, 1h]` 与 LoadCardFor 的默认范围共用，为 HOST 和 Agent
     进程提供始终较新的低频事实。用户切到更长范围后，长历史另走带范围的缓存键，
     不会让 HOST 因为 24 小时查询而延迟刷新；
     跳的存活同理，与下面的 HopHealth 共用 useHopStats。 */
  const initialRange = DEFAULT_LOAD_RANGE;
  const load = useQuery({
    ...nodeLoadRangeQuery(id, initialRange),
    refetchInterval: 10_000,
  });
  /* 首屏观测和图表分包与基础详情并行。详情框架不再等待这三个查询或图表模块：机器清单
     已经足够画出真实页头与页签，各观测卡先在自己的最终位置显示统一的图表加载态，完成后各自
     替换。这样最慢的 Ping 或图表包不会把已经可用的机器身份一起藏起来。 */
  const initialUsage = useQuery({
    ...nodeUsageRangeQuery(id, initialRange),
  });
  const initialPing = useQuery({
    ...nodePingRangeQuery(id, initialRange),
  });
  const observationModules = useNodeObservationModules();
  const { dead } = useHopStats(id);

  /* 页签是一次浏览中的位置，不进地址栏。安装页可定向打开配置，但不改变既有详情 URL。
     换机器时使用新入口指定的位置，不继承上一台的阅读位置。 */
  const [tabState, setTabState] = useState<{ id: string; tab: NodeTab }>({
    id,
    tab: initialTab,
  });
  const tabIdBase = useId();
  const activeTab: NodeTab = tabState.id !== id ? initialTab : tabState.tab;
  const setTab = (next: NodeTab) => {
    if (next === activeTab) return true;
    if (!confirmDiscardChanges(`node-tab:${id}:${activeTab}`)) return false;
    cancelVisualTransition();
    // 页签按钮的选中态已经明确表达切换结果。内容直接替换，避免整个阅读区先变暗再恢复，
    // 同时也不捕获高度不同的表单和 ECharts 画布。
    setTabState({ id, tab: next });
    return true;
  };
  const tabs: NodeTab[] = ['observed', 'config', 'chains'];
  const tabId = (value: NodeTab) => `${tabIdBase}-${value}-tab`;
  const panelId = (value: NodeTab) => `${tabIdBase}-${value}-panel`;
  const moveTab = (event: ReactKeyboardEvent<HTMLButtonElement>, currentTab: NodeTab) => {
    const currentIndex = tabs.indexOf(currentTab);
    let nextIndex: number | null = null;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') nextIndex = (currentIndex + 1) % tabs.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp')
      nextIndex = (currentIndex - 1 + tabs.length) % tabs.length;
    if (event.key === 'Home') nextIndex = 0;
    if (event.key === 'End') nextIndex = tabs.length - 1;
    if (nextIndex === null) return;
    event.preventDefault();
    const next = tabs[nextIndex];
    if (!setTab(next)) return;
    window.requestAnimationFrame(() => document.getElementById(tabId(next))?.focus());
  };
  const [loadRangeState, setLoadRangeState] = useState<{ id: string; range: LoadRange }>({
    id,
    range: DEFAULT_LOAD_RANGE,
  });
  const [loadRangeTransition, setLoadRangeTransition] = useState<
    | { id: string; status: 'pending'; range: LoadRange }
    | { id: string; status: 'error'; range: LoadRange; error: unknown }
    | null
  >(null);
  const loadRangeTransitionVersion = useRef(0);
  useEffect(
    () => () => {
      // NodeDetail can be reused for another id. A response started by the previous machine must
      // never become the displayed range if it finishes after navigation.
      loadRangeTransitionVersion.current += 1;
    },
    [id],
  );

  /* 页头的稀有/危险操作（重签 token、退役下线）收进 ⋯ 菜单：它们的视觉权重原与
     使用频率成反比——最稀有的危险操作画着最抢眼的红框。菜单项可以带一行说明，
     按钮上放不下的解释（旧 token 立即失效、打标记不删行）落在菜单里。
     开合机制与顶栏更多菜单相同（shell.tsx）：document 级点击与 Escape 关闭。 */
  const [actsOpen, setActsOpen] = useState(false);
  useEffect(() => {
    if (!actsOpen) return;
    const close = () => setActsOpen(false);
    const esc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    document.addEventListener('click', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', esc);
    };
  }, [actsOpen]);

  const verifyAllowed = can(who.role, 'edit');
  const system = can(who.role, 'system');
  /* 访客（public）不显示页头操作：这些按钮全部触发写接口，访客没有一个可用。
     隐藏而非禁用与顶栏对访客的处理一致（shell.tsx 同样按 isPublic 移除入口）；
     「禁用而不隐藏」的约定针对登录后的角色，见 session.tsx。 */
  const pub = isVisitor(who);
  const onSaved = () => {
    refresh();
    qc.invalidateQueries({ queryKey: ['revisions'] });
  };

  const observationBusy =
    observationModules.status === 'pending' || load.isPending || initialUsage.isPending || initialPing.isPending;
  // A direct deep-link has no inventory row to name yet, so it keeps the neutral page fallback.
  // Navigation from the machine list already has this query and can reveal the real frame at once.
  if (nodes.isPending) return <Loading variant="detail" sheeted={sheeted} />;
  if (nodes.error) return <ErrorBox error={nodes.error} />;
  if (!n) return <ErrorBox error={new Error(`没有这台机器：${id}`)} />;

  const lifecycleOrder = n.lifecycle_deployment_id
    ? deployments.data?.deployments.find(deployment => deployment.id === n.lifecycle_deployment_id)
    : undefined;
  const teardownNeedsRepair =
    n.lifecycle_phase === 'retiring' &&
    (!n.lifecycle_deployment_id ||
      (deployments.isSuccess &&
        (!lifecycleOrder ||
          !lifecycleOrder.active ||
          !['planned', 'running', 'halted'].includes(lifecycleOrder.status))));

  // The snapshot is draft-aware while `/nodes/agent-state` is committed state. Use the former so
  // turning WG off removes the backend warning immediately, before the draft is committed and
  // before the agent's next half-hourly runtime report clears its old observation.
  const wireguardEnabled = snapshot.data?.snapshot.nodes?.find(modelNode => modelNode.id === id)?.overlay ?? n.overlay;
  const isolatedNodeIds = new Set(
    (nodes.data?.nodes ?? []).filter(node => node.operationally_isolated).map(node => node.node_id),
  );
  const wgListenPort =
    snapshot.data?.snapshot.nodes?.find(modelNode => modelNode.id === id)?.wireguard?.listen_port ?? null;

  // 该机器所属的链。不能只列出 chain id：分叉节点、显式规则边、主干默认边都会影响
  // accept/hop_in 是否保留。此处将关系拆分计算，下方的整链规则树使用同一份数据。
  const inChains = (snapshot.data?.snapshot.apps ?? []).flatMap(a =>
    (a.chains ?? [])
      .map(chain =>
        chainUseOf({
          appId: a.id,
          appLabel: a.label,
          chain,
          spine: chainSpine(a, chain.id),
          ingress: (a.ingresses ?? []).find(ingress => ingress.chain === chain.id) ?? null,
          steps: (a.steps ?? []).filter(step => step.chain === chain.id),
          nodeId: id,
        }),
      )
      .filter((use): use is NodeChainUse => use !== null),
  );
  const machineEgressPolicies = (snapshot.data?.node_egress_dns ?? []).filter(policy => policy.node === id);
  /* 反向标签里只有 app 和 chain 的 id，链名来自快照。app 段可缺省（见 parsePair），
     此时在全部项目里按 chain id 查找；查不到的链只显示 id，不留空。 */
  const chainNameOf = (appId: string | null, chainId: string) => {
    const apps = snapshot.data?.snapshot.apps ?? [];
    const app = appId ? apps.find(a => a.id === appId) : undefined;
    const chains = app ? (app.chains ?? []) : apps.flatMap(a => a.chains ?? []);
    return chains.find(chain => chain.id === chainId)?.name ?? null;
  };

  // 规则页同时承载机器 DNS 策略和链路规则，所以没有加入链路的机器也保留这一页；两块
  // 各自显示空状态，不能再把“无链路”等同于“没有规则页面”。
  const tab = activeTab;
  const loadRange = loadRangeState.id === id ? loadRangeState.range : DEFAULT_LOAD_RANGE;
  const activeLoadRangeTransition = loadRangeTransition?.id === id ? loadRangeTransition : null;
  const changeLoadRange = (range: LoadRange) => {
    const nextKey = loadRangeKey(range);
    if (activeLoadRangeTransition?.status === 'pending' && loadRangeKey(activeLoadRangeTransition.range) === nextKey)
      return;
    if (loadRangeKey(loadRange) === nextKey) {
      loadRangeTransitionVersion.current += 1;
      setLoadRangeTransition(null);
      return;
    }

    const version = ++loadRangeTransitionVersion.current;
    setLoadRangeTransition({ id, status: 'pending', range });
    void Promise.all([
      qc.fetchQuery(nodeLoadRangeQuery(id, range)),
      qc.fetchQuery(nodeUsageRangeQuery(id, range)),
      qc.fetchQuery(nodePingRangeQuery(id, range)),
    ]).then(
      () => {
        if (loadRangeTransitionVersion.current !== version) return;
        // All three keys now have data. The existing panels stay mounted until this single state
        // update, then read the prepared cache together instead of exposing request completion order.
        setLoadRangeState({ id, range });
        setLoadRangeTransition(null);
      },
      error => {
        if (loadRangeTransitionVersion.current !== version) return;
        setLoadRangeTransition({ id, status: 'error', range, error });
      },
    );
  };
  /* 同一观测页里的图表始终共享时间位置与 Tooltip，不再把页面级一致行为做成用户开关。 */
  const chartsLinked = true;

  /* 观测页的角标数。此处不按 `detailOnly` 过滤：那个标记的含义是「列表里不占标记位，
     进详情页才读」，而这里就是详情页——角标指向的正是它下面那几张卡里会展开的说明。
     跳不通与 finding 合计成一个数：两者在这一页上是同一件事，「有几处要看」。 */
  const findingCount = runtimeFindings(n, wireguardEnabled, isolatedNodeIds).length + dead.length;
  const lamp = nodeLampState(n, wireguardEnabled, isolatedNodeIds);

  /* A1 页头：身份、页签与操作共用 sheet 内的一条横梁，当前页由满宽底部刻度标记。 */
  const detailToolbar = (
    <>
      <div className="nd-tabs" role="tablist" aria-label={`${n.name || id} 详情页签`}>
        <div className="nd-tabs-seg">
          <button
            type="button"
            id={tabId('observed')}
            role="tab"
            aria-selected={tab === 'observed'}
            aria-controls={panelId('observed')}
            tabIndex={tab === 'observed' ? 0 : -1}
            onKeyDown={event => moveTab(event, 'observed')}
            onClick={() => setTab('observed')}
          >
            <Icon of="observe" size={14} className="nd-tab-ic" />
            观测
            {/* 金色数字＝有几处要看。零时不画，一个常驻的「0」会被当成一种状态。 */}
            {findingCount > 0 && <span className="nd-tab-badge gold">{findingCount}</span>}
          </button>
          <button
            type="button"
            id={tabId('config')}
            role="tab"
            aria-selected={tab === 'config'}
            aria-controls={panelId('config')}
            tabIndex={tab === 'config' ? 0 : -1}
            onKeyDown={event => moveTab(event, 'config')}
            onClick={() => setTab('config')}
          >
            <Icon of="config" size={14} className="nd-tab-ic" />
            配置
            {/* 圆点＝身份表单有未保存的改动，与身份面板的保存条使用同一个判定。 */}
            {dirty && <span className="nd-tab-badge dot" />}
          </button>
          <button
            type="button"
            id={tabId('chains')}
            role="tab"
            aria-selected={tab === 'chains'}
            aria-controls={panelId('chains')}
            tabIndex={tab === 'chains' ? 0 : -1}
            onKeyDown={event => moveTab(event, 'chains')}
            onClick={() => setTab('chains')}
          >
            <Icon of="chains" size={14} className="nd-tab-ic" />
            规则
            {(inChains.length > 0 || machineEgressPolicies.length > 0) && (
              <span className="nd-tab-badge dim">{inChains.length + machineEgressPolicies.length}</span>
            )}
          </button>
        </div>
      </div>
      {(tab === 'observed' || !pub) && (
        <div className="nd-tools">
          {tab === 'observed' && (
            <ObserveRangeControl
              value={loadRange}
              pending={activeLoadRangeTransition?.status === 'pending'}
              onChange={changeLoadRange}
            />
          )}
          {!pub && (
            <div className="nd-acts">
              <div className="fg-menuwrap">
                {/* stopPropagation 是必须的：document 级关闭监听在 effect 里绑定，
                而打开菜单的这次点击绑定发生后仍会冒泡到 document——不拦下，
                菜单开同一瞬间又被自己关掉（shell.tsx 的两个菜单同样拦）。 */}
                <button
                  type="button"
                  className="btn fg-more"
                  aria-label="更多操作"
                  aria-haspopup="menu"
                  aria-expanded={actsOpen}
                  onClick={e => {
                    e.stopPropagation();
                    setActsOpen(v => !v);
                  }}
                >
                  <Icon of="more" size={14} className="nd-tool-icon" />
                </button>
                {actsOpen && (
                  <div className="fg-menu action-menu" role="menu" onClick={() => setActsOpen(false)}>
                    {tab === 'observed' && (
                      <button
                        role="menuitem"
                        disabled={!verifyAllowed || verify.isPending}
                        onClick={() => verify.mutate()}
                      >
                        <Icon of="check" size={14} className="action-menu-icon" />
                        <span>
                          {verify.isPending ? '验证中…' : '验证接入'}
                          <small>空跑一次收敛，确认这台与当前模型一致，不改任何配置</small>
                        </span>
                      </button>
                    )}
                    <button
                      role="menuitem"
                      disabled={!system || issue.isPending || n.lifecycle_phase !== 'active'}
                      onClick={() => issue.mutate()}
                    >
                      <Icon of="access" size={14} className="action-menu-icon" />
                      <span>
                        {n.token_prefix && !n.token_revoked_at ? '重签 token' : '签发 token'}
                        <small>凭据只显示一次；重签后旧 token 立即失效</small>
                      </span>
                    </button>
                    <hr />
                    {!n.operationally_isolated && n.lifecycle_phase === 'active' && (
                      <button
                        role="menuitem"
                        className="dg"
                        disabled={!system || isolate.isPending}
                        onClick={() => setConfirmIsolationFor(id)}
                      >
                        <Icon of="warn" size={14} className="action-menu-icon" />
                        <span>
                          隔离机器
                          <small>立即停止承载流量；进行中的发布转为隔离待补偿</small>
                        </span>
                      </button>
                    )}
                    <button
                      role="menuitem"
                      className={n.lifecycle_phase === 'active' ? 'dg' : undefined}
                      disabled={!system || retire.isPending}
                      onClick={() => retire.mutate(n.lifecycle_phase === 'active' ? 'retired' : 'active')}
                    >
                      <Icon
                        of={n.lifecycle_phase === 'active' ? 'dash' : 'check'}
                        size={14}
                        className="action-menu-icon"
                      />
                      <span>
                        {retire.isPending ? '提交中…' : n.lifecycle_phase === 'active' ? '退役机器' : '恢复机器'}
                        <small>
                          {n.lifecycle_phase === 'active'
                            ? '立即创建完整停用发布；不会删除机器记录'
                            : '递增生命周期代次并创建恢复发布；需要重新签发 Token'}
                        </small>
                      </span>
                    </button>
                    {n.lifecycle_phase === 'retiring' && (
                      <>
                        {teardownNeedsRepair && (
                          <button
                            role="menuitem"
                            disabled={!system || retire.isPending}
                            onClick={() => retire.mutate('retired')}
                          >
                            <Icon of="deploy" size={14} className="action-menu-icon" />
                            <span>
                              重建停用发布
                              <small>当前没有可继续执行的停用单；沿用本次退役代次重新规划</small>
                            </span>
                          </button>
                        )}
                        <button
                          role="menuitem"
                          className="dg"
                          disabled={!system || abandon.isPending}
                          onClick={() => abandon.mutate()}
                        >
                          <Icon of="warn" size={14} className="action-menu-icon" />
                          <span>
                            {abandon.isPending ? '处理中…' : '强制退役'}
                            <small>仅用于机器永久失联；结果会保留为警告状态</small>
                          </span>
                        </button>
                      </>
                    )}
                    {n.lifecycle_deployment_id && (
                      <button role="menuitem" onClick={() => navigate('deploy')}>
                        <Icon of="deploy" size={14} className="action-menu-icon" />
                        <span>
                          查看发布 #{n.lifecycle_deployment_id}
                          <small>打开发布页查看停用动作与波次确认</small>
                        </span>
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </>
  );

  return (
    <NodeDetailLayout sheeted={sheeted} node={n} lamp={lamp} toolbar={detailToolbar}>
      {n.operationally_isolated && (
        <div className="callout warn node-isolation-banner">
          <div className="node-isolation-copy">
            <b>
              已隔离
              {n.isolated_at && (
                <>
                  {'\u202f'}
                  <Ago at={n.isolated_at} withPastSuffix={false} />
                </>
              )}
              ，
            </b>
            已停止承载流量；
            {n.convergence_debt_count > 0
              ? `${pollTone(n) === 'ok' ? '' : '重新上线并'}同步剩余 ${n.convergence_debt_count} 项配置后`
              : n.service_reentry_ready
                ? '当前已满足恢复条件'
                : `${pollTone(n) === 'ok' ? '状态恢复' : '重新上线'}后`}
            ，可手动恢复服务。
          </div>
          <div className="toolbar">
            <button
              className="btn primary"
              disabled={!system || !n.service_reentry_ready || serviceRestore.isPending}
              onClick={() => serviceRestore.mutate()}
            >
              恢复服务
            </button>
          </div>
        </div>
      )}
      {issued && (
        <div className="callout warn">
          <b>{id}</b> 的 node token —— 只显示这一次：
          <div className="mono" style={{ overflowWrap: 'anywhere', color: 'var(--gold)', margin: '8px 0' }}>
            {issued.token}
          </div>
          {/* 重签会立即使机器上的旧 token 失效，因此此处直接提供可将新 token 写入机器的命令，
              无需手动传输。安装脚本支持 --node-token，跳过纳管流程只更新凭据。 */}
          <p className="note" style={{ marginBottom: 4 }}>
            旧 token 已失效。在这台机器上执行：
          </p>
          <div
            className="mono"
            style={{ overflowWrap: 'anywhere', fontSize: 11, background: 'var(--sunk)', padding: 8, borderRadius: 6 }}
          >
            {issued.install_command}
          </div>
          <div className="toolbar">
            <CopyButton className="btn" text={issued.token} label="复制 token" />
            <CopyButton className="btn primary" text={issued.install_command} label="复制命令" />
            <span className="sp" />
            <button className="btn" onClick={() => setIssued(null)}>
              我抄好了
            </button>
          </div>
        </div>
      )}

      {(issue.error || verify.error || retire.error || abandon.error || isolate.error || serviceRestore.error) && (
        <ErrorBox
          error={issue.error ?? verify.error ?? retire.error ?? abandon.error ?? isolate.error ?? serviceRestore.error}
        />
      )}

      {!pub && deployments.error && <ErrorBox error={deployments.error} />}
      {tab === 'observed' && snapshot.error && <ErrorBox error={snapshot.error} />}
      {tab === 'observed' && observationModules.status === 'error' && <ErrorBox error={observationModules.error} />}

      {tab === 'observed' && (
        /* LOAD 是监控页的第一视图；Ping 紧随流量曲线，三张机器状态卡顺延到下一块。 */
        <section
          id={panelId('observed')}
          className="nd-tab-observed nd-tab-panel"
          role="tabpanel"
          aria-labelledby={tabId('observed')}
          aria-busy={observationBusy || activeLoadRangeTransition?.status === 'pending'}
          tabIndex={0}
        >
          {activeLoadRangeTransition?.status === 'error' && (
            <div className="callout err nd-range-error" role="alert">
              <span>
                {activeLoadRangeTransition.range.menuLabel}读取失败：
                {activeLoadRangeTransition.error instanceof Error
                  ? activeLoadRangeTransition.error.message
                  : String(activeLoadRangeTransition.error)}
              </span>
              <button className="btn" type="button" onClick={() => changeLoadRange(activeLoadRangeTransition.range)}>
                重试
              </button>
            </div>
          )}
          {observationModules.status === 'ready' ? (
            <LoadCardFor
              nodeId={id}
              range={loadRange}
              linked={chartsLinked}
              observationModules={observationModules.modules}
            />
          ) : (
            <ObservationKpisState state={observationModules.status === 'error' ? 'error' : 'pending'} />
          )}
          {/* 吞吐（网卡 + XRAY）与 Ping（ICMP + TCP）分别同卡堆叠，两栏并排。 */}
          <div className="nd-observe-throughput">
            {observationModules.status === 'ready' ? (
              <>
                <ThroughputPanel
                  nodeId={id}
                  range={loadRange}
                  linked={chartsLinked}
                  observationModules={observationModules.modules}
                />
                <PingProbePanel
                  nodeId={id}
                  range={loadRange}
                  linked={chartsLinked}
                  observationModules={observationModules.modules}
                />
              </>
            ) : (
              <>
                <ThroughputPanelState state={observationModules.status === 'error' ? 'error' : 'pending'} />
                <PingProbePanelState state={observationModules.status === 'error' ? 'error' : 'pending'} />
              </>
            )}
          </div>
          {/* Mux 与反向隧道来自同一条节点实时流，只建立一个 EventSource。两张卡都只在对应
              快照存在时渲染；计数基线留在浏览器内，不进入遥测历史或数据库。 */}
          <NodeRealtimeProvider nodeId={id}>
            <VpngateObservationCard nodeId={id} />
            <MuxObservationCard nodeId={id} nodeName={nodeNameOf} chainName={chainNameOf} />
            <ReverseHealthCard nodeId={id} nodeName={nodeNameOf} chainName={chainNameOf} />
          </NodeRealtimeProvider>
          {/* AGENT → HOST → CONFIG 是一组状态事实，合成一张整宽卡，见 RuntimeCard。 */}
          <RuntimeCard
            node={n}
            load={load.data}
            loadPending={load.isPending}
            agentStartedAt={load.data?.processes.find(p => p.proc === 'agent')?.started_at_unix_secs ?? null}
            revisionOf={revisionOf}
            wireguardEnabled={wireguardEnabled}
            isolatedNodeIds={isolatedNodeIds}
          >
            {verify.data && (
              <div className={verify.data.converged ? 'callout blue' : 'callout warn'} style={{ marginBottom: 0 }}>
                {verify.data.converged
                  ? `已收敛：跟修订 ${verify.data.revision_id} 一致，没有要改的。`
                  : `还没对齐修订 ${verify.data.revision_id}：有变更待推（${
                      verify.data.targets.find(t => t.node_id === id)?.actions.join('、') ?? '—'
                    }）。到「发布 → 计划预览」创建 deployment 才会推下去。`}
              </div>
            )}
          </RuntimeCard>
        </section>
      )}

      {/* ── 写入控制面模型的配置。 ──
            两列而不是铺满：表单行是一条标签栏加 220px 输入，整幅宽度只会让标签和值之间
            空出一大片。分栏依据是「改这一项影响谁」：
              左栏 —— 只影响这台机器自己（它叫什么、地址是什么、如何解析、wg0 怎么起）；
              右栏 —— 影响它与外部的关系，以及它与机队默认值的差异。
            右栏末尾的「本机覆盖」把设置页里按机器分列的两段（连接策略、日志保留）收在
            这台机器名下：那两段在设置页是一张机队表，逐台调一项要先在表里找到这一行。 */}
      {tab === 'config' && !snapshot.data && (
        <NodeDetailTabState tab="config" tabId={tabId('config')} panelId={panelId('config')} error={snapshot.error} />
      )}
      {tab === 'config' && snapshot.data && (
        <section
          id={panelId('config')}
          className="nd-tab-config nd-tab-panel"
          role="tabpanel"
          aria-labelledby={tabId('config')}
          tabIndex={0}
        >
          <div>
            <div className="panel config-panel">
              <header>
                <PanelTitle of="identity">身份</PanelTitle>
              </header>
              <div className="fgrid one">
                <Row k="名称">
                  <input
                    className="f"
                    style={IDENT_FIELD}
                    value={cur.name}
                    disabled={!system}
                    onChange={e => setForm({ ...cur, name: e.target.value })}
                  />
                </Row>
                {/* NAT 独占一行，位于其对应的 IP 之上。行标签已标明是哪条 IP 的 NAT，
                  开关不再附带文字。两条 NAT 的说明相同，只写一次——重复显示不增加信息，
                  只增加行数。 */}
                <Row k="IPv4 NAT">
                  <SegSwitch
                    checked={cur.public_ipv4_nat}
                    disabled={!system}
                    onChange={checked => setForm({ ...cur, public_ipv4_nat: checked })}
                    off="直连"
                    on="经 NAT"
                  />
                  <span className="sub">经 NAT 的地址不会被当作可直连的落点。</span>
                </Row>
                <Row k="公网 IPv4">
                  <input
                    className="f"
                    style={IDENT_FIELD}
                    value={cur.public_ipv4}
                    disabled={!system}
                    onChange={e => setForm({ ...cur, public_ipv4: e.target.value })}
                  />
                  <EgressMirror configured={n.public_ipv4} observed={n.route_ipv4} nat={n.public_ipv4_nat} />
                </Row>
                <Row k="IPv6 NAT">
                  <SegSwitch
                    checked={cur.public_ipv6_nat}
                    disabled={!system}
                    onChange={checked => setForm({ ...cur, public_ipv6_nat: checked })}
                    off="直连"
                    on="经 NAT"
                  />
                </Row>
                <Row k="公网 IPv6">
                  <input
                    className="f"
                    style={IDENT_FIELD}
                    value={cur.public_ipv6}
                    disabled={!system}
                    onChange={e => setForm({ ...cur, public_ipv6: e.target.value })}
                  />
                  <EgressMirror configured={n.public_ipv6} observed={n.route_ipv6} nat={n.public_ipv6_nat} />
                </Row>
                {/* 位于地址之后：上面几行表示外部如何访问该机器，该行表示流量能否从该机器出网——
                  两者都属于该机器的对外属性，但方向相反。 */}
                <EgressRow node={n} canEdit={system} onSaved={onSaved} />
              </div>
              {dirty && (
                <>
                  {save.error && <ErrorBox error={save.error} />}
                  <div className="toolbar">
                    <span className="sp" />
                    <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
                      {save.isPending ? '保存中…' : '保存到草稿'}
                    </button>
                    <button className="btn" disabled={save.isPending} onClick={() => setForm(null)}>
                      还原
                    </button>
                  </div>
                </>
              )}
            </div>

            <DnsCard node={n} canEdit={system} onSaved={onSaved} />
            <WgCard
              node={n}
              listenPort={wgListenPort}
              peers={nodes.data?.nodes ?? []}
              disabledLinks={snapshot.data?.snapshot.settings?.overlay.disabled_links ?? []}
              enabled={wireguardEnabled}
              canEdit={system}
              onSaved={onSaved}
            />
          </div>

          <div>
            {/* 证书组决定这台机器出示的 SNI，与身份里的公网地址一样是它的对外属性；
                与它们不同的是改动不经过发布，所以卡里自己就保存了。 */}
            <CertGroupCard node={n} canEdit={system} />
            <ConnectionCard node={n} canEdit={system} onSaved={onSaved} />
            <TrafficAccountingCard node={n} canEdit={system} />
            <LogRetentionCard node={n} canEdit={system} />
          </div>
        </section>
      )}

      {tab === 'chains' && (
        <section
          id={panelId('chains')}
          className="nd-tab-rules nd-tab-panel"
          role="tabpanel"
          aria-labelledby={tabId('chains')}
          tabIndex={0}
        >
          <div className="panel config-panel rule-sheet-card node-egress-rule-sheet">
            <MachineEgressDnsRules
              key={id}
              nodeId={id}
              nodeName={n.name || id}
              readOnly={!can(who.role, 'edit')}
              showHeader
              snapshotReady={!!snapshot.data}
              snapshotError={snapshot.error}
            />
          </div>
          <div className="panel config-panel rule-sheet-card node-chain-sheet">
            <NodeChainsSection
              id={id}
              inChains={inChains}
              nodes={nodes.data?.nodes ?? []}
              canEdit={can(who.role, 'edit')}
              canCreate={can(who.role, 'edit')}
              settingsReadable={!isVisitor(who)}
              go={go}
              snapshotReady={!!snapshot.data}
              snapshotError={snapshot.error}
            />
          </div>
        </section>
      )}
      {confirmIsolationFor === id && (
        <Confirm
          title={`隔离 ${n.name || id}`}
          body={
            <p>
              这台机器会立即从服务视图摘除，不再承载新流量。若发布已经下发，现场状态会标记为未知，并在恢复服务前重新收敛完整期望。
            </p>
          }
          confirmLabel="确认隔离"
          confirmDisabled={isolate.isPending}
          onConfirm={() => isolate.mutate()}
          onCancel={() => setConfirmIsolationFor(null)}
        />
      )}
    </NodeDetailLayout>
  );
}

// ══ 纳管向导 ══
//
// 纳管与建链共用详情页纸面和配置卡。机器尚未入库时填写一张表单；创建成功后
// 切到安装与上线页。页头的三段进度只负责交代整条流程和当前阶段，不拆分表单、也不能
// 点击跳过：第一阶段提交会立即写库，后两阶段依赖一次性 token 和 Agent 心跳。
//
// 此前分为五步，其中第 2、3 屏显示 store 补全字段和编译诊断。它们只是创建回显，
// 既不是后续操作，也会迫使用户确认一次“完成”；创建成功后现直接进入安装与上线步骤。
//
// 与建链向导的主要差异：建链向导写入的全部是草稿，提交前可随时丢弃；此处的按钮
// 点击后立即写库并产生一版修订，没有草稿也无法撤销。因此主按钮不使用「提交」，
// 页脚的提示文字也不是装饰性内容。

function Provision({ drill, go }: { drill: WizDrill; go: (d: Drill) => void }) {
  // 存在 node 表示机器已入库，本屏切换为安装与上线页；
  // result 是创建时的响应，从地址栏返回时不存在。
  const node = drill.p === 'install' ? drill.node : null;
  const result = drill.p === 'install' ? drill.result : undefined;
  if (node) return <ProvisionInstall node={node} result={result} go={go} />;
  return <ProvisionForm go={go} />;
}

/* 第一种状态：机器尚未入库。 */
export function ProvisionForm({ go }: { go: (d: Drill) => void }) {
  const qc = useQueryClient();
  /* GET /tenants 在服务端已按操作者的租户子树过滤，因此该列表即为其可见范围。 */
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const options = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  /* 已有的机器：用于判断 id 是否被占用。 */
  const existingNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  /* 证书组的下拉选项。纳管是 system-admin 操作，其有权读取；失败不能伪装成“没有证书组”。 */
  const certs = useQuery({ queryKey: ['certs'], queryFn: () => fetchCerts(), retry: false });

  const [form, setForm] = useState(() => ({ ...PROVISION_FORM_DEFAULTS }));
  const [attempted, setAttempted] = useState(false);
  const existingIds = new Set((existingNodes.data?.nodes ?? []).map(node => node.node_id));
  const fieldErrors = provisionFormErrors(form, existingIds);
  const formError = firstProvisionError(fieldErrors);
  const defaultCertLabelId = (certs.data?.groups ?? []).find(group => group.is_default)?.id ?? '';
  const selectedCertLabelId = form.cert_label_id === '__none__' ? '' : form.cert_label_id || defaultCertLabelId;

  // 单租户阶段不显示归属选择，并且只按数量判断：恰好一条才可纳管，不识别任何特殊名称。
  const defaultTenant = options.length === 1 ? options[0].id : '';
  const tenantId = defaultTenant;
  const tenantError =
    options.length === 0
      ? '当前账号没有可用于纳管机器的租户'
      : options.length > 1
        ? '当前流程要求恰好一个可见租户，请先收窄账号范围'
        : null;

  const guardScope = 'node-provision';
  const dirty = JSON.stringify(form) !== JSON.stringify(PROVISION_FORM_DEFAULTS);
  const clearUnsavedChanges = useUnsavedChanges(dirty, '纳管机器表单', guardScope);
  const provision = useMutation({
    mutationFn: () => {
      const dns: Dns =
        form.dns.trim() === '' || form.dns.trim() === 'system'
          ? { t: 'system' }
          : {
              t: 'servers',
              v: form.dns
                .split(',')
                .map(s => s.trim())
                .filter(Boolean),
            };
      return provisionNode({
        id: form.id.trim(),
        tenant_id: tenantId,
        name: form.name.trim() || form.id.trim(),
        public_ipv4: form.public_ipv4.trim() || null,
        public_ipv6: form.public_ipv6.trim() || null,
        // 没有地址时 NAT 没有含义。界面会禁用对应开关，此处仍做一次归一化，避免
        // 浏览器恢复旧表单状态或将来新增调用路径后写入“空地址 + NAT”。
        public_ipv4_nat: !!form.public_ipv4.trim() && form.public_ipv4_nat,
        public_ipv6_nat: !!form.public_ipv6.trim() && form.public_ipv6_nat,
        wg_listen_port: Number(form.wg_listen_port),
        api_port: form.api_port.trim() ? Number(form.api_port) : null,
        overlay: form.overlay,
        egress_allowed: form.egress_allowed,
        dns,
        domain_strategy: form.domain_strategy,
        cert_label_id: selectedCertLabelId || null,
      });
    },
    onSuccess: result => {
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      // 该操作完成后机器已入库，直接进入唯一仍需处理的安装页。明文 token 只能在这次
      // 响应中取得，不能为了“返回详情”而丢掉；页面不再展示创建字段或修订摘要。
      clearUnsavedChanges();
      go({ p: 'install', node: result.node.id, step: 2, result });
    },
  });

  // 租户决定写入归属，机器列表用于防止覆盖既有 ID，证书列表决定新机器的证书关联。
  // 任一依赖未知时都不展示一张看似完整、实际采用危险默认值的表单。
  if (tenants.isPending || existingNodes.isPending || certs.isPending) return <Loading variant="form" />;
  if (tenants.error || existingNodes.error || certs.error) {
    return <ErrorBox error={tenants.error ?? existingNodes.error ?? certs.error} />;
  }

  const provisionBlocker = formError ?? tenantError;
  const ready = !provisionBlocker && !!tenantId;

  return (
    <WizardPaper
      onSubmit={e => {
        e.preventDefault();
        setAttempted(true);
        if (!ready || provision.isPending) return;
        provision.mutate();
      }}
    >
      <WizardPaperHeader
        icon="nodes"
        title="纳管机器"
        meta={[form.id.trim() || '机器 ID 未填写', form.name.trim()].filter(Boolean).join(' · ')}
        stages={[
          { label: '登记配置', state: 'current' },
          { label: '安装 Agent', state: 'next' },
          { label: '上线发布', state: 'next' },
        ]}
        aside={<span className="st st-gold">立即写库</span>}
      />
      <div className="nd-paper-body pv-body">
        <fieldset className="pv-fields" disabled={provision.isPending}>
          <div className="nd-tab-config">
            <div>
              <WizardCard title="身份" icon="identity">
                <div className="fgrid one">
                  <WizardField label="机器 ID" htmlFor="provision-node-id">
                    <input
                      id="provision-node-id"
                      className="f mono"
                      value={form.id}
                      placeholder="hk-01"
                      autoFocus
                      autoComplete="off"
                      autoCapitalize="none"
                      spellCheck={false}
                      aria-required="true"
                      aria-invalid={!!fieldErrors.id && (attempted || !!form.id)}
                      aria-describedby="provision-node-id-note"
                      onChange={e => setForm({ ...form, id: e.target.value })}
                    />
                    <span
                      className={fieldErrors.id && (attempted || !!form.id) ? 'sub bad' : 'sub'}
                      id="provision-node-id-note"
                    >
                      {fieldErrors.id && (attempted || !!form.id)
                        ? fieldErrors.id + '。'
                        : '唯一键，创建后不可修改。可用字符：a-z 0-9 . _ -'}
                    </span>
                  </WizardField>
                  <WizardField label="名称" htmlFor="provision-node-name">
                    <input
                      id="provision-node-name"
                      aria-label="机器名称"
                      className="f"
                      value={form.name}
                      placeholder="香港入口"
                      autoComplete="off"
                      onChange={e => setForm({ ...form, name: e.target.value })}
                    />
                    <span className="sub">留空时使用机器 ID，创建后可在配置页修改。</span>
                  </WizardField>
                  {(['public_ipv4', 'public_ipv6'] as const).map(key => {
                    const ipv4 = key === 'public_ipv4';
                    const natKey = ipv4 ? 'public_ipv4_nat' : 'public_ipv6_nat';
                    const inputId = ipv4 ? 'provision-public-ipv4' : 'provision-public-ipv6';
                    const label = ipv4 ? '公网 IPv4' : '公网 IPv6';
                    return (
                      <WizardField key={key} label={label} htmlFor={inputId}>
                        <div className="nd-ctl-line">
                          <input
                            id={inputId}
                            className="f mono"
                            value={form[key]}
                            placeholder={ipv4 ? '203.0.113.10 或主机名' : '2001:db8::10'}
                            autoComplete="off"
                            autoCapitalize="none"
                            spellCheck={false}
                            aria-invalid={!!fieldErrors[key]}
                            aria-describedby={inputId + '-note'}
                            onChange={e =>
                              setForm({
                                ...form,
                                [key]: e.target.value,
                                [natKey]: !!e.target.value.trim() && form[natKey],
                              })
                            }
                          />
                          <SegSwitch
                            checked={form[natKey]}
                            disabled={!form[key].trim()}
                            onChange={checked => setForm({ ...form, [natKey]: checked })}
                            off="直连"
                            on="经 NAT"
                            ariaLabel={label + ' 可达方式'}
                          />
                        </div>
                        <span className={fieldErrors[key] ? 'sub bad' : 'sub'} id={inputId + '-note'}>
                          {fieldErrors[key]
                            ? fieldErrors[key] + '。'
                            : !form[key].trim()
                              ? '留空时由首次心跳自动识别。'
                              : form[natKey]
                                ? '经 NAT 的地址不会被当作可直连的落点。'
                                : '手填地址不会被自动识别结果覆盖。'}
                        </span>
                      </WizardField>
                    );
                  })}
                  <WizardField label="出网权限">
                    <SegSwitch
                      checked={form.egress_allowed}
                      onChange={checked => setForm({ ...form, egress_allowed: checked })}
                      off="禁止出网"
                      on="可出网"
                      ariaLabel="机器出网权限"
                    />
                    <span className="sub">
                      {form.egress_allowed
                        ? '允许链最终从这台机器访问互联网。'
                        : '只作为中转节点，指向它的本机出网规则会在编译时被拒绝。'}
                    </span>
                  </WizardField>
                </div>
              </WizardCard>
            </div>
            <div>
              <WizardCard title="WIREGUARD" icon="tunnels">
                <div className="fgrid one">
                  <WizardField label="WireGuard">
                    <SegSwitch
                      checked={form.overlay}
                      onChange={checked => setForm({ ...form, overlay: checked })}
                      off="关闭"
                      on="启用"
                      ariaLabel="WireGuard overlay"
                    />
                    <span className="sub">
                      {form.overlay
                        ? '分配 overlay 地址，与其他成员全互联。'
                        : '不加入 overlay；仍可使用公网地址中转。'}
                    </span>
                  </WizardField>
                  <WizardField label="监听端口" htmlFor="provision-wg-port">
                    <input
                      id="provision-wg-port"
                      aria-label="WireGuard 端口"
                      className="f mono pv-port"
                      value={form.wg_listen_port}
                      inputMode="numeric"
                      aria-invalid={!!fieldErrors.wg_listen_port}
                      aria-describedby="provision-wg-port-note"
                      onChange={e => setForm({ ...form, wg_listen_port: e.target.value })}
                    />
                    <span className={fieldErrors.wg_listen_port ? 'sub bad' : 'sub'} id="provision-wg-port-note">
                      {fieldErrors.wg_listen_port
                        ? fieldErrors.wg_listen_port + '。'
                        : form.overlay
                          ? '对端连入的 UDP 端口。'
                          : '启用 WireGuard 时使用此端口。'}
                    </span>
                  </WizardField>
                </div>
              </WizardCard>
              <WizardCard title="证书组" icon="certificate">
                <div className="fgrid one">
                  <WizardField label="所属组" htmlFor="provision-cert-group">
                    <select
                      id="provision-cert-group"
                      aria-label="证书组"
                      className="f"
                      value={selectedCertLabelId || '__none__'}
                      onChange={e => setForm({ ...form, cert_label_id: e.target.value })}
                    >
                      <option value="__none__">不关联证书</option>
                      {(certs.data?.groups ?? []).map(g => (
                        <option key={g.id} value={g.id}>
                          {g.name}
                          {g.is_default ? '（默认）' : ''} · {g.names[1] ?? g.names[0]}
                        </option>
                      ))}
                    </select>
                    <span className={selectedCertLabelId ? 'sub' : 'sub warn'}>
                      {selectedCertLabelId
                        ? '与组内其他机器出示相同证书。组内换证书不改 SNI，已发出的订阅继续可用。'
                        : '不关联证书组：TLS 与 Hysteria 2 接入面会在编译时被拒绝。REALITY 指向外部站点不受影响。'}
                    </span>
                  </WizardField>
                </div>
              </WizardCard>
              <WizardCard
                title="DNS"
                icon="dns"
                summary={
                  <>
                    {form.dns.trim() || 'system'} · {DOMAIN_STRATEGY_LABEL[form.domain_strategy]}
                  </>
                }
                invalid={!!fieldErrors.dns}
              >
                <div className="fgrid one">
                  <WizardField label="服务器" htmlFor="provision-dns">
                    <input
                      id="provision-dns"
                      aria-label="DNS"
                      className="f mono"
                      value={form.dns}
                      placeholder="system 或 1.1.1.1,8.8.8.8"
                      autoComplete="off"
                      autoCapitalize="none"
                      spellCheck={false}
                      aria-invalid={!!fieldErrors.dns}
                      aria-describedby="provision-dns-note"
                      onChange={e => setForm({ ...form, dns: e.target.value })}
                    />
                    <span className={fieldErrors.dns ? 'sub bad' : 'sub'} id="provision-dns-note">
                      {fieldErrors.dns
                        ? fieldErrors.dns + '。'
                        : '留空或填 system = 跟随系统解析；多个地址用逗号分隔。'}
                    </span>
                  </WizardField>
                  <WizardField label="域名解析" htmlFor="provision-domain-strategy">
                    <select
                      id="provision-domain-strategy"
                      className="f"
                      value={form.domain_strategy}
                      onChange={e => setForm({ ...form, domain_strategy: e.target.value as DomainStrategy })}
                    >
                      {DOMAIN_STRATEGIES.map(s => (
                        <option key={s.v} value={s.v}>
                          {DOMAIN_STRATEGY_LABEL[s.v]}
                        </option>
                      ))}
                    </select>
                    <span
                      className={['use_ipv4', 'use_ipv6', 'as_is'].includes(form.domain_strategy) ? 'sub warn' : 'sub'}
                    >
                      {strategyNote(form.domain_strategy, parseDns(form.dns))}
                    </span>
                  </WizardField>
                </div>
              </WizardCard>
              <WizardCard
                title="XRAY"
                icon="xray"
                summary={form.api_port.trim() ? '管理端口 ' + form.api_port.trim() : '管理接口关闭'}
                invalid={!!fieldErrors.api_port}
              >
                <div className="fgrid one">
                  <WizardField label="管理端口" htmlFor="provision-api-port">
                    <input
                      id="provision-api-port"
                      aria-label="Xray 管理端口"
                      className="f mono pv-port"
                      value={form.api_port}
                      inputMode="numeric"
                      aria-invalid={!!fieldErrors.api_port}
                      aria-describedby="provision-api-port-note"
                      onChange={e => setForm({ ...form, api_port: e.target.value })}
                    />
                    <span className={fieldErrors.api_port ? 'sub bad' : 'sub'} id="provision-api-port-note">
                      {fieldErrors.api_port ? fieldErrors.api_port + '。' : '留空表示不开启管理接口。'}
                    </span>
                  </WizardField>
                </div>
              </WizardCard>
            </div>
          </div>
        </fieldset>
        {tenants.data && tenants.data.tenants.length !== 1 && (
          <div className="callout warn">
            系统归属配置异常：当前必须恰好有一条，实际为 {tenants.data.tenants.length} 条。
          </div>
        )}
        {provision.error && <ErrorBox error={provision.error} />}
      </div>
      <WizardFooter
        id="provision-submit-note"
        tone={provision.isPending ? 'busy' : ready ? 'ready' : 'idle'}
        title={provision.isPending ? '纳管中…' : provisionBlocker ? '还不能纳管' : '准备就绪'}
        description={
          provision.isPending
            ? '正在创建机器记录与修订。'
            : provisionBlocker || '立即创建机器与修订，并生成一次性安装命令；撤回需通过机器退役流程保留审计。'
        }
      >
        <button type="button" className="btn" disabled={provision.isPending} onClick={() => returnTo('nodes')}>
          取消
        </button>
        <button
          className="btn primary"
          type="submit"
          disabled={!ready || provision.isPending}
          aria-describedby="provision-submit-note"
          aria-busy={provision.isPending}
        >
          {provision.isPending ? '纳管中…' : '纳管这台机器'}
        </button>
      </WizardFooter>
    </WizardPaper>
  );
}

// 第二种状态：机器已入库，但尚未上线。
//
// 页面只保留后续操作：在机器上执行安装命令、等待首次心跳、确认证书。创建字段、修订号
// 和编译诊断不在成功后重复展示；当前配置与诊断分别由机器详情和全局诊断入口承载。
//
// 明文 token 只存在于创建时的响应中：服务端只保存 hash 和前缀
// （brocade-store/src/agent.rs），无法再次获取。因此命令区分两种情况——
// 从创建流程直接进入时显示当前仍有效的命令；从地址栏返回或兑换失败时提供重签按钮。
// 重签会让旧 token 立即失效，因此对刚兑换的 token 留出启动宽限，并在其余场景要求确认。
export function ProvisionInstall({
  node,
  result,
  go,
}: {
  node: string;
  result?: ProvisionNodeResult;
  go: (d: Drill) => void;
}) {
  const { who } = useSession();
  const system = can(who.role, 'system');
  const qc = useQueryClient();
  // 上线判定：token 被 install.sh 使用 → agent 启动 → 首次 desired 心跳。
  // agent 每 15s 拉取一次 desired 并更新 last_poll_at，脚本执行完成到首次心跳的
  // 端到端时间为 3~20 秒，3s 轮询间隔足够。查询键与其他位置相同，共享缓存。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), refetchInterval: 3_000 });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const [reissued, setReissued] = useState<{ token: string; install_command: string; token_prefix: string } | null>(
    null,
  );
  const [confirmIssue, setConfirmIssue] = useState(false);
  const [enableOpenvpn, setEnableOpenvpn] = useState(false);
  const issue = useMutation({
    mutationFn: () => issueNodeToken(node),
    onSuccess: r => {
      setReissued({ token: r.token, install_command: r.install_command, token_prefix: r.token_prefix });
      setConfirmIssue(false);
      qc.invalidateQueries({ queryKey: ['nodes'] });
    },
    onError: () => setConfirmIssue(false),
  });

  const command = reissued?.install_command ?? result?.enrollment?.install_command;
  const prefix = reissued?.token_prefix ?? result?.enrollment?.token_prefix;
  const nodeRow = (nodes.data?.nodes ?? []).find(x => x.node_id === node);
  const nodeLabel = nodeRow?.name || node;

  const probe = useAgentLiveness(nodeRow);
  const online = probe?.state === 'online';
  const redeemed = probe?.state === 'polling' || online;
  const now = useNow();
  const tokenUsedAt = nodeRow?.token_last_used_at
    ? Date.parse(
        nodeRow.token_last_used_at.endsWith('Z') || nodeRow.token_last_used_at.includes('+')
          ? nodeRow.token_last_used_at
          : `${nodeRow.token_last_used_at}Z`,
      )
    : NaN;
  const enrollmentExpiresAt = result?.enrollment?.expires_at
    ? Date.parse(
        result.enrollment.expires_at.endsWith('Z') || result.enrollment.expires_at.includes('+')
          ? result.enrollment.expires_at
          : `${result.enrollment.expires_at}Z`,
      )
    : NaN;
  const enrollmentExpired = !reissued && !Number.isNaN(enrollmentExpiresAt) && now >= enrollmentExpiresAt;
  // token 刚兑换时，重签会使正在启动的 Agent 立刻失去凭据，因此先留出四轮拉取周期。
  // 超时后必须恢复自救入口；否则安装进程在兑换 token 后失败会让节点永久卡在本页。
  const enrollmentStalled =
    probe?.state === 'polling' && !probe.onceOnline && (Number.isNaN(tokenUsedAt) || now - tokenUsedAt > 60_000);
  const canReissue =
    !online && (probe?.state === 'waiting' || (probe?.state === 'polling' && (probe.onceOnline || enrollmentStalled)));
  // 创建或重签响应里的命令只在当前 token 尚未兑换时有效。节点缓存尚未反映刚签发的
  // token 时仍先展示响应里的命令；缓存切到相同前缀且记录兑换后便立即隐藏，避免复制失效命令。
  const nodeReflectsReissuedToken = !!reissued && nodeRow?.token_prefix === reissued.token_prefix;
  const installCommand =
    command && !enrollmentExpired && (!redeemed || (!!reissued && !nodeReflectsReissuedToken)) ? command : undefined;
  // 仅附加安装器参数，不修改 enrollment token 或登记配置；sudo 只保留 token 环境变量。
  const commandToCopy = installCommand ? installCommand + (enableOpenvpn ? ' --enable-openvpn' : '') : '';
  useUnsavedChanges(installCommand !== undefined, `${nodeLabel} 的一次性安装命令`, `node-route:install:${node}`);

  // 证书与本屏的另外两步不同：它不经过下发流程，也不需要等待机器上线——控制面签发后入库，
  // agent 每十分钟一轮自行获取。放在此处是因为**缺少证书时 TLS / Hysteria 2 接入面无法编译**
  // （`ingress.tls-no-certificate`），而该错误只在建链时出现，那时已离开本屏。
  // 纳管阶段确认证书状态是唯一及时的时机。
  const certs = useQuery({
    queryKey: ['certs'],
    queryFn: () => fetchCerts(),
    // 创建机器需要 system-admin 权限，因此此处通常可以获取。读取失败时安装步骤仍可继续，
    // 但证书状态必须明确标为未知，不能伪装成“没有证书域”。
    enabled: system,
    // 通过 CA 签发一轮约半分钟。轮询到该机器所属的组签出证书后停止；机器没有选组时不必轮询，
    // 那不是等得到的状态，要人去选。
    refetchInterval: query => {
      const view = query.state.data;
      if (!view?.domain) return false;
      const row = (view.nodes ?? []).find(r => r.node_id === node);
      if (!row) return false;
      const group = (view.groups ?? []).find(g => g.id === row.label_id);
      return group?.certificates.some(c => c.status === 'serving') ? false : 5_000;
    },
  });
  const certDomain = certs.data?.domain ?? null;
  const certRow = (certs.data?.nodes ?? []).find(r => r.node_id === node);
  const certGroup = (certs.data?.groups ?? []).find(g => g.id === certRow?.label_id);
  // 机器出示的是这个组正在服务的那张。组内换证书只换它，SNI 不变。
  const certServing = certGroup?.certificates.find(c => c.status === 'serving');
  const certFailed = certGroup?.certificates.find(c => c.status === 'failed');

  const target = result?.revision_id ?? current;
  if (nodes.isPending || revisions.isPending) return <Loading variant="form" />;
  if (nodes.error || revisions.error) return <ErrorBox error={nodes.error ?? revisions.error} />;
  return (
    <WizardPaper>
      <WizardPaperHeader
        icon="nodes"
        title={nodeLabel}
        meta={node + ' · 修订 R' + (target ?? '…')}
        created
        online={online}
        stages={[
          { label: '登记配置', state: 'done' },
          { label: '安装 Agent', state: redeemed ? 'done' : 'current' },
          { label: '上线发布', state: redeemed ? 'current' : 'next' },
        ]}
        aside={
          <span className={online ? 'st st-succeeded' : 'st'}>
            {online ? '机器已上线' : redeemed ? '等待心跳' : '等待安装'}
          </span>
        }
      />
      <div className="nd-paper-body pv-body">
        <WizardCard
          title="安装 AGENT"
          icon="agent"
          className="pv-install"
          hint={
            <span className={online ? 'st st-succeeded' : 'st st-gold'}>
              {online
                ? '已装好'
                : installCommand
                  ? 'token 只显示这一次'
                  : enrollmentExpired
                    ? '命令已过期'
                    : redeemed
                      ? enrollmentStalled
                        ? '启动超时'
                        : 'token 已兑换'
                      : '命令没接住'}
            </span>
          }
        >
          <div className="fgrid one">
            <WizardField label="OpenVPN 扩展">
              {online ? (
                <span className="st" title={nodeRow?.runtime_versions?.openvpn || undefined}>
                  {nodeRow?.runtime_versions == null
                    ? '等待 Agent 上报'
                    : nodeRow.runtime_versions.openvpn?.trim()
                      ? '已安装'
                      : '未安装'}
                </span>
              ) : (
                <SegSwitch
                  checked={enableOpenvpn}
                  disabled={!installCommand || issue.isPending}
                  onChange={setEnableOpenvpn}
                  off="不安装"
                  on="安装"
                  ariaLabel="是否安装 OpenVPN 扩展"
                />
              )}
              <span className="sub">
                {online
                  ? '安装状态来自 Agent 上报，不以安装选项推断。'
                  : '可选。安装 OpenVPN 与 iptables，供 VPN Gate 按需使用；不改变登记配置。'}
              </span>
            </WizardField>
          </div>
          {installCommand && !online ? (
            <div className="pv-cmd">
              <div className="pv-cmd-bar">
                在目标机器以系统权限执行
                <CopyButton className="btn sm" text={commandToCopy} label="复制命令" />
              </div>
              <pre className="code" aria-label="安装命令">
                {installCommand.split(/(\s+)/).map((part, index) =>
                  /\s/.test(part) ? (
                    part
                  ) : (
                    <span className="pv-cmd-word" key={index}>
                      {part}
                    </span>
                  ),
                )}
                {enableOpenvpn && (
                  <>
                    {' '}
                    <span className="pv-flag">--enable-openvpn</span>
                  </>
                )}
              </pre>
              <div className="pv-cmd-meta">
                <span>
                  token 前缀 <b>{prefix}…</b>
                </span>
                <span>
                  {!reissued && result?.enrollment?.expires_at
                    ? '到期 ' + result.enrollment.expires_at
                    : '一次性使用，不过期'}
                  。兑换后立即失效。
                </span>
              </div>
            </div>
          ) : (
            <div className="pv-card-note">
              <span>
                {online
                  ? 'Agent 已使用自己的机器凭据持续连接，无需再次执行安装命令或重签 token。'
                  : enrollmentExpired
                    ? '一次性 enrollment token 已过期且从未兑换。重新生成命令不会影响已安装的 Agent。'
                    : probe?.state === 'polling'
                      ? probe.onceOnline
                        ? '这台机器曾上线、当前已离线。先检查机器和网络；确需重装时再重新生成命令。'
                        : enrollmentStalled
                          ? 'token 已兑换，但超过 60 秒仍没有心跳。检查安装日志；确需重装时可重新生成命令。'
                          : 'token 已兑换，Agent 正在启动。通常会在 15 秒内拉取配置，请先等待心跳。'
                      : '明文 token 只在创建时返回一次，服务端只保存 hash。机器已在库中，重新生成即可得到一条新命令。'}
              </span>
              {canReissue && (
                <button
                  className="btn sm"
                  type="button"
                  disabled={!system || issue.isPending}
                  onClick={() => (nodeRow?.token_prefix ? setConfirmIssue(true) : issue.mutate())}
                >
                  {issue.isPending ? '签发中…' : '重新生成命令'}
                </button>
              )}
            </div>
          )}
        </WizardCard>
        <div className="nd-tab-config pv-pair">
          <WizardCard title="上线状态" icon="observe" hint="每 3 秒刷新">
            <ol className="pv-checks" aria-live="polite">
              <li>
                <span
                  className={redeemed ? 'pv-mark done' : enrollmentExpired ? 'pv-mark warn' : 'pv-mark wait'}
                  aria-hidden="true"
                >
                  {redeemed ? '✓' : enrollmentExpired ? '!' : ''}
                </span>
                <span className="pv-check-copy">
                  <b>token 兑换</b>
                  <span>
                    {redeemed
                      ? '安装器已兑换机器凭据。'
                      : enrollmentExpired
                        ? 'token 已过期，请重新生成命令。'
                        : 'install.sh 尚未使用这枚 token 纳管。'}
                  </span>
                </span>
                <span className="pv-check-time">{redeemed ? '已兑换' : '等待兑换'}</span>
              </li>
              <li>
                <span
                  className={
                    online
                      ? 'pv-mark done'
                      : enrollmentStalled || (probe?.state === 'polling' && probe.onceOnline)
                        ? 'pv-mark warn'
                        : redeemed
                          ? 'pv-mark wait'
                          : 'pv-mark'
                  }
                  aria-hidden="true"
                >
                  {online ? '✓' : enrollmentStalled ? '!' : ''}
                </span>
                <span className="pv-check-copy">
                  <b>首次心跳</b>
                  <span>
                    {online
                      ? 'Agent 已连接控制台，可以预览并发布配置。'
                      : probe?.state === 'polling'
                        ? probe.onceOnline
                          ? '曾经上线，当前心跳中断；请检查机器和网络。'
                          : enrollmentStalled
                            ? 'Agent 未按时上线，请检查安装日志。'
                            : 'Agent 启动后会拉取配置，请等待心跳。'
                        : '执行安装命令后自动检测。'}
                  </span>
                </span>
                <span className="pv-check-time">{online ? probe.agoSec + ' 秒前' : '等待心跳'}</span>
              </li>
            </ol>
          </WizardCard>
          {system && (
            <WizardCard title="证书组" icon="certificate">
              <div className="fgrid one">
                <WizardField label="状态">
                  <span className={certServing ? 'st st-succeeded' : 'st'}>
                    {certs.isPending
                      ? '读取中'
                      : certs.error
                        ? '证书状态读取失败'
                        : !certDomain
                          ? '没有证书域'
                          : !certRow
                            ? '未选证书组'
                            : certServing
                              ? '已签发'
                              : certFailed
                                ? '签发失败'
                                : '签发中'}
                  </span>
                  {!certs.isPending && !certs.error && (
                    <span className="sub">
                      {!certDomain
                        ? '前往设置配置证书域。缺少证书时 TLS 与 Hysteria 2 接入面无法编译，REALITY 指向外部站点不受影响。'
                        : !certRow
                          ? '这台机器没有选证书组。请在机器配置中关联；REALITY 指向外部站点不受影响。'
                          : certServing
                            ? '签发者 ' + (certServing.issuer ?? '未知')
                            : certFailed
                              ? (certFailed.last_error ?? '上一轮签发失败，请检查证书设置。')
                              : '组 ' + certRow.group_name + ' 已排入签发队列，约半分钟。'}
                    </span>
                  )}
                </WizardField>
                {certRow && (
                  <WizardField label="所属组">
                    {certRow.group_name}
                    <span className="sub mono">{certRow.certificate_name}</span>
                  </WizardField>
                )}
              </div>
              {certs.error && <ErrorBox error={certs.error} />}
              {!certs.isPending && (
                <div className="pv-card-note">
                  {certs.error ? (
                    <button className="btn sm" type="button" onClick={() => void certs.refetch()}>
                      重试读取
                    </button>
                  ) : !certDomain || certFailed ? (
                    <button className="btn sm" type="button" onClick={() => navigate('settings')}>
                      打开证书设置
                    </button>
                  ) : !certRow ? (
                    <button className="btn sm" type="button" onClick={() => go({ p: 'node', id: node, tab: 'config' })}>
                      打开机器配置
                    </button>
                  ) : null}
                </div>
              )}
            </WizardCard>
          )}
        </div>
        {issue.error && <ErrorBox error={issue.error} />}
      </div>
      <WizardFooter
        tone={online ? 'ok' : enrollmentExpired || enrollmentStalled ? 'warn' : redeemed ? 'busy' : 'idle'}
        title={
          online
            ? '机器已就绪'
            : enrollmentExpired
              ? '安装命令已过期'
              : enrollmentStalled || (probe?.state === 'polling' && probe.onceOnline)
                ? 'Agent 需要检查'
                : redeemed
                  ? '正在等待 Agent'
                  : '等待执行安装命令'
        }
        description={
          online
            ? '进入发布计划前仍可先查看机器详情；未发布的配置不会下发。'
            : enrollmentExpired
              ? '请重新生成一条安装命令，再到目标机器执行。'
              : redeemed
                ? enrollmentStalled || (probe?.state === 'polling' && probe.onceOnline)
                  ? '先检查机器网络与安装日志；需要重装时再重新生成一次性命令。'
                  : '本页每 3 秒刷新一次，无需手动重载。'
                : '请在目标机器以系统权限执行上方完整命令。'
        }
      >
        <button type="button" className="btn" onClick={() => returnTo('nodes')}>
          回机器列表
        </button>
        <button type="button" className="btn" onClick={() => go({ p: 'node', id: node })}>
          看{nodeLabel}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={target == null || !online}
          title={online ? '' : '等 agent 上线后再发布'}
          onClick={() => navigate('deploy', { p: 'plan', revision: target })}
        >
          {online ? '去发布 · 计划预览（修订 ' + (target ?? '…') + '）' : '等 agent 上线…'}
        </button>
      </WizardFooter>
      {confirmIssue && (
        <Confirm
          title="重新生成安装命令"
          body={
            <p>
              这台机器已经持有 Agent 凭据。继续会立即让旧 token 失效；必须在机器上执行新命令后，Agent 才能重新连接。
            </p>
          }
          confirmLabel="使旧 token 失效并生成"
          confirmDisabled={issue.isPending}
          onConfirm={() => issue.mutate()}
          onCancel={() => setConfirmIssue(false)}
        />
      )}
    </WizardPaper>
  );
}
