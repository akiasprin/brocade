import { Fragment, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import * as echarts from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { GridComponent, TooltipComponent, MarkLineComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  abandonNode,
  fetchArtifactContentView,
  fetchCompileView,
  fetchDeployments,
  fetchLinkHealth,
  fetchLinkMtu,
  fetchNodes,
  fetchRevisions,
  fetchTenants,
  fetchUsageNodeSeries,
  fetchNodeLoad,
  fetchCerts,
  fetchNodeLoadList,
  fetchNodePingProbe,
  fetchNodePingProbeList,
  monthBytes,
  issueNodeToken,
  provisionNode,
  setNodeCertGroup,
  setNodeStatus,
  updateNode,
  verifyDeployment,
  type Dns,
  type NodeConnection,
  type DomainStrategy,
  type LinkHealthItem,
  type NodeAgentStateItem,
  type NodeLoadView,
  type NodePingProbeView,
  type LoadSample,
  type ProvisionNodeResult,
  type UsageNodeSeries,
  type UsageNodeBucket,
} from '../api';
import { fetchPreviewStatus } from '../preview/api';
import type { LinkIr } from '../topo/model';
import { PreviewProvision, type PreviewWizDrill } from '../preview/provision';
import { can, isPublic, useSession } from '../session';
import { Ago, Confirm, Empty, ErrorBox, Loading, SegSwitch } from '../ui/bits';
import { Icon, ListIcon } from '../ui/icons';
import { bytes } from '../ui/format';
import { copyText } from '../ui/platform';
import { useNarrow } from '../ui/viewport';
import { useNow } from '../ui/clock';
import { useAgentLiveness } from '../ui/agent-alive';
import { pingLatencyMs, pingLatencyText, pingSampleText } from '../ui/ping-probe';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import { openTabByKey } from '../ui/topbar';
import { RegionFlag } from '../ui/region-flag';
import { navigate } from '../forge/route';
import {
  OBSERVE_MS_UNIT,
  OBSERVE_SERIES_COLOR_VARS,
  observeAreaStyle,
  observeAxisLine,
  observeAxisTick,
  observeColors,
  observeMinorTick,
  observeMsUnit,
  observeSeriesLine,
  observeTimeInterval,
  observeValueAxis,
} from '../ui/observe-chart';
import { LoadCard, bps, ThroughputChart, dur, iso, throughputAxis } from './telemetry';
import { theme } from '../forge/theme';
import { palette } from '../forge/palette';
import { ChainWizard } from './chain-wizard';
import { ChainRulesPanel, IngressPortEditor } from './chains';
import { MachineEgressDnsRules, RuleDraftScope, isForwardTargetInChain } from './rules';
import { chainSpine, fetchSnapshot, type SnapshotChain, type SnapshotIngress, type SnapshotStep } from '../api';
import type { AppIr } from '../topo/model';

// 纳管分两个阶段，中间是一次不可逆的写库：
// `provision` 是填写信息（此时机器尚未创建），`install` 是为已创建的机器安装 agent。
// 因此 install 的标识是 node_id——它会进入地址栏，切换后返回仍可定位；
// `result` 只是创建流程返回的一次性响应，重新进入后不再存在（见 ProvisionResult）。
export type Drill =
  | { p: 'list' }
  | { p: 'node'; id: string }
  | { p: 'provision'; step: number }
  | { p: 'install'; node: string; step: number; result?: ProvisionNodeResult }
  | { p: 'chain'; id: string };

/* 向导的两个阶段共用同一套步骤条，此处收敛组件签名 */
export type WizDrill = Extract<Drill, { p: 'provision' } | { p: 'install' }>;

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
  const go = (d: Drill) => wm.setData(win.id, { ...win.data, drill: d });
  // 面包屑用机器名。nodes 查询在列表页已拉取，通常命中缓存；名称缺失或未加载时回退到 id。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const nameOf = (id: string) => nodes.data?.nodes.find(n => n.node_id === id)?.name || id;
  useCrumb(win, crumbOf(drill, nameOf));

  if (drill.p === 'list') return <NodeList go={go} sheeted={bare} />;

  const body =
    drill.p === 'provision' || drill.p === 'install' ? (
      <ProvisionGate drill={drill} go={go} />
    ) : drill.p === 'chain' ? (
      <ChainStep id={drill.id} go={go} />
    ) : (
      <NodeDetail id={drill.id} go={go} sheeted={bare} />
    );

  return bare && drill.p === 'node' ? body : bare ? <div className="fg-sheet">{body}</div> : body;
}

function ProvisionGate({ drill, go }: { drill: WizDrill; go: (d: Drill) => void }) {
  const preview = useQuery({
    queryKey: ['preview-status'],
    queryFn: fetchPreviewStatus,
    retry: false,
  });
  if (preview.isPending) return <Loading />;
  if (preview.error) return <ErrorBox error={preview.error} />;
  if (preview.data.enabled) {
    return <PreviewProvision drill={drill as PreviewWizDrill} go={go} status={preview.data} />;
  }
  return <Provision drill={drill} go={go} />;
}

