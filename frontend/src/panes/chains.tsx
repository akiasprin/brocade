import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from 'react';
import { hopWireLabel } from '../ui/format';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchE2eProbes,
  fetchNodes,
  fetchRevisions,
  chainMembers,
  chainSpine,
  fetchSnapshot,
  fetchSettings,
  ingressUpsertBody,
  type IngressGuard,
  type Transport,
  type TransportKind,
  type Hysteria2Settings,
  type AnyTlsSettings,
  type AnyTlsMasquerade,
  type Xhttp,
  DEFAULT_XHTTP_XMUX,
  DEFAULT_XHTTP_TUNING,
  transportIsXhttp,
  type XhttpMode,
  type XhttpTuning,
  type HysteriaBbrProfile,
  type HysteriaQuic,
  HY2_QUIC_LIMITS,
  deleteStep,
  deleteChain,
  upsertApp,
  upsertChain,
  upsertIngress,
  type Rule,
  type SnapshotApp,
  type SnapshotChain,
  type SnapshotIngress,
  type E2eProbeItem,
  type E2eProbeSample,
  type UpsertIngressBody,
  type SnapshotStep,
  type IngressProjection,
  type ProjectionEndpoint,
  type RealityFallbackMode,
  type Wires,
  currentWires,
  reorderApps,
  reorderChains,
} from '../api';
import { draft } from '../draft';
import {
  compatibleXhttpMode,
  transportKindFor,
  type IngressSecurity,
} from '../ingress-transport';
import {
  fallbackLimitDraft,
  fallbackLimitsFromDraft,
  type FallbackLimitDraft,
  type FallbackRateDraft,
} from '../reality-fallback';
import { REALITY_FINGERPRINT_OPTIONS, realityFingerprintIsValid, realityServerNameIsValid } from '../reality';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading, SegSwitch } from '../ui/bits';
import { FLAG_SHEET } from '../ui/flags';
import { RegionFlag } from '../ui/region-flag';
import { ListIcon } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { ProbeBanner, byChain, toneOf, toneTitle } from '../ui/probe';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { useCrumb } from '../wm/crumb';
import {
  RuleDraftScope,
  RuleEditor,
  forwardPeers,
  isForwardTargetInChain,
  seedHops,
  type EgressDnsDraft,
  type EgressDnsOrderDraft,
  type HopsDraft,
} from './rules';
import { SLUG_MAX, freePortAcross, freeSpanAcross, isValidSlug, occupiedPorts, portClash, spanClash } from './ports';

/* Hysteria 2 的端口从此值起分配，跳转区间的默认长度同此。与 model.rs 的
 * HYSTERIA2_PORT_BASE / DEFAULT_HYSTERIA2_HOP_SPAN 保持一致——两处不一致不会报错，
 * 只会使界面给出的默认值无法通过后端校验。
 *
 * 起始值的实际来源是全局设置（`settings.ports.hy2_base`），下面的常量只是设置尚未加载
 * 时的回退值，写法与 rules.tsx 的 HOP_PORT_BASE 一致：直接使用硬编码时，运营者修改设置后
 * 界面仍会填入 18000。 */
const HY2_PORT_BASE = 18000;
const ANYTLS_PORT_BASE = 19000;
const DEFAULT_HOP_SPAN = 100;

function anyTlsHeadersText(headers: Record<string, string> | undefined): string {
  return Object.entries(headers ?? {})
    .map(([name, value]) => `${name}: ${value}`)
    .join('\n');
}

function parseAnyTlsHeaders(text: string): Record<string, string> | null {
  const headers: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const separator = line.indexOf(':');
    if (separator <= 0) return null;
    const name = line.slice(0, separator).trim();
    const value = line.slice(separator + 1).trim();
    if (!/^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$/.test(name) || /[\r\n]/.test(value) || name in headers) {
      return null;
    }
    headers[name] = value;
  }
  return headers;
}

function anyTlsPaddingValid(text: string): boolean {
  if (new TextEncoder().encode(text).length > 65_535) return false;
  const keys = new Set<string>();
  let stopSeen = false;
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const separator = trimmed.indexOf('=');
    if (separator <= 0) return false;
    const key = trimmed.slice(0, separator).trim();
    const value = trimmed.slice(separator + 1).trim();
    if (!key || !value || keys.has(key)) return false;
    keys.add(key);
    if (key === 'stop') {
      if (!/^\d+$/.test(value)) return false;
      stopSeen = true;
      continue;
    }
    if (!/^\d+$/.test(key)) return false;
    for (const token of value.split(',').map(item => item.trim())) {
      if (token === 'c') continue;
      const range = /^(\d+)\s*-\s*(\d+)$/.exec(token);
      if (!range) return false;
      const from = Number(range[1]);
      const to = Number(range[2]);
      if (!Number.isSafeInteger(from) || !Number.isSafeInteger(to) || from < 1 || from > to || to > 4 * 1024 * 1024) {
        return false;
      }
    }
  }
  return stopSeen;
}

/** Hysteria 2 的起始端口，取自全局设置；设置尚未加载时使用回退值。 */
function useHy2PortBase(): number {
  const settings = useQuery({ queryKey: ['settings'], queryFn: fetchSettings });
  return settings.data?.ports?.hy2_base || HY2_PORT_BASE;
}
import { ChainWizard } from './chain-wizard';

// 链路页：链在此页首次有独立的位置。
// 此前链只是节点详情中的一行 `chain → …`，可创建但不可修改。
//
// 一条链由一组规则和一个接入面构成。链头是接入面所在的机器（`ingress.node`），
// 主干是从链头沿 `any → Forward` 规则得出的路径——顺序写在规则表中，模型不单独存储主干。
// 校验规则见 ir/validate.rs 的 chain.no-ingress（链必须有接入面）。
// 本页提供的「按顺序重排规则」是修改该顺序的方式。

type Drill =
  | { p: 'list' }
  | { p: 'chain'; app: string; chain: string }
  // 建链独立一层，与链详情同级。不在列表内就地展开：向导现在包含路径、逐跳属性、
  // id 折叠、草稿预览等内容，置于项目头下方会把该项目的链推出可视范围，
  // 而此时需要显示的正是该项目当前有哪些链。
  | { p: 'new'; app: string };

/* 将当前下钻层级转换为外壳顶部的面包屑。顶层那一段（「线路」）由外壳补全。
   段的标签用链名而非链 id——名称是日常识别依据；drill 仍按 id 跳转。 */
const crumbOf = (d: Drill, chainName: (app: string, chain: string) => string): CrumbSeg[] =>
  d.p === 'chain' ? [{ label: chainName(d.app, d.chain) }] : d.p === 'new' ? [{ label: '建链向导' }] : [];

export function ChainsPane({ win }: { win: Win }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (d: Drill) => wm.setData(win.id, { ...win.data, drill: d });
  // 面包屑用链名。snapshot 在列表页已拉取，通常命中缓存；名称缺失或未加载时回退到链 id。
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const chainName = (app: string, chain: string) =>
    snapshot.data?.snapshot.apps.find(a => a.id === app)?.chains.find(c => c.id === chain)?.name || chain;
  useCrumb(win, crumbOf(drill, chainName));
  if (drill.p === 'chain') return <ChainDetail app={drill.app} chain={drill.chain} />;
  if (drill.p === 'new') return <NewChain app={drill.app} go={go} />;
  return <ChainList go={go} />;
}

// 建链页。项目由路由传入（入口是该项目的「＋ 新建链」），
// 链头由向导中路径的第一行指定。
function NewChain({ app, go }: { app: string; go: (d: Drill) => void }) {
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const a = snapshot.data?.snapshot.apps.find(x => x.id === app);
  const usable = (nodeList.data?.nodes ?? []).filter(n => !n.retired_at);

  if (snapshot.isPending) return <Loading />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (!a) return <ErrorBox error={new Error(`没有这条线路：${app}`)} />;

  return (
    <>
      <div className="chain-hd">
        <b>建链向导</b>
        <span className="subid mono">
          {a.label || a.id} / {a.id}
        </span>
      </div>
      {usable.length === 0 ? (
        <div className="callout">还没有机器。先去「机器」里加一台——链头就是接入面所在那台。</div>
      ) : (
        <ChainWizard
          fixedApp={{ id: a.id, label: a.label }}
          onDone={() => {
            qc.invalidateQueries({ queryKey: ['snapshot'] });
            qc.invalidateQueries({ queryKey: ['revisions'] });
            qc.invalidateQueries({ queryKey: ['compile'] });
            go({ p: 'list' });
          }}
        />
      )}
    </>
  );
}

// 使用人数统计的是用户数，不是授权条数。
// 一个用户可以通过同一条链的多个接入面接入（每个接入面一条 grant），逐条 grant 计数会把
// 同一个人算成好几个。因此此处保留用户集合，链上的人数取集合大小。
// 用户的标识是 `租户/用户`：user id 只在租户内唯一。
const userKeysOf = (a: SnapshotApp, chainId: string) =>
  new Set(
    (a.grants ?? [])
      .filter(g => (a.ingresses ?? []).some(x => x.chain === chainId && x.id === g.ingress))
      .map(g => `${g.tenant}/${g.user}`),
  );

// 删除控件。两处使用：项目列表中删除整条链、规则树中将机器移出链。
// 两处执行的是同一操作——向草稿推入一条删除——因此使用相同的样式和反馈。
// 此前一处是标准红色按钮、一处是 21px 见方的小格，同一操作有两种外观。
//
// 常驻显示，不依赖 hover。此前两处都使用 `opacity:0` 加 hover 显示，移动端无法访问
// （项目头上的「改名」也因此改为常驻）。视觉权重通过颜色控制：静止时为灰色，
// 悬停时变为红色（见 styles.css 的 .del-ctl）。
//
// 不设二次确认。删除只写入草稿，而存在草稿时快照读取的是草稿预览——点击后该行立即消失，
// 该反馈本身即为确认。顶栏草稿条会列出「删除链 xxx」，可逐条丢弃或全部丢弃，
// 提交前可查看 diff。在此处增加确认拦截的是可随时撤销的操作，
// 而不可逆的步骤在提交环节。操作后果写入 title——它需要在点击前被读到。
function DelBtn({ title, onClick }: { title: string; onClick: () => void }) {
  return (
    <button
      className="del-ctl"
      title={title}
      aria-label={title}
      onClick={e => {
        e.stopPropagation();
        onClick();
      }}
    >
      ×
    </button>
  );
}

function chainRows(apps: SnapshotApp[]) {
  return apps.flatMap(a =>
    (a.chains ?? []).map(c => {
      const userKeys = userKeysOf(a, c.id);
      return {
        app: a,
        chain: c,
        /* 主干由规则派生（api.ts 的 chainSpine），列表和详情共用同一份数据 */
        spine: chainSpine(a, c.id),
        // 成员是从链头可达的全部机器，范围大于主干——判断链上是否存在退役机器依据的是
        // 成员而非主干（见 api.ts 的 chainMembers）。
        members: chainMembers(a, c.id),
        ingress: (a.ingresses ?? []).find(g => g.chain === c.id) ?? null,
        userKeys,
        users: userKeys.size,
      };
    }),
  );
}

/* 链卡上的接入方式只写用户需要区分的层级：安全层（REALITY / TLS）、承载层
 * （普通 TCP / XHTTP）以及是否另开 HY2。端口已写在路径的入口机器上，再在这里重复会让
 * “接入方式”变成一串配置摘要。 */
function chainAccessLabel(ingress: SnapshotIngress | null): string {
  if (!ingress) return '—';
  const labels: string[] = [];
  const vless = ingress.wires.vless;
  if (vless) {
    const security = vless.kind.includes('reality') ? 'REALITY' : 'TLS';
    labels.push(transportIsXhttp(vless.kind) ? `${security} · XHTTP` : `VLESS · ${security}`);
  }
  if (ingress.wires.hysteria2) labels.push('HY2');
  if (ingress.wires.anytls) labels.push('AnyTLS');
  return labels.join(' + ') || '—';
}

function ChainExit({ probe }: { probe: E2eProbeItem | undefined }) {
  const code = probe?.status === 'ok' ? probe.exit_loc?.trim().toUpperCase() : null;
  const country = code && /^[A-Z]{2}$/.test(code) ? code : null;
  return (
    <span className={`exit${country ? '' : ' unknown'}`} title={probe ? toneTitle(probe) : '还没探过这条链'}>
      <span>出口</span>
      {country && <RegionFlag code={country} />}
      <span>{country ?? '—'}</span>
    </span>
  );
}

function ChainLatency({ probe }: { probe: E2eProbeItem | undefined }) {
  /* 显示均值而非最新一次：这个数用于比较链与链的整体快慢，单次探测只表示最后一分钟的
     抖动。窗口与下方曲线相同（samples 的近 6 小时），只计入成功的样本——失败样本的耗时
     是超时时间，与链路速度无关。当前不通时仍显示「不通」：此刻没有延迟可言，历史均值
     会把一个进行中的故障读成正常。 */
  const valid = (probe?.samples ?? []).map(probeValue).filter((v): v is number => v != null);
  const avg = valid.length > 0 ? Math.round(valid.reduce((sum, v) => sum + v, 0) / valid.length) : null;
  const shown = avg ?? probe?.ttfb_ms ?? null;
  const ok = probe?.status === 'ok' && shown != null;
  return (
    <span
      className={`chain-card-latency${ok ? '' : ' word'}`}
      title={
        !probe
          ? '还没探过这条链'
          : ok
            ? `近 6 小时 ${valid.length} 次探测的平均落点首字节，最新一次 ${probe.ttfb_ms}ms`
            : toneTitle(probe)
      }
    >
      <small>平均延迟</small>
      <span>
        <b>{ok ? shown : probe ? '不通' : '未探测'}</b>
        {ok && <em>ms</em>}
      </span>
    </span>
  );
}

type LatencyPoint = { x: number; y: number; value: number | null };
const PROBE_HISTORY_MS = 6 * 60 * 60 * 1000;
const PROBE_PLOT_HEIGHT = 36;

/** PostgreSQL's timestamptz text can contain microseconds and a short `+08` offset. Normalize
 * both forms before handing it to Date.parse so Chromium, Safari and Firefox place a sample on
 * the same six-hour axis. */
function probeTimeMs(value: string): number {
  const normalized = value
    .trim()
    .replace(' ', 'T')
    .replace(/(\.\d{3})\d+/, '$1')
    .replace(/([+-]\d{2})$/, '$1:00');
  return Date.parse(normalized);
}

/** A bounded curve between probe samples. Both control points keep an endpoint's y coordinate,
 * so a segment cannot overshoot its two measurements. Failed samples are excluded from the
 * min/max calculation and drawn as a dashed baseline instead of the false value 0ms. */
function latencyCurve(points: LatencyPoint[]): string {
  if (points.length === 0) return '';
  let d = `M${points[0].x.toFixed(2)} ${points[0].y.toFixed(2)}`;
  for (let index = 1; index < points.length; index += 1) {
    const previous = points[index - 1];
    const current = points[index];
    const middle = (previous.x + current.x) / 2;
    d += ` C${middle.toFixed(2)} ${previous.y.toFixed(2)},${middle.toFixed(2)} ${current.y.toFixed(2)},${current.x.toFixed(2)} ${current.y.toFixed(2)}`;
  }
  return d;
}

function probeValue(sample: E2eProbeSample): number | null {
  return sample.status === 'ok' && sample.ttfb_ms != null ? sample.ttfb_ms : null;
}

function ProbeLatencyPlot({ probe }: { probe: E2eProbeItem | undefined }) {
  const samples = probe?.samples ?? [];
  const exit = <ChainExit probe={probe} />;
  if (samples.length === 0) {
    return (
      // 只留文案，不画基线。容器仍占满 52px：同一行的链路卡高度必须一致。
      // 下面「探过但每次都不通」那一档仍然画线——那条红色虚线是读数，不是占位。
      <div className="history-plot chain-latency-plot empty">
        <span className="plot-label">近 6H 落点首字节 · 尚无样本</span>
        {exit}
      </div>
    );
  }

  const values = samples.map(probeValue);
  const valid = values.filter((value): value is number => value != null);
  const failures = values.length - valid.length;
  // 「落点首字节」而不是「出网延迟」：「出网」在本产品里指 Egress 那一跳（rules.tsx 的
  // 「从该机器出网」），而这个数从链头量到落点回的第一个字节，覆盖整条链加落点自身的响应，
  // 还含每一跳的握手。叫「出网延迟」既点错了范围，又会被拿去跟 ping 比。
  const label = failures > 0 ? `近 6H 落点首字节 · ${failures} 次不通` : '近 6H 落点首字节';
  if (valid.length === 0) {
    return (
      <div className="history-plot chain-latency-plot empty">
        <span className="plot-graph">
          <svg viewBox="0 0 100 36" preserveAspectRatio="none" role="img" aria-label={label}>
            <path className="down-line" d="M0 29 H100" />
          </svg>
        </span>
        <span className="plot-label">{label}</span>
        {exit}
      </div>
    );
  }

  const min = Math.min(...valid);
  const max = Math.max(...valid);
  const range = max - min;
  const sampleTimes = samples.map(sample => probeTimeMs(sample.probed_at));
  const finiteTimes = sampleTimes.filter(Number.isFinite);
  const windowEnd = finiteTimes.length > 0 ? Math.max(...finiteTimes) : NaN;
  const windowStart = windowEnd - PROBE_HISTORY_MS;
  const points = values.map<LatencyPoint>((value, index) => ({
    value,
    x: Number.isFinite(sampleTimes[index])
      ? Math.min(100, Math.max(0, ((sampleTimes[index] - windowStart) / PROBE_HISTORY_MS) * 100))
      : values.length === 1
        ? 100
        : (index / (values.length - 1)) * 100,
    y: value == null ? 29 : range === 0 ? 14 : 4 + ((max - value) / range) * 20,
  }));

  const groups: LatencyPoint[][] = [];
  for (let index = 0; index < points.length; index += 1) {
    const point = points[index];
    if (point.value == null) continue;
    if (index === 0 || points[index - 1].value == null) groups.push([]);
    groups.at(-1)?.push(point);
  }

  const downPaths: string[] = [];
  for (let index = 0; index < points.length; index += 1) {
    if (points[index].value != null) continue;
    const start = index;
    while (index + 1 < points.length && points[index + 1].value == null) index += 1;
    const end = index;
    const previous = start > 0 && points[start - 1].value != null ? points[start - 1] : null;
    const next = end + 1 < points.length && points[end + 1].value != null ? points[end + 1] : null;
    let d = previous
      ? `M${previous.x.toFixed(2)} ${previous.y.toFixed(2)} L${points[start].x.toFixed(2)} 29`
      : `M${points[start].x.toFixed(2)} 29`;
    d += ` H${points[end].x.toFixed(2)}`;
    if (next) d += ` L${next.x.toFixed(2)} ${next.y.toFixed(2)}`;
    downPaths.push(d);
  }

  const maxPoint = points[values.indexOf(max)];
  const minPoint = points[values.lastIndexOf(min)];
  const edgeClass = (point: LatencyPoint) => (point.x < 14 ? ' edge-left' : point.x > 86 ? ' edge-right' : '');
  const marker = (kind: '最高' | '最低' | '延迟', value: number, point: LatencyPoint, position: 'max' | 'min') => (
    <button
      type="button"
      className={`extreme-point ${position}${edgeClass(point)}`}
      style={{ left: `${point.x}%`, top: `${(point.y / PROBE_PLOT_HEIGHT) * 100}%` }}
      aria-label={`${kind === '延迟' ? '' : kind}延迟 ${value} ms`}
      onClick={event => event.stopPropagation()}
    >
      <span className="plot-tip">
        <span>{kind}</span>
        <b>{value}</b>
        <em>ms</em>
      </span>
    </button>
  );
  const aria = `${label}，最高 ${max} ms，最低 ${min} ms`;

  return (
    <div className="history-plot chain-latency-plot">
      <span className="plot-graph">
        <svg viewBox="0 0 100 36" preserveAspectRatio="none" role="img" aria-label={aria}>
          {groups.map((group, index) => {
            const curve = latencyCurve(group);
            const first = group[0];
            const last = group.at(-1) as LatencyPoint;
            return (
              <path
                key={`area-${index}`}
                className="area"
                d={`${curve} L${last.x.toFixed(2)} 36 L${first.x.toFixed(2)} 36Z`}
              />
            );
          })}
          {groups.map((group, index) => (
            <path key={`line-${index}`} className="line" d={latencyCurve(group)} />
          ))}
          {downPaths.map((path, index) => (
            <path key={`down-${index}`} className="down-line" d={path} />
          ))}
        </svg>
        {max === min ? marker('延迟', max, maxPoint, 'max') : marker('最高', max, maxPoint, 'max')}
        {max !== min && marker('最低', min, minPoint, 'min')}
      </span>
      <span className="plot-label">{label}</span>
      {exit}
    </div>
  );
}

/* ── 一块接入面板 = 一次提交 ──
 *
 * 面板里的每一行各有自己的草稿（端口、目标站点、回落域名、限速、XHTTP 参数），但它们
 * 提交的是同一个对象：`upsertIngress` 是整体覆盖，每一行此前各自 `ingressUpsertBody(ingress)`
 * 重建一份完整 body、只改自己那几个字段，再单独发一次。于是一块面板上并排四个保存按钮，
 * 改两处要按两次，盖出两个修订；更糟的是两行同时有草稿时，后发的那次覆盖前一次
 * ——它的 body 是从改动前的快照重建的。
 *
 * 改为：行只登记「我脏了，以及怎么把我的草稿贴到 body 上」，面板收齐后构造一份 body，
 * 依次贴上所有登记项，发一次。按钮因此只有一个，落在面板右下角，没有登记项时不渲染。
 *
 * 行不需要知道彼此，也不需要知道自己在哪块面板里：`put` 取自上下文，为空时（例如机器页
 * 里单独用的端口编辑器）行仍按自己那套保存，两条路径并存。 */
type PanelEntry = {
  blocked?: boolean;
  apply: (body: UpsertIngressBody) => UpsertIngressBody;
  /** 面板上的「还原」按下时调用：草稿在各行自己手里，面板只负责挨个通知。 */
  reset: () => void;
};
type PanelPut = (id: string, entry: PanelEntry | null) => void;

const PanelSaveCtx = createContext<PanelPut | null>(null);

/** 线路配置与机器详情配置共用的面板骨架。连通性结论仍使用 `.blk tone-*`：它是状态面，
 * 不是配置表单；其余线路配置不再维护第二套 `.blk/.blk-hd/.blk-bd` 材质。 */
function ConfigPanel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="panel config-panel">
      <header>
        <h4>{title}</h4>
      </header>
      {children}
    </section>
  );
}

/** 行侧登记。`watch` 里放草稿的当前取值——它变了就要重新登记，否则面板拿到的
    还是上一次渲染时捕获的那份 apply。 */