/* 向导需要该机器的完整信息（租户、名称），从 nodes 列表中获取 */
function ChainStep({ id, go }: { id: string; go: (d: Drill) => void }) {
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const n = nodes.data?.nodes.find(x => x.node_id === id);
  if (nodes.isPending) return <Loading />;
  if (!n) return <ErrorBox error={new Error(`没有这台机器：${id}`)} />;
  // 标题与项目页的入口一致（chains.tsx 的 NewChain）：同一个向导的两个入口应使用相同
  // 的外框，副标题中改为说明从哪台机器进入。创建后不跳转到发布页——向导的四个步骤
  // 全部写入草稿（`draft.push`），未提交则没有修订，发布页只会显示「已收敛」，
  // 跳转到该页会与实际操作不符。后续操作是顶栏草稿条上的「提交」。
  return (
    <>
      <div className="chain-hd">
        <b>建链向导</b>
        <span className="subid mono">
          {n.name || n.node_id} / {n.node_id}
        </span>
      </div>
      <ChainWizard node={n} onDone={() => go({ p: 'node', id })} />
    </>
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
   无法查明——端口跳转刚上线时即是如此，库中记录为 unknown，界面上没有任何显示。 */
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

function StateChip({ state }: { state: string }) {
  return (
    <span className={`st ${STATE_CLASS[state] ?? ''}`} title={STATE_TITLE[state] ?? state}>
      {STATE_SHORT[state] ?? state}
    </span>
  );
}

/** 状态使用缩写。四件产物加授权同步共五格，三字的词会导致这一行换行。
 *  正常与异常已由 STATE_CLASS 的颜色区分，文字只需区分具体状态；全称写在 title 中。
 *
 *  ON 与 OFF 成对，说的是同一件事的两个值：这件产物在这台机器上开着还是关着。
 *  此处曾写作 OK，而 OK 的反义是失败，与旁边的 OFF 不构成一组，读起来像两套词混用。 */
const STATE_SHORT: Record<string, string> = {
  present: 'ON',
  disabled: 'OFF',
  unmanaged: 'N/A',
  unknown: '?',
  dirty: 'DIFF',
};

const STATE_TITLE: Record<string, string> = {
  present: '已应用',
  disabled: '已关闭',
  unmanaged: '未纳管',
  unknown: '未知',
  dirty: '已变化',
};

/* ── 字段的两种排列方式。选用依据见 styles.css，此处只提供结构。 ── */

/* ⑥ 内联条：字段数量少、值较短、彼此平级 */
/** 一个字段格。第三个元素写 'newline' 表示从新行开始——`.fstrip` 是 flex-wrap 布局，
    插入一个占满整行、零高度的空元素后，后续元素自然换行。
    该方式比为某个格设置固定宽度可靠：格宽取决于其中的值，而值在运行时才确定
    （「重放了 …」可能很长，「正常」只有两个字），按最宽值排布会使常态下该行留出大片空白。
    使用字面量而非 boolean，是为了在调用处能直接看出该参数的含义。 */
type StripItem = [string, ReactNode] | [string, ReactNode, 'newline'];

function Strip({ items }: { items: StripItem[] }) {
  return (
    <div className="fstrip">
      {items.map(([k, v, brk]) => (
        <Fragment key={k}>
          {brk === 'newline' && <span className="fstrip-brk" />}
          <span className="p">
            <span className="k">{k}</span>
            <span className="v">{v}</span>
          </span>
        </Fragment>
      ))}
    </div>
  );
}

function IpValue({ value, nat }: { value: string | null; nat: boolean }) {
  if (!value) return <span className="dim">—</span>;
  return (
    <>
      {value}
      {nat && ' (NAT)'}
    </>
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
  });
  // 卡片底部的本月计费用量。曲线已改用 NIC 速率，这份 usage 数据只负责累计值。
  // 与节点列表分开查询：请求失败时列表仍可使用，数值显示为空。
  const usage = useQuery({
    queryKey: ['usage-node-series', LIST_USAGE_WINDOW_SECS],
    queryFn: () => fetchUsageNodeSeries(LIST_USAGE_WINDOW_SECS),
    refetchInterval: 30_000,
  });
  // 各机器的近期负载。列表用它绘制 NIC 曲线，并决定状态灯的颜色与 title。
  // 与上面两个查询一样独立：该端点不可用，或控制面版本尚不支持该端点时，
  // 列表正常显示，状态点回退为只反映活性。
  const load = useQuery({
    queryKey: ['node-load-list'],
    queryFn: () => fetchNodeLoadList(LIST_NIC_WINDOWS),
    refetchInterval: 30_000,
    retry: false,
  });
  const loadOf = useMemo(() => new Map((load.data?.nodes ?? []).map(n => [n.node_id, n])), [load.data]);
  const pingProbe = useQuery({
    queryKey: ['ping-probe-nodes', 3600],
    queryFn: () => fetchNodePingProbeList(3600),
    refetchInterval: 5_000,
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
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [retiredOpen, setRetiredOpen] = useState<boolean | null>(null);
  const togglePick = (id: string) =>
    setPicked(prev => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const exitSelect = () => {
    setSelecting(false);
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

  const section = sheeted ? 'fg-sheet' : 'panel';
  if (nodes.isPending) return <Loading sheeted={sheeted} />;
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
  const alive = live.filter(n => pollTone(n) === 'ok').length;
  const offline = live.filter(n => pollTone(n) === 'bad').length;
  const idle = live.filter(n => pollTone(n) === 'idle').length;
  const retiredCount = list.length - live.length;
  // 已退役的不计入：对其再次提交退役是不产生任何变更的草稿操作，
  // 而计入后会使操作者认为本次退役了 N 台。
  const pickedLive = [...picked].filter(id => list.some(n => n.node_id === id && !n.retired_at));
  const seriesOf = new Map((usage.data?.nodes ?? []).map(s => [s.node_id, s]));
  const renderHeader = (count: number, includeRetired: boolean) => (
    /* 标题栏：标题加一组读数。在线数和掉线数是本页的汇总信息，
     * 在此显示比逐个查看状态条更直接。 */
    <header>
      <ListIcon of="nodes" />
      <h4>机器</h4>
      <span className="hint">{count} 台</span>
      <span className="rd">
        <b>{alive}</b> 在线
        {offline > 0 && (
          <>
            {' '}
            · <i>{offline}</i> 掉线
          </>
        )}
        {idle > 0 && ` · ${idle} 待纳管`}
        {includeRetired && retiredCount > 0 && ` · ${retiredCount} 退役/处理中`}
      </span>
      {selecting ? (
        <>
          <button className="btn" onClick={exitSelect}>
            取消
          </button>
          {/* 退役是直接创建停用发布的操作，不进入浏览器草稿。确认框会明确说明它可能
              取消并替换当前配置单。 */}
          <button
            className="btn danger"
            disabled={pickedLive.length === 0 || retireAll.isPending}
            title={`退役选中的 ${pickedLive.length} 台：保留记录，并为 Agent 创建完整停用发布`}
            onClick={() => {
              if (
                window.confirm(
                  `确认退役 ${pickedLive.length} 台机器？系统会提交退役修订、取消冲突中的发布，并立即创建完整停用发布。`,
                )
              )
                retireAll.mutate();
            }}
          >
            {retireAll.isPending ? '提交中…' : `退役下线${pickedLive.length > 0 ? ` ${pickedLive.length}` : ''}`}
          </button>
        </>
      ) : (
        <>
          <button className="btn" disabled={!can(who.role, 'system')} onClick={() => setSelecting(true)}>
            多选
          </button>
          <button
            className="btn primary"
            disabled={!can(who.role, 'system')}
            onClick={() => go({ p: 'provision', step: 1 })}
          >
            ＋ 纳管节点
          </button>
        </>
      )}
    </header>
  );
  const renderCards = (items: NodeAgentStateItem[]) => (
    <div className="ncards">
      {items.map(n => (
        <NodeCard
          key={n.node_id}
          node={n}
          series={seriesOf.get(n.node_id)}
          load={loadOf.get(n.node_id)}
          pingProbe={pingProbeOf.get(n.node_id)}
          pingProbeReady={pingProbe.isSuccess}
          usagePending={usage.isPending}
          selecting={selecting}
          checked={picked.has(n.node_id)}
          onPick={() => togglePick(n.node_id)}
          go={go}
        />
      ))}
    </div>
  );

  if (sheeted) {
    return (
      <div className="cardpage node-cardpage">
        <section className="panel titled node-list-panel">
          {renderHeader(live.length, false)}
          {list.length === 0 ? (
            <Empty>还没有节点。点「纳管节点」加一台。</Empty>
          ) : live.length === 0 ? (
            <Empty>没有在用机器。</Empty>
          ) : (
            renderCards(live)
          )}
        </section>
        {retired.length > 0 && (
          <details
            className="panel titled node-retired-panel"
            open={retiredOpen ?? retired.some(node => node.lifecycle_phase === 'retiring')}
            onToggle={event => setRetiredOpen(event.currentTarget.open)}
          >
            <summary>
              <h4>退役机器</h4>
              <span className="hint">{retired.length} 台</span>
            </summary>
            {renderCards(retired)}
          </details>
        )}
      </div>
    );
  }

  return (
    <div className={section}>
      {renderHeader(list.length, true)}
      {list.length === 0 ? <Empty>还没有节点。点「纳管节点」加一台。</Empty> : renderCards(list)}
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
function nodeLampState(node: NodeAgentStateItem, wireguardEnabled?: boolean): NodeLampState {
  if (node.lifecycle_phase === 'retiring') return { tone: 'warn', why: '退役中：等待停用收敛' };
  if (node.lifecycle_phase === 'retired') {
    return node.lifecycle_last_error
      ? { tone: 'warn', why: `已停用，外部资源待清理：${node.lifecycle_last_error}` }
      : { tone: 'idle', why: '已退役并确认停用' };
  }
  if (node.lifecycle_phase === 'abandoned') return { tone: 'bad', why: '强制退役：未确认远端停用' };

  const live = pollTone(node);
  if (live === 'idle') return { tone: 'idle', why: '从未上报' };
  if (live === 'bad') return { tone: 'bad', why: '失联' };

  const findings = runtimeFindings(node, wireguardEnabled).filter(f => !f.detailOnly);
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
  load,
  pingProbe,
  pingProbeReady,
  series,
  usagePending,
  selecting,
  checked,
  onPick,
  go,
}: {
  node: NodeAgentStateItem;
  /** 该机器的近期负载。undefined 表示尚未读取，或当前控制面版本没有该端点 */
  load?: NodeLoadView;
  /** 近一小时 Ping 读数；配置 TCP 目标后在卡片右下角替换 IP。 */
  pingProbe?: NodePingProbeView;
  /** 只有接口成功返回才能证明「没有配置目标」；加载中和请求失败都不能回退显示 IP。 */
  pingProbeReady: boolean;
  series?: UsageNodeSeries;
  usagePending: boolean;
  selecting: boolean;
  checked: boolean;
  onPick: () => void;
  go: (d: Drill) => void;
}) {
  const { who } = useSession();
  const canCreate = can(who.role, 'edit');
  const retired = node.lifecycle_phase !== 'active';
  const narrow = useNarrow();
  const live = pollTone(node);
  // 具体项写入 title，悬停即可在扫视列表时确认黄/红色对应的问题。
  const lamp = nodeLampState(node);

  const open = () => (selecting ? onPick() : go({ p: 'node', id: node.node_id }));
  return (
    <article
      role="button"
      tabIndex={0}
      className={`ncard tone-${lamp.tone}${retired ? ' off' : ''}${checked ? ' picked' : ''}`}
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
              disabled={retired}
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
        <MonthTotal series={series} pending={usagePending} retired={retired} />
        <span className="sp" />
        {/* 按钮位于卡片右下角（CSS 中 position:absolute），悬停时覆盖在 IP 或 TCP P95 之上。
            由于脱离文档流，它的显示和隐藏都不会改变右下角摘要的位置。
            「打开」已移除——整张卡片本身可点击，一屏九张各带一个按钮会形成密集的按钮排列。 */}
        <span className="nc-dock" onClick={e => e.stopPropagation()}>
          {!narrow && !retired && (
            <button className="btn" disabled={!canCreate} onClick={() => go({ p: 'chain', id: node.node_id })}>
              建一条链
            </button>
          )}
        </span>
        {!pingProbeReady || (pingProbe && pingProbe.targets.some(target => target.address.startsWith('tcp://'))) ? (
          <TcpProbeP95 view={pingProbe} pending={!pingProbeReady} />
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
export type LoadRange = (typeof LOAD_RANGES)[number];

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

export function ObserveRangeControl({ value, onChange }: { value: LoadRange; onChange: (value: LoadRange) => void }) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLSpanElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
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
        className="observe-range-trigger"
        aria-label={`观测时间范围：${value.menuLabel}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen(current => !current)}
        onKeyDown={event => {
          const selected = LOAD_RANGES.findIndex(option => option.seconds === value.seconds);
          if (event.key === 'ArrowDown') {
            event.preventDefault();
            openFromKeyboard(selected);
          } else if (event.key === 'ArrowUp') {
            event.preventDefault();
            openFromKeyboard(selected);
          }
        }}
      >
        <svg
          className="observe-range-clock"
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          aria-hidden="true"
        >
          <circle cx="12" cy="12" r="9" />
          <path d="M12 7.5v5l3.5 2" />
        </svg>
        <span>{value.menuLabel}</span>
      </button>
      <span
        className="observe-range-menu"
        role="listbox"
        aria-label="观测时间范围"
        hidden={!open}
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
        {LOAD_RANGES.map((option, index) => (
          <button
            key={option.seconds}
            ref={element => {
              optionRefs.current[index] = element;
            }}
            type="button"
            role="option"
            aria-selected={value.seconds === option.seconds}
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
      </span>
    </span>
  );
}

// 列表的 NIC 曲线使用 24 个 30 秒窗口，即近 12 分钟。该值同时是 load 端点的请求数和绘图上限，
// 不分成两个常量，避免「拉了 12 格却为 24 格留位」这类错位。
const LIST_NIC_WINDOWS = 24;
const LIST_NIC_WINDOW_SECS = 30;
// 上一版的最低尺度是 1 Mb/s。改为每窗口字节数后做等价换算，避免只因换单位就改变曲线高度。
const LIST_NIC_MIN_CEILING_BYTES = (1_000_000 * LIST_NIC_WINDOW_SECS) / 8;
// usage 查询仍需要一个近期窗口参数，但列表现在只读其中的本月累计字段。
const LIST_USAGE_WINDOW_SECS = 5 * 60;

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
 * 该下限与原来的 1 Mb/s 等价，避免将系统心跳放大成满幅波峰。卡片只表达该机自身的流量趋势，
 * 极值圆点悬停显示的是字节/窗口，不是 bit/s。`has_gap` 样本的速率不可比，直接断线，
 * 不补 0（补 0 会把「无法测量」说成「实际没有流量」）。 */
function p95(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.ceil(sorted.length * 0.95) - 1];
}

function nodeCardP95(value: number | null): string {
  return value == null ? '—' : String(Math.round(value));
}

function TcpProbeP95({ view, pending = false }: { view?: NodePingProbeView; pending?: boolean }) {
  if (pending && !view) return null;
  if (!view || view.targets.length === 0) return null;
  const values = view.targets
    .filter(target => target.address.startsWith('tcp://'))
    .map(target => ({
      name: target.name,
      value: p95(
        target.samples.flatMap(sample => {
          const latency = pingLatencyMs(sample);
          return latency == null ? [] : [latency];
        }),
      ),
    }));
  if (values.length === 0) return null;
  const compact = values
    .slice(0, 3)
    .map(item => nodeCardP95(item.value))
    .join(' / ');
  const title = values
    .map(item => `${item.name}：${nodeCardP95(item.value)}${item.value == null ? '' : ' ms'}`)
    .join('\n');
  return (
    <span className="nc-tcp-p95" title={title}>
      <span className="values">{compact}</span>
      {values.some(item => item.value != null) && <em>ms</em>}
    </span>
  );
}

function NicWave({ load }: { load?: NodeLoadView }) {
  const samples = (load?.series ?? []).slice(-LIST_NIC_WINDOWS);
  const label = 'NIC · 30 秒 / 窗口';
  const windowBytes = (sample: LoadSample) => {
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
  // 样本不足 24 个时靠右放：右缘是最近窗口，新纳管机器不应把两个点拉满 12 分钟。
  const slotOffset = LIST_NIC_WINDOWS - samples.length;
  const xOf = (index: number) => ((slotOffset + index) * W) / (LIST_NIC_WINDOWS - 1);
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

/* 本月累计。月界为控制面本地 +08 时区的自然月，由服务端确定——与用户页的该列口径一致。 */
function MonthTotal({ series, pending, retired }: { series?: UsageNodeSeries; pending: boolean; retired: boolean }) {
  const total = series ? monthBytes(series) : 0;
  if (!series || total === 0) {
    return (
      <span className="lst-sum void" title={retired ? '已退役' : pending ? '读取中' : '这个月还没有样本'}>
        —<small>本月</small>
      </span>
    );
  }
  return (
    <span
      className="lst-sum"
      title={
        `用户流量 ${bytes(series.month_user_uplink_bytes + series.month_user_downlink_bytes)}` +
        ` · 中继流量 ${bytes(series.month_relay_uplink_bytes + series.month_relay_downlink_bytes)}` +
        (series.month_has_gap ? '\n本月至少一次采集有缺口，数字只小不大' : '')
      }
    >
      {bytes(total)}
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

/** 把 usage 桶按角色拆成速率序列（bps），右对齐：最近的完整窗口在下标 59——与网卡曲线
 * （LoadDashboard 的 `slice(-LOAD_SLOTS)`）同一对齐方式，两张图的右缘是同一个 30 秒窗口。
 * 字节/窗口 ×8 转比特、÷30 窗口秒，得到窗口平均速率——与网卡上报的速率同型，因此可对比。
 * 不用 since 推算绝对槽位：since 与桶的窗口边界存在相位差，最近一个完整桶会被算进 slot 22，
 * 把「现在」（slot 23）留空，表现为曲线右缘永远掉到 0。无桶的窗口计 0（无转发字节），不是缺失。 */
function usageRoleSlots(series: UsageNodeSeries | undefined, slots: number): { user: number[]; relay: number[] } {
  const tail = (series?.buckets ?? []).slice(-slots);
  const pad = slots - tail.length;
  const padArr = new Array<number>(pad).fill(0);
  return {
    user: [...padArr, ...tail.map(b => ((b.user_uplink_bytes + b.user_downlink_bytes) * 8) / 30)],
    relay: [...padArr, ...tail.map(b => ((b.relay_uplink_bytes + b.relay_downlink_bytes) * 8) / 30)],
  };
}

/* 网卡吞吐与 XRAY 承载吞吐堆进一个面板。两者同窗口、同粒度、用 group 联动十字线，但口径
 * 不同：网卡按方向（接收/发送）统计全部流量，XRAY 按角色（用户/中继）只统计 xray 转发的
 * 字节——不能合并进一张图，各自的 Y 轴与图例保持这个差异可见，所以是同卡内的两张图。
 * node-load 与 usage 两个查询的 queryKey 都与页面其它处一致，React Query 去重、不多发请求。 */
export function ThroughputPanel({ nodeId, range, linked }: { nodeId: string; range: LoadRange; linked: boolean }) {
  const load = useQuery({
    queryKey: ['node-load-history', nodeId, range.seconds],
    queryFn: () => fetchNodeLoad(nodeId, range.seconds / 30),
    refetchInterval: range.seconds <= 60 * 60 ? 10_000 : 30_000,
    retry: false,
  });
  const usage = useQuery({
    queryKey: ['usage-node-series', nodeId, range.seconds],
    queryFn: () => fetchUsageNodeSeries(range.seconds, nodeId),
    refetchInterval: 30_000,
  });
  const report = load.data;
  if (!report || report.series.length === 0) return null;

  const group = linked ? `nd-tp-${nodeId}` : undefined;
  const windows = range.seconds / 30;

  // 网卡：按方向统计全部流量。右对齐、缺口断开、历史不足选定范围时左侧补 null。
  const series = report.series;
  const last = series[series.length - 1];
  const host = report.host;
  const tail = series.slice(-windows);
  const pad = windows - tail.length;
  const nicRx: (number | null)[] = [
    ...Array<number | null>(pad).fill(null),
    ...tail.map(s => (s.has_gap ? null : s.nic_rx_bps)),
  ];
  const nicTx: (number | null)[] = [
    ...Array<number | null>(pad).fill(null),
    ...tail.map(s => (s.has_gap ? null : s.nic_tx_bps)),
  ];
  const drops = last.nic_rx_drop + last.nic_tx_drop + last.nic_err;
  const nicMeta = [
    last.nic_rx_drop > 0 ? `接收丢弃 ${last.nic_rx_drop.toLocaleString()}` : null,
    last.nic_tx_drop > 0 ? `发送丢弃 ${last.nic_tx_drop.toLocaleString()}` : null,
    last.nic_err > 0 ? `网卡错误 ${last.nic_err.toLocaleString()}` : null,
    host?.nic ?? null,
    typeof host?.nic_mtu === 'number' ? `MTU ${host.nic_mtu}` : null,
  ].filter((value): value is string => value !== null);

  // XRAY：按角色只统计 xray 转发的字节。
  const mine = usage.data?.nodes.find(n => n.node_id === nodeId);
  const { user, relay } = usageRoleSlots(mine, windows);
  const monthTotal = mine ? monthBytes(mine) : 0;

  return (
    <section className="chart-card nd-throughput-panel" aria-label="吞吐">
      <div className="nd-throughput-block">
        <div className="load-network-cap">
          <b>网卡流量</b>
          {/* 量纲写在标题后，刻度只留数字。峰值与单位都取自 throughputAxis，
              与图内那次调用同源，标题和刻度不会各说各话。 */}
          <span className="chart-unit">({throughputAxis(nicRx, nicTx).unit.name})</span>
          {nicMeta.length > 0 && <span className={drops > 0 ? 'hot' : undefined}>{nicMeta.join(' · ')}</span>}
          <footer className="load-network-legend" aria-label="网卡流量图例">
            <span className="rx">
              <i />
              接收 <b>{bps(last.nic_rx_bps)}</b>
            </span>
            <span className="tx">
              <i />
              发送 <b>{bps(last.nic_tx_bps)}</b>
            </span>
          </footer>
        </div>
        <ThroughputChart rx={nicRx} tx={nicTx} rxName="接收" txName="发送" group={group} />
      </div>
      <div className="nd-throughput-block">
        <div className="load-network-cap">
          <b>XRAY 流量</b>
          <span className="chart-unit">({throughputAxis(user, relay).unit.name})</span>
          <span>本月 {bytes(monthTotal)}</span>
          <footer className="load-network-legend" aria-label="XRAY 流量图例">
            <span className="rx">
              <i />
              用户 <b>{bps(user[user.length - 1])}</b>
            </span>
            <span className="tx">
              <i />
              中继 <b>{bps(relay[relay.length - 1])}</b>
            </span>
          </footer>
        </div>
        <ThroughputChart rx={user} tx={relay} rxName="用户" txName="中继" group={group} />
      </div>
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
  const probe = useQuery({
    queryKey: ['link-mtu'],
    queryFn: () => fetchLinkMtu(),
    refetchInterval: 60_000,
  });
  const [value, setValue] = useState<string | null>(null);
  const mine = probe.data?.nodes.find(n => n.node_id === node.node_id);
  const effective = node.mtu ?? probe.data?.default_mtu ?? null;

  const save = useMutation({
    mutationFn: () => updateNode(node.node_id, { mtu: Number(value) || 0 }),
    onSuccess: () => {
      setValue(null);
      onSaved();
    },
  });

  if (value !== null) {
    return (
      <Row k="MTU">
        <input
          className="f"
          style={{ width: 100 }}
          value={value}
          placeholder="留空 = 用默认"
          onChange={e => setValue(e.target.value)}
        />
        <button
          className="btn primary"
          style={{ marginLeft: 6 }}
          disabled={save.isPending}
          onClick={() => save.mutate()}
        >
          保存
        </button>
        <button className="btn" style={{ marginLeft: 4 }} onClick={() => setValue(null)}>
          取消
        </button>
        <span className="sub">
          清空 = 恢复全局默认 {probe.data?.default_mtu ?? '—'}。修改 MTU 会重新生成 wg 配置，触发一次链路重连。
        </span>
      </Row>
    );
  }

  return (
    <Row k="MTU">
      <span className="mono">{effective ?? '—'}</span>{' '}
      <span className="dim">{node.mtu === null ? '全局默认' : '本机单独设置'}</span>
      <button
        className="btn"
        style={{ marginLeft: 8 }}
        disabled={!canEdit}
        onClick={() => setValue(node.mtu === null ? '' : String(node.mtu))}
      >
        改
      </button>
      {mine?.suggested_mtu != null && mine.suggested_mtu !== effective && (
        <button
          className="btn"
          style={{ marginLeft: 4 }}
          disabled={!canEdit}
          onClick={() => setValue(String(mine.suggested_mtu))}
        >
          采纳 {mine.suggested_mtu}
        </button>
      )}
      <span className="sub">
        {mine?.suggested_mtu != null ? (
          <>
            探测建议 {mine.suggested_mtu}
            {mine.tightest_peer && <>（最小路径通向 {mine.tightest_peer}）</>}
            {mine.inconclusive > 0 && <>；另有 {mine.inconclusive} 条未探出，该值可能偏大</>}
          </>
        ) : (
          <>还没有探测结果</>
        )}
      </span>
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
  const [open, setOpen] = useState(false);
  // 默认使用高位、未被常见服务占用的端口。不使用 443：该端口在本机上很可能已被
  // 接入面占用，且在 443 上运行非 TLS 服务会在主动探测下暴露。
  const [staged, setStaged] = useState<{ fake: boolean; port: number } | null>(null);

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  // 当前值取自编译结果而非 `node.wg_transport_kind`，原因与 OverlayRow 一致：
  // `/nodes/agent-state` 是直连接口，不经过草稿预览，以它为数据源会导致选择 phantun、
  // 写入草稿后该行显示回退为直接 UDP——改动已在草稿中而界面显示为未修改。
  //
  // 判定依据是是否存在与该机器相邻的链路将 phantun 服务端部署在该机器上。封装是链路属性
  // （启用时两端同时启用），而 `servers` 的键正是被连接的一侧，即该行表示的入站方向。
  // 地址中包含端口，可一并获取。
  //
  // 不存在相邻链路时回退到模型值，而不是判定为 UDP：该机器可能不在 overlay 中，
  // 没有链路可供推断，此时显示「直接 UDP」相当于把无法推断当作推断结果。
  const links = (compile.data?.system as { links?: LinkIr[] } | undefined)?.links;
  const touching = links?.filter(link => link.a === node.node_id || link.b === node.node_id);
  const served = touching
    ?.map(link => (link.wrap?.t === 'fake_tcp' ? link.wrap.v?.servers?.[node.node_id] : undefined))
    .find(Boolean);
  const known = touching !== undefined && touching.length > 0;
  const currentFake = known ? served !== undefined : node.wg_transport_kind === 'fake_tcp';
  const currentPort = served ? Number(served.split(':').pop()) || null : known ? null : node.wg_fake_tcp_port;

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
      setOpen(false);
      onSaved();
    },
  });

  if (!open) {
    return (
      <Row k="入站传输">
        {currentFake ? '伪 TCP（phantun）' : '直接 UDP'}
        <button className="btn" style={{ marginLeft: 8 }} disabled={!canEdit} onClick={() => setOpen(true)}>
          改
        </button>
        {currentFake && currentPort !== null && <span className="sub">对端连接 :{currentPort}</span>}
      </Row>
    );
  }

  return (
    <Row k="入站传输">
      <label style={{ marginRight: 14 }}>
        <input type="radio" checked={!fake} onChange={() => setFake(false)} /> 直接 UDP
      </label>
      <label>
        <input type="radio" checked={fake} onChange={() => setFake(true)} /> 伪 TCP（phantun）
      </label>
      {fake && (
        <div style={{ marginTop: 6 }}>
          <input
            className="f"
            style={{ width: 110 }}
            value={port}
            onChange={e => setPort(Number(e.target.value) || 0)}
          />
          <span className="sub">使用高位端口。不要使用 443：容易与接入面冲突，也容易被探测。</span>
        </div>
      )}
      <span className="sub" style={{ color: 'var(--gold)' }}>
        所有对端的 wg0.conf 会随之变更。属破坏性变更。
      </span>
      {save.error && <ErrorBox error={save.error} />}
      <div className="toolbar">
        <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
          {save.isPending ? '保存中…' : '保存到草稿'}
        </button>
        <button className="btn" onClick={() => setOpen(false)}>
          取消
        </button>
      </div>
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

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  // 当前值取自编译结果而非 `node.overlay`：`/nodes/agent-state` 是直连接口，不经过
  // 草稿预览，以它为数据源会导致切换开关、写入草稿后开关显示回退——改动已在草稿中
  // 而界面显示为未修改。判定依据是在系统层中且有 wg 配置：不在 overlay 中的中继机同样
  // 出现在 `system.nodes` 中，只是 `wireguard` 为 null，两者都不满足的机器不在该数组内，
  // 两种情况都视为不在 overlay 中。
  // 编译结果尚未返回时回退到模型值，避免该行为空。
  const systemNodes = (compile.data?.system as { nodes?: { id: string; wireguard?: unknown }[] } | undefined)?.nodes;
  const currentValue = systemNodes ? systemNodes.find(n => n.id === node.node_id)?.wireguard != null : node.overlay;
  const value = staged ?? currentValue;
  const dirty = staged !== null && staged !== currentValue;

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

  return (
    <Row k="WireGuard">
      <SegSwitch checked={value} disabled={!canEdit} onChange={checked => setStaged(checked)} off="关闭" on="启用" />
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
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
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

  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  // 每个 app 的 nodes 都是全量节点表（ir/routing.rs 中的 filter 只过滤退役节点），
  // 因此取任意一个 app 即可，无需合并所有 app。没有任何项目时无法读取——
  // 此时回退到模型值。
  const appNodes = (compile.data?.apps as AppIr[] | undefined)?.[0]?.nodes;
  const currentValue = appNodes
    ? (appNodes.find(n => n.id === node.node_id)?.egress_allowed ?? node.egress_allowed)
    : node.egress_allowed;
  const value = staged ?? currentValue;
  const dirty = staged !== null && staged !== currentValue;

  const save = useMutation({
    mutationFn: () => updateNode(node.node_id, { egress_allowed: value }),
    onSuccess: () => {
      setStaged(null);
      /* 该行的当前值来自编译视图，不重新编译则不会反映刚写入的改动 */
      qc.invalidateQueries({ queryKey: ['compile'] });
      onSaved();
    },
  });

  return (
    <Row k="出网权限">
      <SegSwitch
        checked={value}
        disabled={!canEdit}
        onChange={checked => setStaged(checked)}
        off="禁止出网"
        on="可出网"
      />
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
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
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

/* 这台机器从哪个证书组取证书。
 *
 * 与相邻几张卡不同，改这里不进草稿也不需要发布：证书走 agent 自己的轮询通道（十分钟一轮），
 * 组决定的 SNI 虽然进编译产物，但换组这件事本身要先落库才谈得上重新编译。所以这里直接保存。
 *
 * 换组会改 SNI，已经发出去的订阅写的是旧名字，改完就连不上。确认框把这句话说清楚，而不是
 * 问一句「确定要修改吗」——后者没有告诉人代价是什么。 */
function CertGroupCard({ node, canEdit }: { node: NodeAgentStateItem; canEdit: boolean }) {
  const qc = useQueryClient();
  const certs = useQuery({ queryKey: ['certs'], queryFn: () => fetchCerts(), retry: false });
  const groups = certs.data?.groups ?? [];
  const current = certs.data?.nodes.find(row => row.node_id === node.node_id);
  const group = groups.find(g => g.id === current?.label_id);
  const serving = group?.certificates.find(c => c.status === 'serving');

  const save = useMutation({
    mutationFn: (labelId: string) => setNodeCertGroup(node.node_id, labelId || null),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['certs'] }),
  });

  const pick = (next: string) => {
    if (next === (current?.label_id ?? '')) return;
    const to = next ? (groups.find(g => g.id === next)?.name ?? next) : '不关联证书';
    const warning = current
      ? `这台机器的 SNI 会从 ${current.certificate_name} 变成${next ? '新组的名字' : '没有'}。\n\n` +
        '已经发出去的订阅里写的是旧名字，改完之后这台机器上的 VLESS + TLS 和 Hysteria 2 线路' +
        '会连不上，需要让用户重新拉取订阅。REALITY 指向外部站点的接入面不受影响。\n\n' +
        `确定改到「${to}」吗？`
      : `把这台机器加入「${to}」。它之后出示这个组的证书。`;
    if (window.confirm(warning)) save.mutate(next);
  };

  return (
    <div className="panel config-panel">
      <header>
        <h4>证书组</h4>
      </header>
      <div className="fgrid one">
        <Row k="所属组">
          <select
            className="f"
            style={{ width: 220 }}
            value={current?.label_id ?? ''}
            disabled={!canEdit || save.isPending || certs.isPending}
            onChange={e => pick(e.target.value)}
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
              : '没有本机证书：这台机器上的 TLS 与 Hysteria 2 接入面会在编译时被拒绝。'}
          </span>
        </Row>
        {save.error && <ErrorBox error={save.error} />}
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
function DnsCard({ node, canEdit, onSaved }: { node: NodeAgentStateItem; canEdit: boolean; onSaved: () => void }) {
  const [servers, setServers] = useState<string | null>(null);
  const [strategy, setStrategy] = useState<DomainStrategy | null>(null);

  const baseServers = formatDns(node.dns);
  const curServers = servers ?? baseServers;
  const curStrategy = strategy ?? node.domain_strategy;
  const dirty = curServers !== baseServers || curStrategy !== node.domain_strategy;

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
        <h4>DNS</h4>
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
            style={{ width: 200 }}
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
 * 默认折叠，展开后才显示内容——本栏中唯一采用折叠的卡片，因为它也是唯一一张多数机器上
 * 没有内容可看的卡：四项全部跟随机队默认是常态。标题常驻显示，其中的一句说明
 * （跟全局一样 / 该机器修改了 N 项）即是折叠状态下需要表达的全部内容。
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
  // 「跟架构 KB」——非数值的取值加上单位后无法读通，且需要为此单独判断是否显示。
  { key: 'conn_idle_secs', label: '空闲回收（秒）', pick: false },
  { key: 'buffer_size_kb', label: '转发缓冲（KB）', pick: false },
  { key: 'uplink_only_secs', label: 'UplinkOnly 等待（秒）', pick: true },
  { key: 'downlink_only_secs', label: 'DownlinkOnly 等待（秒）', pick: true },
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
  // null 表示本次尚未修改任何字段，当前值直接读取模型。与 DnsCard 一致：控件常驻，
  // 修改后才显示工具条。
  const [form, setForm] = useState<Record<ConnKey, string> | null>(null);

  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });

  const fromModel = snapshot.data?.snapshot.nodes?.find(n => n.id === node.node_id)?.connection;
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
    <details className="panel config-panel conn-card">
      <summary>
        <h4>连接策略</h4>
        <span className="hint">{ownCount === 0 ? '与全局一致' : `本机覆盖 ${ownCount} 项`}</span>
        {/* 折叠状态下内部的改动不可见——`details` 不卸载子树，编辑内容仍然存在，
            但界面上无法看出已有修改。由标题说明该状态。 */}
        {dirty && (
          <span className="hint" style={{ color: 'var(--gold)' }}>
            有未保存的改动
          </span>
        )}
      </summary>
      <div className="fgrid one">
        {CONN_FIELDS.map(f => (
          <Row k={f.label} key={f.key}>
            {f.pick ? (
              /* 数字组始终显示当前生效值。点数字即创建/修改本机覆盖；只在覆盖存在时
                 显示独立的「取消覆盖」，将字段恢复为 null。不额外增加模式开关，保留原有单行操作。 */
              <span className="conn-pick-row">
                <span className="segsw" role="group" aria-label={`${f.label}的数值`}>
                  {picks(f.key).map(value => (
                    <button
                      key={value}
                      type="button"
                      aria-pressed={String(value) === effective(f.key)}
                      disabled={!canEdit}
                      onClick={() => set(f.key, String(value))}
                    >
                      {value}
                    </button>
                  ))}
                </span>
                {isOwn(f.key) && (
                  <button type="button" className="btn" disabled={!canEdit} onClick={() => set(f.key, '')}>
                    取消覆盖
                  </button>
                )}
              </span>
            ) : (
              <input
                className="f mono"
                style={{ width: 96 }}
                value={cur(f.key)}
                disabled={!canEdit}
                placeholder={globalText(f.key)}
                onChange={e => set(f.key, e.target.value)}
              />
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

/** 将输入解析为模型值。空值对应 null，表示使用全局默认；非数值同样按空值处理，
    服务端会拒绝非法输入。 */
function connValue(raw: string): number | null {
  const t = raw.trim();
  if (t === '') return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

function WgCard({ node, canEdit, onSaved }: { node: NodeAgentStateItem; canEdit: boolean; onSaved: () => void }) {
  return (
    <div className="panel config-panel">
      <header>
        <h4>WIREGUARD</h4>
      </header>
      <div className="fgrid one">
        <OverlayRow node={node} canEdit={canEdit} onSaved={onSaved} />
        <NodeMtuRow node={node} canEdit={canEdit} onSaved={onSaved} />
        <WgTransportRow node={node} canEdit={canEdit} onSaved={onSaved} />
      </div>
    </div>
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
      例如「该版本 agent 不上报该字段」，它解释了相邻字段显示为「—」的原因，
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

export function runtimeFindings(node: NodeAgentStateItem, wireguardEnabled?: boolean): Finding[] {
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

  const spool = node.spool_backlog;
  if (spool && spool.dropped > 0) {
    out.push({
      tone: 'bad',
      chip: `丢了 ${spool.dropped.toLocaleString()} 条账`,
      text: (
        <>
          发不出去被丢掉的上报<b>找不回来</b>。症状是这台机器某几段时间「没有流量」，
          跟真的没流量分不开。先看它为什么连不上控制面。
        </>
      ),
    });
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

  /* 旧版本 agent 不上报这几项。该条需要最后添加：它说明上面各项显示为「—」的原因。 */
  if (!node.runtime_versions && node.agent_version) {
    out.push({
      tone: 'warn',
      detailOnly: true,
      // 构建号截断显示：完整 sha 有 64 位，完整显示会导致该行超出宽度。
      chip: `agent ${node.agent_version.replace(/^brocade-agent\//, '').slice(0, 12)}`,
      text: (
        <>
          还没收到这台的运行时观测：要么是刚纳管、还没轮到（周期 30 分钟）， 要么是这一版 agent <b>不上报</b>
          。所以那几处是「—」不是 0。
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
 * 替换手写版本号是因为该值不具备标识能力——发布 agent 时通常不会同步修改 workspace
 * 的版本号，因此从上次改动至今的每次构建都上报同一个值，而发布时需要确认的正是
 * 该机器是否已完成替换。
 *
 * 有两种来源和两种格式：poll 的 User-Agent 是 `brocade-agent/<sha>`，runtime 上报是裸 sha。
 * 两者都提取出 sha 后显示前 12 位（与 git 的惯例一致），完整值写入 title 以便完整比对。
 *
 * 无法识别为 sha 的按原样显示：旧版本 agent 上报的是 `0.1.0` 这类值，这些实例仍在运行，
 * 显示内容需要可读。 */
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
    <span className="mono" title="这一版 agent 报的还是手写版本号，认不出是哪次构建，也不会自更新">
      {bare}
    </span>
  );
}

/** 该机器的当前负载。自行获取数据，与 UsageCard 结构相同——详情页不统一管理各卡的数据。
 *
 * 获取失败时整张卡不渲染（`retry: false` 且不提示）：该端点是后加的，未升级的控制面
 * 会返回 404，而这不是该机器的问题，不应在其详情页显示错误。 */
/* ── 网络吞吐面板：全机队 NIC 网卡汇总 对 XRAY 承载 ──
   机器列表页顶部的一张总览：把所有机器的网卡吞吐与被代理承载各自相加，画成上下镜像。
   接收在上、发送在下，零线居中；每侧 NIC 是描边外层、XRAY 是实心内层——两线之间的缝
   就是全机队的封装与系统开销（wg / phantun 封装、系统与探测流量）。NIC ≥ XRAY 恒成立。

   两条数据都取自现有的全机器上报：
   - NIC  来自 node_load_samples 的 nic_rx_bps / nic_tx_bps（bit/s），逐机相加
   - XRAY 来自 usage node-series 的 user + relay 字节，折算 bit/s 后逐机相加
   汇总的关键是对齐：各机 30 秒窗口相位不同，直接按精确 window_end 求和会漏桶、塌成 0。
   因此两条一律按 floor(window_end / 30) 落到同一时间网格再相加——相位错开的样本这才对齐。
   上下共用一个峰值缩放：接收、发送的相对大小要诚实，不各自拉满自己那半。
   月度累计不在这里重复：用量页已按机器 / 用户 / 中继给出。 */
const FLEET_WINDOWS = 240; // 2 小时 / 30 秒。列表端点上限即 240，且它一次返回全机队。
const FLEET_SECS = FLEET_WINDOWS * 30;
const GRID_SECS = 30; // 汇总时间网格：agent 的上报窗口即 30 秒。

// 只此面板用 ECharts，按需注册（核心 + 折线 + 网格 + 提示 + 标记区域/线 + canvas），不引全量。
echarts.use([LineChart, GridComponent, TooltipComponent, MarkLineComponent, CanvasRenderer]);

const NET_MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';

interface NetPoint {
  nicRx: number;
  nicTx: number;
  xrayRx: number;
  xrayTx: number;
}

/* 镜像面积图。接收为正（朝上）、发送取负（朝下），零线居中；每方向 NIC 外层（淡填充 +
   描边）套 XRAY 内层（实心），两线之间的缝即封装 / 系统开销。方向由镜像位置表达，用色
   同一数据色的两档（接收=data、发送=data-secondary），来源用填充手法区分。颜色与坐标
   全部读自 CSS 令牌，随明暗主题与调色盘选择重绘。 */
function FleetNetChart({ times, pts, peak }: { times: number[]; pts: NetPoint[]; peak: number }) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);

  // 实例只建一次；容器尺寸变化时 resize。
  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, []);

  // 数据或主题变化时重设 option。数据角色色从 :root 的 CSS 令牌读，canvas 里字体要给具体栈。
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const data = cv('--data');
    const dataSecondary = cv('--data-secondary');
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');

    // sign=-1 把发送翻到零线下方，做镜像；inner=true 是 XRAY 实心内层。
    const area = (name: string, key: keyof NetPoint, color: string, sign: 1 | -1, inner: boolean) => ({
      name,
      type: 'line' as const,
      showSymbol: false,
      smooth: true,
      sampling: 'lttb' as const,
      lineStyle: { color, width: inner ? 1 : 1.3, opacity: inner ? 1 : 0.9 },
      areaStyle: { color, opacity: inner ? 0.42 : 0.12 },
      emphasis: { disabled: true },
      z: inner ? 3 : 2,
      data: times.map((t, i) => [t * 1000, sign * pts[i][key]] as [number, number]),
    });

    const series = [
      area('接收 · 网卡', 'nicRx', data, 1, false),
      area('接收 · 承载', 'xrayRx', data, 1, true),
      area('发送 · 网卡', 'nicTx', dataSecondary, -1, false),
      area('发送 · 承载', 'xrayTx', dataSecondary, -1, true),
    ];
    // 零线挂在第一条系列上。
    (series[0] as Record<string, unknown>).markLine = {
      silent: true,
      symbol: 'none',
      data: [{ yAxis: 0 }],
      lineStyle: { color: ink4, width: 0.8, opacity: 0.55 },
      label: { show: false },
    };

    chart.setOption(
      {
        animation: false,
        grid: { left: 48, right: 12, top: 10, bottom: 20 },
        textStyle: { fontFamily: NET_MONO },
        tooltip: {
          trigger: 'axis',
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: NET_MONO },
          formatter: (params: unknown) => {
            const arr = params as { seriesName: string; color: string; value: [number, number] }[];
            const when = new Date(arr[0].value[0]).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
            const row = (p: { seriesName: string; color: string; value: [number, number] }) =>
              `<div style="display:flex;gap:8px;align-items:center;line-height:1.75">` +
              `<span style="width:8px;height:8px;border-radius:2px;background:${p.color}"></span>` +
              `<span>${p.seriesName}</span>` +
              `<b style="margin-left:auto;color:${ink}">${bps(Math.abs(p.value[1]))}</b></div>`;
            return `<div style="color:${ink4};font-size:9px;margin-bottom:3px">${when}</div>${arr.map(row).join('')}`;
          },
        },
        xAxis: {
          type: 'time',
          axisLabel: { color: ink4, fontSize: 9, hideOverlap: true },
          // onZero:false 把 x 轴框落到镜像底部；零线另由 series[0] 的 markLine 画。
          axisLine: { ...observeAxisLine(ink3), onZero: false },
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: false },
        },
        yAxis: {
          type: 'value',
          min: -peak,
          max: peak,
          axisLabel: { color: ink4, fontSize: 9, formatter: (v: number) => bps(Math.abs(v)) },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          splitLine: { lineStyle: { color: lineSoft } },
        },
        series,
      },
      true,
    );
  }, [times, pts, peak, themeName, paletteKey]);

  return <div ref={elRef} className="ndnet-echart" />;
}

export function FleetNetPanel() {
  /* 30 秒刷新。两条都是全机队查询：列表页已在拉 24 窗口的 load（状态点用），这里另拉
     240 窗口的 2 小时版做趋势；usage node-series 同样按 2 小时取，两者都逐机相加。 */
  const load = useQuery({
    queryKey: ['node-load-list', FLEET_WINDOWS],
    queryFn: () => fetchNodeLoadList(FLEET_WINDOWS),
    refetchInterval: 30_000,
    retry: false,
  });
  const usage = useQuery({
    queryKey: ['usage-node-series', FLEET_SECS],
    queryFn: () => fetchUsageNodeSeries(FLEET_SECS),
    refetchInterval: 30_000,
  });

  const geo = useMemo(() => {
    // 全机队按 30 秒网格累加：各机相位不同，floor 到同一格后相加，避免精确秒对不上而漏桶。
    const acc = new Map<number, NetPoint>();
    const at = (key: number): NetPoint => {
      let e = acc.get(key);
      if (!e) {
        e = { nicRx: 0, nicTx: 0, xrayRx: 0, xrayTx: 0 };
        acc.set(key, e);
      }
      return e;
    };
    for (const node of load.data?.nodes ?? []) {
      for (const s of node.series as LoadSample[]) {
        // has_gap 的样本速率不可比，跳过——汇总里少一台就是那一格总量低一点，不画缺口。
        if (s.has_gap) continue;
        const key = Math.floor(s.window_end_unix_secs / GRID_SECS) * GRID_SECS;
        const e = at(key);
        e.nicRx += s.nic_rx_bps;
        e.nicTx += s.nic_tx_bps;
      }
    }
    for (const node of usage.data?.nodes ?? []) {
      for (const b of node.buckets as UsageNodeBucket[]) {
        const key = Math.floor(Date.parse(b.window_end) / 1000 / GRID_SECS) * GRID_SECS;
        const e = at(key);
        // 接收=下行(downlink)，发送=上行(uplink)；用户 + 中继是该机该方向的承载。字节 ×8÷30 = bit/s。
        e.xrayRx += ((b.user_downlink_bytes + b.relay_downlink_bytes) * 8) / GRID_SECS;
        e.xrayTx += ((b.user_uplink_bytes + b.relay_uplink_bytes) * 8) / GRID_SECS;
      }
    }
    const allKeys = [...acc.keys()].sort((a, b) => a - b);
    if (allKeys.length === 0) return null;
    // 锚到最新一格，向前取至多 2 小时；连续铺网格，中间无数据的格记 0（多机同时静默才出现）。
    const endKey = allKeys[allKeys.length - 1];
    const startKey = Math.max(allKeys[0], endKey - (FLEET_WINDOWS - 1) * GRID_SECS);
    const n = Math.round((endKey - startKey) / GRID_SECS) + 1;
    const times: number[] = [];
    const pts: NetPoint[] = [];
    for (let i = 0; i < n; i += 1) {
      const key = startKey + i * GRID_SECS;
      times.push(key);
      pts.push(acc.get(key) ?? { nicRx: 0, nicTx: 0, xrayRx: 0, xrayTx: 0 });
    }
    // 上下共用一个峰值：接收、发送同尺度，相对大小才诚实，不各自拉满自己那半。
    let peak = 1;
    for (const p of pts) peak = Math.max(peak, p.nicRx, p.nicTx, p.xrayRx, p.xrayTx);
    return { pts, times, peak };
  }, [load.data, usage.data]);

  const head = (
    <header>
      <h4>网络吞吐 · 全机队</h4>
      <span className="sp" />
      <span className="hint">网卡汇总 对 XRAY 承载 · 接收在上 / 发送在下 · 近 2 小时</span>
    </header>
  );

  if (!load.data) return null;
  if (!geo) {
    return (
      <div className="panel titled ndnet">
        {head}
        <p className="note">还没有网络读数。</p>
      </div>
    );
  }

  return (
    <div className="panel titled ndnet">
      {head}
      <FleetNetChart times={geo.times} pts={geo.pts} peak={geo.peak} />
      <div className="ndnet-legend">
        <span>
          <i className="sw rx" />
          接收
        </span>
        <span>
          <i className="sw tx" />
          发送
        </span>
        <span className="vr" />
        <span>
          <i className="sw solid" />
          XRAY 承载
        </span>
        <span>
          <i className="sw out" />
          NIC 网卡
        </span>
        <span className="tail">缝隙 = 封装 / 系统开销</span>
      </div>
    </div>
  );
}

function LoadCardFor({ nodeId, range, linked }: { nodeId: string; range: LoadRange; linked: boolean }) {
  /* 10s 而不是与上报窗口相同的 30s。窗口每 30 秒关一次（agent 的
     `SUBS_PER_WINDOW × SUB_INTERVAL_SECS`），前端也每 30 秒拉一次时两者不同相：
     最坏情况拿到的是刚过期 30 秒的窗口，再等 30 秒才拉下一次，端到端能到 60 秒。
     按 10s 拉，窗口一关最多 10 秒就被取到——这一段是纯等待，缩掉不损失任何东西。
     真正的分辨率仍是 30 秒，那要改 agent 的窗口，且用量窗口得一起动（两者故意对齐，
     遥测页把流量柱和 CPU 线叠在同一根时间轴上就靠这个）。 */
  const load = useQuery({
    queryKey: ['node-load-history', nodeId, range.seconds],
    queryFn: () => fetchNodeLoad(nodeId, range.seconds / 30),
    // 长范围响应最大有 2,880 个深度窗口，而原数据本身每 30 秒才增加一点。
    // 30m/1h 保留 10s 的低延迟；6h 以上按数据分辨率拉取，避免重复传输同一大段历史。
    refetchInterval: range.seconds <= 60 * 60 ? 10_000 : 30_000,
    retry: false,
  });
  if (!load.data) return null;
  // 吞吐两图（网卡 / XRAY）已移到 ThroughputPanel，与 Ping 面板并列；本卡只留生命体征与历史。
  return (
    <LoadCard report={load.data} historyLabel={range.heading} historyWindows={range.seconds / 30} linked={linked} />
  );
}

const PING_SERIES_CSS = OBSERVE_SERIES_COLOR_VARS;
type PingProtocol = 'icmp' | 'tcp';

function html(value: string): string {
  return value.replace(
    /[&<>"']/g,
    char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]!,
  );
}

function PingLatencyChart({ view, range, group }: { view: NodePingProbeView; range: LoadRange; group?: string }) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    if (group) chart.group = group;
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, [group]);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const colors = observeColors(themeName, cv);
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');
    const byTarget = view.targets.map(
      target => new Map(target.samples.map(sample => [sample.probed_at_unix_secs, sample])),
    );
    const times = [
      ...new Set(view.targets.flatMap(target => target.samples.map(sample => sample.probed_at_unix_secs))),
    ].sort((a, b) => a - b);
    const valuesAt = new Map(times.map(time => [time, byTarget.map(samples => samples.get(time))] as const));
    const successful = view.targets.flatMap(target =>
      target.samples.flatMap(sample => {
        const latency = pingLatencyMs(sample);
        return latency == null ? [] : [latency];
      }),
    );
    const valueAxis = observeValueAxis(Math.max(...successful, 0.1));
    const now = Date.now();
    const start = now - range.seconds * 1000;
    // x 轴显示墙钟时刻（hh:mm）。轴起点即首个样本时刻（无样本时退回窗口起点），曲线紧贴
    // y 轴；不再向下取整到整分，以免首点的秒数变成左端留白。见 ThroughputChart 同处说明。
    const firstMs = times.length > 0 ? times[0] * 1000 : start;
    const xStep = observeTimeInterval(now - firstMs);
    const xMin = Math.min(firstMs, now - 30_000);
    const hm = (ms: number) =>
      new Date(ms).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
    const series: Array<Record<string, unknown>> = view.targets.map((target, index) => {
      const color = colors[index % colors.length];
      return {
        name: target.name,
        type: 'line' as const,
        symbol: 'circle',
        symbolSize: 5,
        showSymbol: false,
        smooth: false,
        connectNulls: false,
        lineStyle: observeSeriesLine(color),
        areaStyle: observeAreaStyle(color, themeName, { count: view.targets.length }),
        itemStyle: { color, borderColor: glass, borderWidth: 1.5 },
        emphasis: { disabled: true },
        data: times.map(time => {
          const sample = byTarget[index].get(time);
          return [time * 1000, sample ? pingLatencyMs(sample) : null] as [number, number | null];
        }),
      };
    });
    chart.setOption(
      {
        animation: false,
        color: colors,
        grid: { left: 10, right: 14, top: 12, bottom: 10, containLabel: true },
        textStyle: { fontFamily: NET_MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: NET_MONO },
          extraCssText: 'border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.18); backdrop-filter:blur(8px);',
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' }, z: 0 },
          formatter: (params: unknown) => {
            const entries = params as { axisValue: number }[];
            const atMs = Number(entries[0]?.axisValue ?? 0);
            const at = Math.round(atMs / 1000);
            const values = valuesAt.get(at) ?? view.targets.map(() => undefined);
            const when = new Date(atMs).toLocaleString('zh-CN', {
              month: '2-digit',
              day: '2-digit',
              hour: '2-digit',
              minute: '2-digit',
              second: '2-digit',
              hour12: false,
            });
            const rows = view.targets
              .map((target, index) => ({
                target,
                index,
                sample: values[index],
                value: values[index] ? pingLatencyMs(values[index]!) : null,
              }))
              .sort(
                (left, right) => (right.value ?? Number.NEGATIVE_INFINITY) - (left.value ?? Number.NEGATIVE_INFINITY),
              )
              .map(({ target, index, sample }) => {
                return (
                  `<div style="display:flex;align-items:center;gap:7px;line-height:1.75">` +
                  `<span style="width:8px;height:8px;border-radius:2px;background:${colors[index % colors.length]};flex:none"></span>` +
                  `<span style="color:${ink3}">${html(target.name)}</span>` +
                  `<b style="margin-left:auto;color:${ink};font-weight:500">${pingSampleText(sample)}</b></div>`
                );
              })
              .join('');
            return `<div style="color:${ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${when}</div>${rows}`;
          },
        },
        xAxis: {
          // 数值轴承载毫秒时间戳：echarts 6 的 time 轴无视 interval，会把竖网格铺成 2 分钟一格；
          // 数值轴才能把主网格钉在稀疏的整分位置，同时保留次刻度。
          type: 'value',
          min: xMin,
          max: now,
          interval: xStep,
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            hideOverlap: true,
            formatter: (value: number) => hm(value),
          },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        yAxis: {
          type: 'value',
          min: 0,
          max: valueAxis.max,
          interval: valueAxis.interval,
          // 刻度只写数字，单位由 PingProbeBlock 写在标题栏（`ICMP PING (ms)`）；
          // 精度随步长走，见 observeMsUnit。tooltip 与图例仍用 pingSampleText。
          axisLabel: { color: ink3, fontSize: 9.5, margin: 8, formatter: observeMsUnit(valueAxis.interval).text },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        series,
      },
      true,
    );
    if (group) echarts.connect(group);
  }, [view, range, group, themeName, paletteKey]);

  return <div ref={elRef} className="ping-probe-chart" />;
}

function PingProbeLegend({ view }: { view: NodePingProbeView }) {
  const shown = view.targets.slice(0, 3);
  return (
    <footer className="load-network-legend ping-probe-legend" aria-label="Ping 图例">
      {shown.map((target, index) => {
        const latest = target.samples.at(-1);
        const percentile = p95(
          target.samples.flatMap(sample => {
            const latency = pingLatencyMs(sample);
            return latency == null ? [] : [latency];
          }),
        );
        const state = !latest?.attempted ? 'gap' : latest.latency_us == null ? 'loss' : undefined;
        const title = `${target.address}\nP95 ${percentile == null ? '—' : pingLatencyText(percentile)}`;
        return (
          <span key={`${target.address}-${index}`} title={title}>
            <i style={{ background: `var(${PING_SERIES_CSS[index % PING_SERIES_CSS.length]})` }} />
            <span className="ping-probe-name">{target.name}</span>
            <b className={state}>{pingSampleText(latest)}</b>
          </span>
        );
      })}
      {view.targets.length > shown.length && (
        <span className="ping-probe-more">+{view.targets.length - shown.length}</span>
      )}
    </footer>
  );
}

function PingProbeBlock({
  view,
  protocol,
  range,
  group,
  loading,
}: {
  view: NodePingProbeView;
  protocol: PingProtocol;
  range: LoadRange;
  group?: string;
  loading: boolean;
}) {
  const targets = view.targets.filter(target => target.address.startsWith(`${protocol}://`));
  const protocolView = { ...view, targets };
  const label = `${protocol.toUpperCase()} PING`;
  const hasSamples = targets.some(target => target.samples.length > 0);
  return (
    <div className="ping-probe-block" aria-label={label}>
      <div className="load-network-cap">
        <b>{label}</b>
        {/* 量纲跟着图走：没画图时刻度也不存在，标题栏就不该挂一个单位。
            时延轴永远是毫秒（见 observeMsUnit），所以这里不必反算值轴。 */}
        {hasSamples && <span className="chart-unit">({OBSERVE_MS_UNIT})</span>}
        {targets.length > 0 && <PingProbeLegend view={protocolView} />}
      </div>
      {loading ? (
        <p className="note ping-probe-empty">正在读取探测数据…</p>
      ) : targets.length === 0 ? (
        <p className="note ping-probe-empty">尚未在设置中配置 {protocol.toUpperCase()} 探测目标。</p>
      ) : hasSamples ? (
        <PingLatencyChart view={protocolView} range={range} group={group} />
      ) : (
        <p className="note ping-probe-empty">目标已经配置，尚无 Agent 样本。</p>
      )}
    </div>
  );
}

function PingProbePanel({ nodeId, range, linked }: { nodeId: string; range: LoadRange; linked: boolean }) {
  const probe = useQuery({
    queryKey: ['node-ping-probe', nodeId, range.seconds],
    queryFn: () => fetchNodePingProbe(nodeId, range.seconds),
    refetchInterval: 5_000,
    retry: false,
  });
  if (probe.error && !probe.data) return null;
  const view = probe.data ?? { node_id: nodeId, targets: [] };
  const group = linked ? `nd-ping-${nodeId}` : undefined;
  return (
    <section className="chart-card ping-probe-panel" aria-label="Ping">
      <PingProbeBlock view={view} protocol="icmp" range={range} group={group} loading={probe.isPending} />
      <PingProbeBlock view={view} protocol="tcp" range={range} group={group} loading={probe.isPending} />
    </section>
  );
}

/** 该机器本身：内核与平台、容量分母（LOAD/KPI 百分比的基数）、内核参数上限。
    组件版本不在这里；过旧或回落用户态等需要处理的结论由 CONFIGURATIONS 的 findings 呈现。 */
function HostCard({ load }: { load: NodeLoadView | undefined }) {
  const host = load?.host ?? null;
  const skew = load?.clock_skew_secs ?? null;
  /* CPU 与内核分别成行：型号可能很长，用 rt-v 截断但 title 保留完整值。旧 Agent 没有
     cpu_model 时仍显示核数；内核行只承担版本和架构，不再混入硬件信息。 */
  const cpuParts = host ? [host.cpu_model || '', host.cores > 0 ? `${host.cores} 核` : ''].filter(p => p !== '') : [];
  const kernelParts = host ? [host.kernel, host.arch].filter(p => p !== '') : [];
  const osParts = host ? [host.os_pretty, host.virt].filter(p => p !== '') : [];
  return (
    <div className="panel">
      <header>
        <h4>HOST</h4>
        <span className="sp" />
        {/* 上报时间固定在标题行右端（与 USAGE 卡一致）：它表示该卡数据的时效，
            不是卡内的一项内容——紧跟标题会被读作 HOST 的第一个字段。 */}
        {load?.reported_at_unix_secs != null && (
          <span className="hint">
            <Ago at={iso(load.reported_at_unix_secs)} /> 上报
          </span>
        )}
      </header>
      {!host ? (
        <p className="note" style={{ margin: 0 }}>
          还没有主机信息上报。
        </p>
      ) : (
        <Strip
          items={[
            [
              'CPU',
              cpuParts.length > 0 ? (
                <span className="rt-v" title={host.cpu_model || cpuParts.join(' · ')}>
                  {cpuParts.join(' · ')}
                </span>
              ) : (
                <span className="dim">—</span>
              ),
            ],
            ['内存', host.mem_total_bytes > 0 ? bytes(host.mem_total_bytes) : <span className="dim">—</span>],
            ['磁盘', host.disk_total_bytes > 0 ? bytes(host.disk_total_bytes) : <span className="dim">—</span>],
            ['发行版', osParts.length > 0 ? osParts.join(' · ') : <span className="dim">—</span>, 'newline'],
            [
              '内核',
              kernelParts.length > 0 ? (
                <span className="rt-v" title={host.kernel}>
                  {kernelParts.join(' · ')}
                </span>
              ) : (
                <span className="dim">—</span>
              ),
            ],
            [
              '时钟偏移',
              skew !== null ? (
                /* 接收上报时测得的 agent 钟减控制面钟。TLS 依赖对时；±5 秒是传输耗时的
                   噪声上限，超过即标出。 */
                <span
                  className={Math.abs(skew) >= 5 ? 'hot' : undefined}
                  title="agent 时钟减控制面时钟，接收上报时测得"
                >
                  {skew >= 0 ? '+' : '-'}
                  {Math.abs(skew)} 秒
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
              host.rmem_max > 0 || host.wmem_max > 0 ? (
                `${bytes(host.rmem_max)} / ${bytes(host.wmem_max)}`
              ) : (
                <span className="dim">—</span>
              ),
              'newline',
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
      )}
    </div>
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
  return (
    <div className="panel">
      <header>
        <h4>AGENT</h4>
      </header>
      <Strip
        items={[
          ['上次来拉', <Ago at={node.last_poll_at} />],
          ['上次用量', <Ago at={node.last_usage_report_at} />],
          [
            '用量明目',
            node.usage_last_result ? (
              <span className="mono">{node.usage_last_result.accepted_readings} 项</span>
            ) : (
              <span className="dim">—</span>
            ),
          ],
          // agent 进程自身的运行时长：崩溃循环的机器上该值恒为几分钟，与「上次来拉 6 秒前」
          // 并排即可区分「连不上」与「一直在重启」。
          [
            '已运行',
            agentStartedAt !== null ? (
              <span className="mono">{dur(Math.max(0, Math.floor(now / 1000) - agentStartedAt))}</span>
            ) : (
              <span className="dim">—</span>
            ),
          ],
          // 「构建」不等同于「版本」：该字段是二进制自身 sha256 的前 12 位。发布 agent 时
          // 需要比对的正是它——控制面上已批准的构建号与该机器实际运行的构建号。
          ['构建', agentIdent(node.agent_version)],
          [
            '协议',
            node.agent_protocol_version === null ? (
              <span className="hot">旧版 · 等待救援升级</span>
            ) : (
              <span className="mono">v{node.agent_protocol_version}</span>
            ),
          ],
          // token 字段已移除：凭据只在签发时出现一次，此处只能显示前几位，既无法核对
          // 也无法复制。需要确认的两项信息都在其他位置——连接状态见上面两个上报时间，
          // 更换凭据使用标题栏的「重签 token」。
          // 「状态巡检」指 agent 侧的本地对账（brocade-agent/src/main.rs），在控制面返回
          // 204 时执行：机器配置发生偏移后由 agent 自行修复，该字段是其执行结果。
          [
            '状态巡检',
            node.last_local_reconcile ? (
              node.last_local_reconcile.actions.length > 0 ? (
                <>
                  <Ago at={new Date(node.last_local_reconcile.at * 1000).toISOString()} />
                  <span className="dim rt-v" title={node.last_local_reconcile.actions.join('、')}>
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
            // 从新行开始：其取值可能长至「重放了 xray、grants」，与前三项同行时
            // 会使该行长度不一，而它是本卡中最需要被读取的一项。
            'newline',
          ],
          [
            '上报积压',
            node.spool_backlog ? (
              <span className="mono">
                {node.spool_backlog.observation + node.spool_backlog.usage} 条<span className="dim"> · 丢弃 </span>
                <span className={node.spool_backlog.dropped > 0 ? 'hot' : undefined}>
                  {node.spool_backlog.dropped.toLocaleString()}
                </span>
              </span>
            ) : (
              <span className="dim">—</span>
            ),
          ],
        ]}
      />
      <Findings
        list={runtimeFindings(node).filter(
          f =>
            f.chip === '自修失败' ||
            f.chip.startsWith('丢了') ||
            f.chip.startsWith('agent ') ||
            f.chip.startsWith('拒收 ') ||
            f.chip.startsWith('未识别 ') ||
            f.chip.startsWith('断点 '),
        )}
      />
    </div>
  );
}

function AppliedCard({
  node,
  revisionOf,
  wireguardEnabled,
  children,
}: {
  node: NodeAgentStateItem;
  revisionOf: (d: number) => number | undefined;
  wireguardEnabled: boolean;
  children?: ReactNode;
}) {
  const a = appliedOf(node);
  const rev = a?.source_deployment_id != null ? revisionOf(a.source_deployment_id) : undefined;
  return (
    <div className="panel">
      <header>
        <h4>CONFIGURATIONS</h4>
      </header>
      {!a ? (
        <p className="note" style={{ margin: 0 }}>
          还没收敛过。
        </p>
      ) : (
        <>
          {/* 收敛来源和逐项状态共用一个字段网格。拆成两组 Strip 时两组各自计算标签列宽，
              「来自 / 上次观察」的值起点与组件状态不在同一条竖轴上。逐项状态需要列全：
              它也是发布前判定待发布的依据，unknown 一律判定为需要操作。 */}
          <Strip
            items={[
              [
                '来自',
                <span className="mono">
                  {a.source_deployment_id != null ? `#${a.source_deployment_id}` : '—'}
                  {rev !== undefined && ` · 修订 ${rev}`}
                </span>,
              ],
              ['上次观察', <Ago at={a.observed_at ?? null} />],
              ...ARTIFACT_KINDS.map((kind): StripItem => [
                ARTIFACT_LABEL[kind],
                <StateChip state={artifactState(node, kind)} />,
              ]),
              ['授权同步', <StateChip state={a.grants?.state ?? 'unknown'} />],
            ]}
          />
          {/* 此处原有 xray 和 wg 的 sha256，已移除：这张卡表示的是收敛来源和各产物状态，
              而 sha256 用于与产物面板逐字节核对，二者用途不同；两个 64 位值还会占满一行。
              需要查看指纹时使用产物面板，那里有完整值。 */}
        </>
      )}
      {/* 版本相关的判定（xray 过旧、wg 回落用户态）挂在这张卡。版本详情已从卡片移除，
          但需要处理的结论不能随之隐藏。放在条件分支外：从未收敛过的机器同样需要这些提示。 */}
      <Findings list={runtimeFindings(node, wireguardEnabled).filter(f => f.chip !== '自修失败')} />
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
  if (hostsIngress || spineIndex === 0) roles.push('入口');
  else if (spineIndex > 0 && spineIndex === spine.length - 1) roles.push('主干末跳');
  else if (spineIndex > 0) roles.push('主干中转');
  if (ownStep && downstreams.size > 0 && spineIndex < 0) roles.push('中转');
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
}: {
  nodeId: string;
  use: NodeChainUse;
  nodes: NodeAgentStateItem[];
  readOnly?: boolean;
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
        rootSummary={
          use.ingress?.node === nodeId ? (
            <IngressPortEditor appId={use.appId} ingress={use.ingress} editable={!readOnly} compact />
          ) : undefined
        }
        readOnly={readOnly}
      />
    </div>
  );
}

function NodeChainsSection({
  id,
  inChains,
  nodes,
  canEdit,
  canCreate,
  go,
}: {
  id: string;
  inChains: NodeChainUse[];
  nodes: NodeAgentStateItem[];
  canEdit: boolean;
  canCreate: boolean;
  go: (d: Drill) => void;
}) {
  return (
    <>
      <header>
        <h4>链路规则</h4>
        <span className="rule-sheet-meta">{inChains.length} 条相关链</span>
        <span className="sp" />
        <button className="btn" disabled={!canCreate} title="以当前节点作为入口" onClick={() => go({ p: 'chain', id })}>
          添加新链
        </button>
      </header>

      {inChains.length === 0 ? null : canEdit ? (
        // 一台机器可能属于多条链，每条链又递归展开多台——若每张规则表各带一个
        // 「保存到草稿」，本屏会出现七八个。统一收敛为整段末尾的一个。
        <RuleDraftScope hint="改动落进草稿，顶栏按「提交」才写进库。">
          {inChains.map(use => (
            <NodeChainRuleTree key={`${use.appId}/${use.chain.id}`} nodeId={id} use={use} nodes={nodes} />
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
          {/* 提示置于树之外：树内每张规则表的标题在该档位下是隐藏的（见 styles.css 的
              `.chain-rule-tree .rule-editor>.toolbar:first-child`），写在内部不可见。
              整段一条即可——机器属于多条链时，每张卡各显示一次属于重复。 */}
          <p className="note" style={{ margin: '0 0 8px' }}>
            只读：规则按原样列出。
          </p>
          {inChains.map(use => (
            <NodeChainRuleTree key={`${use.appId}/${use.chain.id}`} nodeId={id} use={use} nodes={nodes} readOnly />
          ))}
        </>
      )}
    </>
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
          </div>
          {identIp && <span className="nd-ident-meta">{identIp}</span>}
        </div>
      </div>
      {toolbar}
    </header>
  );

  return (
    <div className={sheeted ? 'nd-sheet nd-page' : 'nd-page'}>
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

function NodeDetail({ id, go, sheeted = false }: { id: string; go: (d: Drill) => void; sheeted?: boolean }) {
  const { who } = useSession();
  const qc = useQueryClient();
  // 详情页的状态灯同样依赖 last_poll_at。列表卸载后不再有它的 10 秒轮询；若这里仅命中
  // 缓存，负载查询触发重绘时会拿旧时间与当前时间比较，停留 60 秒后误报「失联」。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), refetchInterval: 10_000 });
  const deployments = useQuery({ queryKey: ['deployments'], queryFn: () => fetchDeployments() });
  const revisionOf = (d: number) => deployments.data?.deployments.find(x => x.id === d)?.revision_id;
  const n = nodes.data?.nodes.find(x => x.node_id === id);
  /* 签发的 node token 同样只显示一次 */
  const [issued, setIssued] = useState<{ token: string; install_command: string } | null>(null);
  const [lifecycleAction, setLifecycleAction] = useState<'retire' | 'restore' | 'abandon' | null>(null);
  const [forceReason, setForceReason] = useState('');

  // 身份表单始终可编辑，不再设置「改名称 / 公网 IP」开关。
  // 取消编辑模式不会导致误改：不保存则不生效，且「保存到草稿」只在有修改时出现。
  const [form, setForm] = useState<{
    name: string;
    public_ipv4: string;
    public_ipv6: string;
    public_ipv4_nat: boolean;
    public_ipv6_nat: boolean;
  } | null>(null);
  const base = {
    name: n?.name ?? '',
    public_ipv4: n?.public_ipv4 ?? '',
    public_ipv6: n?.public_ipv6 ?? '',
    public_ipv4_nat: n?.public_ipv4_nat ?? false,
    public_ipv6_nat: n?.public_ipv6_nat ?? false,
  };
  const cur = form ?? base;
  const dirty =
    form !== null &&
    (form.name !== base.name ||
      form.public_ipv4 !== base.public_ipv4 ||
      form.public_ipv6 !== base.public_ipv6 ||
      form.public_ipv4_nat !== base.public_ipv4_nat ||
      form.public_ipv6_nat !== base.public_ipv6_nat);

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
      setLifecycleAction(null);
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['deployments'] });
    },
  });
  const abandon = useMutation({
    mutationFn: (reason: string) => abandonNode(id, reason, true),
    onSuccess: () => {
      setLifecycleAction(null);
      setForceReason('');
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['deployments'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
    },
  });

  /* 验证接入：执行一次不做任何修改的空收敛，确认该机器是否已与当前模型一致 */
  const verify = useMutation({ mutationFn: () => verifyDeployment({ node_id: id }) });

  /* ── 页签角标的两个来源 ──
     分页之后另外两页不在屏幕上，「那边有事」只能由页签自己说出来，否则在配置页改地址时
     不会知道观测页刚出了 finding。两处都复用已有的查询键，不引入新的取数：
     `['node-load-history', id, 30m]` 与 LoadCardFor 的默认范围共用，为 HOST 和 Agent
     进程提供始终较新的低频事实。用户切到更长范围后，长历史另走带范围的缓存键，
     不会让 HOST 因为 24 小时查询而延迟刷新；
     跳的存活同理，与下面的 HopHealth 共用 useHopStats。 */
  const load = useQuery({
    queryKey: ['node-load-history', id, 30 * 60],
    queryFn: () => fetchNodeLoad(id, 60),
    refetchInterval: 10_000,
    retry: false,
  });
  const { dead } = useHopStats(id);

  /* 页签不进地址栏。`Drill` 的字段是地址中的路径段，而 route.ts 的 parse 要求段数不少于
     字段数——给 `node` 补一个可选的第四段会让已有的 `#/nodes/node/<id>` 解析失败，退回
     机器列表。页签是一次浏览中的位置，不是可分享的位置，留在组件里即可。
     换一台机器时回到观测：id 变了而状态还在，是上一台的阅读位置。 */
  const [tabState, setTabState] = useState<{ id: string; tab: NodeTab }>({ id, tab: 'observed' });
  const setTab = (tab: NodeTab) => setTabState({ id, tab });
  const [loadRangeState, setLoadRangeState] = useState<{ id: string; range: LoadRange }>({
    id,
    range: LOAD_RANGES[0],
  });

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
  const pub = isPublic(who);
  const onSaved = () => {
    refresh();
    qc.invalidateQueries({ queryKey: ['revisions'] });
  };

  if (!n) return <Loading sheeted={sheeted} />;

  const activeOrders = (deployments.data?.deployments ?? []).filter(
    deployment => deployment.active && ['planned', 'running', 'halted'].includes(deployment.status),
  );
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

  // 规则页同时承载机器 DNS 策略和链路规则，所以没有加入链路的机器也保留这一页；两块
  // 各自显示空状态，不能再把“无链路”等同于“没有规则页面”。
  const tab: NodeTab = tabState.id !== id ? 'observed' : tabState.tab;
  const loadRange = loadRangeState.id === id ? loadRangeState.range : LOAD_RANGES[0];
  /* 同一观测页里的图表始终共享时间位置与 Tooltip，不再把页面级一致行为做成用户开关。 */
  const chartsLinked = true;

  /* 观测页的角标数。此处不按 `detailOnly` 过滤：那个标记的含义是「列表里不占标记位，
     进详情页才读」，而这里就是详情页——角标指向的正是它下面那几张卡里会展开的说明。
     跳不通与 finding 合计成一个数：两者在这一页上是同一件事，「有几处要看」。 */
  const findingCount = runtimeFindings(n, wireguardEnabled).length + dead.length;
  const lamp = nodeLampState(n, wireguardEnabled);

  /* A1 页头：身份、页签与操作共用 sheet 内的一条横梁，当前页由满宽底部刻度标记。 */
  const detailToolbar = (
    <>
      <div className="nd-tabs" role="tablist">
        <div className="nd-tabs-seg">
          <button role="tab" aria-selected={tab === 'observed'} onClick={() => setTab('observed')}>
            <Icon of="observe" size={14} className="nd-tab-ic" />
            观测
            {/* 金色数字＝有几处要看。零时不画，一个常驻的「0」会被当成一种状态。 */}
            {findingCount > 0 && <span className="nd-tab-badge gold">{findingCount}</span>}
          </button>
          <button role="tab" aria-selected={tab === 'config'} onClick={() => setTab('config')}>
            <Icon of="config" size={14} className="nd-tab-ic" />
            配置
            {/* 圆点＝身份表单有未保存的改动，与身份面板的保存条使用同一个判定。 */}
            {dirty && <span className="nd-tab-badge dot" />}
          </button>
          <button role="tab" aria-selected={tab === 'chains'} onClick={() => setTab('chains')}>
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
            <ObserveRangeControl value={loadRange} onChange={range => setLoadRangeState({ id, range })} />
          )}
          {!pub && (
            <div className="nd-acts">
              <div className="fg-menuwrap">
                {/* stopPropagation 是必须的：document 级关闭监听在 effect 里绑定，
                而打开菜单的这次点击绑定发生后仍会冒泡到 document——不拦下，
                菜单开同一瞬间又被自己关掉（shell.tsx 的两个菜单同样拦）。 */}
                <button
                  className="btn fg-more"
                  aria-label="更多操作"
                  onClick={e => {
                    e.stopPropagation();
                    setActsOpen(v => !v);
                  }}
                >
                  ⋯
                </button>
                {actsOpen && (
                  <div className="fg-menu" onClick={() => setActsOpen(false)}>
                    {tab === 'observed' && (
                      <button disabled={!verifyAllowed || verify.isPending} onClick={() => verify.mutate()}>
                        {verify.isPending ? '验证中…' : '验证接入'}
                        <small>空跑一次收敛，确认这台与当前模型一致，不改任何配置</small>
                      </button>
                    )}
                    <button
                      disabled={!system || issue.isPending || n.lifecycle_phase !== 'active'}
                      onClick={() => issue.mutate()}
                    >
                      {n.token_prefix && !n.token_revoked_at ? '重签 token' : '签发 token'}
                      <small>凭据只显示一次；重签后旧 token 立即失效</small>
                    </button>
                    <hr />
                    <button
                      className={n.lifecycle_phase === 'active' ? 'dg' : undefined}
                      disabled={!system || retire.isPending}
                      onClick={() => {
                        retire.reset();
                        setLifecycleAction(n.lifecycle_phase === 'active' ? 'retire' : 'restore');
                      }}
                    >
                      {retire.isPending ? '提交中…' : n.lifecycle_phase === 'active' ? '退役机器' : '恢复机器'}
                      <small>
                        {n.lifecycle_phase === 'active'
                          ? '立即创建完整停用发布；不会删除机器记录'
                          : '递增生命周期代次并创建恢复发布；需要重新签发 Token'}
                      </small>
                    </button>
                    {n.lifecycle_phase === 'retiring' && (
                      <>
                        {teardownNeedsRepair && (
                          <button
                            disabled={!system || retire.isPending}
                            onClick={() => {
                              retire.reset();
                              setLifecycleAction('retire');
                            }}
                          >
                            重建停用发布
                            <small>当前没有可继续执行的停用单；沿用本次退役代次重新规划</small>
                          </button>
                        )}
                        <button
                          className="dg"
                          disabled={!system || abandon.isPending}
                          onClick={() => {
                            abandon.reset();
                            setForceReason('');
                            setLifecycleAction('abandon');
                          }}
                        >
                          {abandon.isPending ? '处理中…' : '强制退役'}
                          <small>仅用于机器永久失联；结果会保留为警告状态</small>
                        </button>
                      </>
                    )}
                    {n.lifecycle_deployment_id && (
                      <button onClick={() => openTabByKey('deploy')}>
                        查看发布 #{n.lifecycle_deployment_id}
                        <small>打开发布页查看停用动作与波次确认</small>
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
      {lifecycleAction === 'retire' && (
        <Confirm
          title={`退役 ${nodeLabel(n)}`}
          confirmLabel={retire.isPending ? '正在创建…' : '创建停用发布'}
          confirmDisabled={retire.isPending}
          onCancel={() => setLifecycleAction(null)}
          onConfirm={() => retire.mutate('retired')}
          body={
            <div className="node-lifecycle-confirm">
              <p>这不是隐藏机器。系统会先提交退役修订，再让 Agent 确认以下四项都已停用：</p>
              <div className="node-lifecycle-artifacts">
                <span>Xray</span>
                <span>WireGuard</span>
                <span>Phantun</span>
                <span>HY2 端口跳转</span>
              </div>
              {activeOrders.length > 0 ? (
                <p className="callout warn">
                  当前活动发布 {activeOrders.map(order => `#${order.id}`).join('、')}{' '}
                  会被取消，并按最新修订创建替代发布。
                </p>
              ) : (
                <p className="note">没有冲突中的活动发布；确认后直接创建停用发布。</p>
              )}
              <p className="note">Agent Token 会保留到停用回报成功，随后自动撤销；机器记录和发布历史不会删除。</p>
              {retire.error && <ErrorBox error={retire.error} />}
            </div>
          }
        />
      )}
      {lifecycleAction === 'restore' && (
        <Confirm
          title={`恢复 ${nodeLabel(n)}`}
          confirmLabel={retire.isPending ? '正在恢复…' : '恢复并创建发布'}
          danger={false}
          confirmDisabled={retire.isPending}
          onCancel={() => setLifecycleAction(null)}
          onConfirm={() => retire.mutate('active')}
          body={
            <div className="node-lifecycle-confirm">
              <p>恢复会递增生命周期代次，因此退役前遗留的发布仍然无效；系统将按当前模型创建新的收敛发布。</p>
              <p className="callout warn">旧 Agent Token 不会复用。恢复后需要重新签发 Token，机器才能领取新发布。</p>
              {retire.error && <ErrorBox error={retire.error} />}
            </div>
          }
        />
      )}
      {lifecycleAction === 'abandon' && (
        <Confirm
          title={`强制退役 ${nodeLabel(n)}`}
          confirmLabel={abandon.isPending ? '正在处理…' : '强制退役'}
          requireWord="强制退役"
          confirmDisabled={abandon.isPending || forceReason.trim().length === 0}
          onCancel={() => {
            setLifecycleAction(null);
            setForceReason('');
          }}
          onConfirm={() => abandon.mutate(forceReason.trim())}
          body={
            <div className="node-lifecycle-confirm">
              <p className="callout err">
                控制面将不再等待这台机器，但无法证明远端 Xray、WireGuard 或监听端口已经停止。
              </p>
              <label className="field">
                <span>原因</span>
                <textarea
                  className="f"
                  rows={3}
                  autoFocus
                  placeholder="例如：机器已由供应商销毁，无法再连接"
                  value={forceReason}
                  onChange={event => setForceReason(event.target.value)}
                />
              </label>
              <p className="note">确认后立即撤销 Agent Token、封住旧发布代次，并尝试注销该机器的托管 WARP 设备。</p>
              {abandon.error && <ErrorBox error={abandon.error} />}
            </div>
          }
        />
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
            <button className="btn" onClick={() => void copyText(issued.token)}>
              复制 token
            </button>
            <button className="btn primary" onClick={() => void copyText(issued.install_command)}>
              复制命令
            </button>
            <span className="sp" />
            <button className="btn" onClick={() => setIssued(null)}>
              我抄好了
            </button>
          </div>
        </div>
      )}

      {(issue.error || verify.error || retire.error || abandon.error) && (
        <ErrorBox error={issue.error ?? verify.error ?? retire.error ?? abandon.error} />
      )}

      {tab === 'observed' && (
        /* LOAD 是监控页的第一视图；Ping 紧随流量曲线，三张机器状态卡顺延到下一块。 */
        <section className="nd-tab-observed">
          <LoadCardFor nodeId={id} range={loadRange} linked={chartsLinked} />
          {/* 吞吐（网卡 + XRAY）与 Ping（ICMP + TCP）分别同卡堆叠，两栏并排。 */}
          <div className="nd-observe-throughput">
            <ThroughputPanel nodeId={id} range={loadRange} linked={chartsLinked} />
            <PingProbePanel nodeId={id} range={loadRange} linked={chartsLinked} />
          </div>
          <div className="nd-observed-status">
            <AgentCard
              node={n}
              agentStartedAt={load.data?.processes.find(p => p.proc === 'agent')?.started_at_unix_secs ?? null}
            />
            {/* HOST 的数据来自负载上报（host facts 是其中的低频段），与节点状态是两个通道。 */}
            <HostCard load={load.data} />
            {/* AGENT → HOST → CONFIGURATIONS 是一组状态事实。验证结果仍留在收敛卡内：
                  页头的「验证接入」调用同一接口，不再产生另一块重复结果。 */}
            <AppliedCard node={n} revisionOf={revisionOf} wireguardEnabled={wireguardEnabled}>
              {verify.data && (
                <div className={verify.data.converged ? 'callout blue' : 'callout warn'} style={{ marginBottom: 0 }}>
                  {verify.data.converged
                    ? `已收敛：跟修订 ${verify.data.revision_id} 一致，没有要改的。`
                    : `还没对齐修订 ${verify.data.revision_id}：有变更待推（${
                        verify.data.targets.find(t => t.node_id === id)?.actions.join('、') ?? '—'
                      }）。到「发布 → 计划预览」创建 deployment 才会推下去。`}
                </div>
              )}
            </AppliedCard>
          </div>
        </section>
      )}

      {/* ── 写入控制面模型的配置。修改后先进入草稿。 ──
            两列而不是铺满：表单行是 72px 标签加 220px 输入，整幅宽度只会让标签和值之间
            空出一大片。左列是这台机器的对外属性（身份 → 骨干网），右列是它怎么解析、
            出示什么证书、连接参数。 */}
      {tab === 'config' && (
        <section className="nd-tab-config">
          <div>
            <div className="panel config-panel">
              <header>
                <h4>身份</h4>
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

            <WgCard node={n} canEdit={system} onSaved={onSaved} />
          </div>

          <div>
            <DnsCard node={n} canEdit={system} onSaved={onSaved} />
            {/* 位于 DNS 之后：证书组决定这台机器出示的 SNI，和上面几张卡一样是它的对外
                属性；与它们不同的是改动不经过发布，所以卡里自己就保存了。 */}
            <CertGroupCard node={n} canEdit={system} />
            {/* 位于最后且默认折叠：多数机器在该卡上的内容是与机队默认一致，
                而上面几张卡各机器均不相同。标题中的说明已表明是否有差异，需要细节时再展开。 */}
            <ConnectionCard node={n} canEdit={system} onSaved={onSaved} />
          </div>
        </section>
      )}

      {tab === 'chains' && (
        <section className="nd-tab-rules">
          <div className="panel config-panel rule-sheet-card node-egress-rule-sheet">
            <MachineEgressDnsRules
              key={id}
              nodeId={id}
              nodeName={n.name || id}
              readOnly={!can(who.role, 'edit')}
              showHeader
            />
          </div>
          <div className="panel config-panel rule-sheet-card node-chain-sheet">
            <NodeChainsSection
              id={id}
              inChains={inChains}
              nodes={nodes.data?.nodes ?? []}
              canEdit={can(who.role, 'edit')}
              canCreate={can(who.role, 'edit')}
              go={go}
            />
          </div>
        </section>
      )}
    </NodeDetailLayout>
  );
}

// ══ 纳管向导 ══
//
// 版面结构与建链向导相同（.wz-* 系列）。
// 两种状态，不使用步骤条：机器尚未入库（一张表单），以及机器已入库但尚未上线
// （一条命令加一个状态指示）。步骤条表达的当前进度由这两种状态本身表示——页面标题从
// 「纳管向导 · 新机器」变为「纳管向导 · hk-01」，比高亮第几格更直接。
//
// 此前分为五步，其中第 2、3 屏（store 补全、诊断）显示的是创建时的响应，从地址栏
// 返回后不再存在，代码中还需要额外的提示说明这两屏只出现一次。它们是结果而非操作步骤，
// 现已收入结果卡的折叠区。
//
// 与建链向导的主要差异：建链向导写入的全部是草稿，提交前可随时丢弃；此处的按钮
// 点击后立即写库并产生一版修订，没有草稿也无法撤销。因此主按钮不使用「提交」，
// 页脚的提示文字也不是装饰性内容。

function Provision({ drill, go }: { drill: WizDrill; go: (d: Drill) => void }) {
  // 存在 node 表示机器已入库，本屏切换为结果卡；
  // result 是创建时的响应，从地址栏返回时不存在。
  const node = drill.p === 'install' ? drill.node : null;
  const result = drill.p === 'install' ? drill.result : undefined;
  if (node) return <ProvisionResult node={node} result={result} go={go} />;
  return <ProvisionForm go={go} />;
}

/* 第一种状态：机器尚未入库。 */
function ProvisionForm({ go }: { go: (d: Drill) => void }) {
  const qc = useQueryClient();
  const { who } = useSession();
  /* GET /tenants 在服务端已按操作者的租户子树过滤，因此该列表即为其可见范围。 */
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const options = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  /* 已有的机器：用于判断 id 是否被占用。 */
  const existingNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  /* 证书组的下拉选项。取不到就只剩「不关联证书」一项，与没有权限读证书配置时的表现一致。 */
  const certs = useQuery({ queryKey: ['certs'], queryFn: () => fetchCerts(), retry: false });

  const [form, setForm] = useState({
    id: '',
    name: '',
    public_ipv4: '',
    public_ipv6: '',
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    tenant_id: '',
    wg_listen_port: 51820,
    api_port: 10085,
    overlay: true,
    egress_allowed: true,
    dns: 'system',
    domain_strategy: 'use_ip' as DomainStrategy,
    cert_label_id: '',
  });

  // id 是 slug：小写字母、数字、点、下划线、连字符，最长 32，与服务端
  // brocade_core::model::is_valid_slug 的规则一致。在输入时校验，不等到提交后返回 400。
  const SLUG_RE = /^[a-z0-9._-]{1,32}$/;
  const idInvalid = form.id !== '' && !SLUG_RE.test(form.id);
  const idTaken = form.id.trim() !== '' && (existingNodes.data?.nodes ?? []).some(n => n.node_id === form.id.trim());

  // 归属租户取默认值：操作者绑定了子树时用该子树，只有一个租户时用该租户，
  // 否则取排序后的第一个。只有需要指定时才修改该项。
  const defaultTenant = options.find(t => t.id === who.tenant_scope)?.id ?? (options.length ? options[0].id : '');
  const tenantId = form.tenant_id || defaultTenant;

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
        public_ipv4_nat: form.public_ipv4_nat,
        public_ipv6_nat: form.public_ipv6_nat,
        wg_listen_port: Number(form.wg_listen_port),
        api_port: Number(form.api_port) || null,
        overlay: form.overlay,
        egress_allowed: form.egress_allowed,
        dns,
        domain_strategy: form.domain_strategy,
        cert_label_id: form.cert_label_id || null,
      });
    },
    onSuccess: result => {
      qc.invalidateQueries({ queryKey: ['nodes'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      // 该操作完成后机器已入库，修订号也递增一版。此后的标识是 node_id：
      // 此时切换离开再返回，进入的是结果卡而非空白表单。
      go({ p: 'install', node: result.node.id, step: 2, result });
    },
  });

  const ready = !!form.id.trim() && !idInvalid && !idTaken && !!tenantId;

  return (
    <form
      className="wz"
      onSubmit={e => {
        e.preventDefault();
        provision.mutate();
      }}
    >
      <div className="chain-hd">
        <b>纳管向导</b>
        <span className="subid mono">新机器</span>
      </div>

      <div className="wz-fields">
        <div className="wz-fld">
          <label>机器 ID</label>
          <input
            className="f mono"
            value={form.id}
            placeholder="hk-01"
            onChange={e => setForm({ ...form, id: e.target.value })}
          />
          {idInvalid ? (
            <p className="note warn">只能使用 a-z 0-9 . _ -，最长 32 个字符。</p>
          ) : idTaken ? (
            <p className="note warn">该 ID 已存在。</p>
          ) : (
            <p className="note">唯一键，创建后不可修改。可用字符：a-z 0-9 . _ -</p>
          )}
        </div>
        <div className="wz-fld">
          <label>机器名称</label>
          <input
            className="f"
            value={form.name}
            placeholder="香港入口"
            onChange={e => setForm({ ...form, name: e.target.value })}
          />
          <p className="note">列表和拓扑图上显示的名称，可随时修改</p>
        </div>
        <div className="wz-fld">
          <label>归属租户</label>
          {/* 始终使用下拉框，只有一个选项时同样如此——与建链向导的该字段规则一致：
              同一字段在不同情况下使用两种控件时，需要先判断当前是否可修改。 */}
          <select className="f" value={tenantId} onChange={e => setForm({ ...form, tenant_id: e.target.value })}>
            {options.length === 0 && <option value="">还没有租户</option>}
            {options.map(t => (
              <option key={t.id} value={t.id}>
                {t.name && t.name !== t.id ? `${t.name}（${t.id}）` : t.id}
              </option>
            ))}
          </select>
          <p className="note">创建后不可修改。</p>
        </div>
      </div>

      {/* ── 网络：一条 IP 一行，NAT 置于行内 ── */}
      <h4 className="sec">
        网络
        <span className="rule" />
      </h4>
      <div className="wz-hops">
        <div className="wz-hop">
          <span className="idx">v4</span>
          <span className="who">
            <b>公网 IPv4</b>
            {form.public_ipv4.trim() ? (
              form.public_ipv4_nat ? (
                <span className="st">经 NAT，不可直连</span>
              ) : (
                <span className="st b-role">可直连</span>
              )
            ) : (
              <span className="st">未填写</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="attr">
              <span className="k">地址</span>
              <input
                className="f mono"
                style={{ width: 190 }}
                value={form.public_ipv4}
                placeholder="203.0.113.10"
                onChange={e => setForm({ ...form, public_ipv4: e.target.value })}
              />
            </span>
            <span className="attr">
              <span className="k">可达</span>
              <SegSwitch
                checked={form.public_ipv4_nat}
                onChange={checked => setForm({ ...form, public_ipv4_nat: checked })}
                off="直连"
                on="经 NAT"
              />
            </span>
            <span className="attr">
              <span className="note">留空 = 该机器没有 IPv4 入口。</span>
            </span>
          </span>
        </div>

        <div className="wz-hop">
          <span className="idx">v6</span>
          <span className="who">
            <b>公网 IPv6</b>
            {form.public_ipv6.trim() ? (
              form.public_ipv6_nat ? (
                <span className="st">经 NAT，不可直连</span>
              ) : (
                <span className="st b-role">可直连</span>
              )
            ) : (
              <span className="st">未填写</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="attr">
              <span className="k">地址</span>
              <input
                className="f mono"
                style={{ width: 220 }}
                value={form.public_ipv6}
                placeholder="2001:db8::10"
                onChange={e => setForm({ ...form, public_ipv6: e.target.value })}
              />
            </span>
            <span className="attr">
              <span className="k">可达</span>
              <SegSwitch
                checked={form.public_ipv6_nat}
                onChange={checked => setForm({ ...form, public_ipv6_nat: checked })}
                off="直连"
                on="经 NAT"
              />
            </span>
            <span className="attr">
              <span className="note">填写地址本身，不带方括号。</span>
            </span>
          </span>
        </div>
      </div>

      {/* ── 角色：两个二选一开关，与上面的 NAT 使用同一种控件 ── */}
      <h4 className="sec">
        角色
        <span className="rule" />
      </h4>
      <div className="wz-fields">
        <div className="wz-fld">
          <label>WireGuard</label>
          <div>
            <SegSwitch
              checked={form.overlay}
              onChange={checked => setForm({ ...form, overlay: checked })}
              off="关闭"
              on="启用"
            />
          </div>
          <p className="note">启用 = 分配一个 overlay 地址，与其他成员全互联。</p>
        </div>
        <div className="wz-fld">
          <label>出网</label>
          <div>
            <SegSwitch
              checked={form.egress_allowed}
              onChange={checked => setForm({ ...form, egress_allowed: checked })}
              off="禁止"
              on="允许"
            />
          </div>
          <p className="note">禁止时它只能作为中转，指向它的落地规则会在编译时被拒绝。</p>
        </div>
        {/* 和上面两项同属角色，因此不收进折叠区：它决定这台机器能不能承载 TLS 与 Hysteria 2，
            与「能不能出网」是同一层的取舍。而且建完再改会让已发出去的订阅失效——组决定 SNI，
            SNI 写进订阅，所以这个选择必须在建机器时就看得见。 */}
        <div className="wz-fld">
          <label>证书组</label>
          <div>
            <select
              className="f"
              value={form.cert_label_id}
              onChange={e => setForm({ ...form, cert_label_id: e.target.value })}
            >
              <option value="">不关联证书</option>
              {(certs.data?.groups ?? []).map(g => (
                <option key={g.id} value={g.id}>
                  {g.name} · {g.names[1] ?? g.names[0]}
                </option>
              ))}
            </select>
          </div>
          <p className="note">
            {form.cert_label_id
              ? '出示这个组的证书，与组内其他机器相同。组内换证书不改 SNI，已发出去的订阅继续可用。'
              : '不关联证书组：这台机器上的 TLS 与 Hysteria 2 接入面会在编译时被拒绝。REALITY 指向外部站点的不受影响。'}
          </p>
        </div>
      </div>

      {/* 端口和 DNS 属于需要专业判断才修改的字段，收入折叠区；默认值适用于多数机器 */}
      <details className="wz-adv">
        <summary>端口和 DNS（默认值对绝大多数机器不用改）</summary>
        <div className="wz-fields">
          <div className="wz-fld">
            <label>WireGuard 端口</label>
            <input
              className="f mono"
              value={form.wg_listen_port}
              inputMode="numeric"
              onChange={e => setForm({ ...form, wg_listen_port: Number(e.target.value) })}
            />
            <p className="note">对端连入使用的端口，仅在本机该端口被占用时修改。</p>
          </div>
          <div className="wz-fld">
            <label>xray 管理端口</label>
            <input
              className="f mono"
              value={form.api_port}
              inputMode="numeric"
              onChange={e => setForm({ ...form, api_port: Number(e.target.value) })}
            />
            <p className="note">留空 = 不开启。</p>
          </div>
          <div className="wz-fld">
            <label>DNS</label>
            <input
              className="f mono"
              value={form.dns}
              placeholder="system 或 1.1.1.1,8.8.8.8"
              onChange={e => setForm({ ...form, dns: e.target.value })}
            />
            <p className="note">system = 使用系统解析；也可填写一组地址。</p>
          </div>
          {/* 位于 DNS 之后：前者表示向谁查询，后者表示如何使用查询结果，顺序连贯。
              说明位置显示的是当前选择的影响，而非该字段的定义——选项是
              UseIPv4v6 这类原值，单条静态说明无法覆盖六个档位。 */}
          <div className="wz-fld">
            <label>域名解析</label>
            <select
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
            <p
              className="note"
              style={
                form.domain_strategy === 'use_ipv4' ||
                form.domain_strategy === 'use_ipv6' ||
                form.domain_strategy === 'as_is'
                  ? { color: 'var(--gold)' }
                  : undefined
              }
            >
              {strategyNote(form.domain_strategy, parseDns(form.dns))}
            </p>
          </div>
        </div>
      </details>

      {tenants.data && tenants.data.tenants.length === 0 && (
        <div className="callout warn" style={{ marginTop: 12 }}>
          还没有租户。机器必须归属一个租户，请先在「租户」页创建。
        </div>
      )}
      {provision.error && <ErrorBox error={provision.error} />}

      <div className="wz-foot">
        {/* 这是纳管与建链的主要差异，说明置于按钮旁 */}
        <span className="note warn">
          <b>这一步没有草稿</b>：提交后立即写入库并盖出一版新修订，不可撤销。
        </span>
        <span className="sp" />
        <button type="button" className="btn" onClick={() => go({ p: 'list' })}>
          取消
        </button>
        <button className="btn primary" type="submit" disabled={!ready || provision.isPending}>
          {provision.isPending ? '纳管中…' : '纳管这台机器'}
        </button>
      </div>
    </form>
  );
}

// 第二种状态：机器已入库，但尚未上线。
//
// 一张卡包含两项内容——在机器上执行安装命令、等待其首次心跳——另有一个折叠区显示
// store 补全的字段和编译诊断。这两项都是创建时的回显，从地址栏返回后不再存在；
// 收入折叠区而非各占一屏，是因为它们仅供参考，不是需要执行的操作。
//
// 明文 token 只存在于创建时的响应中：服务端只保存 hash 和前缀
// （brocade-store/src/agent.rs），无法再次获取。因此命令区分两种情况——
// 从创建流程直接进入的显示完整命令；从地址栏返回的提供重签按钮，
// 签发新 token 后旧 token 立即失效（旧 token 未被获取过）。
function ProvisionResult({ node, result, go }: { node: string; result?: ProvisionNodeResult; go: (d: Drill) => void }) {
  const { who } = useSession();
  const system = can(who.role, 'system');
  const qc = useQueryClient();
  // 上线判定：token 被 install.sh 使用 → agent 启动 → 首次 desired 心跳。
  // agent 每 15s 拉取一次 desired 并更新 last_poll_at，脚本执行完成到首次心跳的
  // 端到端时间为 3~20 秒，3s 轮询间隔足够。查询键与其他位置相同，共享缓存。
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), refetchInterval: 3_000 });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });
  const [reissued, setReissued] = useState<{ token: string; install_command: string; token_prefix: string } | null>(
    null,
  );
  const issue = useMutation({
    mutationFn: () => issueNodeToken(node),
    onSuccess: r => {
      setReissued({ token: r.token, install_command: r.install_command, token_prefix: r.token_prefix });
      qc.invalidateQueries({ queryKey: ['nodes'] });
    },
  });

  const command = reissued?.install_command ?? result?.enrollment?.install_command;
  const prefix = reissued?.token_prefix ?? result?.enrollment?.token_prefix;
  const nodeRow = (nodes.data?.nodes ?? []).find(x => x.node_id === node);
  const nodeLabel = nodeRow?.name || node;

  const probe = useAgentLiveness(nodeRow);

  // 证书与本屏的另外两步不同：它不经过下发流程，也不需要等待机器上线——控制面签发后入库，
  // agent 每十分钟一轮自行获取。放在此处是因为**缺少证书时 TLS / Hysteria 2 接入面无法编译**
  // （`ingress.tls-no-certificate`），而该错误只在建链时出现，那时已离开本屏。
  // 纳管阶段确认证书状态是唯一及时的时机。
  const certs = useQuery({
    queryKey: ['certs'],
    queryFn: () => fetchCerts(),
    // 创建机器需要 system-admin 权限，因此此处通常可以获取；获取失败时整段不显示，
    // 而不是显示错误提示。
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
  const summary = compile.data?.summary;

  return (
    <>
      <div className="chain-hd">
        <b>纳管向导</b>
        <span className="subid mono">
          {nodeLabel} / {node}
        </span>
      </div>

      <div className="callout blue" style={{ marginBottom: 14 }}>
        <b>{nodeLabel} 已经进库</b>
        {result ? (
          <>
            ，盖出<b>修订 {result.revision_id}</b>
          </>
        ) : null}
        。下面两件事做完它才真的上线。
      </div>

      <div className="wz-hops">
        <div className="wz-hop">
          <span className="idx">01</span>
          <span className="who">
            <b>在这台机器上执行</b>
            {/* 已上线表示该步骤已完成——此时再显示命令未执行的提示已无意义
                （从地址栏返回的情况下，命令本身只提供一次）。 */}
            {probe?.state === 'online' ? (
              <span className="st st-succeeded">已装好</span>
            ) : command ? (
              <span className="st st-gold">token 只显示这一次</span>
            ) : (
              <span className="st st-warn">命令没接住</span>
            )}
          </span>
          <span className="ctl">
            {command ? (
              <button className="btn sm" onClick={() => void copyText(command)}>
                复制
              </button>
            ) : (
              <button className="btn sm" disabled={!system || issue.isPending} onClick={() => issue.mutate()}>
                {issue.isPending ? '签发中…' : '重签一枚'}
              </button>
            )}
          </span>
          <span className="attrs" style={{ display: 'block' }}>
            {command ? (
              <>
                <pre className="code" style={{ margin: '0 0 6px' }}>
                  {command}
                </pre>
                <p className="note">
                  token 前缀 {prefix}…
                  {!reissued && result?.enrollment?.expires_at
                    ? ` · 到期 ${result.enrollment.expires_at}`
                    : ' · 一次性使用，不过期'}
                  。兑换后立即失效。
                </p>
              </>
            ) : (
              <p className="note">
                明文 token 只在创建时返回一次，服务端只保存 hash。机器已在库中，缺的只是带 token
                的安装命令。重新签发即可得到新的一枚，机器上原有的那枚立即作废。
              </p>
            )}
          </span>
        </div>

        <div className="wz-hop">
          <span className="idx">02</span>
          <span className="who">
            <b>等它上线</b>
            {probe?.state === 'online' ? (
              <span className="st st-succeeded">已上线</span>
            ) : probe?.state === 'polling' ? (
              <span className="st st-warn">已兑换，等心跳</span>
            ) : (
              <span className="st">等待兑换</span>
            )}
          </span>
          <span className="ctl" />
          <span className="attrs">
            <span className="note">
              {probe?.state === 'online'
                ? `agent 心跳于 ${probe.agoSec} 秒前。`
                : probe?.state === 'polling'
                  ? probe.onceOnline
                    ? 'token 已兑换，但超过 60 秒没有心跳，机器可能已离线。'
                    : 'token 已兑换。agent 启动后 15 秒内会拉取配置。'
                  : 'install.sh 尚未使用这枚 token 纳管。'}
            </span>
          </span>
        </div>

        {system && (
          <div className="wz-hop">
            <span className="idx">03</span>
            <span className="who">
              <b>拿到证书</b>
              {!certDomain ? (
                <span className="st st-warn">没有证书域</span>
              ) : !certRow ? (
                <span className="st st-warn">未选证书组</span>
              ) : certServing ? (
                <span className="st st-succeeded">已签发</span>
              ) : certFailed ? (
                <span className="st st-warn">签发失败</span>
              ) : (
                <span className="st">签发中</span>
              )}
            </span>
            <span className="ctl" />
            <span className="attrs">
              <span className="note">
                {!certDomain ? (
                  <>
                    尚未配置证书域，这台机器不会有证书，其上的 TLS 和 Hysteria 2 接入面会在<b>编译时被拒绝</b>，
                    直到建链那一步才暴露。前往<b>设置 → 证书</b>填写域名与 Cloudflare token。 REALITY
                    指向外部站点的接入面不受影响。
                  </>
                ) : !certRow ? (
                  <>
                    这台机器没有选证书组，因此没有本机证书，其上的 TLS 和 Hysteria 2 接入面会在
                    <b>编译时被拒绝</b>。在下方「证书组」里选一个。 REALITY 指向外部站点的接入面不受影响。
                  </>
                ) : certServing ? (
                  <>
                    {certRow.certificate_name} · 组 {certRow.group_name} · 签发者 {certServing.issuer ?? '未知'}
                  </>
                ) : certFailed ? (
                  <>{certFailed.last_error ?? '上一轮签发失败。'}详情见设置 → 证书。</>
                ) : (
                  <>组 {certRow.group_name} 已排入签发队列，约半分钟。</>
                )}
              </span>
            </span>
          </div>
        )}
      </div>

      {/* 创建时的回显和当前编译诊断。收入折叠区：它们仅供参考，不是需要执行的操作。 */}
      <details className="wz-adv">
        <summary>
          store 替这台补了什么
          {summary ? `（编译 ${summary.errors} 错 · ${summary.warnings} 警）` : ''}
        </summary>
        {result ? (
          <ul className="wz-ops">
            <li>
              <span className="op">overlay</span>
              <span className="arg">{result.node.overlay_addr}/32</span>
            </li>
            <li>
              <span className="op">公网 IPv4</span>
              <span className="arg">
                <IpValue value={result.node.public_ipv4} nat={result.node.public_ipv4_nat} />
              </span>
            </li>
            <li>
              <span className="op">公网 IPv6</span>
              <span className="arg">
                <IpValue value={result.node.public_ipv6} nat={result.node.public_ipv6_nat} />
              </span>
            </li>
            <li>
              <span className="op">wg 公钥</span>
              <span className="arg">{result.node.wg_public_key}</span>
            </li>
            <li>
              <span className="op">私钥</span>
              <span className="arg">已 clamp 并落库，不下发</span>
            </li>
            <li>
              <span className="op">盖出修订</span>
              <span className="arg">修订 {result.revision_id}</span>
            </li>
          </ul>
        ) : (
          <p className="note">
            以上为创建时的回显，只显示一次。当前状态见 <a onClick={() => go({ p: 'node', id: node })}>它的详情页</a>。
          </p>
        )}
        {summary && summary.errors > 0 && (
          <p className="note warn">编译有 {summary.errors} 条错误，发布会被阻止。诊断见顶栏徽章。</p>
        )}
      </details>

      <div className="wz-foot">
        <span className="note">
          纳管会变更<b>所有机器的对端表</b>。不断线，一次推送完成。
        </span>
        <span className="sp" />
        <button className="btn" onClick={() => go({ p: 'list' })}>
          回机器列表
        </button>
        <button className="btn" onClick={() => go({ p: 'node', id: node })}>
          看{nodeLabel}
        </button>
        <button
          className="btn primary"
          disabled={target == null || probe?.state !== 'online'}
          title={probe?.state === 'online' ? '' : '等 agent 上线后再发布'}
          onClick={() => {
            // 两套外壳各有一套机制：旧外壳（workbench）使用窗口和台面，新外壳使用 nav 和地址栏。
            // navigate 会创建窗口、写入 drill 并更新地址栏。
            openTabByKey('deploy');
            navigate('deploy', { p: 'plan', revision: target });
            go({ p: 'list' });
          }}
        >
          {probe?.state === 'online' ? `去发布 · 计划预览（修订 ${target ?? '…'}）` : '等 agent 上线…'}
        </button>
      </div>
      {issue.error && <ErrorBox error={issue.error} />}
    </>
  );
}