function usePanelEntry(id: string, dirty: boolean, entry: PanelEntry, watch: unknown) {
  const put = useContext(PanelSaveCtx);
  const blocked = entry.blocked;
  const apply = entry.apply;
  const reset = entry.reset;
  useLayoutEffect(() => {
    if (!put) return;
    put(id, dirty ? { blocked, apply, reset } : null);
    return () => put(id, null);
    // apply 每次渲染都是新函数，不能进依赖；watch 覆盖了它闭包里的全部草稿取值。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [put, id, dirty, blocked, watch]);
  return put !== null;
}

export function IngressPanel({
  appId,
  ingress,
  title,
  editable,
  children,
}: {
  appId: string;
  ingress: SnapshotIngress;
  title: string;
  editable: boolean;
  children: React.ReactNode;
}) {
  const qc = useQueryClient();
  const [entries, setEntries] = useState<Record<string, PanelEntry>>({});
  const put = useCallback<PanelPut>((id, entry) => {
    setEntries(prev => {
      if (!entry) {
        if (!(id in prev)) return prev;
        const next = { ...prev };
        delete next[id];
        return next;
      }
      return { ...prev, [id]: entry };
    });
  }, []);

  const pending = Object.values(entries);
  const blocked = pending.some(entry => entry.blocked);
  const save = useMutation({
    mutationFn: () => {
      const base = ingressUpsertBody(ingress);
      return upsertIngress(
        appId,
        pending.reduce((body, entry) => entry.apply(body), base),
        base,
      );
    },
    onSuccess: async () => {
      /* 先等快照回来再让各行收草稿：行的 dirty 是「草稿 ≠ 快照」，快照未更新时
         它们仍是脏的，会立刻重新登记一遍。 */
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });

  return (
    <PanelSaveCtx.Provider value={put}>
      <ConfigPanel title={title}>
        <dl className="kv form2 chain-face fill">{children}</dl>
        {save.error && <ErrorBox error={save.error} />}
        <footer className="config-panel-savebar">
          <span className="sp" />
          <button
            type="button"
            className="btn"
            disabled={pending.length === 0 || save.isPending}
            onClick={() => pending.forEach(entry => entry.reset())}
          >
            还原
          </button>
          <button
            type="button"
            className={pending.length > 0 && !blocked ? 'btn primary' : 'btn'}
            disabled={!editable || pending.length === 0 || blocked || save.isPending}
            title={blocked ? '有一项填得不对，先改好' : pending.length === 0 ? '没有未保存的修改' : ''}
            onClick={() => save.mutate()}
          >
            {save.isPending ? '保存中…' : '保存'}
          </button>
        </footer>
      </ConfigPanel>
    </PanelSaveCtx.Provider>
  );
}

export function IngressPortEditor({
  appId,
  ingress,
  editable,
  onSaved,
  compact,
}: {
  appId: string;
  ingress: SnapshotIngress;
  editable: boolean;
  onSaved?: () => void;
  /** 只渲染输入框和按钮，不带「机器 @ 地址 :」前缀。
   *
   *  在链详情页中它位于 `.kv` 的格内，左侧列已标注「监听端口」，
   *  机器名也在同一张表的上一行——再次显示属于重复。
   *  机器详情页的情况不同：那里一台机器关联多条链，前缀是区分依据。 */
  compact?: boolean;
}) {
  const qc = useQueryClient();
  const [draftPort, setDraftPort] = useState<string | null>(null);
  const value = draftPort ?? String(ingress.port);
  const parsed = Number(value);
  const valid = /^\d+$/.test(value) && Number.isInteger(parsed) && parsed > 0 && parsed < 65536;

  // 改为该机器上已被占用的端口时，编译才会报 node.port-clash——该错误要到发布前才可见，
  // 而此处可即时计算。判定依据与建链向导共用同一份实现（ports.ts）。
  // 查询键均为其他位置已使用的，通常命中缓存。
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });
  const taken = useMemo(
    () =>
      occupiedPorts(
        snapshot.data?.snapshot.apps ?? [],
        nodeList.data?.nodes ?? [],
        compiled.data?.system,
        ingress.id,
        // 固定为 tcp：该编辑器修改的是 ingress.port，即 VLESS 一侧的端口。
        // 此前在只有 hy2 时会切换为 udp，因为当时两者共用同一个端口号；端口拆分后
        // hy2 有独立的编辑器（IngressHy2PortRow），此处再切换协议会用 TCP 的端口号查询 UDP 的占用。
        'tcp',
      ),
    [snapshot.data, nodeList.data, compiled.data, ingress.id],
  );
  // 只读视角（readonly / public）拿到的是脱敏后的快照，端口被替换为字符串 "***"，据此
  // 算冲突只会得到误报，且该视角改不了端口，提示没有意义。因此仅在可编辑时检测。
  // editable = can(role,'edit')，readonly 与 public 同为 false，两者表现一致。
  const clash = editable && valid ? portClash(taken, [ingress.node], parsed) : null;

  const dirty = draftPort !== null && valid && !clash && parsed !== ingress.port;
  /* 端口这一行在两处出现：链详情页的 VLESS 面板里（有面板，登记进那一次提交），
     以及机器页某台机器的接入面列表里（没有面板，仍用自己的按钮）。 */
  const inPanel = usePanelEntry(
    'port',
    dirty,
    { apply: body => ({ ...body, port: parsed }), reset: () => setDraftPort(null) },
    parsed,
  );
  const nameOf = useNodeNames();
  const save = useMutation({
    mutationFn: () => {
      const base = ingressUpsertBody(ingress);
      return upsertIngress(appId, { ...base, port: parsed }, base);
    },
    onSuccess: () => {
      setDraftPort(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      onSaved?.();
    },
  });

  return (
    <div className="toolbar" style={{ margin: 0, gap: 6 }}>
      {!compact && (
        <>
          <span title={ingress.node}>{nameOf(ingress.node)}</span>
          <span className="dim">@</span>
          <span className="mono">{ingress.bind}</span>
          <span className="dim">:</span>
        </>
      )}
      {editable ? (
        <>
          <input
            className="f mono"
            style={{ width: 86 }}
            value={value}
            inputMode="numeric"
            onChange={e => setDraftPort(e.target.value)}
          />
          {!inPanel && (
            <button
              className="btn"
              disabled={!dirty || save.isPending}
              onClick={() => save.mutate()}
              title={valid ? (clash ?? '') : '端口必须是 1-65535'}
            >
              {save.isPending ? '保存中…' : '保存端口'}
            </button>
          )}
          {draftPort !== null && (
            <button className="btn" disabled={save.isPending} onClick={() => setDraftPort(null)}>
              还原
            </button>
          )}
          {clash && (
            <span className="sub" style={{ color: 'var(--warn)' }}>
              {clash}
            </span>
          )}
        </>
      ) : (
        <span className="mono">{ingress.port}</span>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </div>
  );
}

/* 流控（XTLS Vision）。
 *
 * 单独一行而非并入传输方式：它属于安全层，传输方式属于传输层。但两者存在一条硬约束——
 * Vision 只支持直连的 TLS/REALITY，与 XHTTP 互斥，而 xray **只在运行时**拒绝该组合
 * （其配置检查会通过）。因此两行相邻放置，冲突在两侧都有说明。
 *
 * 只有开和关两档，没有「跟随全局」：快照给出的是生效值，无法区分该接入面是自行设置还是继承，
 * 强行提供第三档相当于显示一个界面无法确定的状态。代价是编辑一次即固定当前生效值，
 * 该行为写在提示中。 */
function IngressFlowRow({
  appId,
  ingress,
  editable,
  xhttp,
}: {
  appId: string;
  ingress: SnapshotIngress;
  editable: boolean;
  xhttp: boolean;
}) {
  const qc = useQueryClient();
  const current = ingress.wires.vless?.flow ?? '';
  const [pending, setPending] = useState<string | null>(null);
  const value = xhttp ? '' : (pending ?? current);
  usePanelEntry(
    'flow',
    value !== current,
    {
      apply: body => ({ ...body, reality: { ...body.reality, flow: value } }),
      reset: () => setPending(null),
    },
    value,
  );

  const save = useMutation({
    mutationFn: (next: string) => {
      const base = ingressUpsertBody(ingress);
      return upsertIngress(appId, { ...base, reality: { ...base.reality, flow: next } }, base);
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setPending(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
    onError: () => setPending(null),
  });

  return (
    <>
      <dt>流控</dt>
      <dd>
        <select
          className="f"
          value={value}
          disabled={!editable || save.isPending}
          onChange={event => setPending(event.target.value)}
        >
          <option value="xtls-rprx-vision" disabled={xhttp}>
            XTLS-RPRX-VISION（默认）
          </option>
          <option value="">关闭（普通 VLESS over TLS）</option>
        </select>
        <div className="note">修改将重启本机 xray，订阅中的 flow 随之变化，客户端需重新导入。</div>
        <div className="note">在此处设置后不再跟随全局设置。</div>
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

/* REALITY 使用哪张证书属于安全层配置，不是另一组 fallback 参数。
 *
 * 后端的 `fallback_mode` 仍是该选择的存储字段：REALITY 握手使用的 SNI、订阅中写入的
 * SNI，以及未通过校验的连接的去向，都由同一来源推导。界面只需一次选择。 */
function IngressRealityRow({
  appId,
  ingress,
  certificateName,
  editable,
}: {
  appId: string;
  ingress: SnapshotIngress;
  certificateName?: string | null;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ['settings'], queryFn: fetchSettings });
  const global = settings.data?.reality_site;
  type RealityCertificateForm = {
    source: RealityFallbackMode;
    dest: string;
    names: string;
    fingerprint: string;
  };
  const initial: RealityCertificateForm = {
    source: ingress.wires.vless?.fallback_mode ?? 'global-site',
    dest: ingress.wires.vless?.dest ?? global?.dest ?? '',
    names: (ingress.wires.vless?.server_names ?? global?.server_names ?? []).join(', '),
    fingerprint: ingress.wires.vless?.fingerprint ?? global?.fingerprint ?? 'chrome',
  };
  const [draft, setDraft] = useState<RealityCertificateForm | null>(null);
  const form = draft ?? initial;
  const save = useMutation({
    mutationFn: (next: RealityCertificateForm) => {
      const body = ingressUpsertBody(ingress);
      const names = next.names
        .split(',')
        .map(value => value.trim())
        .filter(Boolean);
      return upsertIngress(
        appId,
        {
          ...body,
          reality: {
            ...body.reality,
            // This source also determines the compiler-owned fallback target. There is deliberately
            // no second control that could make the certificate and fallback disagree.
            fallback_mode: next.source,
            dest: next.source === 'custom-site' ? next.dest.trim() : '',
            server_names: next.source === 'custom-site' ? names : [],
            fingerprint: next.source === 'custom-site' ? next.fingerprint.trim() : undefined,
          },
        },
        body,
      );
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setDraft(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });
  const names = form.names
    .split(',')
    .map(value => value.trim())
    .filter(Boolean);
  const valid =
    (form.source !== 'custom-site' ||
      (form.dest.trim() !== '' &&
        names.length > 0 &&
        names.every(realityServerNameIsValid) &&
        realityFingerprintIsValid(form.fingerprint))) &&
    (form.source !== 'node-certificate' || !!certificateName);
  const dirty = JSON.stringify(form) !== JSON.stringify(initial);
  /* 贴到面板那份 body 上。取值的整理（拆逗号、custom 之外清空）与下面 save 里的一致。 */
  usePanelEntry(
    'reality-site',
    dirty,
    {
      /* valid 为假 = 自定义站点没填全，或选了本机证书而这台机器还没有证书。 */
      blocked: !valid,
      apply: body => ({
        ...body,
        reality: {
          ...body.reality,
          fallback_mode: form.source,
          dest: form.source === 'custom-site' ? form.dest.trim() : '',
          server_names:
            form.source === 'custom-site'
              ? form.names
                  .split(',')
                  .map(value => value.trim())
                  .filter(Boolean)
              : [],
          fingerprint: form.source === 'custom-site' ? form.fingerprint.trim() : undefined,
        },
      }),
      reset: () => setDraft(null),
    },
    JSON.stringify(form),
  );
  // 选项中直接显示当前指向的站点：下拉框收起后，该行即表示客户端将看到哪张证书。
  const globalSite = (global?.dest ?? '').replace(/:\d+$/, '');

  return (
    <>
      <dt>REALITY 目标站点</dt>
      <dd>
        <select
          className="f"
          value={form.source}
          disabled={!editable || save.isPending}
          onChange={event => setDraft({ ...form, source: event.target.value as RealityFallbackMode })}
        >
          <option value="node-certificate" disabled={!certificateName}>
            {certificateName ? `本机证书 ${certificateName}` : '本机证书（未签发）'}
          </option>
          <option value="global-site">{globalSite ? `全局站点 ${globalSite}` : '全局站点（未配置）'}</option>
          <option value="custom-site">自定义站点</option>
        </select>
        {form.source === 'custom-site' && (
          <>
            <div className="ing-pj" style={{ marginTop: 6 }}>
              <input
                className="f mono"
                value={form.dest}
                placeholder="目标站点"
                disabled={!editable || save.isPending}
                onChange={e => setDraft({ ...form, dest: e.target.value })}
              />
              <input
                className="f mono"
                value={form.names}
                placeholder="允许 SNI，逗号分隔"
                disabled={!editable || save.isPending}
                onChange={e => setDraft({ ...form, names: e.target.value })}
              />
              <select
                className="f mono"
                value={form.fingerprint}
                disabled={!editable || save.isPending}
                onChange={e => setDraft({ ...form, fingerprint: e.target.value })}
              >
                {REALITY_FINGERPRINT_OPTIONS.map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </select>
            </div>
          </>
        )}
        {form.source === 'global-site' && (
          <div className="note">跟随「设置」中的 REALITY 站点，修改将影响所有跟随的接入面。</div>
        )}
        {form.source === 'node-certificate' && (
          <div className="note">
            客户端 SNI 使用 <span className="mono">{certificateName}</span>。
          </div>
        )}
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

function FallbackRateFields({
  direction,
  value,
  disabled,
  onChange,
}: {
  direction: '上传' | '下载';
  value: FallbackRateDraft;
  disabled: boolean;
  onChange: (next: FallbackRateDraft) => void;
}) {
  const field = (label: string, unit: string, key: keyof FallbackRateDraft) => (
    <label className="fallback-rate-field">
      <span>
        {label} <small>{unit}</small>
      </span>
      <input
        className="f mono"
        type="number"
        min={key === 'afterBytes' ? 0 : 1}
        step={1}
        inputMode="numeric"
        aria-label={`${direction}${label}`}
        value={value[key]}
        disabled={disabled}
        onChange={event => onChange({ ...value, [key]: event.target.value })}
      />
    </label>
  );

  return (
    <div className="fallback-rate-dir">
      <b>{direction}</b>
      {field('触发阈值', 'bytes', 'afterBytes')}
      {field('持续速率', 'bytes/s', 'bytesPerSec')}
      {field('突发速率', 'bytes/s', 'burstBytesPerSec')}
    </div>
  );
}

/* 回落限定为借用的那个域名。
 *
 * REALITY 在读取 ClientHello 之前已建立到 dest 的连接，未通过校验的连接原样转发到该连接上，
 * 不检查请求的域名。借用站点通常位于 CDN 上，该地址同时服务其上的所有站点，
 * 因此任何能访问该端口的人都可以通过该机器的带宽访问 CDN 上的任意站点。
 *
 * 启用时编译器会在 dest 之前插入一道本机检查，识别请求的域名，只放行 server_names 中的域名。
 * 它与 Fallback 限速配套：后者限制未授权请求的速率，前者限制其可达范围。 */
/* ── 接入面的安全策略 ──
 *
 * 五个开关，右侧是它们编译产生的规则表。规则表不是装饰：这些限制必须排在该链**自身规则之前**
 * 才生效——链的最后一条通常是 `任意 → 落地`，它匹配所有请求，而 xray 使用第一条匹配的规则。
 * 排在其后的限制不会被触发，而界面上五个开关仍显示为启用。将顺序呈现出来是使该问题可见的方式。
 *
 * 保存使用整表覆盖的 upsert，与修改端口、迁移机器为同一入口，因此产生修订并随发布下发。 */
function IngressGuardBlock({
  appId,
  ingress,
  editable,
}: {
  appId: string;
  ingress: SnapshotIngress;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const stored = ingress.guard;
  const [draft, setDraft] = useState<IngressGuard | null>(null);
  const form = draft ?? stored;
  const dirty = (Object.keys(stored) as (keyof IngressGuard)[]).some(key => form[key] !== stored[key]);
  const save = useMutation({
    mutationFn: () => {
      const base = ingressUpsertBody(ingress);
      return upsertIngress(appId, { ...base, guard: form }, base);
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setDraft(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });
  const set = (key: keyof IngressGuard, value: boolean) => setDraft({ ...form, [key]: value });

  /* 使用前置代理的入口不做协议嗅探，「禁 BT」在该场景下不会生效——编译器会直接拒绝
   * （`ingress.guard-needs-sniffing`）。此处提前说明，避免保存后才在诊断中发现。 */
  const fronted = !!ingress.front;

  const SWITCHES: { key: keyof IngressGuard; name: string; why: string; expr: string; danger?: boolean }[] = [
    {
      key: 'no_private',
      name: '禁止访问内网与机队',
      why: '阻止访问机器出口内网和机队 WG 内网。',
      expr: 'geoip:private + 机队网段',
    },
    {
      key: 'no_bittorrent',
      name: '禁止 BT 下载',
      why: '按协议识别，加密 BT 与 DHT 仍可绕过。',
      expr: 'protocol:bittorrent',
    },
    {
      key: 'no_mail',
      name: '禁止邮件端口',
      why: '防止垃圾邮件导致 IP 进入黑名单。',
      expr: 'port:25,465,587',
    },
    {
      key: 'no_udp_amplification',
      name: '禁止 UDP 放大端口',
      why: '防止反射放大攻击。',
      expr: 'udp:19,53,123,161,389,1900,11211',
    },
    {
      key: 'tcp_and_quic_only',
      name: '只放行 TCP 与 UDP/443',
      why: '最严格，游戏、语音、自建 UDP 服务会中断。',
      danger: true,
      expr: 'udp port ≠ 443',
    },
  ];

  return (
    <ConfigPanel title="安全策略">
      {/* 标题栏不放读数：启用了哪几项，五个勾选框自己就写着，而「排在链路规则之前」
          是这一块恒定的性质，不是一项随取值变化的状态。 */}
      <div className="guard-switches">
        {SWITCHES.map(row => (
          <label key={row.key} className={form[row.key] ? 'guard-row on' : 'guard-row'}>
            <input
              type="checkbox"
              checked={form[row.key]}
              disabled={!editable || save.isPending}
              onChange={event => set(row.key, event.target.checked)}
            />
            <span>
              <b className={row.danger && form[row.key] ? 'bad' : undefined}>{row.name}</b>
              <span className="note">{row.why}</span>
              {row.key === 'no_bittorrent' && form.no_bittorrent && fronted && (
                <span className="note bad">
                  这个接入面有前置代理、不做协议嗅探，该项无法拦截任何流量。编译会以{' '}
                  <span className="mono">ingress.guard-needs-sniffing</span> 阻止发布。
                </span>
              )}
            </span>
          </label>
        ))}
      </div>

      {dirty && (
        <div className="toolbar">
          <button className="btn" disabled={save.isPending} onClick={() => setDraft(null)}>
            还原
          </button>
          <button className="btn primary" disabled={!editable || save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? '保存中…' : '保存'}
          </button>
        </div>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </ConfigPanel>
  );
}

function IngressRealityGuardRow({
  appId,
  ingress,
  editable,
}: {
  appId: string;
  ingress: SnapshotIngress;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const initial = ingress.wires.vless?.fallback_guard ?? true;
  const [draft, setDraft] = useState<boolean | null>(null);
  const form = draft ?? initial;
  const dirty = form !== initial;
  usePanelEntry(
    'fallback-guard',
    dirty,
    {
      apply: body => ({ ...body, reality: { ...body.reality, fallback_guard: form } }),
      reset: () => setDraft(null),
    },
    form,
  );
  const save = useMutation({
    mutationFn: () => {
      const body = ingressUpsertBody(ingress);
      return upsertIngress(
        appId,
        {
          ...body,
          reality: { ...body.reality, fallback_guard: form },
        },
        body,
      );
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setDraft(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });

  /* 使用本机证书时回落不会离开该机器（返回本地固定的 403），不存在需要保护的外部站点。
   * 开关仍然显示但不可修改：隐藏会使人认为该接入面缺少这项防护。 */
  const local = ingress.wires.vless?.fallback_mode === 'node-certificate';

  return (
    <>
      <dt>Fallback 域名</dt>
      <dd>
        <select
          className="f"
          value={form ? 'on' : 'off'}
          disabled={!editable || local || save.isPending}
          onChange={event => setDraft(event.target.value === 'on')}
        >
          <option value="on">仅放行 dest 站点</option>
          <option value="off">不限制</option>
        </select>
        {local ? (
          <div className="note">回落至本机，返回固定 403，不产生外部连接。</div>
        ) : (
          <>
            <div className={form ? 'note' : 'note bad'}>
              {form
                ? '编译时在 dest 前插入 SNI 校验，防止 Cloudflare 转发导致的流量盗刷。'
                : '回落原样转发至 dest。dest 位于 CDN 时，第三方可通过此端口代理该 CDN 上的任意站点。'}
            </div>
            <div className="note">仅作用于未通过 REALITY 校验的连接。</div>
          </>
        )}
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

function IngressRealityLimitsRow({
  appId,
  ingress,
  editable,
}: {
  appId: string;
  ingress: SnapshotIngress;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const initial = fallbackLimitDraft(ingress.wires.vless?.fallback_limits ?? { mode: 'off' });
  const [draft, setDraft] = useState<FallbackLimitDraft | null>(null);
  const form = draft ?? initial;
  const policy = fallbackLimitsFromDraft(form);
  const dirty = JSON.stringify(form) !== JSON.stringify(initial);
  /* policy 为空表示自定义档填得不全，登记为 blocked：面板的按钮变灰而不是发出一个会被
     服务端拒绝的请求。 */
  usePanelEntry(
    'fallback-limits',
    dirty,
    {
      blocked: !policy,
      apply: body => (policy ? { ...body, reality: { ...body.reality, fallback_limits: policy } } : body),
      reset: () => setDraft(null),
    },
    JSON.stringify(form),
  );
  const save = useMutation({
    mutationFn: () => {
      if (!policy) throw new Error('限速参数不完整');
      const body = ingressUpsertBody(ingress);
      return upsertIngress(
        appId,
        {
          ...body,
          reality: { ...body.reality, fallback_limits: policy },
        },
        body,
      );
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setDraft(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });

  const note =
    form.mode === 'balanced'
      ? '上传超过 1 MiB 后限速至 256 KiB/s，下载超过 8 MiB 后限速至 1 MiB/s。'
      : form.mode === 'strict'
        ? '上传超过 256 KiB 后限速至 64 KiB/s，下载超过 1 MiB 后限速至 256 KiB/s。'
        : form.mode === 'off'
          ? '不向 xray 写入 fallback 限速参数。'
          : '按 bytes 与 bytes/s 原样写入。突发速率不得低于持续速率。';

  return (
    <>
      <dt>Fallback 限速</dt>
      <dd>
        <select
          className="f"
          value={form.mode}
          disabled={!editable || save.isPending}
          onChange={event => setDraft({ ...form, mode: event.target.value as FallbackLimitDraft['mode'] })}
        >
          <option value="balanced">均衡（默认，适合公网入口）</option>
          <option value="strict">严格（更早、更低速）</option>
          <option value="custom">自定义上传、下载参数</option>
          <option value="off">关闭限速</option>
        </select>
        <div className={form.mode === 'off' ? 'note warn' : 'note'}>{note}</div>
        <div className="note">仅限制未通过 REALITY 校验、进入 fallback 的连接。</div>
        {form.mode === 'custom' && (
          <div className="fallback-rate-grid">
            <FallbackRateFields
              direction="上传"
              value={form.upload}
              disabled={!editable || save.isPending}
              onChange={upload => setDraft({ ...form, upload })}
            />
            <FallbackRateFields
              direction="下载"
              value={form.download}
              disabled={!editable || save.isPending}
              onChange={download => setDraft({ ...form, download })}
            />
          </div>
        )}
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

/* TODO: 主界面暂不提供任意 Headers、Padding 放置/编码细节、ALPN/H3 和原始 extra JSON；
 * 这些仅用于实验或故障诊断。
 *
 * 接入面的传输层：TCP 或 XHTTP。
 *
 * 它与安全层（REALITY）是两个独立维度。TCP 下每条客户端连接对应一条 TCP 连接，而在 REALITY
 * 下每条新连接都需要服务端建立一次到借用站点的 TLS 握手以获取证书——因此握手频繁的入口，
 * 成本不止于握手本身。XHTTP 将多条流复用到一条 HTTP/2 连接上，该开销相应降低。
 *
 * 代价在界面上明确说明：多路复用共用一条 TCP 连接，丢包会导致该连接上的所有流一同阻塞。
 *
 * 有一项必须在保存前说明，不能等服务端报错：XHTTP 与流控（Vision）互斥，而流控默认启用。
 * xray 自身不拦截该组合——其配置检查会通过，但运行时所有连接都会被拒绝。 */
type ProjectionFamily = 'v4' | 'v6';
const PROJECTION_FAMILIES = ['v4', 'v6'] as const;

type XmuxDraft = {
  maxConcurrency: string;
  maxConnections: string;
  requestFrom: string;
  requestTo: string;
  reusableFrom: string;
  reusableTo: string;
  keepAlive: string;
};

function xmuxDraftOf(value: Xhttp['xmux']): XmuxDraft {
  const visible = (actual: number | undefined, fallback: number) =>
    actual === undefined || actual === fallback ? '' : String(actual);
  return {
    maxConcurrency: visible(value?.max_concurrency ?? undefined, DEFAULT_XHTTP_XMUX.max_concurrency ?? 1),
    maxConnections: value?.max_connections ? String(value.max_connections) : '',
    requestFrom: visible(value?.h_max_request_times.from, DEFAULT_XHTTP_XMUX.h_max_request_times.from),
    requestTo: visible(value?.h_max_request_times.to, DEFAULT_XHTTP_XMUX.h_max_request_times.to),
    reusableFrom: visible(value?.h_max_reusable_secs.from, DEFAULT_XHTTP_XMUX.h_max_reusable_secs.from),
    reusableTo: visible(value?.h_max_reusable_secs.to, DEFAULT_XHTTP_XMUX.h_max_reusable_secs.to),
    keepAlive: value?.h_keep_alive_period_secs == null ? '' : String(value.h_keep_alive_period_secs),
  };
}

function xmuxOfDraft(value: XmuxDraft): Exclude<Xhttp['xmux'], undefined> {
  if (Object.values(value).every(item => item.trim() === '')) return null;
  const number = (actual: string, fallback: number) => (actual.trim() === '' ? fallback : Number(actual));
  const connections = value.maxConnections.trim();
  return {
    max_concurrency: connections
      ? value.maxConcurrency.trim()
        ? Number(value.maxConcurrency)
        : null
      : number(value.maxConcurrency, DEFAULT_XHTTP_XMUX.max_concurrency ?? 1),
    max_connections: connections ? Number(connections) : null,
    h_max_request_times: {
      from: number(value.requestFrom, DEFAULT_XHTTP_XMUX.h_max_request_times.from),
      to: number(value.requestTo, DEFAULT_XHTTP_XMUX.h_max_request_times.to),
    },
    h_max_reusable_secs: {
      from: number(value.reusableFrom, DEFAULT_XHTTP_XMUX.h_max_reusable_secs.from),
      to: number(value.reusableTo, DEFAULT_XHTTP_XMUX.h_max_reusable_secs.to),
    },
    h_keep_alive_period_secs: value.keepAlive.trim() ? Number(value.keepAlive) : null,
  };
}

type TuningDraft = {
  paddingFrom: string;
  paddingTo: string;
};

function tuningDraftOf(value: Xhttp['tuning']): TuningDraft {
  const range = (actual: { from: number; to: number } | null | undefined, fallback: { from: number; to: number }) => ({
    from: !actual || (actual.from === fallback.from && actual.to === fallback.to) ? '' : String(actual.from),
    to: !actual || (actual.from === fallback.from && actual.to === fallback.to) ? '' : String(actual.to),
  });
  const padding = range(value?.x_padding_bytes, DEFAULT_XHTTP_TUNING.x_padding_bytes);
  return {
    paddingFrom: padding.from,
    paddingTo: padding.to,
  };
}

function tuningOfDraft(value: TuningDraft): XhttpTuning | null {
  const range = (from: string, to: string, fallback: { from: number; to: number }) =>
    !from.trim() && !to.trim()
      ? null
      : {
          from: from.trim() ? Number(from) : fallback.from,
          to: to.trim() ? Number(to) : fallback.to,
        };
  const xPaddingBytes = range(value.paddingFrom, value.paddingTo, DEFAULT_XHTTP_TUNING.x_padding_bytes);
  return xPaddingBytes ? { x_padding_bytes: xPaddingBytes } : null;
}

type XhttpDownloadDraft = {
  host: string;
  port: string;
  originPort: string | null;
  httpHost: string;
  mux: string;
};

type XhttpDownloadDrafts = Partial<Record<ProjectionFamily, XhttpDownloadDraft | null>>;

function xhttpDownloadDraftsOf(
  downloadConfig: Xhttp['download'],
  legacyProjection?: IngressProjection | null,
): XhttpDownloadDrafts {
  const drafts: XhttpDownloadDrafts = {};
  for (const family of PROJECTION_FAMILIES) {
    const download = downloadConfig?.[family] ?? legacyProjection?.[family]?.download;
    if (!download) {
      drafts[family] = null;
      continue;
    }
    drafts[family] = {
      host: download.host,
      port: String(download.port),
      originPort: download.origin_port == null ? null : String(download.origin_port),
      httpHost: download.http_host ?? '',
      mux: download.mux == null ? '' : String(download.mux),
    };
  }
  return drafts;
}

function xhttpDownloadDraftOf(endpoint: ProjectionEndpoint, splitReality: boolean): XhttpDownloadDraft {
  const download = endpoint.download;
  return {
    host: download?.host ?? endpoint.host,
    port: String(download?.port ?? endpoint.port),
    originPort: splitReality ? String(download?.origin_port ?? download?.port ?? endpoint.port) : null,
    httpHost: download?.http_host ?? '',
    mux: download?.mux == null ? '' : String(download.mux),
  };
}

function currentXhttpDownloadDrafts(downloadConfig: Xhttp['download'], draft: XhttpDownloadDrafts | null): XhttpDownloadDrafts {
  const current = xhttpDownloadDraftsOf(downloadConfig);
  if (!draft) return current;
  for (const family of PROJECTION_FAMILIES) {
    if (family in draft) current[family] = draft[family];
  }
  return current;
}

function xhttpDownloadOfDrafts(drafts: XhttpDownloadDrafts, kind: TransportKind | null): Xhttp['download'] {
  const allowDownload = kind !== null && transportIsXhttp(kind);
  const splitReality = kind === 'vless-reality-xhttp';
  if (!allowDownload) return null;
  const next: NonNullable<Xhttp['download']> = {};
  for (const family of PROJECTION_FAMILIES) {
    const draft = drafts[family];
    if (draft === undefined || draft === null) continue;
    next[family] = {
      host: draft.host.trim(),
      port: Number(draft.port),
      origin_port:
        splitReality && draft.originPort !== null && draft.originPort.trim() ? Number(draft.originPort) : null,
      http_host: draft.httpHost.trim() || null,
      mux: draft.mux.trim() ? Number(draft.mux) : null,
    };
  }
  return Object.keys(next).length > 0 ? next : null;
}

/* Hysteria 2 一侧的端口及其跳转区间。
 *
 * 端口归属协议栈而非落点：落点表示由哪台机器接收，端口表示该线路使用其哪个端口接收。
 * 两条线此前共用 ingress.port，引入跳转后不再可行——跳转是对一段 UDP 端口的重定向，
 * 遗漏 `-p udp` 会同时影响同号的 TCP 一侧，因此两个端口号必须分开
 *（后端 ingress.hy2-port-shared）。
 *
 * 取值保存在上层的 hy2 草稿中（与带宽、混淆同一份），保存也共用同一个按钮：端口和参数分两次
 * 保存时，中间会先将线路迁移到新端口再修改参数，导致已连接的用户中断两次。 */
function IngressHy2PortRow({
  ingress,
  value,
  onChange,
  editable,
}: {
  ingress: SnapshotIngress;
  value: Hysteria2Settings;
  onChange: (patch: Partial<Hysteria2Settings>) => void;
  editable: boolean;
}) {
  const hy2Base = useHy2PortBase();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });
  const taken = useMemo(
    () =>
      occupiedPorts(
        snapshot.data?.snapshot.apps ?? [],
        nodeList.data?.nodes ?? [],
        compiled.data?.system,
        ingress.id,
        'udp',
      ),
    [snapshot.data, nodeList.data, compiled.data, ingress.id],
  );

  const nodes = [ingress.node];
  // 只读视角（readonly / public）的 value.port 被脱敏为字符串 "***"：portClash 会误命中其他
  // 同样被脱敏成 "***" 的端口，hopBad 的 `hop.start <= "***"` 恒为 false 而报「区间无效」。
  // 这些检测只对能改端口、且拿到未脱敏数据的可编辑视角有意义，因此统一门控在 editable。
  // editable = can(role,'edit')，readonly 与 public 同为 false，两者表现一致。
  const clash = editable ? portClash(taken, nodes, value.port) : null;
  const hop = value.hop ?? null;
  const span = hop ? hop.end - hop.start + 1 : 0;
  const hopBad = editable && hop ? hop.start > hop.end || !(hop.start <= value.port && value.port <= hop.end) : false;
  const hopClash = editable && hop && !hopBad ? spanClash(taken, nodes, hop.start, hop.end) : null;

  const reallocate = () => {
    const port = freePortAcross(taken, nodes, hy2Base);
    /* 区间随监听端口变化：区间起点即监听端口，区间长度不变。分别计算时，重新分配后
       区间可能不再包含监听端口——这正是后端拒绝的情况（ingress.hy2-hop-listener）。 */
    onChange(hop ? { port, hop: { start: port, end: port + span - 1 } } : { port });
  };
  const toggleHop = (on: boolean) => {
    if (!on) return onChange({ hop: null });
    const start = freeSpanAcross(taken, nodes, value.port, DEFAULT_HOP_SPAN);
    onChange({ port: start, hop: { start, end: start + DEFAULT_HOP_SPAN - 1 } });
  };

  return (
    <>
      <dt>监听端口</dt>
      <dd>
        <div className="toolbar" style={{ margin: 0, gap: 6 }}>
          <input
            className="f mono"
            style={{ width: 96, borderColor: clash ? 'var(--err)' : undefined }}
            value={value.port}
            disabled={!editable}
            onChange={event => {
              const port = Number(event.target.value.replace(/\D/g, '')) || 0;
              onChange(hop ? { port, hop: { start: port, end: port + span - 1 } } : { port });
            }}
          />
          <span className="dim">UDP</span>
          <button className="btn" disabled={!editable} onClick={reallocate}>
            重新分配
          </button>
        </div>
        {clash && <div className="note bad">{clash}</div>}
      </dd>

      <dt>端口跳转</dt>
      <dd>
        <div className="segsw" role="group">
          <button type="button" aria-pressed={!hop} disabled={!editable} onClick={() => toggleHop(false)}>
            关
          </button>
          <button type="button" aria-pressed={!!hop} disabled={!editable} onClick={() => toggleHop(true)}>
            开
          </button>
        </div>
        {hop && (
          <>
            <div className="toolbar" style={{ margin: '8px 0 0', gap: 6 }}>
              <input
                className="f mono"
                style={{ width: 72, borderColor: hopBad ? 'var(--err)' : undefined }}
                value={hop.start}
                disabled={!editable}
                onChange={event =>
                  onChange({ hop: { ...hop, start: Number(event.target.value.replace(/\D/g, '')) || 0 } })
                }
              />
              <span className="dim">–</span>
              <input
                className="f mono"
                style={{ width: 72, borderColor: hopBad ? 'var(--err)' : undefined }}
                value={hop.end}
                disabled={!editable}
                onChange={event =>
                  onChange({ hop: { ...hop, end: Number(event.target.value.replace(/\D/g, '')) || 0 } })
                }
              />
              <span className="dim">共 {span > 0 ? span : 0} 个口</span>
            </div>
            <div className="note">
              端口跳转将使用机器系统上的 DNAT 规则强制转发整段到 Hysteria。
              <br />
              请确保无其他 UDP 协议服务监听本段端口。
              <br />
              区间只写入订阅，由客户端自行轮换。
            </div>
            {hopBad && <div className="note bad">区间无效：起点须小于等于终点，且必须包含监听端口</div>}
            {hopClash && <div className="note bad">{hopClash}</div>}
          </>
        )}
        {!hop && <div className="note">关闭时客户端只连接监听端口，机器上不部署 DNAT 规则。</div>}
      </dd>
    </>
  );
}

export function IngressStreamRow({
  appId,
  ingress,
  certificateName,
  editable,
  section,
}: {
  appId: string;
  ingress: SnapshotIngress;
  certificateName?: string | null;
  editable: boolean;
  /** 本次渲染的是哪一段。
   *
   *  三段位于三块面板中，但状态、草稿和保存逻辑只有一份——修改一条线时需要将另一条原样带上
   *  （请求是全量覆盖），拆分为三个组件需要复制该逻辑三次。因此同一组件渲染三次，
   *  每次只渲染对应的一段。 */
  section: 'protocols' | 'vless' | 'anytls' | 'hy2';
}) {
  const qc = useQueryClient();
  const hy2Base = useHy2PortBase();
  const streamSnapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const streamNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const streamRevisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const streamCompiled = useQuery({
    queryKey: ['compile', streamRevisions.data?.current_revision],
    queryFn: () => fetchCompileView(streamRevisions.data!.current_revision!),
    enabled: !!streamRevisions.data?.current_revision,
  });
  const tcpTaken = useMemo(
    () =>
      occupiedPorts(
        streamSnapshot.data?.snapshot.apps ?? [],
        streamNodes.data?.nodes ?? [],
        streamCompiled.data?.system,
        ingress.id,
        'tcp',
      ),
    [streamSnapshot.data, streamNodes.data, streamCompiled.data, ingress.id],
  );
  /* 安全层字段表示客户端握手时看到的是哪张证书，REALITY 档对应的是其指向的站点，
     而该站点可能来自全局设置。使用相同的 queryKey，与 IngressRealityRow 共用缓存。 */
  /* 两条线是否启用是两个独立的状态。VLESS 一侧未启用时 `storedKind` 为 null，
     下方所有与安全层和承载层相关的控件都不显示——它们描述的是该侧。 */
  const storedVless = ingress.wires.vless ?? null;
  const storedKind = storedVless?.kind ?? null;
  const storedAnyTls = useMemo<AnyTlsSettings>(() => {
    if (ingress.wires.anytls) return ingress.wires.anytls;
    let port = freePortAcross(tcpTaken, [ingress.node], ANYTLS_PORT_BASE);
    // `occupiedPorts` excludes this ingress while editing. Its VLESS port still belongs to the
    // same Xray process, so keep the generated AnyTLS default distinct from it explicitly.
    while (port === ingress.port && port < 65536) port += 1;
    return {
      port,
      padding_scheme: [],
      masquerade: { kind: 'not-found' },
    };
  }, [ingress.node, ingress.port, ingress.wires.anytls, tcpTaken]);
  const [pendingTransport, setPendingTransport] = useState<Transport | null>(null);
  const stagedTransport = pendingTransport?.kind === storedKind ? null : pendingTransport;
  const kind: TransportKind | null = stagedTransport?.kind ?? storedKind;
  const vlessOn = kind !== null;
  const [pendingAnyTlsOn, setPendingAnyTlsOn] = useState<boolean | null>(null);
  const stagedAnyTlsOn = pendingAnyTlsOn === !!ingress.wires.anytls ? null : pendingAnyTlsOn;
  const anytlsOn = stagedAnyTlsOn ?? !!ingress.wires.anytls;
  const [draftAnyTls, setDraftAnyTls] = useState<AnyTlsSettings | null>(null);
  const anytlsValue = draftAnyTls ?? storedAnyTls;
  const [draftAnyTlsPadding, setDraftAnyTlsPadding] = useState<string | null>(null);
  const [draftAnyTlsHeaders, setDraftAnyTlsHeaders] = useState<string | null>(null);
  const [draftAnyTlsStatus, setDraftAnyTlsStatus] = useState<string | null>(null);
  const current = stagedTransport && 'xhttp' in stagedTransport ? stagedTransport.xhttp : (storedVless?.xhttp ?? null);
  const on = kind !== null && transportIsXhttp(kind);
  const tls = kind !== null && kind.startsWith('vless-tls');
  /* 尚未启用 hy2 时用于填充表单的默认值。端口不能固定为 50000：同一台机器上启用第二个
     接入面时会与第一个端口冲突，且操作时无法察觉。因此此处调用一次端口分配器。 */
  const udpTaken = useMemo(
    () =>
      occupiedPorts(
        streamSnapshot.data?.snapshot.apps ?? [],
        streamNodes.data?.nodes ?? [],
        streamCompiled.data?.system,
        ingress.id,
        'udp',
      ),
    [streamSnapshot.data, streamNodes.data, streamCompiled.data, ingress.id],
  );
  const storedHy2: Hysteria2Settings =
    ingress.wires.hysteria2 ??
    (() => {
      const port = freePortAcross(udpTaken, [ingress.node], hy2Base);
      const hopStart = freeSpanAcross(udpTaken, [ingress.node], port, DEFAULT_HOP_SPAN);
      return {
        port: hopStart,
        hop: { start: hopStart, end: hopStart + DEFAULT_HOP_SPAN - 1 },
        bandwidth: {},
        congestion: 'brutal',
        obfs: { kind: 'salamander' as const, password: 'quick-brown-fox' },
        masquerade: { kind: 'not-found' as const },
      };
    })();
  const [pendingHy2On, setPendingHy2On] = useState<boolean | null>(null);
  /* 待写入的值在与快照一致后自动失效，写法与上面的 `stagedTransport` 相同。
     必须通过推导得出，不能只依赖 onSuccess 中清除：该清除曾遗漏一次，而 checkbox 的
     disabled 依赖该值，表现为勾选后无法再次点击，尽管保存已成功。 */
  const stagedHy2On = pendingHy2On === !!ingress.wires.hysteria2 ? null : pendingHy2On;
  const hy2 = stagedHy2On ?? !!ingress.wires.hysteria2;
  const activeHy2 = storedHy2;
  const [draftHy2, setDraftHy2] = useState<Hysteria2Settings | null>(null);
  const hy2Value = draftHy2 ?? activeHy2;
  const path = current?.path ?? '';
  const host = current?.host ?? '';
  const xmux = current?.xmux ?? null;
  const tuning = current?.tuning ?? null;
  const storedMode: XhttpMode = current?.mode ?? 'auto';
  const storedDownloadDrafts = xhttpDownloadDraftsOf(current?.download, ingress.projection);
  const [draftPath, setDraftPath] = useState<string | null>(null);
  const [draftHost, setDraftHost] = useState<string | null>(null);
  const [draftXmux, setDraftXmux] = useState<XmuxDraft | undefined>(undefined);
  const [draftTuning, setDraftTuning] = useState<TuningDraft | undefined>(undefined);
  const [draftDownload, setDraftDownload] = useState<XhttpDownloadDrafts | null>(null);
  const [draftMode, setDraftMode] = useState<XhttpMode | null>(null);
  const mode = draftMode ?? storedMode;
  const downloadDrafts = currentXhttpDownloadDrafts(current?.download, draftDownload);
  const hasDownload = PROJECTION_FAMILIES.some(
    family => downloadDrafts[family] !== null && downloadDrafts[family] !== undefined,
  );
  const splitReality = kind === 'vless-reality-xhttp';
  /* 「跟随两端」在产物中不写入该字段，由两端各自解析。显示解析结果才是该字段的实际状态——
     只显示「跟随」需要自行记住两端的解析规则。 */
  const resolvedMode: XhttpMode = hasDownload ? 'stream-up' : tls ? 'packet-up' : 'stream-one';
  // 流控是该字段的前置条件而非并列项：同时启用 XHTTP 和 Vision 时，运行后所有连接都会失败。
  const flowOn = (storedVless?.flow ?? '').trim() !== '';
  const downloadBad = PROJECTION_FAMILIES.some(family => {
    const draft = downloadDrafts[family];
    if (!draft) return false;
    const port = Number(draft.port);
    const effectiveOriginPort = draft.originPort ?? draft.port;
    const originPort = Number(effectiveOriginPort);
    const mux = draft.mux.trim() ? Number(draft.mux) : null;
    return (
      draft.host.trim() === '' ||
      !/^\d+$/.test(draft.port) ||
      !Number.isInteger(port) ||
      port < 1 ||
      port > 65535 ||
      (splitReality &&
        (!/^\d+$/.test(effectiveOriginPort) ||
          !Number.isInteger(originPort) ||
          originPort < 1 ||
          originPort > 65535)) ||
      (mux !== null && (!Number.isInteger(mux) || mux < 2 || mux > 128))
    );
  });
  const downloadDirty = on && JSON.stringify(downloadDrafts) !== JSON.stringify(storedDownloadDrafts);

  const save = useMutation({
    mutationFn: (next: Wires) => {
      const vless =
        next.vless && hasDownload && 'xhttp' in next.vless && next.vless.xhttp.mode === 'stream-one'
          ? ({
              ...next.vless,
              xhttp: { ...next.vless.xhttp, mode: compatibleXhttpMode(next.vless.xhttp.mode, true) },
            } as Transport)
          : (next.vless ?? null);
      const wires: Wires = {
        vless,
        anytls: next.anytls ?? null,
        hysteria2: next.hysteria2 ?? null,
      };
      const base = ingressUpsertBody(ingress);
      return upsertIngress(appId, { ...base, wires }, base);
    },
    onSuccess: async () => {
      setDraftPath(null);
      setDraftHost(null);
      setDraftXmux(undefined);
      setDraftTuning(undefined);
      setDraftDownload(null);
      setDraftHy2(null);
      setDraftAnyTls(null);
      setDraftAnyTlsPadding(null);
      setDraftAnyTlsHeaders(null);
      setDraftAnyTlsStatus(null);
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setDraftMode(null);
      setPendingTransport(null);
      setPendingAnyTlsOn(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
    onError: () => {
      setDraftHy2(null);
      setDraftMode(null);
      setPendingTransport(null);
      setPendingAnyTlsOn(null);
      setPendingHy2On(null);
      setDraftAnyTls(null);
      setDraftAnyTlsPadding(null);
      setDraftAnyTlsHeaders(null);
      setDraftAnyTlsStatus(null);
    },
  });

  /* 修改一侧时另一侧原样带上。请求是全量覆盖：遗漏即表示关闭该线路，而关闭一条线路会
     移除一个 inbound，导致其上的用户全部断开——这不应是修改一个下拉框的后果。 */
  const saveHy2 = (next: Hysteria2Settings | null) =>
    save.mutate({
      vless: vlessOn ? (stagedTransport ?? currentWires(ingress).vless) : null,
      anytls: anytlsOn ? anytlsValue : null,
      hysteria2: next,
    });

  const switchKind = (next: TransportKind) => {
    if (hasDownload && next !== storedKind) {
      const message = transportIsXhttp(next)
        ? next === 'vless-reality-xhttp'
          ? '独立下载将从纯订阅投影变成节点 TLS 下载前置，需要发布并重启 xray。继续切换吗？'
          : '独立下载将变成纯订阅投影，节点 TLS 下载前置会被移除。继续切换吗？'
        : '目标传输不支持独立下载，继续会移除现有下载线路。继续切换吗？';
      if (!window.confirm(message)) return;
    }
    const nextXhttp: Xhttp = {
      path: current?.path ?? `/${Math.random().toString(36).slice(2, 10)}`,
      host: current?.host ?? null,
      xmux: current?.xmux ?? null,
      tuning: current?.tuning ?? null,
      mode: current?.mode ?? 'auto',
      download: current?.download ?? null,
    };
    const nextTransport: Transport = transportIsXhttp(next)
      ? next === 'vless-tls-xhttp'
        ? { kind: next, xhttp: nextXhttp }
        : { kind: next, xhttp: nextXhttp }
      : next === 'vless-tls'
        ? { kind: next }
        : { kind: 'vless-reality' };
    /* 只暂存。此前这里直接提交——切一下下拉就盖一个修订、重启一次 xray，
       而同一块面板上的其他改动还得再按一次保存。现在统一由面板那一个按钮发出。 */
    setPendingTransport(nextTransport);
  };

  /* 启用或关闭一条线路。两条都关闭表示该接入面不接收任何连接，服务端的类型定义和库中的
     CHECK 约束都不允许该状态，因此在点击前拦截，而不是点击后返回错误。 */
  const toggleWire = (wire: 'vless' | 'anytls' | 'hy2', enabled: boolean) => {
    const otherWireOn =
      wire === 'vless' ? anytlsOn || hy2 : wire === 'anytls' ? vlessOn || hy2 : vlessOn || anytlsOn;
    if (!enabled && !otherWireOn) {
      window.alert('至少要保留一条线路：两条都关闭后，这个接入面不再接收任何流量。');
      return;
    }
    if (wire === 'vless') {
      if (
        !enabled &&
        !window.confirm('关闭 VLESS 会移除对应的 inbound，当前连接在其上的用户会断开一次。确定继续吗？')
      ) {
        return;
      }
      const nextVless: Transport | null = enabled ? { kind: 'vless-reality' } : null;
      setPendingTransport(nextVless);
      save.mutate({
        vless: nextVless,
        anytls: anytlsOn ? anytlsValue : null,
        hysteria2: hy2 ? hy2Value : null,
      });
      return;
    }
    if (wire === 'anytls') {
      if (
        !enabled &&
        !window.confirm('关闭 AnyTLS 会移除对应的 inbound，当前连接在其上的用户会断开一次。确定继续吗？')
      ) {
        return;
      }
      setPendingAnyTlsOn(enabled);
      save.mutate({
        vless: vlessOn ? (stagedTransport ?? currentWires(ingress).vless) : null,
        anytls: enabled ? anytlsValue : null,
        hysteria2: hy2 ? hy2Value : null,
      });
      return;
    }
    if (
      !enabled &&
      !window.confirm('关闭 Hysteria 2 会移除对应的 inbound，当前连接在其上的用户会断开一次。确定继续吗？')
    ) {
      return;
    }
    setPendingHy2On(enabled);
    saveHy2(enabled ? hy2Value : null);
  };

  const pathValue = draftPath ?? path;
  const hostValue = draftHost ?? host;
  const xmuxDraft = draftXmux ?? xmuxDraftOf(xmux);
  const xmuxValue = draftXmux === undefined ? xmux : xmuxOfDraft(draftXmux);
  const tuningDraft = draftTuning ?? tuningDraftOf(tuning);
  const tuningValue = draftTuning === undefined ? tuning : tuningOfDraft(draftTuning);
  const badRange = (range: { from: number; to: number }) =>
    !Number.isSafeInteger(range.from) ||
    !Number.isSafeInteger(range.to) ||
    range.from < 1 ||
    range.from > range.to ||
    range.to > 2_147_483_647;
  const xmuxBad =
    xmuxValue !== null &&
    ((xmuxValue.max_concurrency == null) === (xmuxValue.max_connections == null) ||
      (xmuxValue.max_concurrency != null &&
        (!Number.isInteger(xmuxValue.max_concurrency) ||
          xmuxValue.max_concurrency < 1 ||
          xmuxValue.max_concurrency > 128)) ||
      (xmuxValue.max_connections != null &&
        (!Number.isInteger(xmuxValue.max_connections) ||
          xmuxValue.max_connections < 1 ||
          xmuxValue.max_connections > 128)) ||
      badRange(xmuxValue.h_max_request_times) ||
      badRange(xmuxValue.h_max_reusable_secs) ||
      (xmuxValue.h_keep_alive_period_secs != null &&
        xmuxValue.h_keep_alive_period_secs !== -1 &&
        (!Number.isInteger(xmuxValue.h_keep_alive_period_secs) ||
          xmuxValue.h_keep_alive_period_secs < 1 ||
          xmuxValue.h_keep_alive_period_secs > 3600)));
  const tuningRangeBad = (range: { from: number; to: number } | null | undefined, min: number, max: number) =>
    !!range &&
    (!Number.isSafeInteger(range.from) ||
      !Number.isSafeInteger(range.to) ||
      range.from < min ||
      range.from > range.to ||
      range.to > max);
  const tuningBad = tuningValue !== null && tuningRangeBad(tuningValue.x_padding_bytes, 1, 4096);
  const pathBad = pathValue.trim() === '' || !pathValue.startsWith('/') || /[\s?#]/.test(pathValue);
  const xhttpForSave = (nextMode = mode): Xhttp => ({
    path: pathValue.trim(),
    host: hostValue.trim() || null,
    xmux: xmuxValue,
    tuning: tuningValue,
    mode: nextMode,
    download: xhttpDownloadOfDrafts(downloadDrafts, kind),
  });
  const transportForXhttp = (nextMode = mode): Transport =>
    kind === 'vless-tls-xhttp'
      ? { kind, xhttp: xhttpForSave(nextMode) }
      : { kind: 'vless-reality-xhttp', xhttp: xhttpForSave(nextMode) };
  /* 这一段待提交的 vless 取值：XHTTP 档要带上完整的传输与调优参数，TCP 档只有 kind
     （目标站点、指纹那些在 body.reality 里，由目标站点行自己登记）。 */
  const vlessForSave = (): Transport | null =>
    kind === null
      ? null
      : transportIsXhttp(kind)
        ? transportForXhttp()
        : kind === 'vless-tls'
          ? { kind }
          : { kind: 'vless-reality' };
  /* 脏的两种来源：暂存了另一档安全层 / 传输层，或改了任一 XHTTP 参数。 */
  const dirty =
    stagedTransport !== null ||
    (on &&
      ((draftPath !== null && pathValue !== path) ||
        (draftHost !== null && hostValue !== host) ||
        (draftXmux !== undefined && JSON.stringify(xmuxValue) !== JSON.stringify(xmux)) ||
        (draftTuning !== undefined && JSON.stringify(tuningValue) !== JSON.stringify(tuning)) ||
        (draftMode !== null && mode !== (current?.mode ?? 'auto')))) ||
    downloadDirty;
  /* 两段各自登记进所在面板的那一次提交。三个下拉（协议 / 安全层 / 传输层）不在其中：
     它们改完即存，是切换而不是编辑。 */
  usePanelEntry(
    'vless-xhttp',
    dirty,
    {
      blocked: on && (pathBad || xmuxBad || tuningBad || downloadBad),
      apply: body => ({
        ...body,
        wires: { ...body.wires!, vless: vlessForSave() },
      }),
      reset: () => {
        setPendingTransport(null);
        setDraftPath(null);
        setDraftHost(null);
        setDraftXmux(undefined);
        setDraftTuning(undefined);
        setDraftDownload(null);
        setDraftMode(null);
      },
    },
    JSON.stringify([kind, pathValue, hostValue, xmuxValue, tuningValue, mode, downloadDrafts]),
  );
  const hy2Up = hy2Value.bandwidth.up ?? '';
  const hy2Down = hy2Value.bandwidth.down ?? '';
  const hy2BandwidthBad = (hy2Up.trim() === '') !== (hy2Down.trim() === '');
  const hy2ObfsBad = hy2Value.obfs?.kind === 'salamander' && hy2Value.obfs.password.trim() === '';
  const hy2MasqueradeBad =
    hy2Value.masquerade.kind === 'proxy' && !hy2Value.masquerade.url.trim().startsWith('https://');
  /* 端口和区间同样需要该校验。它们与带宽等字段不同：库中有 CHECK 约束
     （ingresses_hy2_port_range / _hop_range），因此不校验的后果不是写入错误数据，而是保存
     按钮可点击、点击后返回原始的数据库错误——且无法定位是哪个字段。 */
  const hy2HopValue = hy2Value.hop ?? null;
  // 与上面的 clash / hopBad 同理：只读视角（readonly / public）的 hy2Value.port 被脱敏为
  // "***"，端口与区间校验都会失真。这两项只用于挡住保存（hy2Bad → blocked），而只读视角
  // 本就不能保存，因此仅在可编辑时计算。editable = can(role,'edit')，两类只读身份一致。
  const hy2HopBad =
    editable && hy2HopValue
      ? hy2HopValue.start > hy2HopValue.end ||
        hy2HopValue.start < 1 ||
        !(hy2HopValue.start <= hy2Value.port && hy2Value.port <= hy2HopValue.end)
      : false;
  const hy2PortBad =
    editable && (hy2Value.port < 1 || hy2Value.port > 65535 || !!portClash(udpTaken, [ingress.node], hy2Value.port));
  /* force-brutal 是唯一不回退到 BBR 的档位，xray 在构建配置时即要求 up 有取值。前端增加一道
     校验，避免保存显示成功、而在 agent 应用时该机器的 xray 启动失败。 */
  const hy2ForceBrutalBad = hy2Value.congestion === 'force-brutal' && hy2Up.trim() === '';
  /* QUIC 调优项的取值范围与 xray 的构建期校验一致。超出范围不是配置不佳，而是无法启动。 */
  const quicValue = hy2Value.quic ?? {};
  const quicBad = (raw: number | null | undefined, min: number, max?: number) =>
    raw !== null && raw !== undefined && (raw < min || (max !== undefined && raw > max));
  const hy2QuicBad =
    quicBad(quicValue.init_stream_receive_window, HY2_QUIC_LIMITS.window.min) ||
    quicBad(quicValue.max_stream_receive_window, HY2_QUIC_LIMITS.window.min) ||
    quicBad(quicValue.init_connection_receive_window, HY2_QUIC_LIMITS.window.min) ||
    quicBad(quicValue.max_connection_receive_window, HY2_QUIC_LIMITS.window.min) ||
    quicBad(quicValue.max_idle_timeout_secs, HY2_QUIC_LIMITS.idle.min, HY2_QUIC_LIMITS.idle.max) ||
    quicBad(quicValue.keep_alive_period_secs, HY2_QUIC_LIMITS.keepalive.min, HY2_QUIC_LIMITS.keepalive.max) ||
    quicBad(quicValue.max_incoming_streams, HY2_QUIC_LIMITS.streams.min);
  const hy2Bad =
    hy2BandwidthBad || hy2ObfsBad || hy2MasqueradeBad || hy2HopBad || hy2PortBad || hy2ForceBrutalBad || hy2QuicBad;
  /* BBR 策略只在该连接实际使用 BBR 时被读取：选择 bbr，或选择 brutal 且带宽留空。
     reno 和已指定带宽的 brutal 都不读取该字段，此时显示该项相当于提供一个无效的选择。 */
  const bbrInPlay = hy2Value.congestion === 'bbr' || (hy2Value.congestion === 'brutal' && hy2Up.trim() === '');
  const hy2Dirty = draftHy2 !== null && JSON.stringify(hy2Value) !== JSON.stringify(activeHy2);
  usePanelEntry(
    'hy2',
    hy2Dirty,
    {
      blocked: hy2Bad,
      apply: body => ({ ...body, wires: { ...body.wires!, hysteria2: hy2Value } }),
      reset: () => setDraftHy2(null),
    },
    JSON.stringify(hy2Value),
  );
  const updateHy2 = (patch: Partial<Hysteria2Settings>) => setDraftHy2({ ...hy2Value, ...patch });

  const anytlsPaddingText = draftAnyTlsPadding ?? (anytlsValue.padding_scheme ?? []).join('\n');
  const anytlsHeaders = anytlsValue.masquerade.headers;
  const anytlsHeadersText = draftAnyTlsHeaders ?? anyTlsHeadersText(anytlsHeaders);
  const anytlsMasqueradeIsString = anytlsValue.masquerade.kind === 'string';
  const anytlsStoredStatus =
    anytlsValue.masquerade.kind === 'string' ? anytlsValue.masquerade.status_code ?? 200 : 404;
  const anytlsStatusText = draftAnyTlsStatus ?? String(anytlsStoredStatus);
  const parsedAnyTlsHeaders = parseAnyTlsHeaders(anytlsHeadersText);
  const anytlsPortBad =
    editable &&
    (!Number.isInteger(anytlsValue.port) ||
      anytlsValue.port < 1 ||
      anytlsValue.port > 65535 ||
      !!portClash(tcpTaken, [ingress.node], anytlsValue.port) ||
      (vlessOn && anytlsValue.port === ingress.port));
  const anytlsStatus = Number(anytlsStatusText);
  const anytlsStatusBad =
    anytlsMasqueradeIsString &&
    (!/^\d+$/.test(anytlsStatusText) || !Number.isInteger(anytlsStatus) || anytlsStatus < 200 || anytlsStatus > 599);
  const anytlsPaddingBad = anytlsPaddingText.trim() !== '' && !anyTlsPaddingValid(anytlsPaddingText);
  const anytlsHeadersBad = parsedAnyTlsHeaders === null;
  const anytlsForSave: AnyTlsSettings = {
    ...anytlsValue,
    padding_scheme: anytlsPaddingText
      .split(/\r?\n/)
      .map(line => line.trim())
      .filter(Boolean),
    masquerade: anytlsMasqueradeIsString
      ? {
          kind: 'string',
          content: anytlsValue.masquerade.kind === 'string' ? anytlsValue.masquerade.content : '',
          headers: parsedAnyTlsHeaders ?? {},
          status_code: anytlsStatus,
        }
      : {
          kind: 'not-found',
          headers: parsedAnyTlsHeaders ?? {},
        },
  };
  const anytlsDirty =
    anytlsOn &&
    (draftAnyTls !== null ||
      draftAnyTlsPadding !== null ||
      draftAnyTlsHeaders !== null ||
      draftAnyTlsStatus !== null);
  const anytlsBad = anytlsPortBad || anytlsPaddingBad || anytlsHeadersBad || anytlsStatusBad;
  usePanelEntry(
    'anytls',
    anytlsDirty,
    {
      blocked: anytlsBad,
      apply: body => ({ ...body, wires: { ...body.wires!, anytls: anytlsForSave } }),
      reset: () => {
        setDraftAnyTls(null);
        setDraftAnyTlsPadding(null);
        setDraftAnyTlsHeaders(null);
        setDraftAnyTlsStatus(null);
      },
    },
    JSON.stringify([anytlsValue, anytlsPaddingText, anytlsHeadersText, anytlsStatusText]),
  );
  const updateAnyTls = (patch: Partial<AnyTlsSettings>) => setDraftAnyTls({ ...anytlsValue, ...patch });
  const updateAnyTlsMasquerade = (masquerade: AnyTlsMasquerade) => updateAnyTls({ masquerade });

  /* 端口归属协议栈：落点只表示由哪台机器接收，使用哪个端口由各线路自行决定。
     因此此处分四段渲染——协议开关一段，VLESS、AnyTLS、Hysteria 2 各自包含自己的端口和参数。 */
  if (section === 'protocols') {
    return (
      <>
        <dt>协议</dt>
        <dd>
          {/* 三个独立开关，不是二选一的下拉框：每条线建立自己的 inbound，共用同一份
            授权凭据。VLESS 与 AnyTLS 都是 TCP，但端口独立；HY2 使用独立 UDP 端口。
            至少保留一条线路，避免把入口保存成完全不接收流量的状态。 */}
          <label className="toolbar" style={{ margin: 0, gap: 6 }}>
            <input
              type="checkbox"
              checked={vlessOn}
              disabled={!editable || save.isPending}
              onChange={event => toggleWire('vless', event.target.checked)}
            />
            <span>VLESS（TCP / XHTTP）</span>
          </label>
          <label className="toolbar" style={{ margin: '4px 0 0', gap: 6 }}>
            <input
              type="checkbox"
              checked={anytlsOn}
              disabled={!editable || save.isPending || stagedAnyTlsOn !== null}
              onChange={event => toggleWire('anytls', event.target.checked)}
            />
            <span>AnyTLS（TCP）</span>
          </label>
          <label className="toolbar" style={{ margin: '4px 0 0', gap: 6 }}>
            <input
              type="checkbox"
              checked={hy2}
              disabled={!editable || save.isPending || stagedHy2On !== null}
              onChange={event => toggleWire('hy2', event.target.checked)}
            />
            <span>Hysteria 2（QUIC over UDP）</span>
          </label>
          {Number(vlessOn) + Number(anytlsOn) + Number(hy2) > 1 && (
            <div className="note">
              多条线路同时开启：每条线各一个监听，<b>各占一个端口</b>，由下方各面板分别配置。
              共用一份凭据和一条授权，订阅中分别输出，由客户端自行选择。
            </div>
          )}
        </dd>
      </>
    );
  }

  if (section === 'anytls') {
    if (!anytlsOn) return null;
    const anytlsMasquerade = anytlsValue.masquerade;
    return (
      <>
        <dt>监听端口</dt>
        <dd>
          <div className="toolbar" style={{ margin: 0, gap: 6 }}>
            <input
              className="f mono"
              style={{ width: 86, borderColor: anytlsPortBad ? 'var(--err)' : undefined }}
              value={String(anytlsValue.port)}
              inputMode="numeric"
              disabled={!editable}
              aria-label="AnyTLS 监听端口"
              onChange={event => updateAnyTls({ port: Number(event.target.value.replace(/\D/g, '')) || 0 })}
            />
            <span className="dim">TCP</span>
          </div>
          {anytlsPortBad && <div className="note bad">AnyTLS 端口必须是 1–65535，且不能与同机其他 TCP 监听冲突。</div>}
        </dd>
        <dt>Padding Scheme</dt>
        <dd>
          <textarea
            className="f mono"
            rows={5}
            style={{ width: '100%', borderColor: anytlsPaddingBad ? 'var(--err)' : undefined }}
            value={anytlsPaddingText}
            disabled={!editable}
            aria-label="AnyTLS Padding Scheme"
            placeholder={'留空使用 Xray 默认\n例如：stop=2\n0=30-30'}
            onChange={event => setDraftAnyTlsPadding(event.target.value)}
          />
          <div className="note">每行一条规则；自定义方案必须包含唯一的 `stop=...`，范围支持到 4 MiB。</div>
          {anytlsPaddingBad && <div className="note bad">Padding Scheme 格式无效：请检查 `=`、重复编号、stop 和字节范围。</div>}
        </dd>
        <dt>Masquerade</dt>
        <dd>
          <select
            className="f"
            value={anytlsMasquerade.kind}
            disabled={!editable}
            aria-label="AnyTLS Masquerade 类型"
            onChange={event =>
              updateAnyTlsMasquerade(
                event.target.value === 'string'
                  ? { kind: 'string', content: '', headers: parsedAnyTlsHeaders ?? {}, status_code: 200 }
                  : { kind: 'not-found', headers: parsedAnyTlsHeaders ?? {} },
              )
            }
          >
            <option value="not-found">404 Not Found（默认）</option>
            <option value="string">自定义响应</option>
          </select>
          {anytlsMasquerade.kind === 'string' && (
            <>
              <div className="toolbar" style={{ margin: '6px 0 0', gap: 6, flexWrap: 'wrap' }}>
                <span className="dim">状态码</span>
                <input
                  className="f mono"
                  type="number"
                  min={200}
                  max={599}
                  style={{ width: 86, borderColor: anytlsStatusBad ? 'var(--err)' : undefined }}
                  value={anytlsStatusText}
                  disabled={!editable}
                  aria-label="AnyTLS Masquerade 状态码"
                  onChange={event => setDraftAnyTlsStatus(event.target.value)}
                />
              </div>
              <textarea
                className="f"
                rows={4}
                style={{ width: '100%', marginTop: 6 }}
                value={anytlsMasquerade.content}
                disabled={!editable}
                aria-label="AnyTLS Masquerade 正文"
                placeholder="响应正文"
                onChange={event => updateAnyTlsMasquerade({ ...anytlsMasquerade, content: event.target.value })}
              />
            </>
          )}
          <textarea
            className="f mono"
            rows={4}
            style={{ width: '100%', marginTop: 6, borderColor: anytlsHeadersBad ? 'var(--err)' : undefined }}
            value={anytlsHeadersText}
            disabled={!editable}
            aria-label="AnyTLS Masquerade Headers"
            placeholder={'可选，每行一个 Header\nCache-Control: no-store'}
            onChange={event => setDraftAnyTlsHeaders(event.target.value)}
          />
          {anytlsHeadersBad && <div className="note bad">Headers 必须是一行一个 `名称: 值`，名称不能重复或包含非法字符。</div>}
          {anytlsStatusBad && <div className="note bad">自定义响应状态码必须是 200–599。</div>}
          <div className="note">AnyTLS 客户端未完成握手时返回此 HTTP 响应；默认严格返回 404。</div>
        </dd>
      </>
    );
  }

  if (section === 'hy2') {
    if (!hy2) return null;
    return (
      <>
        <IngressHy2PortRow ingress={ingress} value={hy2Value} onChange={updateHy2} editable={editable} />
        <dt>参数</dt>
        <dd>
          <>
            <div className="toolbar" style={{ margin: 0, gap: 6, flexWrap: 'wrap' }}>
              <span className="dim">上行</span>
              <input
                className="f mono"
                style={{ width: 110, borderColor: hy2BandwidthBad ? 'var(--err)' : undefined }}
                placeholder="留空用 BBR"
                value={hy2Up}
                disabled={!editable}
                onChange={event => updateHy2({ bandwidth: { ...hy2Value.bandwidth, up: event.target.value || null } })}
              />
              <span className="dim">下行</span>
              <input
                className="f mono"
                style={{ width: 110, borderColor: hy2BandwidthBad ? 'var(--err)' : undefined }}
                placeholder="留空用 BBR"
                value={hy2Down}
                disabled={!editable}
                onChange={event =>
                  updateHy2({ bandwidth: { ...hy2Value.bandwidth, down: event.target.value || null } })
                }
              />
              <select
                className="f"
                value={hy2Value.congestion}
                disabled={!editable}
                onChange={event => updateHy2({ congestion: event.target.value as Hysteria2Settings['congestion'] })}
              >
                <option value="brutal">Brutal（带宽留空时退回 BBR）</option>
                <option value="reno">New Reno（不看带宽）</option>
                <option value="bbr">BBR（忽略带宽）</option>
                <option value="force-brutal">Brutal 强制</option>
              </select>
            </div>
            {bbrInPlay && (
              <div className="toolbar" style={{ margin: '6px 0 0', gap: 6, flexWrap: 'wrap' }}>
                <span className="dim">BBR 策略</span>
                <select
                  className="f"
                  value={hy2Value.bbr_profile ?? 'standard'}
                  disabled={!editable}
                  onChange={event => updateHy2({ bbr_profile: event.target.value as HysteriaBbrProfile })}
                >
                  <option value="standard">Standard（默认）</option>
                  <option value="conservative">Conservative（更保守）</option>
                  <option value="aggressive">Aggressive（更激进）</option>
                </select>
              </div>
            )}
            <div className="toolbar" style={{ margin: '6px 0 0', gap: 6, flexWrap: 'wrap' }}>
              <span className="dim">混淆</span>
              <select
                className="f"
                value={hy2Value.obfs ? 'salamander' : 'off'}
                disabled={!editable}
                onChange={event =>
                  updateHy2({
                    obfs:
                      event.target.value === 'salamander' ? { kind: 'salamander', password: 'quick-brown-fox' } : null,
                  })
                }
              >
                <option value="off">关闭</option>
                <option value="salamander">Salamander</option>
              </select>
              {hy2Value.obfs?.kind === 'salamander' && (
                <input
                  className="f mono"
                  type="password"
                  autoComplete="new-password"
                  style={{ width: 220, borderColor: hy2ObfsBad ? 'var(--err)' : undefined }}
                  placeholder="混淆密码"
                  value={hy2Value.obfs.password}
                  disabled={!editable}
                  onChange={event => updateHy2({ obfs: { kind: 'salamander', password: event.target.value } })}
                />
              )}
            </div>
            {hy2Value.obfs && (
              <>
                {/* 默认密码提示只对要填这个字段的可编辑视角有用；readonly / public 不填表单，
                    也不必向只读视角显示默认凭据。 */}
                {editable && (
                  <div className="note warn">
                    默认混淆密码：<span className="mono">quick-brown-fox</span>（不含句号）。
                  </div>
                )}
                <div className="note">在 QUIC 数据包外层叠加一层对称加密，使流量特征不可被 DPI 识别为 Hysteria。</div>
              </>
            )}
            <div className="toolbar" style={{ margin: '6px 0 0', gap: 6, flexWrap: 'wrap' }}>
              <span className="dim">伪装</span>
              <select
                className="f"
                value={hy2Value.masquerade.kind}
                disabled={!editable}
                onChange={event =>
                  updateHy2({
                    masquerade: event.target.value === 'proxy' ? { kind: 'proxy', url: '' } : { kind: 'not-found' },
                  })
                }
              >
                <option value="not-found">404</option>
                <option value="proxy">反代真站点</option>
              </select>
              {hy2Value.masquerade.kind === 'proxy' && (
                <input
                  className="f mono"
                  style={{ width: 260, borderColor: hy2MasqueradeBad ? 'var(--err)' : undefined }}
                  placeholder="https://www.example.com"
                  value={hy2Value.masquerade.url}
                  disabled={!editable}
                  onChange={event => updateHy2({ masquerade: { kind: 'proxy', url: event.target.value } })}
                />
              )}
            </div>
            {hy2BandwidthBad && <div className="note bad">上行和下行要一起填；都留空就使用 BBR</div>}
            {hy2ForceBrutalBad && <div className="note bad">force-brutal 必须填上行带宽，否则 xray 启动失败。</div>}
            {/* QUIC 调优收入折叠区。八项留空时均使用 xray 的默认值，修改需要具体依据；
                展开显示会使一个不常修改的分组占用该区域一半的高度。 */}
            <details className="form-adv" style={{ width: '100%' }}>
              <summary>QUIC 调优（留空 = 用 xray 默认）</summary>
              <div className="hy2-quic">
                {(
                  [
                    ['init_stream_receive_window', '流初始窗口', 'bytes', HY2_QUIC_LIMITS.window.min, undefined],
                    ['max_stream_receive_window', '流最大窗口', 'bytes', HY2_QUIC_LIMITS.window.min, undefined],
                    ['init_connection_receive_window', '连接初始窗口', 'bytes', HY2_QUIC_LIMITS.window.min, undefined],
                    ['max_connection_receive_window', '连接最大窗口', 'bytes', HY2_QUIC_LIMITS.window.min, undefined],
                    ['max_idle_timeout_secs', '空闲超时', '秒', HY2_QUIC_LIMITS.idle.min, HY2_QUIC_LIMITS.idle.max],
                    [
                      'keep_alive_period_secs',
                      '保活间隔',
                      '秒',
                      HY2_QUIC_LIMITS.keepalive.min,
                      HY2_QUIC_LIMITS.keepalive.max,
                    ],
                    ['max_incoming_streams', '最大并发流', '', HY2_QUIC_LIMITS.streams.min, undefined],
                  ] as [keyof HysteriaQuic, string, string, number, number | undefined][]
                ).map(([key, label, unit, min, max]) => {
                  const raw = quicValue[key] as number | null | undefined;
                  const bad = quicBad(raw, min, max);
                  return (
                    <label key={key} className="hy2-quic-fld">
                      <span>
                        {label} {unit && <small>{unit}</small>}
                      </span>
                      <input
                        className="f mono"
                        type="number"
                        min={min}
                        max={max}
                        inputMode="numeric"
                        placeholder={max === undefined ? `≥ ${min}` : `${min}–${max}`}
                        style={{ borderColor: bad ? 'var(--err)' : undefined }}
                        value={raw ?? ''}
                        disabled={!editable}
                        onChange={event => {
                          const text = event.target.value.trim();
                          updateHy2({
                            quic: { ...quicValue, [key]: text === '' ? null : Number(text) },
                          });
                        }}
                      />
                    </label>
                  );
                })}
                <label className="hy2-quic-fld wide">
                  <input
                    type="checkbox"
                    checked={quicValue.disable_path_mtu_discovery ?? false}
                    disabled={!editable}
                    onChange={event =>
                      updateHy2({
                        quic: { ...quicValue, disable_path_mtu_discovery: event.target.checked },
                      })
                    }
                  />
                  <span>关闭路径 MTU 探测</span>
                </label>
              </div>
              <div className="note">上游建议流窗口与连接窗口保持 2:5。改这些要发布并重启 xray。</div>
              {hy2QuicBad && <div className="note bad">标红的值超出 xray 接受的范围，它会启动失败。</div>}
            </details>
          </>
        </dd>
        {/* 段级保存：这一段的所有取值共用一份草稿，也共用一次提交，因此只需要一个按钮。
            排在段末右下角——改完往下走就是它，不必回到改动所在的那一行去找。
            没有改动时不渲染：一个常驻的灰按钮会让「能不能存」变成需要辨认的状态。 */}
      </>
    );
  }

  // section === 'vless'
  if (!vlessOn) return null;
  return (
    <>
      <dt>监听端口</dt>
      <dd>
        <IngressPortEditor appId={appId} ingress={ingress} editable={editable} compact />
      </dd>
      {
        <>
          <dt>安全层</dt>
          <dd>
            <select
              className="f"
              value={tls ? 'tls' : 'reality'}
              disabled={!editable || save.isPending}
              onChange={event => switchKind(transportKindFor(event.target.value as IngressSecurity, on))}
            >
              <option value="reality">REALITY</option>
              <option value="tls">TLS</option>
            </select>
            {tls && certificateName && (
              <div className="note">
                使用本机证书 <span className="mono">{certificateName}</span>。
              </div>
            )}
            {tls && !certificateName && (
              <div className="note bad">本机证书未签发，编译会拒绝这个接入面。前往「机器」页签发。</div>
            )}
            {tls && !on && (
              <div className="note warn">TLS 直接承载于 TCP 时存在 TLS-in-TLS 特征，封装进 XHTTP 后消失。</div>
            )}
          </dd>
          {!tls && (
            <IngressRealityRow appId={appId} ingress={ingress} certificateName={certificateName} editable={editable} />
          )}

          <dt>传输层</dt>
          <dd>
            <select
              className="f"
              value={on ? 'xhttp' : 'tcp'}
              disabled={!editable || save.isPending}
              onChange={event =>
                switchKind(
                  transportKindFor(
                    (kind?.startsWith('vless-tls') ? 'tls' : 'reality') as IngressSecurity,
                    event.target.value === 'xhttp',
                  ),
                )
              }
            >
              <option value="tcp">TCP</option>
              <option value="xhttp">XHTTP</option>
            </select>
            {on && (
              <>
                <div className="note">
                  {tls
                    ? 'TLS + XHTTP 支持前置 CDN。常见端口支持：443，2053，2083，2087，2096，8443。'
                    : 'REALITY + XHTTP 不支持前置 CDN。'}
                </div>
                {flowOn && <div className="note">已将下方流控重置为关闭，流控选项与 XHTTP 互斥。</div>}
              </>
            )}
          </dd>
        </>
      }

      {/* XHTTP 自身的参数单独一行，排在三层之后。
          放入「传输层」字段时，启用 XHTTP 后该字段会包含大量调优控件和说明，
          将相邻排列的三行撑开——而这三行的连续排列正是表示它们属于三个层次的唯一方式。 */}
      {on && (
        <>
          <dt>XHTTP</dt>
          <dd>
            <div className="toolbar" style={{ margin: 0, gap: 6 }}>
              <span className="dim">路径</span>
              <input
                className="f mono"
                style={{ borderColor: pathBad ? 'var(--err)' : undefined }}
                value={pathValue}
                disabled={!editable}
                onChange={e => setDraftPath(e.target.value)}
              />
            </div>
            {pathBad && <div className="note bad">路径需以 / 开头，不能包含空白或 ? #</div>}
            <div className="note">修改路径会使已下发的客户端配置全部失效，客户端需重新导入。</div>
            <div className="toolbar" style={{ margin: '6px 0 0', gap: 6 }}>
              <span className="dim">上传 HTTP Host</span>
              <input
                className="f mono"
                placeholder="跟随 SNI"
                value={hostValue}
                disabled={!editable}
                onChange={e => setDraftHost(e.target.value)}
              />
            </div>
            <div className="toolbar" style={{ margin: '6px 0 0', gap: 6 }}>
              <span className="dim">上行模式</span>
              <select
                className="f"
                value={mode}
                disabled={!editable || save.isPending}
                onChange={e => setDraftMode(e.target.value as XhttpMode)}
              >
                <option value="auto">跟随两端</option>
                <option value="packet-up">packet-up（拆成多个小 POST）</option>
                <option value="stream-up">stream-up（上行一条流，下行另开）</option>
                <option value="stream-one" disabled={hasDownload}>
                  stream-one（上下行同一个请求）
                </option>
              </select>
            </div>
            {mode === 'auto' && (
              <div className="note">
                不写入该字段，由两端各自决定。当前解析为 <span className="mono">{resolvedMode}</span>。
              </div>
            )}
            {mode === 'packet-up' && (
              <div className="note warn">服务端仅接受 packet-up。已下发的客户端配置全部无法连接，需重新导入订阅。</div>
            )}
            {hasDownload && (
              <div className="note">
                已启用独立下载，Xray 不支持 <span className="mono">stream-one</span>。
              </div>
            )}
            <details className="form-adv" style={{ width: '100%' }}>
              <summary>Padding 与 XMUX 调优（留空 = 用 Xray 默认）</summary>
              <div className="hy2-quic xhttp-xmux">
                <label className="hy2-quic-fld xhttp-xmux-range-fld">
                  <span>
                    Padding <small>字节</small>
                  </span>
                  <span className="xhttp-xmux-range">
                    <input
                      className="f mono"
                      type="number"
                      min={1}
                      max={4096}
                      inputMode="numeric"
                      placeholder={String(DEFAULT_XHTTP_TUNING.x_padding_bytes.from)}
                      value={tuningDraft.paddingFrom}
                      disabled={!editable}
                      aria-label="XHTTP Padding 下限"
                      onChange={e => setDraftTuning({ ...tuningDraft, paddingFrom: e.target.value })}
                    />
                    <i>–</i>
                    <input
                      className="f mono"
                      type="number"
                      min={1}
                      max={4096}
                      inputMode="numeric"
                      placeholder={String(DEFAULT_XHTTP_TUNING.x_padding_bytes.to)}
                      value={tuningDraft.paddingTo}
                      disabled={!editable}
                      aria-label="XHTTP Padding 上限"
                      onChange={e => setDraftTuning({ ...tuningDraft, paddingTo: e.target.value })}
                    />
                  </span>
                </label>
                <label className="hy2-quic-fld">
                  <span>
                    最大并发流 <small>流</small>
                  </span>
                  <input
                    className="f mono"
                    type="number"
                    min={1}
                    max={128}
                    inputMode="numeric"
                    placeholder={String(DEFAULT_XHTTP_XMUX.max_concurrency ?? 1)}
                    value={xmuxDraft.maxConcurrency}
                    disabled={!editable}
                    aria-label="XMUX 最大并发流"
                    onChange={e =>
                      setDraftXmux({
                        ...xmuxDraft,
                        maxConcurrency: e.target.value,
                        maxConnections: e.target.value ? '' : xmuxDraft.maxConnections,
                      })
                    }
                  />
                </label>
                <label className="hy2-quic-fld">
                  <span>
                    最大连接数 <small>条</small>
                  </span>
                  <input
                    className="f mono"
                    type="number"
                    min={1}
                    max={128}
                    inputMode="numeric"
                    placeholder="不限制"
                    value={xmuxDraft.maxConnections}
                    disabled={!editable}
                    aria-label="XMUX 最大连接数"
                    onChange={e =>
                      setDraftXmux({
                        ...xmuxDraft,
                        maxConnections: e.target.value,
                        maxConcurrency: e.target.value ? '' : xmuxDraft.maxConcurrency,
                      })
                    }
                  />
                </label>
                {(
                  [
                    [
                      '请求轮换',
                      '次',
                      'requestFrom',
                      'requestTo',
                      'XMUX 请求轮换下限',
                      'XMUX 请求轮换上限',
                      DEFAULT_XHTTP_XMUX.h_max_request_times,
                    ],
                    [
                      '复用时长',
                      '秒',
                      'reusableFrom',
                      'reusableTo',
                      'XMUX 复用时长下限',
                      'XMUX 复用时长上限',
                      DEFAULT_XHTTP_XMUX.h_max_reusable_secs,
                    ],
                  ] as const
                ).map(([label, unit, fromKey, toKey, fromLabel, toLabel, defaults]) => (
                  <label className="hy2-quic-fld xhttp-xmux-range-fld" key={label}>
                    <span>
                      {label} <small>{unit}</small>
                    </span>
                    <span className="xhttp-xmux-range">
                      <input
                        className="f mono"
                        type="number"
                        min={1}
                        max={2_147_483_647}
                        inputMode="numeric"
                        placeholder={String(defaults.from)}
                        value={xmuxDraft[fromKey]}
                        disabled={!editable}
                        aria-label={fromLabel}
                        onChange={e => setDraftXmux({ ...xmuxDraft, [fromKey]: e.target.value })}
                      />
                      <i>–</i>
                      <input
                        className="f mono"
                        type="number"
                        min={1}
                        max={2_147_483_647}
                        inputMode="numeric"
                        placeholder={String(defaults.to)}
                        value={xmuxDraft[toKey]}
                        disabled={!editable}
                        aria-label={toLabel}
                        onChange={e => setDraftXmux({ ...xmuxDraft, [toKey]: e.target.value })}
                      />
                    </span>
                  </label>
                ))}
                <label className="hy2-quic-fld">
                  <span>
                    空闲保活 <small>秒</small>
                  </span>
                  <input
                    className="f mono"
                    type="number"
                    min={-1}
                    max={3600}
                    inputMode="numeric"
                    placeholder="Xray 默认"
                    value={xmuxDraft.keepAlive}
                    disabled={!editable}
                    aria-label="XMUX 空闲保活间隔"
                    onChange={e => setDraftXmux({ ...xmuxDraft, keepAlive: e.target.value })}
                  />
                </label>
              </div>
              <div className="note">
                最大并发流和最大连接数二选一；前者限制每条连接承载的流，后者固定连接池上限。保活填 -1 表示关闭，留空采用
                Xray 的 H2/H3 默认值。
              </div>
              <div className="note">
                Padding 默认 {DEFAULT_XHTTP_TUNING.x_padding_bytes.from}–{DEFAULT_XHTTP_TUNING.x_padding_bytes.to}{' '}
                字节；XMUX 空值分别采用 {DEFAULT_XHTTP_XMUX.max_concurrency}、
                {DEFAULT_XHTTP_XMUX.h_max_request_times.from}–{DEFAULT_XHTTP_XMUX.h_max_request_times.to} 次和{' '}
                {DEFAULT_XHTTP_XMUX.h_max_reusable_secs.from}–{DEFAULT_XHTTP_XMUX.h_max_reusable_secs.to}{' '}
                秒；全部留空时不写入 xmux。
              </div>
              {xmuxBad && (
                <div className="note bad">
                  并发流和连接数必须二选一且为 1–128；范围需为正整数；保活只能填 -1 或 1–3600。
                </div>
              )}
              {tuningBad && <div className="note bad">Padding 必须是 1–4096 的有效整数范围。</div>}
            </details>
            <div className="toolbar" style={{ margin: '8px 0 0', gap: 6 }}>
              <span className="dim">独立下载</span>
            </div>
            {PROJECTION_FAMILIES.map(family => {
              const node = streamNodes.data?.nodes.find(node => node.node_id === ingress.node);
              const projected = ingress.projection?.[family];
              const publicHost = family === 'v4' ? node?.public_ipv4 : node?.public_ipv6;
              const publicNat = family === 'v4' ? node?.public_ipv4_nat : node?.public_ipv6_nat;
              const endpoint =
                projected ??
                (publicHost && !publicNat
                  ? {
                      host: publicHost,
                      port: ingress.port,
                    }
                  : null);
              if (!endpoint) return null;
              const draft = downloadDrafts[family];
              const label = family === 'v4' ? 'IPv4' : 'IPv6';
              const mux = draft?.mux.trim() ? Number(draft.mux) : null;
              const muxBad = mux !== null && (!Number.isInteger(mux) || mux < 2 || mux > 128);
              const setDownload = (next: XhttpDownloadDraft | null) =>
                setDraftDownload({ ...downloadDrafts, [family]: next });
              return (
                <div key={family} style={{ marginTop: 8 }}>
                  <div className="toolbar" style={{ margin: 0, gap: 6 }}>
                    <span className="dim">{label}</span>
                    <SegSwitch
                      checked={draft !== null}
                      disabled={!editable}
                      onChange={next => setDownload(next ? xhttpDownloadDraftOf(endpoint, splitReality) : null)}
                      off="跟随主连接"
                      on="独立下载"
                    />
                  </div>
                  {draft && (
                    <>
                      <div className="ing-pj">
                        <span className="dim">下载地址</span>
                        <input
                          className="f mono"
                          value={draft.host}
                          placeholder="地址或域名"
                          disabled={!editable}
                          onChange={event => setDownload({ ...draft, host: event.target.value })}
                        />
                        <span className="dim">:</span>
                        <input
                          className="f mono ing-pj-port"
                          value={draft.port}
                          inputMode="numeric"
                          disabled={!editable}
                          onChange={event => setDownload({ ...draft, port: event.target.value })}
                        />
                      </div>
                      {splitReality && (
                        <div className="ing-pj">
                          <span className="dim">节点实际监听端口</span>
                          <input
                            className="f mono ing-pj-port"
                            value={draft.originPort ?? draft.port}
                            inputMode="numeric"
                            disabled={!editable}
                            onChange={event => setDownload({ ...draft, originPort: event.target.value })}
                          />
                        </div>
                      )}
                      <div className="toolbar" style={{ margin: '6px 0 0', gap: 6 }}>
                        <span className="dim">下载 HTTP Host</span>
                        <input
                          className="f mono"
                          placeholder="跟随下载 SNI"
                          value={draft.httpHost}
                          disabled={!editable}
                          onChange={event => setDownload({ ...draft, httpHost: event.target.value })}
                        />
                        <span className="dim">下载 XMUX</span>
                        <input
                          className="f mono"
                          type="number"
                          min={2}
                          max={128}
                          inputMode="numeric"
                          style={{ width: 100, borderColor: muxBad ? 'var(--err)' : undefined }}
                          title="留空使用客户端默认"
                          placeholder="两端默认"
                          value={draft.mux}
                          disabled={!editable}
                          aria-label={`${label} 下载 XMUX`}
                          onChange={event => setDownload({ ...draft, mux: event.target.value })}
                        />
                      </div>
                    </>
                  )}
                </div>
              );
            })}
            {!PROJECTION_FAMILIES.some(family => {
              const node = streamNodes.data?.nodes.find(node => node.node_id === ingress.node);
              const publicHost = family === 'v4' ? node?.public_ipv4 : node?.public_ipv6;
              const publicNat = family === 'v4' ? node?.public_ipv4_nat : node?.public_ipv6_nat;
              return !!ingress.projection?.[family] || !!publicHost && !publicNat;
            }) && <div className="note">节点没有可用的 IPv4 或 IPv6 公网入口，无法生成独立下载订阅。</div>}
            {downloadBad && (
              <div className="note bad">
                独立下载的地址、端口、REALITY 节点实际监听端口和 XMUX 必须填写有效值；XMUX 范围为 2–128。
              </div>
            )}
            {splitReality && hasDownload && (
              <div className="note">REALITY 独立下载需要本机证书，修改后需发布并重启 xray。</div>
            )}
          </dd>
        </>
      )}
      {section === 'vless' && <IngressFlowRow appId={appId} ingress={ingress} editable={editable} xhttp={on} />}
      {!tls && (
        <>
          <IngressRealityGuardRow appId={appId} ingress={ingress} editable={editable} />
          <IngressRealityLimitsRow appId={appId} ingress={ingress} editable={editable} />
        </>
      )}
    </>
  );
}

// 单个地址族的投影。填写后订阅中该族使用该地址；未填写则使用节点公网地址加监听端口。
// 普通投影只影响订阅；REALITY + XHTTP 的独立下载还会生成 TLS 前置，因此需要发布。
//
// 「不投影」表示开关关闭，不是地址留空。两者必须区分：空串写入库后无法判断是意图关闭
// 还是填写不完整，而产物中会生成一个无法连接的地址，机器侧则一切正常。
// 因此关闭时不渲染输入框——保留一个空输入框会使人认为清空也可以关闭投影。

export type ProjectionHandle = {
  dirty: boolean;
  blocked: boolean;
  apply: (projection: IngressProjection) => IngressProjection;
  save: () => void;
  reset: () => void;
};

export function useProjectionHandles() {
  const [projHandles, setProjHandles] = useState<Partial<Record<ProjectionFamily, ProjectionHandle>>>({});

  const register = useCallback((family: ProjectionFamily, handle: ProjectionHandle) => {
    setProjHandles(previous => (previous[family] === handle ? previous : { ...previous, [family]: handle }));
  }, []);
  const onV4Handle = useCallback((handle: ProjectionHandle) => register('v4', handle), [register]);
  const onV6Handle = useCallback((handle: ProjectionHandle) => register('v6', handle), [register]);

  return {
    projHandles,
    projDirty: Object.values(projHandles).some(handle => handle.dirty),
    projBlocked: Object.values(projHandles).some(handle => handle.blocked),
    onV4Handle,
    onV6Handle,
  };
}

export function IngressProjectionRow({
  appId,
  ingress,
  family,
  node,
  editable,
  onHandle,
}: {
  appId: string;
  ingress: SnapshotIngress;
  family: 'v4' | 'v6';
  node?: { public_ipv4: string | null; public_ipv6: string | null };
  editable: boolean;
  onHandle?: (h: ProjectionHandle) => void;
}) {
  const qc = useQueryClient();
  const current = ingress.projection?.[family] ?? null;
  const [draft, setDraft] = useState<{ host: string; port: string } | null>(null);
  const [disabledDraft, setDisabledDraft] = useState(false);

  const publicAddr = family === 'v4' ? node?.public_ipv4 : node?.public_ipv6;
  const label = family === 'v4' ? 'IPv4' : 'IPv6';

  const port = Number(draft?.port ?? '');
  const valid =
    !!draft &&
    draft.host.trim() !== '' &&
    /^\d+$/.test(draft.port) &&
    Number.isInteger(port) &&
    port > 0 &&
    port < 65536;

  const desired = useMemo(
    () =>
      disabledDraft
        ? null
        : draft && valid
          ? { host: draft.host.trim(), port }
          : current,
    [current, disabledDraft, draft, port, valid],
  );

  const applyProjection = useCallback(
    (projection: IngressProjection): IngressProjection => {
      if (disabledDraft) return { ...projection, [family]: null };
      if (!draft || !valid) return projection;
      return {
        ...projection,
        [family]: { host: draft.host.trim(), port },
      };
    },
    [disabledDraft, draft, family, port, valid],
  );

  const save = useMutation({
    mutationFn: (next: ProjectionEndpoint | null) => {
      const base = ingressUpsertBody(ingress);
      return upsertIngress(appId, { ...base, projection: { ...(base.projection ?? {}), [family]: next } }, base);
    },
    onSuccess: () => {
      setDraft(null);
      setDisabledDraft(false);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });
  const mutateProjection = save.mutate;

  const managed = onHandle !== undefined;
  const on = draft !== null || (current !== null && !disabledDraft);
  const dirty = disabledDraft || draft !== null;
  const doSave = useCallback(() => {
    if (managed) {
      if (disabledDraft) mutateProjection(null);
      else if (desired) mutateProjection(desired);
      return;
    }
    if (disabledDraft) {
      mutateProjection(null);
      return;
    }
    if (draft && valid) mutateProjection(desired);
  }, [desired, disabledDraft, draft, managed, mutateProjection, valid]);
  const resetProjection = useCallback(() => {
    setDraft(null);
    setDisabledDraft(false);
  }, []);
  const handle = useMemo<ProjectionHandle>(
    () => ({
      dirty,
      blocked: dirty && !disabledDraft && !valid,
      apply: applyProjection,
      save: doSave,
      reset: resetProjection,
    }),
    [applyProjection, disabledDraft, dirty, doSave, resetProjection, valid],
  );
  useEffect(() => {
    onHandle?.(handle);
  }, [handle, onHandle]);

  return (
    <>
      <dt>{label} 地址</dt>
      <dd>
        <SegSwitch
          checked={on}
          disabled={!editable || save.isPending}
          onChange={next => {
            if (next) {
              setDisabledDraft(false);
              setDraft({
                host: current?.host ?? '',
                port: String(current?.port ?? ingress.port),
              });
            } else if (current) {
              setDraft(null);
              if (managed) setDisabledDraft(true);
              else save.mutate(null);
            } else {
              setDraft(null);
              setDisabledDraft(false);
            }
          }}
          off="默认"
          on="转换"
        />
        {draft ? (
          <div className="ing-pj">
            <input
              className="f mono"
              value={draft.host}
              placeholder="地址或域名"
              onChange={e => setDraft({ ...draft, host: e.target.value })}
            />
            <span className="dim">:</span>
            <input
              className="f mono ing-pj-port"
              value={draft.port}
              inputMode="numeric"
              onChange={e => setDraft({ ...draft, port: e.target.value })}
            />
          </div>
        ) : on && current ? (
          <>
            <span className="mono">
              {current.host}:{current.port}
            </span>
            <button
              className="btn"
              style={{ marginLeft: 8 }}
              disabled={!editable || save.isPending}
              onClick={() => {
                setDisabledDraft(false);
                setDraft({ host: current.host, port: String(current.port) });
              }}
            >
              编辑
            </button>
          </>
        ) : (
          <div className="note">
            {publicAddr ? (
              <>
                使用机器公网地址{' '}
                <span className="mono">
                  {publicAddr}:{ingress.port}
                </span>
              </>
            ) : (
              <>机器无公网 {label}，不生成此条订阅</>
            )}
          </div>
        )}
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

type OrderDrag =
  | { kind: 'apps'; active: string; original: string[]; order: string[] }
  | { kind: 'chains'; appId: string; active: string; original: string[]; order: string[] };

const sameOrder = (left: readonly string[], right: readonly string[]) =>
  left.length === right.length && left.every((id, index) => id === right[index]);

function moveOrder(order: readonly string[], active: string, target: string): string[] {
  const from = order.indexOf(active);
  const to = order.indexOf(target);
  if (from < 0 || to < 0 || from === to) return [...order];
  const next = [...order];
  next.splice(from, 1);
  next.splice(to, 0, active);
  return next;
}

function orderByIds<T>(items: readonly T[], ids: readonly string[] | undefined, idOf: (item: T) => string): T[] {
  if (!ids || ids.length !== items.length) return [...items];
  const byId = new Map(items.map(item => [idOf(item), item]));
  const ordered = ids.map(id => byId.get(id)).filter((item): item is T => item !== undefined);
  return ordered.length === items.length ? ordered : [...items];
}

export type OrderSlot = {
  id: string;
  left: number;
  top: number;
  width: number;
  height: number;
};

/**
 * Resolve a drag against the slots captured at pointer-down, not against cards that React has
 * already moved. Reading the live layout makes a stationary pointer alternate between the dragged
 * card and its neighbour: each swap changes which card is under that pointer, so the next event
 * immediately swaps them back.
 */
export function orderForPointer(
  baseline: readonly string[],
  active: string,
  slots: readonly OrderSlot[],
  x: number,
  y: number,
  scrollDelta = 0,
): string[] {
  let target = active;
  let distance = Infinity;
  for (const slot of slots) {
    const dx = slot.left + slot.width / 2 - x;
    const dy = slot.top + slot.height / 2 - scrollDelta - y;
    const nextDistance = dx * dx + dy * dy;
    if (nextDistance < distance) {
      distance = nextDistance;
      target = slot.id;
    }
  }
  return moveOrder(baseline, active, target);
}

// 拖到视口上下缘时要滚动的容器：从被拖元素向上找第一个真正能纵向滚动的祖先，
// 找不到就用整个窗口（本站是固定顶栏 + 文档滚动）。
function findScrollParent(el: HTMLElement | null): HTMLElement | Window {
  for (let node = el?.parentElement ?? null; node; node = node.parentElement) {
    const overflow = getComputedStyle(node).overflowY;
    if ((overflow === 'auto' || overflow === 'scroll') && node.scrollHeight > node.clientHeight) return node;
  }
  return window;
}

function orderElements(root: HTMLElement, order: OrderDrag): HTMLElement[] {
  const selector = order.kind === 'apps' ? '[data-order-kind="app"]' : '[data-order-kind="chain"]';
  return [...root.querySelectorAll<HTMLElement>(selector)].filter(
    el => order.kind === 'apps' || el.dataset.orderApp === order.appId,
  );
}

function ChainList({ go }: { go: (d: Drill) => void }) {
  const { who } = useSession();
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  // 探测结果使用独立查询，不与 snapshot 合并：它的更新频率远高于模型（一分钟一轮），
  // 合并后每轮探测都会导致整棵模型树重新渲染。
  const probes = useQuery({
    queryKey: ['e2e-probes'],
    queryFn: () => fetchE2eProbes(),
    refetchInterval: 30_000,
  });
  const probeOf = byChain(probes.data?.chains);

  // 创建项目和重命名都调用 `upsert_app_tx`，该接口要求 system-admin（store/console.rs）。
  // 判定放宽到 edit 会导致 editor 点了返回 403——禁用与否的依据必须与服务端一致。
  const owner = can(who.role, 'system');
  // 多选删除。行尾常驻的 × 已移除：它在每一行都显示，而删除链是本页影响最大的操作，
  // 常驻显示相当于把该操作放在最易误触的位置；且一次只能删除一条，删除三条需要点击三次，
  // 期间没有统一的撤销机会。
  // 改为显式进入多选：未点击「多选」时本页没有删除按钮，点击后行首出现勾选框、
  // 主按钮变为删除——当前处于可删除状态由整页形态表示，而不依靠一个图标。
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const rowKey = (appId: string, chainId: string) => `${appId}/${chainId}`;
  const togglePick = (key: string) =>
    setPicked(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const exitSelect = () => {
    setSelecting(false);
    setPicked(new Set());
  };
  /* 建链和创建接入面只需 editor 权限，低于创建项目所需的权限（chain-wizard 中有相同判定）。 */
  const editable = can(who.role, 'edit');
  const [creating, setCreating] = useState<{ id: string; label: string } | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; label: string } | null>(null);
  const [orderDrag, setOrderDragState] = useState<OrderDrag | null>(null);
  const orderDragRef = useRef<OrderDrag | null>(null);
  // FLIP 用：`.chain-sections` 容器、上一帧各卡片的位置、以及「这一帧要不要播动画」的开关。
  const sectionsRef = useRef<HTMLDivElement | null>(null);
  const flipRects = useRef<Map<string, DOMRect>>(new Map());
  const animateReorder = useRef(false);
  // 整卡可拖后，一次真实拖动结束时紧跟的 click 不应再触发「打开这条链」。
  const justDragged = useRef(false);

  const setOrderDrag = (next: OrderDrag | null) => {
    orderDragRef.current = next;
    setOrderDragState(next);
  };

  useEffect(
    () => () => {
      document.body.classList.remove('chain-order-dragging');
      sectionsRef.current?.classList.remove('app-order-dragging');
      // 拖拽进行中卸载（如导航离开）时，幽灵是挂在 body 上的游离节点，一并清掉。
      for (const stray of document.querySelectorAll('.order-ghost')) stray.remove();
    },
    [],
  );

  // 重排后让卡片从旧位平滑滑到新位（FLIP），取代瞬移——四列网格换行时尤其明显。
  // 只在 animateReorder 置位的那一帧播放（拖动移动、方向键微调）；探测数据刷新等
  // 不改变顺序的重渲染只更新记录、不触发动画。
  useLayoutEffect(() => {
    const root = sectionsRef.current;
    const current = orderDragRef.current;
    if (!root || !current) return;
    const play = animateReorder.current;
    animateReorder.current = false;
    const seen = new Set<string>();
    for (const el of orderElements(root, current)) {
      const key = `${el.dataset.orderKind}:${el.dataset.orderApp ?? ''}:${el.dataset.orderId}`;
      seen.add(key);
      const last = el.getBoundingClientRect();
      const first = flipRects.current.get(key);
      flipRects.current.set(key, last);
      if (!play || !first) continue;
      const dx = first.left - last.left;
      const dy = first.top - last.top;
      if (!dx && !dy) continue;
      for (const running of el.getAnimations()) {
        if ((running as { _flip?: boolean })._flip) running.cancel();
      }
      const anim = el.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: 'translate(0, 0)' }], {
        duration: 180,
        easing: 'cubic-bezier(0.2, 0.7, 0.25, 1)',
      });
      (anim as { _flip?: boolean })._flip = true;
    }
    for (const key of [...flipRects.current.keys()]) if (!seen.has(key)) flipRects.current.delete(key);
  });

  const appsNow = () => snapshot.data?.snapshot.apps ?? [];
  const invalidate = () => {
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['compile'] });
  };

  const persistOrder = (order: OrderDrag) => {
    if (sameOrder(order.order, order.original)) return;
    if (order.kind === 'apps') {
      void reorderApps(order.order).then(invalidate);
    } else {
      void reorderChains(order.appId, order.order).then(invalidate);
    }
  };

  const nudgeOrder = (event: ReactKeyboardEvent<HTMLButtonElement>, order: OrderDrag) => {
    const offset =
      event.key === 'ArrowUp' || event.key === 'ArrowLeft'
        ? -1
        : event.key === 'ArrowDown' || event.key === 'ArrowRight'
          ? 1
          : 0;
    if (offset === 0) return;
    event.preventDefault();
    event.stopPropagation();
    const from = order.order.indexOf(order.active);
    const to = Math.max(0, Math.min(order.order.length - 1, from + offset));
    if (from === to) return;
    const target = order.order[to];
    animateReorder.current = true;
    persistOrder({ ...order, order: moveOrder(order.order, order.active, target) });
  };

  // 整卡拖拽 · 幽灵跟随 · 实时平滑重排。
  // - chains 从卡面任意处发起（触摸除外——触摸留给页面滚动，仅抓手可拖）；apps 仍从抓手发起。
  // - 越过 4px 阈值才真正开始，阈值之内的按下仍是一次点击（打开这条链）。
  // - app 起拖后切为紧凑行列表并卸载链卡；链拖拽保持卡片网格。两者均保留清晰的原位内容。
  // - 抬起一枚跟随光标的克隆体作幽灵；落点只按起拖时固定下来的槽位判定。
  // - 拖到视口上下缘自动滚动。
  const beginOrderDrag = (event: ReactPointerEvent<HTMLElement>, descriptor: OrderDrag) => {
    justDragged.current = false;
    if (!owner || selecting || event.button !== 0) return;
    const onGrip = !!(event.target as HTMLElement).closest('.order-grip');
    // 触摸时卡面留给页面滚动，只有抓手能发起拖动；鼠标与触控笔整卡可拖。
    if (event.pointerType === 'touch' && !onGrip) return;
    // 此处不 preventDefault：按下若未越过阈值仍是一次点击（打开这条链），而在部分实现里
    // 取消 pointerdown 会连带吞掉紧随的 click。文本选区改由越阈后 start() 清除、并靠
    // `.chain-order-dragging` 的 user-select:none 兜住。

    const listener = event.currentTarget;
    const pointerId = event.pointerId;
    const startX = event.clientX;
    const startY = event.clientY;
    // 幽灵克隆源：链取整张卡，线路取标题条（整段太大，取标题条作一枚轻量条）。
    const ghostSrc =
      descriptor.kind === 'chains'
        ? (listener.closest<HTMLElement>('[data-order-kind="chain"]') ?? listener)
        : (listener.closest<HTMLElement>('.chain-section-head') ??
          listener.closest<HTMLElement>('[data-order-kind="app"]') ??
          listener);
    const scroller = findScrollParent(ghostSrc);
    const dragRoot = sectionsRef.current;

    let started = false;
    let ended = false;
    let ghost: HTMLElement | null = null;
    let offX = 0;
    let offY = 0;
    let lastX = startX;
    let lastY = startY;
    let scrollV = 0;
    let raf = 0;
    let frameDirty = false;
    let scrollOrigin = 0;
    let slots: OrderSlot[] = [];

    const scrollPosition = () => (scroller === window ? window.scrollY : (scroller as HTMLElement).scrollTop);

    const placeGhost = () => {
      if (!ghost) return;
      ghost.style.transform = `translate(${lastX - offX}px, ${lastY - offY}px)`;
    };

    const evaluate = () => {
      const current = orderDragRef.current;
      if (!current) return;
      const order = orderForPointer(
        descriptor.order,
        descriptor.active,
        slots,
        lastX,
        lastY,
        scrollPosition() - scrollOrigin,
      );
      if (sameOrder(order, current.order)) return;
      animateReorder.current = true;
      setOrderDrag({ ...current, order });
    };

    const start = () => {
      started = true;
      justDragged.current = true;
      try {
        listener.setPointerCapture(pointerId);
      } catch {
        /* 捕获失败不致命，事件仍由绑定在 listener 上的监听驱动 */
      }
      document.body.classList.add('chain-order-dragging');
      // 越阈前若已起了一点文本选区，起拖时清掉；之后由 user-select:none 兜住。
      window.getSelection()?.removeAllRanges();
      // App 的卡片可能让一个分组高出数屏。直接拖整个分组不但昂贵，按“分组中心”判定时
      // 还必须把指针拖过半屏才会换位，看起来就像中途卡住。先切成仅标题的紧凑列表；
      // 记录起拖点在标题内的偏移，并平移列表，让切换形态时当前行仍留在指针下面。
      const sourceBeforeCompact = ghostSrc.getBoundingClientRect();
      offX = Math.max(0, Math.min(sourceBeforeCompact.width, lastX - sourceBeforeCompact.left));
      offY = Math.max(0, Math.min(sourceBeforeCompact.height, lastY - sourceBeforeCompact.top));
      if (descriptor.kind === 'apps' && dragRoot) {
        dragRoot.classList.add('app-order-dragging');
        const compactTop = ghostSrc.getBoundingClientRect().top;
        // 紧凑列表必须留在正常文档流里，面板背景与高度才会自然包住它。列表收缩后通过
        // 滚动补偿当前标题的视口位移，而不是 transform 整个列表（transform 不参与布局）。
        const alignScroll = compactTop - sourceBeforeCompact.top;
        if (scroller === window) window.scrollBy(0, alignScroll);
        else (scroller as HTMLElement).scrollTop += alignScroll;
      }
      // 以起拖瞬间的位置重新作为 FLIP 基准：上一次渲染后若滚动过页面，视口坐标已变，
      // 不重采会让第一次重排的补间带上滚动位移、卡片「飞入」。
      const root = sectionsRef.current;
      if (root) {
        flipRects.current.clear();
        slots = orderElements(root, descriptor).flatMap(el => {
          const id = el.dataset.orderId;
          if (!id) return [];
          const rect = el.getBoundingClientRect();
          const key = `${el.dataset.orderKind}:${el.dataset.orderApp ?? ''}:${el.dataset.orderId}`;
          flipRects.current.set(key, rect);
          return [{ id, left: rect.left, top: rect.top, width: rect.width, height: rect.height }];
        });
      }
      scrollOrigin = scrollPosition();
      setOrderDrag(descriptor);
      const rect = ghostSrc.getBoundingClientRect();
      const clone = ghostSrc.cloneNode(true) as HTMLElement;
      clone.classList.add('order-ghost');
      clone.classList.remove('order-drag-active');
      clone.removeAttribute('data-order-id');
      clone.style.width = `${rect.width}px`;
      clone.style.height = `${rect.height}px`;
      document.body.appendChild(clone);
      ghost = clone;
      placeGhost();
      frameDirty = true;
      const tick = () => {
        raf = requestAnimationFrame(tick);
        if (scrollV) {
          if (scroller === window) window.scrollBy(0, scrollV);
          else (scroller as HTMLElement).scrollTop += scrollV;
          frameDirty = true;
        }
        if (!frameDirty) return;
        frameDirty = false;
        placeGhost();
        evaluate();
      };
      raf = requestAnimationFrame(tick);
    };

    const move = (pointer: PointerEvent) => {
      if (pointer.pointerId !== pointerId) return;
      lastX = pointer.clientX;
      lastY = pointer.clientY;
      if (!started) {
        if (Math.hypot(lastX - startX, lastY - startY) < 4) return;
        start();
      }
      pointer.preventDefault();
      frameDirty = true;
      const edge = 76;
      const top = lastY;
      const bottom = window.innerHeight - lastY;
      scrollV = top < edge ? -Math.ceil((edge - top) / 5) : bottom < edge ? Math.ceil((edge - bottom) / 5) : 0;
    };

    const finish = (commit: boolean) => {
      if (ended) return;
      ended = true;
      cancelAnimationFrame(raf);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', cancel);
      document.body.classList.remove('chain-order-dragging');
      if (ghost) {
        ghost.remove();
        ghost = null;
      }
      // pointerup may arrive before the next animation frame. Resolve its final coordinates once so
      // a quick drag commits the slot visibly reached by the pointer rather than the preceding one.
      if (started && commit && frameDirty) evaluate();
      const current = orderDragRef.current;
      if (descriptor.kind === 'apps' && dragRoot) {
        dragRoot.classList.remove('app-order-dragging');
      }
      setOrderDrag(null);
      if (!started) return;
      if (commit && current && !sameOrder(current.order, current.original)) persistOrder(current);
    };
    const up = (pointer: PointerEvent) => {
      if (pointer.pointerId !== pointerId) return;
      lastX = pointer.clientX;
      lastY = pointer.clientY;
      frameDirty = true;
      finish(true);
    };
    const cancel = (pointer: PointerEvent) => {
      if (pointer.pointerId === pointerId) finish(false);
    };
    // 监听放在 window：App 换位会搬动包含抓手的 DOM 分组，部分浏览器会在此时中断
    // 元素级 pointer 事件；window 与 pointer capture 配合可让整个手势持续到抬手。
    window.addEventListener('pointermove', move, { passive: false });
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', cancel);
  };

  const create = useMutation({
    mutationFn: async () => {
      const id = (creating?.id ?? '').trim();
      if (!isValidSlug(id)) throw new Error(`线路 ID 只能用 a-z 0-9 . _ -，最长 ${SLUG_MAX}`);
      // id 冲突时不会被拒绝，而是会将该 id 对应项目的链连同主干一并迁移（论证见
      // ports.ts 的 freeId）——因此此处必须先行拦截。
      if (appsNow().some(a => a.id === id)) throw new Error(`线路 ${id} 已经有了`);
      await upsertApp({ id, label: (creating?.label ?? '').trim() || id });
    },
    onSuccess: () => {
      setCreating(null);
      invalidate();
    },
  });

  const rename = useMutation({
    mutationFn: async () => {
      const label = (renaming?.label ?? '').trim();
      if (!label) throw new Error('线路名不能为空');
      // 只传 id 和 label：`upsert_app_tx` 的 ON CONFLICT 只更新 label，
      // created_revision 通过 COALESCE 保留，链和接入面关联在 id 上，不受影响。
      await upsertApp({ id: renaming!.id, label });
    },
    onSuccess: () => {
      setRenaming(null);
      invalidate();
    },
  });

  /* 默认 id 与拓扑图处使用同一规则（app-1、app-2…），使两个入口生成的名称一致。 */
  const beginCreate = () => {
    const used = new Set(appsNow().map(a => a.id));
    let id = '';
    for (let i = appsNow().length + 1; !id; i += 1) if (!used.has(`app-${i}`)) id = `app-${i}`;
    create.reset();
    setCreating({ id, label: '' });
  };

  if (snapshot.isPending) return <Loading />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;

  // 含退役节点即视为停用（与编译器判定一致）。列表不依赖编译结果——草稿或未发布时
  // 编译视图中还没有该链，而停用状态是模型事实，从节点侧计算。判定依据是成员而非主干：
  // 此前使用 `r.spine.some(...)`，导致分叉上有机器退役时界面仍显示为正常，而编译器
  // （`disabled_chains` 使用 `chain_members`）已将整条链判定为停用。
  const retired = new Set((nodeList.data?.nodes ?? []).filter(n => n.retired_at).map(n => n.node_id));
  const rows = chainRows(snapshot.data.snapshot.apps ?? []).map(r => ({
    ...r,
    disabled: r.members.some(n => retired.has(n)),
  }));
  // 两层都直接保持 snapshot 的 position 顺序。停用状态只改变外观，不再把链挪到末尾：
  // 否则操作者刚排好的 chain 顺序会在这张主列表上失效，而其他页面又是另一种顺序。
  // 没有链的项目同样列出：新建的项目没有链，过滤掉会使「新建项目」看起来没有生效。
  const snapshotApps = snapshot.data.snapshot.apps ?? [];
  // Draft writes synchronously before its preview request begins. Reading the final order from the
  // draft itself keeps the dropped shape on screen during that round-trip and also makes “discard
  // this draft item” immediately remove the optimistic order without a second local cache.
  const pendingOps = draft.ops();
  const pendingAppOrder = pendingOps.find(op => op.op === 'reorder_apps');
  const pendingChainOrders = new Map(
    pendingOps.filter(op => op.op === 'reorder_chains').map(op => [op.app_id, op.ids] as const),
  );
  const appOrder = orderDrag?.kind === 'apps' ? orderDrag.order : pendingAppOrder?.ids;
  const orderedApps = orderByIds(snapshotApps, appOrder, app => app.id);
  const groups = orderedApps.map(a => ({
    app: a,
    chains: orderByIds(
      rows.filter(r => r.app.id === a.id),
      orderDrag?.kind === 'chains' && orderDrag.appId === a.id ? orderDrag.order : pendingChainOrders.get(a.id),
      row => row.chain.id,
    ),
  }));

  // 卡片颜色表示是否需要处理，而不是“有没有流量”。模型事实优先于探测结果：含退役成员或
  // 缺接入面时即使上一轮探测仍为成功，当前草稿中的链也已经不可用。模型正常后再看端到端
  // 探测；没有用户或尚未探测保持中性，它们都不等同于故障。
  const chainTone = (r: (typeof rows)[number]): 'ok' | 'warn' | 'bad' | 'idle' => {
    if (r.disabled || !r.ingress) return 'bad';
    const probeTone = toneOf(probeOf.get(r.chain.id));
    if (probeTone === 'down') return 'bad';
    if (probeTone === 'odd' || probeTone === 'slow') return 'warn';
    if (probeTone === 'none' || r.users === 0) return 'idle';
    return 'ok';
  };
  const chainToneTitle = (r: (typeof rows)[number]) => {
    if (r.disabled) return '含退役节点，整链停用';
    if (!r.ingress) return '缺接入面：没有入口，这条链无法接入';
    const probe = probeOf.get(r.chain.id);
    const probeTone = toneOf(probe);
    if (probeTone !== 'ok' && probeTone !== 'none') return toneTitle(probe);
    if (!probe) return '还没探过这条链';
    if (r.users === 0) return '通，但还没有人被授权';
    return `${r.users} 人在用`;
  };

  const brokenCount = rows.filter(r => chainTone(r) === 'bad').length;

  return (
    <div className="cardpage chain-cardpage">
      <section className="panel titled chain-list-panel">
        {/* 标题栏结构与机器页一致：标题 + 计数 + 右端一组读数 + 一个主按钮。
          机器页右端是「＋ 纳管节点」，此处是「＋ 新建线路」——两页标题结构相同，
          缺少按钮的一页会被理解为不支持新建。

          因此按钮不按角色隐藏，只按角色禁用：此前整组包在 `owner &&` 里，readonly 和
          public 看到的线路页标题栏右端是空的，而机器页和用户页是灰按钮，同一个台面上
          三页的标题栏各一种形态。 */}
        <header>
          <ListIcon of="chains" />
          <h4>线路</h4>
          <span className="hint">{groups.length} 个</span>
          <span className="rd">
            <b>{rows.length}</b> 条链
            {brokenCount > 0 && (
              <>
                {' '}
                · <i>{brokenCount}</i> 不通
              </>
            )}
          </span>
          {selecting ? (
            <>
              <button className="btn" onClick={exitSelect}>
                取消
              </button>
              {/* 选中数量显示在按钮上：它是此时唯一需要确认的信息。删除只写入草稿，
                  顶栏草稿条会逐条列出且提交前可预览，因此此处不再增加确认。 */}
              <button
                className="btn danger"
                disabled={picked.size === 0}
                title={`删除选中的 ${picked.size} 条链：接入面和挂在下面的授权关系一起删。进草稿，提交才生效`}
                onClick={() => {
                  for (const key of picked) {
                    const slash = key.indexOf('/');
                    deleteChain(key.slice(0, slash), key.slice(slash + 1));
                  }
                  exitSelect();
                }}
              >
                删除链{picked.size > 0 && ` ${picked.size}`}
              </button>
            </>
          ) : (
            <>
              <button className="btn" disabled={!!creating || !owner} onClick={() => setSelecting(true)}>
                多选
              </button>
              <button className="btn primary" disabled={!!creating || !owner} onClick={beginCreate}>
                ＋ 新建线路
              </button>
            </>
          )}
        </header>

        {/* 新建项目表现为列表最前面增加一条待创建的行，而不是插入一块面板。
          两个输入框不需要独立的下沉底色区域——此前的版本高 130px（ID 的说明折为两行，
          将按钮推到第三行），会把第一个项目挤出视野，而其内容只有 ID 和名称两项。

          说明收入 placeholder 和下方的一行注脚：「创建后不可修改」需要提示的时机是
          提交前（草稿条会列出「项目 app-4」），而非填写时。 */}
        {creating && (
          <form
            className="prj-newbar"
            onSubmit={e => {
              e.preventDefault();
              create.mutate();
            }}
          >
            <div className="row">
              <span className="k">新线路</span>
              <input
                className="f mono id"
                autoFocus
                value={creating.id}
                placeholder="ID"
                onChange={e => setCreating({ ...creating, id: e.target.value })}
              />
              <input
                className="f nm"
                value={creating.label}
                // 名称默认留空：名称由用户指定，复制 ID 作为默认值不提供任何信息，
                // 而留空可直接表明该字段需要填写。提交时留空则回退为 ID（见 create）。
                placeholder="名称，比如：「三网优化线路接入」"
                onChange={e => setCreating({ ...creating, label: e.target.value })}
                onKeyDown={e => e.key === 'Escape' && setCreating(null)}
              />
              <button className="btn primary" disabled={create.isPending}>
                {create.isPending ? '创建中…' : '创建'}
              </button>
              <button type="button" className="btn ghost" onClick={() => setCreating(null)}>
                取消
              </button>
            </div>
            {/* 只说明该字段的含义。字符集由 ID 输入框的校验负责（输入错误时即时提示），
              「改动写入草稿」由顶栏草稿条表示，此处无需重复。 */}
            <p className="note">
              线路是<b>计费单元</b>：填写对外提供的服务。
            </p>
            {create.error && <ErrorBox error={create.error} />}
          </form>
        )}

        {groups.length === 0 ? (
          <Empty>还没有线路。用右上角「＋ 新建线路」建一个。</Empty>
        ) : (
          <div className={`chain-sections${orderDrag?.kind === 'apps' ? ' app-order-dragging' : ''}`} ref={sectionsRef}>
            {groups.map((g, gi) => (
              <section
                className={`chain-section${orderDrag?.kind === 'apps' && orderDrag.active === g.app.id ? ' order-drag-active' : ''}`}
                key={g.app.id}
                data-order-kind="app"
                data-order-id={g.app.id}
              >
                <header className="chain-section-head">
                  <span className="no">{String(gi + 1).padStart(2, '0')}</span>
                  <button
                    type="button"
                    className="order-grip app-order-grip"
                    disabled={!owner || selecting}
                    aria-label={`拖动调整线路 ${g.app.label} 的顺序`}
                    title={owner ? '按住拖动排序；聚焦后也可使用方向键' : '只有系统管理员可以调整顺序'}
                    onPointerDown={event =>
                      beginOrderDrag(event, {
                        kind: 'apps',
                        active: g.app.id,
                        original: groups.map(group => group.app.id),
                        order: groups.map(group => group.app.id),
                      })
                    }
                    onKeyDown={event =>
                      nudgeOrder(event, {
                        kind: 'apps',
                        active: g.app.id,
                        original: groups.map(group => group.app.id),
                        order: groups.map(group => group.app.id),
                      })
                    }
                  />
                  {renaming?.id === g.app.id ? (
                    <form
                      className="prj-ren"
                      onSubmit={e => {
                        e.preventDefault();
                        rename.mutate();
                      }}
                    >
                      <input
                        className="f"
                        autoFocus
                        value={renaming.label}
                        onChange={e => setRenaming({ ...renaming, label: e.target.value })}
                        onKeyDown={e => e.key === 'Escape' && setRenaming(null)}
                      />
                      <button className="btn primary" disabled={rename.isPending}>
                        {rename.isPending ? '保存中…' : '保存'}
                      </button>
                      <button type="button" className="btn" onClick={() => setRenaming(null)}>
                        取消
                      </button>
                      {rename.error && <ErrorBox error={rename.error} />}
                    </form>
                  ) : (
                    <>
                      {/* 线路名本身就是改名的入口，不再走「⋯ → 改线路名」两步。那个菜单里只有
                          这一项，等于给一个动作套了一层抽屉。没有改名权限的人看到的仍是纯文本。 */}
                      {owner ? (
                        <button
                          type="button"
                          className="chain-section-name"
                          title="点一下改线路名"
                          onClick={() => {
                            rename.reset();
                            setRenaming({ id: g.app.id, label: g.app.label || g.app.id });
                          }}
                        >
                          {g.app.label || g.app.id}
                        </button>
                      ) : (
                        <b>{g.app.label || g.app.id}</b>
                      )}
                      {g.app.label && g.app.label !== g.app.id && <span className="id">{g.app.id}</span>}
                      <span className="count">{g.chains.length} 条链</span>
                      <div className="chain-section-actions">
                        <button
                          className="new-chain"
                          disabled={!editable}
                          title="在这条线路下建一条链"
                          onClick={() => go({ p: 'new', app: g.app.id })}
                        >
                          ＋ 新建链
                        </button>
                      </div>
                    </>
                  )}
                </header>
                {orderDrag?.kind === 'apps' ? null : g.chains.length === 0 ? (
                  <p className="chain-section-empty">还没有链。用上面的「＋ 新建链」建一条。</p>
                ) : (
                  <div className="chain-card-grid">
                    {g.chains.map(r => {
                      const tone = chainTone(r);
                      const probe = probeOf.get(r.chain.id);
                      const key = rowKey(r.app.id, r.chain.id);
                      const checked = picked.has(key);
                      const open = () => {
                        // 刚结束一次拖动排序：抬手后紧跟的 click 不应再打开这条链。
                        if (justDragged.current) {
                          justDragged.current = false;
                          return;
                        }
                        if (selecting) togglePick(key);
                        else go({ p: 'chain', app: r.app.id, chain: r.chain.id });
                      };
                      return (
                        <div
                          role="button"
                          tabIndex={0}
                          key={key}
                          className={`chain-card tone-${tone}${r.disabled ? ' off' : ''}${checked ? ' picked' : ''}${orderDrag?.kind === 'chains' && orderDrag.active === r.chain.id ? ' order-drag-active' : ''}`}
                          data-order-kind="chain"
                          data-order-app={g.app.id}
                          data-order-id={r.chain.id}
                          // 整卡即拖动手柄（方案 A）。越过 4px 阈值才起拖，之内仍是一次点击；
                          // 触摸时卡面留给滚动、仅抓手可拖（判定在 beginOrderDrag 内）。
                          onPointerDown={event =>
                            beginOrderDrag(event, {
                              kind: 'chains',
                              appId: g.app.id,
                              active: r.chain.id,
                              original: g.chains.map(row => row.chain.id),
                              order: g.chains.map(row => row.chain.id),
                            })
                          }
                          onClick={open}
                          onKeyDown={e => {
                            if (e.target !== e.currentTarget) return;
                            if (e.key === 'Enter' || e.key === ' ') {
                              e.preventDefault();
                              open();
                            }
                          }}
                        >
                          <header className="chain-card-head">
                            <span className="chain-status-slot">
                              {selecting ? (
                                <input
                                  type="checkbox"
                                  className="chain-card-pick"
                                  checked={checked}
                                  aria-label={`选中 ${r.chain.name || r.chain.id}`}
                                  onChange={() => togglePick(key)}
                                  onClick={e => e.stopPropagation()}
                                />
                              ) : (
                                <i
                                  className={`chain-card-status ${tone}`}
                                  title={chainToneTitle(r)}
                                  aria-label={chainToneTitle(r)}
                                />
                              )}
                            </span>
                            <button
                              type="button"
                              className="order-grip chain-order-grip"
                              disabled={!owner || selecting}
                              aria-label={`拖动调整链 ${r.chain.name || r.chain.id} 的顺序`}
                              title={owner ? '按住拖动排序；聚焦后也可使用方向键' : '只有系统管理员可以调整顺序'}
                              // 抓手仍是排序的可见提示与键盘入口，但拖动由整张卡统一发起
                              // （卡片的 onPointerDown 冒泡即含抓手），这里不再单独起拖，
                              // 否则按住抓手会同时触发两个拖动会话。
                              onClick={event => event.stopPropagation()}
                              onKeyDown={event =>
                                nudgeOrder(event, {
                                  kind: 'chains',
                                  appId: g.app.id,
                                  active: r.chain.id,
                                  original: g.chains.map(row => row.chain.id),
                                  order: g.chains.map(row => row.chain.id),
                                })
                              }
                            />
                            <span className="chain-card-title">
                              <b>{r.chain.name || r.chain.id}</b>
                              {r.chain.name && r.chain.name !== r.chain.id && <span>{r.chain.id}</span>}
                            </span>
                            <ChainLatency probe={probe} />
                          </header>
                          <div className="chain-card-path">
                            <ChainPath
                              spine={r.spine}
                              retired={retired}
                              ingress={r.ingress}
                              disabled={r.disabled}
                              showTail={false}
                            />
                          </div>
                          <ProbeLatencyPlot probe={probe} />
                          <footer className="chain-card-foot">
                            <span className="stat">
                              <small>用户</small>
                              <b>{r.users}</b>
                            </span>
                            <span className="stat access">
                              <small>接入</small>
                              <b title={chainAccessLabel(r.ingress)}>{chainAccessLabel(r.ingress)}</b>
                            </span>
                          </footer>
                        </div>
                      );
                    })}
                  </div>
                )}
              </section>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

// 一条链的路径图：入口在左，一跳一格，末尾是出网。
// 与原先的标记序列相比，差异是跳与跳之间用连线相接——中间的实线加箭头表示流量方向，
// 而间隙中放一个 → 会被读作两个并列标签之间的分隔符。
// 入口一格带端口（客户端连接的位置）。
function ChainPath({
  spine,
  retired,
  ingress,
  disabled,
  showTail = true,
}: {
  spine: string[];
  retired: Set<string>;
  ingress: SnapshotIngress | null;
  disabled: boolean;
  showTail?: boolean;
}) {
  const nameOf = useNodeNames();
  if (spine.length === 0) {
    return (
      <span className="chain-path">
        <span className="cp-none">没有主干</span>
      </span>
    );
  }
  return (
    <span className="chain-path">
      {spine.map((n, i) => (
        <span key={n} className="cp-seg">
          {i > 0 && <i className="cp-wire" aria-hidden="true" />}
          <span
            className="cp-hop"
            data-role={i === 0 ? 'entry' : i === spine.length - 1 ? 'exit' : 'relay'}
            data-retired={retired.has(n) ? '' : undefined}
            title={n}
          >
            {nameOf(n)}
            {i === 0 && ingress && <em>:{ingress.port}</em>}
          </span>
        </span>
      ))}
      {showTail && (
        <span className="cp-tail">
          {disabled
            ? '含退役节点，整链停用'
            : !ingress
              ? '缺接入面'
              : spine.length === 1
                ? '直出'
                : `${spine.length - 1} 跳中继`}
        </span>
      )}
    </span>
  );
}

// 链的标识信息：名称、id、租户。它们位于标题中，样式与其他位置的标题一致
//（`.chain-hd`，机器详情和建链向导使用同一样式）：名称使用大号字，其后是灰色的 id。
//
// 此前左栏有一块「身份」。而标题中已显示名称和 id，重复显示会削弱标题的作用；
// 更主要的问题是左栏因此只剩三行只读文本，右栏有九个字段，
// 而「两列底部对齐」的规则会把该差额转化为「身份」块中间的大片空白。
//
// 重命名不设按钮：点击名称本身即变为输入框，点击其他位置或回车保存，Esc 取消。
// 在标题旁常驻一个「保存」按钮的视觉权重高于其功能——本页多数时间不涉及重命名，
// 该按钮多数时间处于禁用状态。
//
// 只有名称可修改。id 只显示：`chains.id` 是全局主键，修改 id 不是重命名而是将该链连同
// 规则表迁移到其他项目下（论证见 ports.ts），提供输入框会使该迁移易于误触发。
// 租户同样只显示，但必须显示——见下面保存部分的说明：它是该操作中最易写错的字段。
export function ChainTitle({ appId, chain, editable }: { appId: string; chain: SnapshotChain; editable: boolean }) {
  const qc = useQueryClient();
  /* null 表示未处于编辑状态。进入编辑时以当前名称为初值，显示值均实时计算。 */
  const [draft, setDraft] = useState<string | null>(null);
  /* Esc 取消需要触发 blur（否则输入框仍保持焦点），而 blur 本身对应保存路径。
     用一个 ref 将本次 blur 标记为取消——使用 state 时该帧的 blur 读取到的仍是旧值。 */
  const escaped = useRef(false);

  const save = useMutation({
    mutationFn: async (name: string) => {
      // 项目、租户和出口地区标识原样回传。`upsert_chain` 会把这条链作为完整值更新，
      // 所以改名时漏掉任何一个字段都会意外修改这条链的其他设置。
      await upsertChain(appId, {
        id: chain.id,
        tenant_id: chain.tenant,
        name,
        subscription_country: chain.subscription_country ?? null,
      });
    },
    onSuccess: () => {
      setDraft(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
    /* 保存失败时保留输入框和已输入的内容，并在旁边显示错误信息 */
  });

  // 点击其他位置（blur）时保存。名称为空或未修改时按未编辑处理并退出——
  // 点击空白处自动保存不应将链的名称清空。
  const commit = () => {
    if (escaped.current) {
      escaped.current = false;
      setDraft(null);
      return;
    }
    const next = (draft ?? '').trim();
    if (!next || next === chain.name) {
      setDraft(null);
      return;
    }
    save.mutate(next);
  };

  return (
    <>
      <div className="chain-hd">
        {editable && draft !== null ? (
          <input
            className="f chain-rename"
            autoFocus
            value={draft}
            aria-label="链名"
            disabled={save.isPending}
            onChange={e => setDraft(e.target.value)}
            onBlur={commit}
            /* 回车不直接保存：交由 blur 走同一条路径，避免两条路径各自实现收尾逻辑 */
            onKeyDown={e => {
              if (e.key === 'Enter') e.currentTarget.blur();
              if (e.key === 'Escape') {
                escaped.current = true;
                e.currentTarget.blur();
              }
            }}
          />
        ) : (
          /* 未设置名称的链回退到 id，与列表和其他位置的标题行规则一致 */
          <b
            role={editable ? 'button' : undefined}
            tabIndex={editable ? 0 : undefined}
            title={editable ? '点一下改名。只改显示的名字，订阅链接、UUID、授权、编译产物一个字节都不动' : undefined}
            onClick={editable ? () => setDraft(chain.name) : undefined}
            onKeyDown={
              editable
                ? e => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      setDraft(chain.name);
                    }
                  }
                : undefined
            }
          >
            {chain.name || chain.id}
          </b>
        )}
        {/* 不可修改的原因写入 title——只读角色本身无法修改任何内容，单独为该项添加说明
            会使其看起来比其他项限制更严。 */}
        <span className="subid mono" title={editable ? '链 ID 改不了：它是全局主键，换 id 是搬家不是改名' : undefined}>
          {appId} / {chain.id}
        </span>
        {save.isPending && <span className="subid">保存中…</span>}
      </div>
      {/* 只在编辑状态下显示退出方式。非编辑状态下该说明不提供信息，且占用首屏空间。 */}
      {draft !== null && <div className="note chain-rename-tip">回车或点击别处保存，Esc 撤销</div>}
      {save.error && <ErrorBox error={save.error} />}
    </>
  );
}

const SUBSCRIPTION_COUNTRY_CODES = FLAG_SHEET.flatMap(line =>
  Array.from({ length: line.length / 2 }, (_, index) => line.slice(index * 2, index * 2 + 2).toUpperCase()),
);
const SUBSCRIPTION_COUNTRY_CODE_SET = new Set(SUBSCRIPTION_COUNTRY_CODES);
const SUBSCRIPTION_COUNTRY_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region' });

/** The compact regional-indicator prefix used in generated subscription node names. */
export function subscriptionFlag(code: string): string {
  return SUBSCRIPTION_COUNTRY_CODE_SET.has(code)
    ? Array.from(code, letter => String.fromCodePoint(127462 + letter.charCodeAt(0) - 65)).join('')
    : '';
}

function subscriptionCountryLabel(code: string): string {
  const name = SUBSCRIPTION_COUNTRY_NAMES.of(code);
  const flag = subscriptionFlag(code);
  return `${flag ? `${flag} ` : ''}${name && name !== code ? `${code} · ${name}` : code}`;
}

export function ChainSubscriptionCountryRow({
  appId,
  chain,
  probe,
  editable,
}: {
  appId: string;
  chain: SnapshotChain;
  probe?: E2eProbeItem;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const configured = chain.subscription_country?.trim().toUpperCase() ?? '';
  const observed = probe?.status === 'ok' ? (probe.exit_loc?.trim().toUpperCase() ?? '') : '';
  const suggested = SUBSCRIPTION_COUNTRY_CODE_SET.has(observed) && observed !== configured ? observed : '';
  const save = useMutation({
    mutationFn: (country: string | null) =>
      upsertChain(appId, {
        id: chain.id,
        tenant_id: chain.tenant,
        name: chain.name,
        subscription_country: country,
      }),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });
  const update = (country: string) => {
    if (country !== configured) save.mutate(country || null);
  };
  const preview = `${configured ? subscriptionFlag(configured) : ''}${chain.name}`;

  return (
    <>
      <dt>出口地区标识</dt>
      <dd>
        <div className="toolbar" style={{ gap: 7 }}>
          {editable ? (
            <select
              className="f"
              aria-label="出口地区标识"
              value={configured}
              disabled={save.isPending}
              onChange={event => update(event.target.value)}
            >
              <option value="">不显示国旗</option>
              {SUBSCRIPTION_COUNTRY_CODES.map(code => (
                <option key={code} value={code}>
                  {subscriptionCountryLabel(code)}
                </option>
              ))}
            </select>
          ) : (
            <span>{configured ? subscriptionCountryLabel(configured) : '不显示国旗'}</span>
          )}
          {editable && suggested && (
            <button className="btn" type="button" disabled={save.isPending} onClick={() => update(suggested)}>
              采用当前出口 {suggested}
            </button>
          )}
        </div>
        <span className="note">
          订阅显示：<span className="mono">{preview}</span>
        </span>
        {save.error && <ErrorBox error={save.error} />}
      </dd>
    </>
  );
}

function ChainDetail({ app, chain }: { app: string; chain: string }) {
  const { who } = useSession();
  const qc = useQueryClient();
  const editable = can(who.role, 'edit');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const probes = useQuery({
    queryKey: ['e2e-probes'],
    queryFn: () => fetchE2eProbes(),
    refetchInterval: 30_000,
  });
  const nameOf = useNodeNames();

  const a = snapshot.data?.snapshot.apps.find(x => x.id === app);
  const c = a?.chains?.find(x => x.id === chain);
  const ingress = (a?.ingresses ?? []).find(x => x.chain === chain) ?? null;
  const [pendingIngressNode, setPendingIngressNode] = useState<string | null>(null);
  const [pendingBind, setPendingBind] = useState<string | null>(null);
  const { projHandles, projDirty, projBlocked, onV4Handle, onV6Handle } = useProjectionHandles();

  // 更换入口即将接入面迁移到另一台机器（链头随之改变，订阅链接也会变化）。
  // 编辑器第 0 位固定为入口，因此该操作只能在接入面区块执行。
  const moveIngress = useMutation({
    mutationFn: async (toNode: string) => {
      const g = (a?.ingresses ?? []).find(x => x.chain === chain);
      if (!g) throw new Error('这条链没有接入面');
      const base = ingressUpsertBody(g);
      await upsertIngress(app, { ...base, node_id: toNode }, base);
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setPendingIngressNode(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
    onError: () => setPendingIngressNode(null),
  });

  const saveBind = useMutation({
    mutationFn: async (bind: string) => {
      const g = (a?.ingresses ?? []).find(x => x.chain === chain);
      if (!g) throw new Error('这条链没有接入面');
      const base = ingressUpsertBody(g);
      await upsertIngress(app, { ...base, bind }, base);
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      setPendingBind(null);
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
    onError: () => setPendingBind(null),
  });

  const saveProjections = useMutation({
    mutationFn: () => {
      if (!ingress) throw new Error('这条链没有接入面');
      const base = ingressUpsertBody(ingress);
      const projection = Object.values(projHandles)
        .filter(handle => handle.dirty)
        .reduce((next, handle) => handle.apply(next), base.projection ?? {});
      return upsertIngress(app, { ...base, projection }, base);
    },
    onSuccess: async () => {
      Object.values(projHandles).forEach(handle => handle.reset());
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });

  if (snapshot.isPending) return <Loading />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (!a || !c) return <ErrorBox error={new Error(`没有这条链：${app}/${chain}`)} />;

  const spine = chainSpine(a, c.id);
  // 停用表示链上含退役节点（与编译器判定一致，即从入口 BFS 的可达范围）。此处列出
  // 具体是哪几台退役，在横幅中标明，无需返回列表核对。
  const retiredSet = new Set((nodes.data?.nodes ?? []).filter(n => n.retired_at).map(n => n.node_id));
  const retiredHops = spine.filter(n => retiredSet.has(n));
  const disabled = retiredHops.length > 0;
  // 该链需要列出的每一跳。主干之外还有分叉：规则中 Forward 到主干外的机器同样是该链的
  // 成员，有各自的规则表和中转端口。只列出主干时，配置分流后该机器会从界面上消失，
  // 其规则无法再访问。
  const chainSteps = (a.steps ?? []).filter(s => s.chain === chain);
  const probe = byChain(probes.data?.chains).get(chain);

  return (
    // 外层不是空片段而是一个具名容器：这一页的内容是一列配置卡，纸只承担页面框，不再
    // 兼任卡片的底。外壳按 `.fg-sheet:has(> .chain-detailpage)` 把亮色的纸压到台面档，
    // 卡片才比它所落的面亮一档（论证见 styles.css 末尾「亮色详情页」一节）。
    <div className="chain-detailpage">
      {/* 「主干」一节已移除：主干由规则表派生（chainSpine 沿 any→Forward 遍历得出），
          而规则树本身按跳排列，两者表达同一内容。添加一跳和重排都在规则表中完成
          （修改转发目标即为重排），删除收入规则树每行的 hover 状态。 */}
      <ChainTitle appId={app} chain={c} editable={editable} />

      {/* 两列，按「用户连接到何处」和「隧道的协议配置」划分。
          左栏：结论 + 落点（由哪台机器接收）+ 订阅投影（客户端连接的地址）。
          右栏：协议栈（协议 / 安全层 / 传输层，以及其下的相关参数）。

          此前的划分是「它是什么」和「它的配置」：左栏是状态和标识，右栏是整个接入面。
          问题不在划分方式，而在两侧的内容量——接入面的字段从两个增加到九个，
          而左栏始终只有三行只读文本，四倍的差额在「两列底部对齐」规则下形成左栏中间的
          大片空白。按新的划分，两侧各自表达一项完整内容，内容量也相当。

          异常状态的两块与结论块结构相同（`.blk`），只是固定为红色档：
          链的当前状态是同一个问题，使用三种形状会增加识别成本。 */}
      <div className="duo chain-config-grid">
        <div className="col">
          {/* 已停用的链不显示探测结论：它未在运行，显示「不通」的结论会造成误解。 */}
          {disabled ? (
            <div className="blk tone-err">
              <div className="blk-hd">这条链已停用</div>
              <div className="blk-bd">
                <div className="probe-face">
                  <span className="probe-lat">
                    <span className="n">停用</span>
                  </span>
                  <span className="probe-say">
                    <span className="l1">链上有退役节点：{retiredHops.map(nameOf).join('、')}</span>
                    <span className="l2">
                      入口不渲染、整链不编译，发布不受影响。把退役节点移出链（或让它复出）后自动恢复。
                    </span>
                  </span>
                </div>
              </div>
            </div>
          ) : (
            <ProbeBanner item={probe} />
          )}

          {/* ── 落点 ──
            机器和端口是同一项内容的两部分（客户端连接的位置在哪台机器、哪个端口），
            此前它们使用两种形态：端口是标题行右端的输入框，机器是下方单独一行。
            同类内容使用不同形态会增加识别成本。

            收入 `.kv` 并采用与设置页相同的布局：左列是项名、右列是控件、说明在控件下方。 */}
          {ingress && (
            <ConfigPanel title="入站">
              <dl className="kv form2 chain-face">
                <dt>入站机器</dt>
                <dd>
                  {editable ? (
                    <select
                      className="f"
                      value={pendingIngressNode ?? ingress.node}
                      onChange={e => {
                        if (!e.target.value) return;
                        setPendingIngressNode(e.target.value);
                        moveIngress.mutate(e.target.value);
                      }}
                    >
                      {(nodes.data?.nodes ?? [])
                        .filter(n => !n.retired_at)
                        .map(n => (
                          <option key={n.node_id} value={n.node_id}>
                            {nameOf(n.node_id)}
                          </option>
                        ))}
                    </select>
                  ) : (
                    <span>{nameOf(ingress.node)}</span>
                  )}
                  {moveIngress.isPending && <span className="note">搬迁中…</span>}
                </dd>
                <dt>绑定地址</dt>
                <dd>
                  {editable ? (
                    <div className="toolbar" style={{ margin: 0, gap: 6 }}>
                      <input
                        className="f mono"
                        value={pendingBind ?? ingress.bind}
                        onChange={e => setPendingBind(e.target.value)}
                      />
                      {pendingBind !== null && pendingBind !== ingress.bind && (
                        <>
                          <button
                            className="btn primary"
                            disabled={saveBind.isPending || !/^\d+\.\d+\.\d+\.\d+$/.test(pendingBind.trim())}
                            onClick={() => saveBind.mutate(pendingBind.trim())}
                          >
                            保存
                          </button>
                          <button className="btn" disabled={saveBind.isPending} onClick={() => setPendingBind(null)}>
                            还原
                          </button>
                        </>
                      )}
                    </div>
                  ) : (
                    <span className="mono">{ingress.bind}</span>
                  )}
                </dd>
              </dl>
            </ConfigPanel>
          )}

          {/* ── 订阅投影 ──
            与落点配套：上一块表示由哪台机器接收，本块表示告知客户端连接的地址。
            相邻放置才能体现两者的区别——这也是它们同在左栏的原因。 */}
          {ingress && (
            <ConfigPanel title="客户端配置">
              <dl className="kv form2 chain-face">
                <ChainSubscriptionCountryRow appId={app} chain={c} probe={probe} editable={editable} />
                <IngressProjectionRow
                  appId={app}
                  ingress={ingress}
                  family="v4"
                  node={nodes.data?.nodes.find(n => n.node_id === ingress.node)}
                  editable={editable}
                  onHandle={onV4Handle}
                />
                <IngressProjectionRow
                  appId={app}
                  ingress={ingress}
                  family="v6"
                  node={nodes.data?.nodes.find(n => n.node_id === ingress.node)}
                  editable={editable}
                  onHandle={onV6Handle}
                />
              </dl>
              {/* 投影的说明只需一次，因此写在面板级而非每行。
                  此前该说明位于每个地址族的开关下方，只在启用时显示——而最需要该说明的
                  情况正是两个族都关闭、界面上只剩两个「不投影」时。 */}
              <div className="note" style={{ margin: 0 }}>
                用于修改客户端入口地址配置。不影响服务端监听。
              </div>
              {saveProjections.error && <ErrorBox error={saveProjections.error} />}
              {projDirty && (
                <div className="toolbar">
                  <button
                    className="btn"
                    disabled={saveProjections.isPending}
                    onClick={() => Object.values(projHandles).forEach(h => h.reset())}
                  >
                    还原
                  </button>
                  <button
                    className={!projBlocked ? 'btn primary' : 'btn'}
                    disabled={!editable || projBlocked || saveProjections.isPending}
                    onClick={() => saveProjections.mutate()}
                  >
                    {saveProjections.isPending ? '保存中…' : '保存'}
                  </button>
                </div>
              )}
            </ConfigPanel>
          )}

          {/* 安全策略排在左栏末尾：它作用于整个入口（谁能连、连上之后能到哪儿），
              与本栏的落点、订阅投影同属「这个入口对外是什么样」；右栏是协议栈的参数。
              放右栏时它还得排在 VLESS 和 HY2 两块之后，与那两块的层级关系并不成立。 */}
          {ingress && <IngressGuardBlock appId={app} ingress={ingress} editable={editable} />}
        </div>

        <div className="col">
          {/* ── 协议栈 ──
            三层（协议 / 安全层 / 传输层）排在最前，三行相同形态的下拉框构成一组；
            单独命名的参数（目标站点 / 流控 / Fallback 限速）排在该组之后。
            不缩进、不画线、不加层级编号：本栏本身是一张表，增加形态会增加识别成本。

            顺序有明确依据——目标站点和流控都属于安全层，Fallback 限速作用于未通过
            REALITY 校验、进入 fallback 的连接，同样属于安全层。传输层的参数
            （路径、并发、Host、上行）在 IngressStreamRow 中构成独立的「XHTTP」一行。 */}
          {ingress ? (
            <>
              <ConfigPanel title="协议">
                {/* 标题右端此前有一枚摘要角标（REALITY · TCP · VISION）。它不含新信息——
                    三层各取一个词，与下方三行下拉框一一对应，而那三行就在同一屏内、
                    永远展开。同一件事说两遍，去掉角标留下拉框。 */}
                <dl className="kv form2 chain-face fill">
                  <IngressStreamRow
                    appId={app}
                    ingress={ingress}
                    certificateName={
                      snapshot.data?.snapshot.nodes?.find(node => node.id === ingress.node)?.certificate_name
                    }
                    editable={editable}
                    section="protocols"
                  />
                </dl>
              </ConfigPanel>
              {!!ingress.wires.vless && (
                <IngressPanel appId={app} ingress={ingress} title="VLESS" editable={editable}>
                  <IngressStreamRow
                    appId={app}
                    ingress={ingress}
                    certificateName={
                      snapshot.data?.snapshot.nodes?.find(node => node.id === ingress.node)?.certificate_name
                    }
                    editable={editable}
                    section="vless"
                  />
                </IngressPanel>
              )}
              {!!ingress.wires.anytls && (
                <IngressPanel appId={app} ingress={ingress} title="AnyTLS" editable={editable}>
                  <IngressStreamRow
                    appId={app}
                    ingress={ingress}
                    certificateName={
                      snapshot.data?.snapshot.nodes?.find(node => node.id === ingress.node)?.certificate_name
                    }
                    editable={editable}
                    section="anytls"
                  />
                </IngressPanel>
              )}
              {!!ingress.wires.hysteria2 && (
                <IngressPanel appId={app} ingress={ingress} title="Hysteria 2" editable={editable}>
                  <IngressStreamRow
                    appId={app}
                    ingress={ingress}
                    certificateName={
                      snapshot.data?.snapshot.nodes?.find(node => node.id === ingress.node)?.certificate_name
                    }
                    editable={editable}
                    section="hy2"
                  />
                </IngressPanel>
              )}
            </>
          ) : (
            <ConfigPanel title="没有接入面">
              <div className="callout warn" style={{ margin: 0 }}>
                编译会报 <span className="mono">chain.no-ingress</span>，发布被挡。
                没有接入面就没有链头，路径也无从排起。
              </div>
            </ConfigPanel>
          )}
        </div>
      </div>
      {moveIngress.error && <ErrorBox error={moveIngress.error} />}

      {/* 线路与机器详情使用同一种规则卡：标题、摘要和规则树属于同一块，不再用独立的
          `h4.sec` 分节线。卡面尺寸与用户页的「授权验证」一致。 */}
      <section className="panel config-panel rule-sheet-card node-chain-sheet">
        <header>
          <h4>链路规则</h4>
          <span className="rule-sheet-meta">{spine.length === 1 ? '直出' : `${spine.length - 1} 跳中继`}</span>
        </header>
        {/* 此处不提供「追加一跳」和「改顺序」。这两项操作都在下方的规则表中完成：
            转发目标的候选是全部机器，链外的机器标注为「链外」——选中后该机器即成为该链的
            下一跳；将某条规则的转发目标改为另一台机器即为重排。
            主干由规则表派生（chainSpine 沿 any→Forward 遍历得出）。

            单独提供「追加一跳」或上移下移相当于为同一操作增加第二个入口，
            且重写整张规则表顺序的保存会覆盖分流规则。 */}
        {/* 只读角色看到的是同一棵树，而不是权限提示。此前该块整体替换为一行提示，
          导致 readonly 角色在链详情页无法查看该链的选路配置——而这正是本页的用途。
          禁用由 RuleEditor 的 readOnly 控制。

          保存条（RuleDraftScope）只在可编辑时包裹：它是一个「保存到草稿」按钮，
          只读时始终处于禁用状态。 */}
        {editable ? (
          <RuleDraftScope hint="改动落进草稿，顶栏按「提交」才写进库。">
            <div className="node-chain-use chain-tree">
              <ChainRulesPanel
                appId={app}
                chain={c}
                spine={spine}
                steps={chainSteps}
                nodes={nodes.data?.nodes ?? []}
                selected={ingress?.node ?? spine[0]}
                onRemove={node => {
                  // 删除 step 即删除成员：steps 的记录是链上成员的唯一数据来源，
                  // 只修改规则表时该成员在编译产物中仍然存在。上游规则表中指向它的
                  // forward 保留，可在编辑器中改为其他目标或改为出网。
                  deleteStep(app, c.id, node);
                }}
                showHeader={false}
              />
            </div>
          </RuleDraftScope>
        ) : (
          <div className="node-chain-use chain-tree">
            {/* 提示置于树之外：树内每张规则表的标题在该档位下是隐藏的（见 styles.css 的
                `.chain-rule-tree .rule-editor>.toolbar:first-child`），写在内部不可见。 */}
            <p className="note" style={{ margin: '0 0 8px' }}>
              只读：规则按原样列出。
            </p>
            <ChainRulesPanel
              appId={app}
              chain={c}
              spine={spine}
              steps={chainSteps}
              nodes={nodes.data?.nodes ?? []}
              selected={ingress?.node ?? spine[0]}
              showHeader={false}
              readOnly
            />
          </div>
        )}
      </section>
    </div>
  );
}

// 编译产物中的规则。字段名与模型层不同：IR 的 Rule 使用 dest_match/action
// （ir/routing.rs），模型层使用 m/a（api.ts）。两者不可混用。
interface CompiledRule {
  dest_match: Rule['m'];
  action: Rule['a'];
}
interface CompiledApp {
  app_id: string | null;
  steps?: { chain: string; node: string; rules?: CompiledRule[] }[];
}

export function compilerFallbackRules(written: Rule[], compiled: CompiledRule[]): Rule[] {
  if (written.at(-1)?.m.t === 'any') return [];
  const fallback = compiled.at(-1);
  return fallback ? [{ m: fallback.dest_match, a: fallback.action }] : [];
}

type ChainRuleRow = {
  node: string;
  spineIndex: number | null;
  hasStep: boolean;
  targeted: boolean;
};

function chainRuleRows(spine: string[], steps: SnapshotStep[]): ChainRuleRow[] {
  const rows: ChainRuleRow[] = [];
  const byNode = new Map<string, ChainRuleRow>();
  const ensure = (node: string) => {
    const existing = byNode.get(node);
    if (existing) return existing;
    const row = { node, spineIndex: null, hasStep: false, targeted: false };
    byNode.set(node, row);
    rows.push(row);
    return row;
  };

  spine.forEach((node, i) => {
    ensure(node).spineIndex = i;
  });
  for (const step of steps) ensure(step.node).hasStep = true;
  for (const step of steps) {
    for (const rule of step.rules) {
      if (rule.a.t === 'forward' && rule.a.to) ensure(rule.a.to).targeted = true;
    }
  }
  return rows;
}

export function defaultChainRuleOccurrence(
  spine: string[],
  steps: SnapshotStep[],
  selected: string | undefined,
): string | null {
  if (!selected) return null;
  const rows = chainRuleRows(spine, steps);
  const graph = chainRuleGraph(steps, rows);
  const roots = spine[0] ? [spine[0]] : rows.slice(0, 1).map(row => row.node);
  const seen = new Set<string>();
  let found: string | null = null;
  const walk = (node: string, path: string[]) => {
    if (found) return;
    const occurrence = [...path, node].join('>');
    if (node === selected) {
      found = occurrence;
      return;
    }
    if (seen.has(node) || path.includes(node)) return;
    seen.add(node);
    for (const edge of graph.get(node) ?? []) walk(edge.to, [...path, node]);
  };
  for (const root of roots) walk(root, []);
  for (const row of rows) if (!seen.has(row.node)) walk(row.node, []);
  return found;
}

export function ChainRulesPanel({
  appId,
  chain,
  spine,
  steps,
  nodes,
  selected,
  showHeader = true,
  onClose,
  onRemove,
  defaultOpenSelected = false,
  rootLabel,
  rootLabelTitle,
  rootSummary,
  readOnly = false,
}: {
  appId: string;
  chain: SnapshotChain;
  // 主干路径（由 api.ts 的 chainSpine 派生）。只用于展示和定位当前节点，
  // 树的边一律来自显式规则。
  spine: string[];
  steps: SnapshotStep[];
  nodes: {
    node_id: string;
    name: string;
    tenant_id: string;
    public_ipv4: string | null;
    public_ipv6: string | null;
    public_ipv4_nat: boolean;
    public_ipv6_nat: boolean;
    retired_at: string | null;
  }[];
  selected?: string;
  showHeader?: boolean;
  onClose?: () => void;
  // 只有链详情页传入该参数——机器详情页中的对应区块表示该机器参与的链，
  // 在该上下文中删除其他机器不合适。不传入时不渲染该组控件。
  onRemove?: (node: string) => void;
  defaultOpenSelected?: boolean;
  rootLabel?: string;
  rootLabelTitle?: string;
  rootSummary?: ReactNode;
  // readonly 角色看到的是同一棵树和同一张表，只是全部禁用（见 RuleEditor 的 readOnly）。
  // 此前整块被替换为「修改规则需要 editor 及以上」——该提示回答的是权限问题，
  // 而进入链详情页需要了解的是当前配置，两者不同。
  readOnly?: boolean;
}) {
  const rows = chainRuleRows(spine, steps);
  const graph = chainRuleGraph(steps, rows);
  /* 中转端口的默认值需要选择未占用的端口。与接入面处共用同一份判定（ports.ts）。 */
  const snapshotForPorts = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodesForPorts = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const revisionsForPorts = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const currentForPorts = revisionsForPorts.data?.current_revision;
  const compiledForPorts = useQuery({
    queryKey: ['compile', currentForPorts],
    queryFn: () => fetchCompileView(currentForPorts!),
    enabled: !!currentForPorts,
  });
  const portPool = useMemo(
    () =>
      occupiedPorts(
        snapshotForPorts.data?.snapshot.apps ?? [],
        nodesForPorts.data?.nodes ?? [],
        compiledForPorts.data?.system,
      ),
    [snapshotForPorts.data, nodesForPorts.data, compiledForPorts.data],
  );
  const rowOf = new Map(rows.map(row => [row.node, row]));
  const stepOf = (node: string) => steps.find(s => s.node === node) ?? null;
  /* 树中显示机器名称，id 写入 title——名称是日常识别依据 */
  const nameMap = new Map(nodes.map(n => [n.node_id, n.name]));
  const nameOf = (id: string) => nameMap.get(id) || id;
  const rendered = new Set<string>();

  // 默认全部折叠：此前每层内联一整张 RuleEditor，四台机器即四张叠放的表单，超出一屏，
  // 且树的结构被表单遮盖。折叠后每台机器收为一行摘要，缩进保留，结构可直接识别。
  // 同一台机器在树中可能出现两次（分叉后汇合），而库中只有一条记录（steps 主键为
  // chain_id + node_id），因此两处编辑的必须是同一份草稿，由此处持有。分别持有会导致
  // 两份 draft 相互覆盖——RuleDraftScope 逐个 handle 保存，后保存的生效。
  const [draftRules, setDraftRules] = useState<Record<string, Rule[]>>({});
  const [draftHops, setDraftHops] = useState<Record<string, HopsDraft>>({});
  const [draftDns, setDraftDns] = useState<Record<string, EgressDnsDraft>>({});
  const [draftDnsOrder, setDraftDnsOrder] = useState<Record<string, EgressDnsOrderDraft>>({});
  const peersOf = (node: string) => forwardPeers({ nodeId: node, spine, tenant: chain.tenant, steps, nodes });

  // 展开状态按出现位置记录（从根到该节点的完整路径），不按机器记录：
  // 同一台机器在树中出现两次时，点击哪一处展开哪一处。另一处不同步展开——
  // 两份相同的表单同时显示时无法确定正在编辑哪一份，而它们本就是同一份数据。
  // 另一处改为高亮显示（见 .same-open），表示该机器在其他位置已展开。
  const defaultOccurrence = defaultOpenSelected ? defaultChainRuleOccurrence(spine, steps, selected) : null;
  const defaultKey = defaultOccurrence ? `${selected ?? ''}:${defaultOccurrence}` : null;
  const [open, setOpen] = useState<Set<string>>(() => new Set(defaultOccurrence ? [defaultOccurrence] : []));
  const lastDefaultKey = useRef(defaultKey);
  useEffect(() => {
    if (!defaultOccurrence || !defaultKey || lastDefaultKey.current === defaultKey) return;
    lastDefaultKey.current = defaultKey;
    setOpen(previous => new Set(previous).add(defaultOccurrence));
  }, [defaultOccurrence, defaultKey]);
  const toggle = (occ: string) =>
    setOpen(prev => {
      const next = new Set(prev);
      if (next.has(occ)) next.delete(occ);
      else next.add(occ);
      return next;
    });
  /* 至少有一处展开的机器——用于为同一机器的其余位置添加高亮 */
  const openNodes = new Set([...open].map(occ => occ.split('>').pop() as string));

  // 编译器补全在规则表末尾的规则。读取编译视图，不在此处推算：
  // 在浏览器中重新计算相当于实现第二个编译器，最终会与 Rust 的实现产生差异。
  // `fetchCompileView` 在存在草稿时读取草稿的编译结果，未保存的改动同样计算正确。
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const currentRevision = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', currentRevision],
    queryFn: () => fetchCompileView(currentRevision!),
    enabled: !!currentRevision,
  });
  const compiledRules = useMemo(() => {
    const apps = (compiled.data?.apps as CompiledApp[] | undefined) ?? [];
    const app = apps.find(a => (a.app_id ?? '') === appId);
    return new Map((app?.steps ?? []).filter(s => s.chain === chain.id).map(s => [s.node, s.rules ?? []]));
  }, [compiled.data, appId, chain.id]);

  const fallbackOf = (node: string) => {
    const written = draftRules[node] ?? stepOf(node)?.rules ?? [];
    const full = compiledRules.get(node) ?? [];
    return compilerFallbackRules(written, full);
  };

  // 先统计每台机器在该树中出现的次数。
  // 出现两次以上的，每一处都标记「共享配置」——包括第一处：只标记后续位置时，
  // 第一处会被认为独占该配置，而实际上它与其他位置对应库中同一条记录（steps 主键 chain+node）。
  // 遍历逻辑必须与下面的 renderNode 一致（相同的防环和防重复展开处理），
  // 否则统计的次数与渲染的树不一致。
  const rootNodes = spine[0] ? [spine[0]] : rows.slice(0, 1).map(row => row.node);
  const occurrences = (() => {
    const count = new Map<string, number>();
    const seen = new Set<string>();
    const walk = (node: string, path: Set<string>) => {
      count.set(node, (count.get(node) ?? 0) + 1);
      if (seen.has(node) || path.has(node)) return;
      seen.add(node);
      const nextPath = new Set(path);
      nextPath.add(node);
      for (const edge of graph.get(node) ?? []) walk(edge.to, nextPath);
    };
    for (const root of rootNodes) walk(root, new Set());
    /* 未从入口连通的节点（孤立组）同样各计入一次 */
    for (const row of rows) if (!seen.has(row.node)) walk(row.node, new Set());
    return count;
  })();

  // depth 只传递给 CSS：连线的对比度按层级递减，每深一层降低一档。
  // 不传递时各层分隔线完全相同，第三层与第一层在视觉上距离相同。
  const renderNode = (node: string, incoming: string[], path: Set<string>, depth = 0) => {
    const row = rowOf.get(node);
    const repeated = rendered.has(node) || path.has(node);
    if (!repeated) rendered.add(node);
    const step = stepOf(node);
    const nextPath = new Set(path);
    nextPath.add(node);
    const children = graph.get(node) ?? [];

    /* 该位置的唯一键：从根到此处的路径。同一台机器的两个位置路径不同。 */
    const occ = [...path, node].join('>');
    const expanded = open.has(occ);
    const rootOccurrence = path.size === 0 && rootNodes.includes(node);
    /* 该机器在其他位置已展开（而非此处）：高亮提示，不同步展开 */
    const sameOpen = !expanded && openNodes.has(node);
    /* 摘要行的两项内容：规则条数、中转端口（含加密档位，PLAIN 即该处的明文提示）。 */
    const written = step?.rules.length ?? 0;

    const badges = (
      <>
        {rootOccurrence && rootLabel && (
          <span className="st b-chain" title={rootLabelTitle}>
            {rootLabel}
          </span>
        )}
        {/* 「共享配置」排在最前：它表示的是该机器的属性（在该链中出现多次、共用一张规则表），
            而其后的 geoip= / domain= 标签表示的是该边的属性（流量因何条件到达此处）。
            机器的属性应紧邻机器名。 */}
        {(occurrences.get(node) ?? 0) > 1 && (
          <span className="st" title="这台机器在这条链上出现多次，共用同一张规则表">
            共享配置
          </span>
        )}
        {/* 「入口」是端点标签，使用 accent；其余 incoming 标签（匹配条件/主干默认/孤立）保持灰色。
            「分叉」标签已移除：它表示的只是不在 spine 数组中这一实现细节，
            与流量的实际结构不对应，显示出来会造成误解。 */}
        {incoming.map(label => (
          <span className={label === '入口' ? 'st b-role' : 'st'} key={label}>
            {label}
          </span>
        ))}
        {row?.spineIndex === null && !row?.targeted && row?.hasStep && <span className="st">规则节点</span>}
        {selected && node === selected && <span className="st st-succeeded">当前节点</span>}
      </>
    );

    return (
      <div
        className={`chain-rule-node${expanded ? ' open' : ''}${sameOpen ? ' same-open' : ''}`}
        style={{ '--depth': depth } as CSSProperties}
        key={`${node}/${incoming.join('+') || 'root'}`}
      >
        <div
          role="button"
          tabIndex={0}
          className="chain-rule-node-head"
          aria-expanded={expanded}
          onClick={() => toggle(occ)}
          onKeyDown={e => {
            if (e.target !== e.currentTarget) return;
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              toggle(occ);
            }
          }}
        >
          <span className="disc" aria-hidden="true">
            {expanded ? '▾' : '▸'}
          </span>
          {/* 名称和标签占一格，宽度不足时截断——右侧两列是定宽的，不应被此处挤占 */}
          <span className="who">
            <b title={node}>{nameOf(node)}</b>
            {badges}
          </span>
          {/* 删除：与项目列表中删除链使用同一控件和同一反馈（见 DelBtn）。
                只对主干上的节点提供，且入口位（spineIndex 0）不可删除——删除它等同于删除
                整条链，该操作在链列表中执行。分叉节点由上游的转发规则控制，不在此处删除。

                该格始终渲染（无按钮时为空 span）：行使用 grid 布局，缺少一个 item 会使
                后续列整体前移一格，右侧两列将与其他行错位。 */}
          <span className="hopctl" onClick={e => e.stopPropagation()}>
            {onRemove && row?.spineIndex != null && row.spineIndex > 0 && (
              <DelBtn
                title="从这条链移除这台机器，其整张规则表一并删除。写入草稿，提交后生效"
                onClick={() => onRemove(node)}
              />
            )}
          </span>
          <div className="meta">
            <span className="m-rules">{written === 0 ? '没写规则' : `${written} 条规则`}</span>
            <div className="m-hop">
              {rootOccurrence && rootSummary ? rootSummary : <span className="mono">{summarizeHopIn(step)}</span>}
            </div>
          </div>
        </div>
        {
          <>
            {/* 折叠的节点仍然保持挂载：卸载会连同未保存的改动一起丢弃，
                且 RuleDraftScope 按挂载顺序保存（上游必须在前）。 */}
            <div className="chain-rule-body" hidden={!expanded}>
              <RuleEditor
                key={`${appId}/${chain.id}/${node}`}
                appId={appId}
                chainId={chain.id}
                nodeId={node}
                initial={step?.rules ?? []}
                accept={step?.accept ?? null}
                hopIn={step?.hop_in ?? null}
                // 草稿由 panel 持有：同一台机器在树中出现两次时，两处是两个组件实例，
                // 但读写同一份数据——库中只有一条记录。
                shared={{
                  rules: draftRules[node] ?? step?.rules ?? [],
                  setRules: next => setDraftRules(prev => ({ ...prev, [node]: next })),
                  hops: draftHops[node] ?? seedHops(peersOf(node), portPool),
                  setHops: next => setDraftHops(prev => ({ ...prev, [node]: next })),
                  dns: draftDns[node] ?? {},
                  setDns: next => setDraftDns(prev => ({ ...prev, [node]: next })),
                  dnsOrder: draftDnsOrder[node] ?? null,
                  setDnsOrder: next => setDraftDnsOrder(prev => ({ ...prev, [node]: next })),
                }}
                peers={peersOf(node)}
                isForwardTarget={isForwardTargetInChain({
                  nodeId: node,
                  steps,
                })}
                /* 整条链的当前状态和链头：保存时据此计算不再被任何规则指向的节点，一并移除 */
                steps={steps}
                root={spine[0]}
                // 同一台机器的多个位置共用一份草稿，由其中一处执行保存即可。
                // 两处都注册时同一内容会被写入两次（结果幂等，但产生一次多余的请求）。
                saves={!repeated}
                readOnly={readOnly}
                fallback={{ rules: fallbackOf(node), pending: compiled.isLoading }}
              />
            </div>
            {children.length > 0 && (
              <div className="chain-rule-children">
                {children.map(edge => renderNode(edge.to, edge.labels, nextPath, depth + 1))}
              </div>
            )}
          </>
        }
      </div>
    );
  };

  const tree = rootNodes.map(root => renderNode(root, ['入口'], new Set()));
  const orphanTree = rows.flatMap(row => (rendered.has(row.node) ? [] : [renderNode(row.node, ['孤立'], new Set())]));

  return (
    <>
      {showHeader && (
        <div className="toolbar" style={{ marginTop: 14 }}>
          <b>整条链规则</b>
          <span className="note">从入口向下游递归展开，每一层都是可编辑规则。</span>
          <span className="sp" />
          {onClose && (
            <button className="btn" onClick={onClose}>
              关闭
            </button>
          )}
        </div>
      )}
      <div className="chain-rule-tree">
        {tree}
        {orphanTree.length > 0 && (
          <div className="chain-rule-orphans">
            <div className="chain-rule-node-head">
              <b>未从入口展开</b>
              <span className="note">这些节点存在 step 或被规则引用，但当前规则图未从入口连通到它。</span>
            </div>
            {orphanTree}
          </div>
        )}
      </div>
    </>
  );
}

type ChainGraphEdge = {
  to: string;
  labels: string[];
};

function chainRuleGraph(steps: SnapshotStep[], rows: ChainRuleRow[]): Map<string, ChainGraphEdge[]> {
  const order = new Map(rows.map((row, i) => [row.node, i]));
  const graph = new Map<string, ChainGraphEdge[]>();
  const addEdge = (from: string, to: string, label: string) => {
    const list = graph.get(from) ?? [];
    const existing = list.find(edge => edge.to === to);
    if (existing) {
      if (label && !existing.labels.includes(label)) existing.labels.push(label);
    } else {
      list.push({ to, labels: label ? [label] : [] });
      graph.set(from, list);
    }
  };

  // 边只来自显式规则。编译器不补全主干默认边——上游未写转发时，下游从链头不可达，
  // 会归入「未从入口展开」一组。
  for (const step of steps) {
    for (const rule of step.rules) {
      if (rule.a.t === 'forward' && rule.a.to) addEdge(step.node, rule.a.to, summarizeEdge(rule));
    }
  }
  for (const edges of graph.values()) {
    edges.sort((a, b) => (order.get(a.to) ?? 9999) - (order.get(b.to) ?? 9999));
  }
  return graph;
}

// 已有的接受凭据需原样回传：label 是 xray 统计指标的键，更换后统计曲线会中断。
// 不存在时传空对象由 store 生成，密钥不经过浏览器。
function summarize(r: Rule, nameOf: (id: string) => string = id => id): string {
  const m =
    r.m.t === 'any' ? '任意' : 'v' in r.m ? `${r.m.t}=${Array.isArray(r.m.v) ? r.m.v.join('/') : r.m.v}` : r.m.t;
  const a =
    r.a.t === 'forward'
      ? `→ ${nameOf(r.a.to ?? '')}`
      : r.a.t === 'proxy'
        ? `外部代理 ${r.a.outbound}`
        : r.a.t === 'egress'
          ? '落地'
          : '拒绝';
  return `${m} ${a}`;
}

// 边上的标签只显示匹配条件本身（如 geosite=openai）。「任意」匹配不生成标签——
// 全量转发的边是常态，标注「任意规则」不提供有效信息。
function summarizeEdge(r: Rule): string {
  return r.m.t === 'any' ? '' : summarize(r).replace(/ → .+$/, '');
}

function summarizeHopIn(step: SnapshotStep | null): string {
  if (!step?.hop_in) return '—';
  const security = hopWireLabel(step.hop_in.security.t);
  return `${step.hop_in.port} / ${security}`;
}

export type { SnapshotChain };
