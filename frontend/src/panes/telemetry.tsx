/** 遥测：LOAD 卡、逐跳链路表，以及它们的判定逻辑。
 *
 * 沿用两条既有规则（均记录在 nodes.tsx 中）：
 *   列表只表示哪台机器存在问题，详情说明具体问题——因此正常的行不显示额外内容
 *   标记写明后果而非字段名——「磁盘剩余 3%，用量记录将开始丢失」优于「disk_free_pct 3」
 *
 * 时间统一使用 unix 秒（服务端两侧共用同一组结构体，见 api.ts 的说明），因此传给
 * `Ago` 之前需要转换为 ISO —— `iso()` 即用于该转换。
 */
import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import * as echarts from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { GridComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import { bytes } from '../ui/format';
import { Ago } from '../ui/bits';
import { useNow } from '../ui/clock';
import { theme } from '../forge/theme';
import { palette } from '../forge/palette';
import type { HopLinkView, LoadSample, NodeLoadView, ProcessSample } from '../api';

// 与 nodes.tsx 的 FleetNetChart 共用同一套注册；echarts.use 对重复注册幂等，
// 但本模块独立使用 echarts，需要自己声明所依赖的组件。
echarts.use([LineChart, GridComponent, TooltipComponent, CanvasRenderer]);

/** canvas 里字体要给具体栈，不能写 var(--mono)。 */
const TP_MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';

/** unix 秒转 ISO。`Ago` 接受 ISO 字符串，而协议中统一使用秒。nodes.tsx 的 HOST 卡复用 */
export const iso = (secs: number) => new Date(secs * 1000).toISOString();

/* ── 格式化 ──────────────────────────────────────────────────
 * 带宽使用 1000 进制（Mb/s 是网络领域的常用单位），字节使用 ui/format 的 1024 进制。
 * 两套单位并存不是疏漏：将 82 Mb/s 显示为 78.2 Mib/s 不符合惯例，
 * 将 184 MB 的 RSS 显示为 193 MB 又与 top 的输出不一致。
 */

export const bps = (n: number | null): string => {
  if (n === null) return '—';
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)} Gb/s`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} Mb/s`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} kb/s`;
  return `${Math.round(n)} b/s`;
};

/** 微秒转毫秒。RTT 统一显示为毫秒——微秒级精度在跨境链路上没有意义，
    且多出三位数字会导致该列不对齐。 */
const ms = (us: number): string => (us >= 10_000 ? `${Math.round(us / 1000)} ms` : `${(us / 1000).toFixed(1)} ms`);

const pct = (n: number, digits = 0): string => `${n.toFixed(digits)}%`;

/** 时长格式化——用于 uptime 和进程启动时刻。Ago 接受 ISO 字符串，此处接受秒数。
    nodes.tsx 的 AGENT 卡（agent 进程已运行）复用。 */
export function dur(secs: number): string {
  if (secs < 60) return `${Math.round(secs)} 秒`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} 小时`;
  return `${Math.floor(secs / 86400)} 天`;
}

/** 一条判定。
 *
 * 文案遵循两条规则：
 *
 * **一、不写通用知识。** 查看该卡的使用者了解 cubic 在丢包时的行为、了解 OOM 终止 xray
 * 的含义。写出这些内容不提供帮助，反而需要在其中查找关键数值。只写本系统特有的事实——
 * 「spool 位于 state_dir 所在的磁盘」「安装时已设置 bbr」——这些无法从通用知识推出。
 *
 * **二、字段已表达的内容不重复。** 卡片中的字段已将 `cubic` 标为金色，
 * 下方再写「该机器使用 cubic」属于重复。`chipOnly` 用于该情况：
 * 列表中的标记保留，详情页的说明行不显示。 */
type Finding = {
  tone: 'warn' | 'bad';
  chip: string;
  /** 详情页的说明行。`chipOnly` 时不渲染，此处传空即可。 */
  text: ReactNode;
  /** 只在列表中显示标记，详情页不显示文字——卡片中的字段已表达该信息。 */
  chipOnly?: boolean;
  /** 只在详情页显示，不进入列表。部分条目是说明而非问题（如「该版本 agent 不上报」），
      进入列表会将正常机器显示为异常。 */
  detailOnly?: boolean;
};

/* ── LOAD 趋势图 ─────────────────────────────────────────────
 * 24 个窗口直接来自现有 node-load 接口。横轴固定为最近 12 分钟；样本不足时右对齐，
 * `has_gap` 时切断路径，不把机器重启前后的两个值连成一条不存在的趋势。
 */

const LOAD_SLOTS = 24;
const PLOT_W = 100;
const PLOT_H = 48;
const PLOT_TOP = 4;
const PLOT_BOTTOM = 44;

interface PlotPoint {
  x: number;
  y: number;
}

interface TrendPaths {
  lines: string[];
  areas: string[];
}

/** Catmull–Rom 转三次贝塞尔。控制点限制在画布内，避免低波动序列在端点发生过冲。 */
function smoothSegment(points: PlotPoint[]): string {
  if (points.length === 0) return '';
  if (points.length === 1) return `M${points[0].x.toFixed(2)} ${points[0].y.toFixed(2)}l.01 0`;
  let path = `M${points[0].x.toFixed(2)} ${points[0].y.toFixed(2)}`;
  for (let i = 0; i < points.length - 1; i += 1) {
    const p0 = points[Math.max(0, i - 1)];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[Math.min(points.length - 1, i + 2)];
    const c1x = p1.x + (p2.x - p0.x) / 6;
    const c1y = Math.max(PLOT_TOP, Math.min(PLOT_BOTTOM, p1.y + (p2.y - p0.y) / 6));
    const c2x = p2.x - (p3.x - p1.x) / 6;
    const c2y = Math.max(PLOT_TOP, Math.min(PLOT_BOTTOM, p2.y - (p3.y - p1.y) / 6));
    path += ` C${c1x.toFixed(2)} ${c1y.toFixed(2)} ${c2x.toFixed(2)} ${c2y.toFixed(2)} ${p2.x.toFixed(2)} ${p2.y.toFixed(2)}`;
  }
  return path;
}

/** 面积封口的 y。
 *
 * 两张图对底边的要求不同，所以这一项要传：
 * - 网卡吞吐画了网格，最下面一条横线就在 `PLOT_BOTTOM`（见 `.load-grid` 的 `M0 44H100`），
 *   面积必须收在那条线上，越过去就是填到坐标轴外面。
 * - 指标趋势（`.load-metric`）没有网格，SVG 贴着卡片下沿铺满，面积要一直铺到 viewBox 底部
 *   `PLOT_H`。收在 `PLOT_BOTTOM` 的话，48 个单位里最下面 4 个永远是空的——每张小卡底部
 *   都留一条透明缝。机器卡的用量图早就是这么处理的：线的下限是 25，面积仍封在 H=36。
 */
function trendPaths(
  series: LoadSample[],
  valueOf: (sample: LoadSample) => number | null,
  domain: [number, number],
  floor: number = PLOT_BOTTOM,
): TrendPaths {
  const tail = series.slice(-LOAD_SLOTS);
  const offset = LOAD_SLOTS - tail.length;
  const span = Math.max(domain[1] - domain[0], Number.EPSILON);
  const segments: PlotPoint[][] = [];
  let current: PlotPoint[] = [];
  const finish = () => {
    if (current.length > 0) segments.push(current);
    current = [];
  };
  tail.forEach((sample, index) => {
    const value = valueOf(sample);
    if (sample.has_gap || value === null || !Number.isFinite(value)) {
      finish();
      return;
    }
    current.push({
      x: ((offset + index) / (LOAD_SLOTS - 1)) * PLOT_W,
      y: PLOT_TOP + ((domain[1] - value) / span) * (PLOT_BOTTOM - PLOT_TOP),
    });
  });
  finish();
  return {
    lines: segments.map(smoothSegment),
    areas: segments.map(points => {
      const line = smoothSegment(points);
      return `${line} L${points[points.length - 1].x.toFixed(2)} ${floor} L${points[0].x.toFixed(2)} ${floor} Z`;
    }),
  };
}

/** 指标卡使用局部自动量程，并给上下各留一段呼吸空间。数值本身常驻显示，因此曲线负责表现
 * 近期变化而不是充当刻度；百分比仍限制在 0–100，不能画出物理上不存在的范围。 */
function metricDomain(values: number[], percent = false): [number, number] {
  if (values.length === 0) return [0, 1];
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const pad = Math.max((hi - lo) * 0.16, Math.abs(hi) * 0.08, hi <= 1 ? 0.08 : 1);
  return [Math.max(0, lo - pad), percent ? Math.min(100, Math.max(hi + pad, lo + 1)) : Math.max(hi + pad, lo + 0.1)];
}

/** 占比条。三种受限时间是同一个 100% 的三个部分，写成三个数字需要自行还原其比例关系。 */
function Meter({ parts }: { parts: { cls: string; v: number; label: string }[] }) {
  return (
    <span className="tmeter" title={parts.map(p => `${p.label} ${pct(p.v)}`).join(' · ')}>
      {parts.map(p => (
        <i key={p.cls} className={p.cls} style={{ width: `${Math.min(100, p.v)}%` }} />
      ))}
    </span>
  );
}

/* ── 判定：机器 ──────────────────────────────────────────────
 *
 * 阈值全部定义为具名常量，因为它们需要被讨论和调整——匿名的 0.85 分散在代码中时，
 * 后续讨论其灵敏度时难以定位。
 */

/** CPU 使用峰值而非均值判定。转发负载的特征是尖峰，均值 40% 的机器可能每分钟出现一次阻塞。 */
const CPU_PEAK_WARN = 85;
/** 软中断单独设置阈值。达到该值表示网卡中断已占用约半个核，
    继续上升会出现丢包，而丢包在业务上表现为间歇性无法连接。 */
const SOFTIRQ_WARN = 30;
/** 可用内存低于该比例。使用 available 而非 free——free 在有 page cache 的机器上
    始终很小，按其判定会将所有正常机器报为内存不足。 */
const MEM_AVAIL_WARN = 0.12;
/** 磁盘。两档：warn 表示需要清理，bad 表示即将开始丢失用量记录。 */
const DISK_FREE_WARN = 0.1;
const DISK_FREE_BAD = 0.05;
/** conntrack。达到该比例时新连接仍可建立，但需要扩容；
    表项占满后的表现是部分用户无法连接，是较难定位的一类故障。 */
const CONNTRACK_WARN = 0.85;
/** 最近重启过。半小时以内视为最近——超过该值时，重启本身不再是当前需要处理的问题。 */
const JUST_REBOOTED_SECS = 1800;
/** fd 使用量。xray 的 fd 达到上限时会拒绝新连接，但进程不会退出，
    因此常规监控无法发现该问题。 */
const FD_WARN = 0.8;

/** BBR 合入主线的内核版本。低于该版本的内核不包含 bbr，修改 sysctl 也无效。
    更换何种内核、能否更换取决于该机器的供应商和虚拟化环境——此处无法判定。 */
const BBR_MIN_KERNEL: [number, number] = [4, 9];

const kernelVer = (raw: string): [number, number] | null => {
  const m = raw.match(/^(\d+)\.(\d+)/);
  return m ? [Number(m[1]), Number(m[2])] : null;
};

const below = (v: [number, number], min: [number, number]) => v[0] < min[0] || (v[0] === min[0] && v[1] < min[1]);

/** BBR 系列算法。低价 VPS 上常见通过脚本安装的定制内核，其中包含 bbrplus / bbr2，
    它们都维护瓶颈带宽估计，可通过 `tcp_bbr_info` 读取，因此不应报为未启用 BBR。 */
const isBbr = (algo: string) => algo.startsWith('bbr');

/** 该机器是否运行 xray。
 *
 * 判定中使用该条件排除一类误报：**BBR 只作用于本机终结重传的 TCP**，
 * 纯 wg 骨干中继上只有转发的 UDP，是否启用 BBR 不产生任何差异。
 * 不加区分地报告未启用 BBR 会导致对正常机器执行不必要的操作。 */
const hasXray = (r: NodeLoadView) => r.processes.some(p => p.proc === 'xray' && p.rss_bytes !== null);

export function loadFindings(r: NodeLoadView): Finding[] {
  const out: Finding[] = [];

  // 从未上报。该情况需要最先判定并直接返回——下面每一条都会用 undefined 参与比较，
  // 而 `undefined > 0.85` 为 false，会导致无任何数据的机器显示为全部正常。
  // 此处不输出判定：卡片中已显示「还没有负载读数」，而缺失原因该函数无法判定——
  // series 为空可能是刚纳管尚未产生第一个窗口、agent 掉线，也可能是 agent 版本过旧，
  // 而 NodeLoadView 中既没有 agent 版本也没有心跳信息。无法区分时不给出结论。
  if (r.series.length === 0) return out;

  const last = r.series[r.series.length - 1];

  // CPU。峰值和 softirq 分为两条，因为二者对应不同的处理方式。
  const peak = Math.max(...r.series.map(s => s.cpu_peak_pct));
  if (peak >= CPU_PEAK_WARN) {
    out.push({
      tone: 'warn',
      chip: `CPU 峰值 ${pct(peak)}`,
      chipOnly: true,
      text: null,
    });
  }
  const softirq = Math.max(...r.series.map(s => s.cpu_softirq_pct));
  if (softirq >= SOFTIRQ_WARN) {
    out.push({
      tone: 'warn',
      chip: `软中断 ${pct(softirq)}`,
      chipOnly: true,
      text: null,
    });
  }

  // 内存。OOM 是已发生的事实，单独一条且级别为 bad——它表示已有进程被终止。
  const oom = r.series.reduce((n, s) => n + s.oom_kills, 0);
  if (oom > 0) {
    out.push({
      tone: 'bad',
      chip: `OOM ${oom} 次`,
      // 卡片中没有常驻位置显示该信息，因此该行是其唯一的呈现位置。需要写明时间范围——
      // 「终止过 1 个」与「最近 12 分钟终止过 1 个」是两种信息。
      text: (
        <>
          最近 {Math.round((r.series.length * 30) / 60)} 分钟内核杀过 <b>{oom}</b> 个进程。
        </>
      ),
    });
  }
  // 分母来自 host（低频变量不进入时序数据）。没有 host 时无法计算比例——已上报读数但未上报
  // 低频变量的机器只可能是某一轮上报被中断，下一轮即完整，此时不输出标记优于输出无效标记。
  const memTotal = r.host?.mem_total_bytes ?? 0;
  const availRatio = memTotal > 0 ? last.mem_available_bytes / memTotal : 1;
  if (availRatio < MEM_AVAIL_WARN) {
    out.push({
      tone: 'warn',
      chip: `内存剩 ${pct(availRatio * 100)}`,
      chipOnly: true,
      text: null,
    });
  }

  // 磁盘。说明需要写到丢失用量记录这一步——只写「磁盘使用率 94%」不会促成处理。
  const diskTotal = r.host?.disk_total_bytes ?? 0;
  const freeRatio = diskTotal > 0 ? last.disk_free_bytes / diskTotal : 1;
  if (freeRatio < DISK_FREE_WARN) {
    const bad = freeRatio < DISK_FREE_BAD;
    out.push({
      tone: bad ? 'bad' : 'warn',
      chip: `盘只剩 ${pct(freeRatio * 100)}`,
      // 保留该行，因为它不属于通用知识：查看数据的人无从知道 spool 位于 state_dir 所在分区，
      // 也无从将磁盘写满与该机器本月没有流量记录关联起来。
      text: <>spool 在这个分区上，写不下就丢账。</>,
    });
  }

  // conntrack。机器其他指标正常时该项最容易被忽略，而它是 NAT 节点最先达到的上限。
  const ctMax = r.host?.conntrack_max ?? null;
  if (last.conntrack_count !== null && ctMax !== null && ctMax > 0) {
    const ratio = last.conntrack_count / ctMax;
    if (ratio >= CONNTRACK_WARN) {
      out.push({
        tone: 'warn',
        chip: `连接表 ${pct(ratio * 100)}`,
        text: (
          <>
            调 <code>nf_conntrack_max</code>。
          </>
        ),
      });
    }
  }

  // 最近重启过。不属于故障，但它说明了相邻各项数值为空或异常的原因。
  if (last.uptime_secs < JUST_REBOOTED_SECS) {
    out.push({
      tone: 'warn',
      chip: `${dur(last.uptime_secs)}前重启`,
      chipOnly: true,
      text: null,
    });
  }

  // fd。达到上限时 xray 不退出，只拒绝新连接——因此其他位置无法反映该问题。
  for (const p of r.processes) {
    if (p.fds === null || p.fd_limit === null) continue;
    if (p.fds / p.fd_limit >= FD_WARN) {
      out.push({
        tone: 'warn',
        chip: `${p.proc} fd ${pct((p.fds / p.fd_limit) * 100)}`,
        text: (
          <>
            {p.proc} fd {p.fds.toLocaleString()} / {p.fd_limit.toLocaleString()}，调这个 unit 的{' '}
            <code>LimitNOFILE</code>。
          </>
        ),
      });
    }
  }

  out.push(...congestionFindings(r));
  return out;
}

/** 拥塞控制相关的几条判定。单独定义为一个函数，因为它们共用同一前提判断（该机器是否运行
 *  xray），且后续会与 RUNTIME 卡合并——它们表示的是该机器的配置，而非当前负载。 */
function congestionFindings(r: NodeLoadView): Finding[] {
  const out: Finding[] = [];
  const host = r.host;
  if (!host?.cc_algo) return out;

  const kv = kernelVer(host.kernel);

  // 纯 wg 中继不做判定：BBR 只作用于本机终结重传的 TCP，该机器上只有转发的 UDP。
  if (hasXray(r) && !isBbr(host.cc_algo)) {
    if (kv && below(kv, BBR_MIN_KERNEL)) {
      out.push({
        tone: 'warn',
        chip: `内核 ${host.kernel.split('-')[0]}，没有 bbr`,
        text: (
          <>
            内核 <b>{host.kernel}</b> 里没有 bbr，要换内核。
          </>
        ),
      });
    } else if (host.sysctl_managed) {
      // 安装时已设置，当前已不是该值。没有 sysctl_managed 字段时无法给出该结论——
      // 只能表述为「不是 bbr」，而该表述对从未设置过的机器同样成立。
      out.push({
        tone: 'warn',
        chip: 'bbr 被改回去了',
        // 安装时已设置是该行存在的原因——它不属于通用知识，只有 sysctl_managed 字段
        // 能提供该信息，它区分了该机器从未配置过和配置被修改两种情况。
        text: <>装机时设过 bbr，现在是 {host.cc_algo}。</>,
      });
    } else {
      out.push({
        tone: 'warn',
        chip: `拥塞算法 ${host.cc_algo}`,
        // 卡片中的该字段已标为金色。此处只输出标记，不重复说明。
        chipOnly: true,
        text: null,
      });
    }
  }

  return out;
}

/* ── LOAD 卡 ────────────────────────────────────────────────── */

const PROC_LABEL: Record<ProcessSample['proc'], string> = {
  xray: 'xray',
  wg: 'wireguard',
  phantun: 'phantun',
  agent: 'agent',
};

/** 该机器的当前负载。
 *
 * 位于观测主栏首位：监控页先回答当前资源是否充足，再向下解释 agent、运行时和配置状态。
 * 卡片底部的进程表是主要内容而非附加信息——只有该部分能区分机器整体负载高和本系统
 * 进程负载高，而机器出现问题时首先会怀疑本系统部署的这几个进程。 */
/** 上报多久后视为数据过期。取两个采样窗口——正常机器每 30 秒上报一次，连续丢失十次才达到该值。
 *
 * 不在顶部标注时，26 分钟前的读数与实时数据的显示相同，而该卡片中每个数值的含义
 * 都取决于它是否为当前时刻的数据。 */
const STALE_SECS = 300;

/** 双序列吞吐曲线（echarts）。NETWORK（网卡，按方向）与 XRAY（承载，按角色）共用这一个组件：
 * 两者口径不同、绝不能合并进一张图，但时间轴一致、用 `echarts.connect(group)` 联动十字线——
 * 悬停任一时刻两图同时高亮，这才是「可对比」。各自独立的 Y 轴与图例标明口径差异。
 *
 * 细节约定（与现网手绘 SVG 一致，逐项对应）：
 * - Y 量程 = 峰值 × 1.12，按三等分画刻度（max / 2/3 / 1/3 / 0），间距必然均匀；写死 max 加
 *   splitNumber 会出现顶格被压扁的不均匀刻度。
 * - option 顶层给 `color: [data, data-secondary]`：否则 tooltip 的圆点回退到 echarts 默认
 *   调色板（蓝/绿），跟线条和 HTML 图例都对不上。
 * - rx 画面积、tx 只画线：面积属于主流向，使两条同族蓝线在不改配色的前提下可区分。
 * - 缺口（has_gap / null）断开不连接。 */
export function ThroughputChart({
  rx,
  tx,
  rxName,
  txName,
  group,
}: {
  /** 24 个 30 秒窗口的速率（bps），下标 23 为最近；null 表示缺口，断开不连。 */
  rx: (number | null)[];
  tx: (number | null)[];
  rxName: string;
  txName: string;
  group?: string;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  // 最近一次真正写入实例的输入签名。父组件可能因无关状态每秒重渲染（LoadCard 的相对
  // 时间标签由 useNow 驱动），传入身份新但值相同的数组；若仅凭数组身份就重设 option，
  // notMerge 会销毁悬停中的 tooltip DOM——值未变时必须跳过。
  const lastSig = useRef<string | null>(null);

  // 实例只建一次；容器尺寸变化时 resize。group 在实例上设置一次，供 connect 按组联动。
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

  // 数据或主题变化时重设 option。数据角色色从 :root 的 CSS 令牌读。
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    const sig = JSON.stringify([rx, tx, rxName, txName, group, themeName, paletteKey]);
    if (sig === lastSig.current) return;
    lastSig.current = sig;
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

    const peak = Math.max(
      ...rx.filter((v): v is number => v !== null),
      ...tx.filter((v): v is number => v !== null),
      1,
    );
    const max = peak * 1.12;
    const slot = (i: number) => (i === LOAD_SLOTS - 1 ? '现在' : `−${Math.round(((LOAD_SLOTS - 1 - i) * 30) / 60)}m`);

    const mk = (name: string, data: (number | null)[], color: string, area: boolean) => ({
      name,
      type: 'line' as const,
      showSymbol: false,
      smooth: 0.32,
      connectNulls: false,
      lineStyle: { color, width: area ? 1.6 : 1.25 },
      ...(area ? { areaStyle: { color, opacity: 0.13 } } : {}),
      emphasis: { disabled: true },
      data,
    });

    chart.setOption(
      {
        animation: false,
        color: [data, dataSecondary],
        grid: { left: 52, right: 14, top: 10, bottom: 20 },
        textStyle: { fontFamily: TP_MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: TP_MONO },
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' } },
          formatter: (params: unknown) => {
            const arr = params as { seriesName: string; color: string; value: number | null; dataIndex: number }[];
            const head = slot(arr[0].dataIndex);
            const row = (p: { seriesName: string; color: string; value: number | null }) =>
              `<div style="display:flex;gap:8px;align-items:center;line-height:1.75">` +
              `<span style="width:8px;height:8px;border-radius:2px;background:${p.color}"></span>` +
              `<span>${p.seriesName}</span>` +
              `<b style="margin-left:auto;color:${ink}">${p.value === null ? '—' : bps(p.value)}</b></div>`;
            return `<div style="color:${ink4};font-size:9px;margin-bottom:3px">${head}</div>${arr.map(row).join('')}`;
          },
        },
        xAxis: {
          type: 'category',
          data: Array.from({ length: LOAD_SLOTS }, (_, i) => String(i)),
          boundaryGap: false,
          axisLine: { lineStyle: { color: line } },
          axisTick: { show: false },
          splitLine: { show: false },
          axisLabel: {
            color: ink4,
            fontSize: 8,
            interval: (i: number) => i === 0 || i === 8 || i === 16 || i === LOAD_SLOTS - 1,
            formatter: (_v: string, i: number) => slot(i),
          },
        },
        yAxis: {
          type: 'value',
          min: 0,
          max,
          interval: max / 3,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: { color: ink4, fontSize: 8, formatter: (v: number) => (v === 0 ? '0' : bps(v)) },
          splitLine: { lineStyle: { color: lineSoft } },
        },
        series: [mk(rxName, rx, data, true), mk(txName, tx, dataSecondary, false)],
      },
      true,
    );
    // connect 按组联动所有已建实例；任一图重设 option 后重连一次，保证最新成员都在组内。
    if (group) echarts.connect(group);
  }, [rx, tx, rxName, txName, group, themeName, paletteKey]);

  return <div ref={elRef} className="ndtp-ec" />;
}

/** KPI 芯片右下角的迷你趋势线。复用 trendPaths + metricDomain，与既有的指标卡曲线同口径；
 * 面积铺到 viewBox 底部（小图无网格，收在 PLOT_BOTTOM 会留一条空缝）。 */
function Spark({
  series,
  valueOf,
  percent = false,
}: {
  series: LoadSample[];
  valueOf: (sample: LoadSample) => number | null;
  percent?: boolean;
}) {
  const values = series
    .filter(sample => !sample.has_gap)
    .map(valueOf)
    .filter((n): n is number => n !== null);
  const paths = trendPaths(series, valueOf, metricDomain(values, percent), PLOT_H);
  return (
    <svg className="kpi-spark" viewBox={`0 0 ${PLOT_W} ${PLOT_H}`} preserveAspectRatio="none" aria-hidden="true">
      {paths.areas.map((path, index) => (
        <path key={`a-${index}`} className="kpi-spark-area" d={path} />
      ))}
      {paths.lines.map((path, index) => (
        <path key={`l-${index}`} className="kpi-spark-line" d={path} />
      ))}
    </svg>
  );
}

function LoadDashboard({
  report,
  reportedAt,
  stale,
}: {
  report: NodeLoadView;
  reportedAt: number | null;
  stale: boolean;
}) {
  const series = report.series;
  const last = series[series.length - 1];
  const host = report.host;
  const cpu = (sample: LoadSample) => sample.cpu_user_pct + sample.cpu_sys_pct + sample.cpu_softirq_pct;
  const mem = (sample: LoadSample) =>
    host && host.mem_total_bytes > 0 ? (1 - sample.mem_available_bytes / host.mem_total_bytes) * 100 : null;
  const disk = (sample: LoadSample) =>
    host && host.disk_total_bytes > 0 ? (1 - sample.disk_free_bytes / host.disk_total_bytes) * 100 : null;
  const cpuNow = cpu(last);
  const memNow = mem(last);
  const diskNow = disk(last);
  const valid = series.filter(sample => !sample.has_gap);
  const peak = Math.max(...valid.map(sample => sample.cpu_peak_pct), 0);
  const drops = last.nic_rx_drop + last.nic_tx_drop + last.nic_err;
  // 与 usage 的 24 桶对齐的 24 个 30 秒窗口；最近在下标 23，缺口断开。不足 24 个左侧补 null。
  const group = `nd-tp-${report.node_id}`;
  const tail = series.slice(-LOAD_SLOTS);
  const pad = LOAD_SLOTS - tail.length;
  const rxData: (number | null)[] = [
    ...Array<number | null>(pad).fill(null),
    ...tail.map(s => (s.has_gap ? null : s.nic_rx_bps)),
  ];
  const txData: (number | null)[] = [
    ...Array<number | null>(pad).fill(null),
    ...tail.map(s => (s.has_gap ? null : s.nic_tx_bps)),
  ];
  // 芯片的语气阈值与既有的判定/指标卡完全一致——同一处越线，卡片、芯片与下方说明三处一致。
  const cpuTone = peak >= CPU_PEAK_WARN ? 'warn' : '';
  const memTone = memNow !== null && 100 - memNow < MEM_AVAIL_WARN * 100 ? 'warn' : '';
  const diskTone =
    diskNow !== null && 100 - diskNow < DISK_FREE_BAD * 100
      ? 'bad'
      : diskNow !== null && 100 - diskNow < DISK_FREE_WARN * 100
        ? 'warn'
        : '';
  const [upNum, upUnit] = dur(last.uptime_secs).split(' ');
  const ctMax = host?.conntrack_max ?? null;
  const ctRatio = last.conntrack_count !== null && ctMax !== null && ctMax > 0 ? last.conntrack_count / ctMax : null;

  return (
    <>
      {/* KPI 芯片带：CPU/内存/磁盘/负载 征收成标题下一条带（各带迷你趋势线）；已运行、连接表
          是标量，只给读数不给趋势线。金/红语气由阈值算出，spark 用 currentColor 随之变色。 */}
      <div className="kpi-band">
        <div className={`kpi ${cpuTone}`}>
          <span className="kpi-l">CPU</span>
          <span className="kpi-v">
            {pct(cpuNow).replace('%', '')}
            <small>%</small>
            {/* steal 只在可读到时挂后缀：>= 0.1% 才显示——裸金属恒为 0，
                常显的「STEAL 0.0%」是不携带信息的噪声 */}
            {last.cpu_steal_pct >= 0.1 && (
              <span
                className="kpi-sub"
                title="steal：虚拟机想要 CPU 但宿主机分给了其他虚拟机的时长占比。超售的 VPS 上该值持续非零"
              >
                STEAL {pct(last.cpu_steal_pct, 1)}
              </span>
            )}
          </span>
          <Spark series={series} valueOf={cpu} percent />
        </div>
        <div className={`kpi ${memTone}`}>
          <span className="kpi-l">内存</span>
          <span className="kpi-v">
            {memNow === null ? '—' : pct(memNow).replace('%', '')}
            {memNow !== null && <small>%</small>}
          </span>
          <Spark series={series} valueOf={mem} percent />
        </div>
        <div className={`kpi ${diskTone}`}>
          <span className="kpi-l">磁盘</span>
          <span className="kpi-v">
            {diskNow === null ? '—' : pct(diskNow).replace('%', '')}
            {diskNow !== null && <small>%</small>}
          </span>
          <Spark series={series} valueOf={disk} percent />
        </div>
        <div className="kpi">
          <span className="kpi-l">Load 1m</span>
          <span className="kpi-v">{last.load1.toFixed(2)}</span>
          <Spark series={series} valueOf={sample => sample.load1} />
        </div>
        <div className={`kpi ${last.uptime_secs < JUST_REBOOTED_SECS ? 'warn' : ''}`}>
          <span className="kpi-l">已运行</span>
          <span className="kpi-v">
            {upNum}
            <small>{upUnit}</small>
          </span>
        </div>
        <div className={`kpi ${ctRatio !== null && ctRatio >= CONNTRACK_WARN ? 'warn' : ''}`}>
          <span className="kpi-l">连接表</span>
          <span className="kpi-v">
            {ctRatio === null ? '—' : pct(ctRatio * 100, 1).replace('%', '')}
            {ctRatio !== null && <small>%</small>}
          </span>
        </div>
      </div>

      {/* 卡框沿用 .chart-card（即原 .load-network 的卡框语言），图区交给 echarts。 */}
      <section className="chart-card">
        <div className="load-network-cap">
          <b>NETWORK THROUGHPUT</b>
          <span className={stale ? 'stale' : undefined}>
            <Ago at={reportedAt === null ? null : iso(reportedAt)} />
            {stale ? ' · 数据不新了' : ' 上报'} · 30 秒 / 窗口
          </span>
        </div>
        <div className="load-network-legend">
          <span className="rx">
            <i />
            接收 <b>{bps(last.nic_rx_bps)}</b>
          </span>
          <span className="tx">
            <i />
            发送 <b>{bps(last.nic_tx_bps)}</b>
          </span>
          <span className="sp" />
          {drops > 0 && (
            <span
              className="hot"
              title={`最近 30 秒增量：接收丢弃 ${last.nic_rx_drop}，发送丢弃 ${last.nic_tx_drop}，网卡错误 ${last.nic_err}。接收丢弃可能包含 802.2/LLC 等二层控制帧，不等同于业务链路丢包；不是开机累计值`}
            >
              {[
                last.nic_rx_drop > 0 ? `接收丢弃 ${last.nic_rx_drop.toLocaleString()}` : null,
                last.nic_tx_drop > 0 ? `发送丢弃 ${last.nic_tx_drop.toLocaleString()}` : null,
                last.nic_err > 0 ? `网卡错误 ${last.nic_err.toLocaleString()}` : null,
              ]
                .filter(Boolean)
                .join(' · ')}
            </span>
          )}
          {host && (
            <span className="mono">
              {host.nic}
              {typeof host.nic_mtu === 'number' && ` · MTU ${host.nic_mtu}`}
            </span>
          )}
        </div>
        <ThroughputChart rx={rxData} tx={txData} rxName="接收" txName="发送" group={group} />
      </section>
    </>
  );
}

export function LoadCard({ report, xrayChart }: { report: NodeLoadView; xrayChart?: ReactNode }) {
  // 计时器需要在提前 return 之前获取（hook 不能有条件地跳过），也不能在渲染中直接调用
  // Date.now()：该调用不是纯函数，同一次渲染的两处会得到不同的值。
  const now = useNow();
  const [showProcs, setShowProcs] = useState(false);
  const findings = loadFindings(report);
  const reportedAt = report.reported_at_unix_secs;
  const stale = reportedAt !== null && now / 1000 - reportedAt > STALE_SECS;
  const head = (
    <header>
      <h4>LOAD</h4>
      <span className="sp" />
      <span className={stale ? 'hint stale' : 'hint'}>
        <Ago at={reportedAt === null ? null : iso(reportedAt)} />
        {stale ? ' · 数据不新了' : ' 上报'}
      </span>
    </header>
  );

  if (report.series.length === 0) {
    return (
      <div className="panel">
        {head}
        <p className="note">还没有负载读数。</p>
        <Findings list={findings} />
      </div>
    );
  }

  const last = report.series[report.series.length - 1];
  // 低频变量位于 host 中，不在每个窗口内。缺失只可能是某一轮上报被中断，下一轮即完整——
  // 因此此处大量使用 `?.`，而不用零参与比例计算（分母为零得出的百分比会触发一批误报）。
  const host = report.host;
  // 标量事实保留在趋势总览下方；磁盘已在上方同时显示占用率和剩余量，不在信息带重复。
  const ctMax = host?.conntrack_max ?? null;
  const ctRatio = last.conntrack_count !== null && ctMax !== null && ctMax > 0 ? last.conntrack_count / ctMax : null;

  // 进程中是否存在需要关注的项。存在时默认展开，不存在时折叠为一行——正常机器上该表的
  // 二十个单元格中有十一个是 0.0% 或 —，占用近半张卡而不提供有效信息。
  const hotProc = report.processes.find(p => p.fds !== null && p.fd_limit !== null && p.fds / p.fd_limit >= FD_WARN);
  const xray = report.processes.find(p => p.proc === 'xray');
  const open = showProcs || !!hotProc;

  return (
    <div className="load-cluster">
      <LoadDashboard report={report} reportedAt={reportedAt} stale={stale} />
      {/* XRAY 承载曲线紧跟 NETWORK 之后（两者时间轴一致、联动十字线）；数据来自 usage，
          由 nodes.tsx 组装后经 xrayChart 传入，本卡只负责把它放在正确的位置。 */}
      {xrayChart}

      {/* 没有时序意义的标量项集中在一条信息带内。 */}
      <div className="load-host-strip">
        <div className="tfacts">
          <span>
            连接表{' '}
            {ctRatio === null ? (
              // 未加载 nf_conntrack 不是故障，而是该机器未配置 NAT。显示「—」会被理解为读取失败。
              <b className="dim">没开</b>
            ) : (
              <>
                <b className={ctRatio >= CONNTRACK_WARN ? 'hot' : undefined}>{pct(ctRatio * 100, 1)}</b>
                <span className="dim"> · {(last.conntrack_count as number).toLocaleString()} 条</span>
              </>
            )}
          </span>
          <span>
            已运行 <b className={last.uptime_secs < JUST_REBOOTED_SECS ? 'hot' : undefined}>{dur(last.uptime_secs)}</b>
          </span>
          {host?.cc_algo && (
            <span>
              拥塞{' '}
              {/* 是否标为金色的判定必须与 congestionFindings 完全一致——纯 wg 中继上使用 cubic 不影响，
                  此处标为金色而下方不输出说明时，会导致查找不到对应的解释。 */}
              <b className={isBbr(host.cc_algo) || !hasXray(report) ? undefined : 'hot'}>{host.cc_algo}</b>
              <span className="dim"> · {host.nic_qdisc || '?'}</span>
            </span>
          )}
        </div>
        {/* ── 进程 ──
            区分机器整体负载高和本系统进程负载高，该区分决定后续是扩容还是排查本系统。
            正常时和主机事实共用一条信息带，不再额外制造一层卡片。 */}
        {!open && (
          <button type="button" className="tproc-fold" onClick={() => setShowProcs(true)}>
            <i className="lamp ok" />
            <span>
              {report.processes.filter(p => p.started_at_unix_secs !== null || p.proc === 'wg').length} 个进程都正常
            </span>
            {xray?.rss_bytes != null && (
              <span className="mono dim">
                xray {bytes(xray.rss_bytes)}
                {xray.fds !== null && ` · ${xray.fds} fd`}
              </span>
            )}
            <span className="sp" />
            <span className="dim">展开</span>
          </button>
        )}
        <Findings list={findings} />
      </div>

      {open && (
        <div className="load-process-panel">
          <table className="t tproc">
            <thead>
              <tr>
                <th>进程</th>
                <th className="d2">内存</th>
                <th className="d2">CPU</th>
                <th className="d2">fd</th>
                <th>起于</th>
              </tr>
            </thead>
            <tbody>
              {report.processes.map(p => (
                <tr key={p.proc}>
                  <td className="mono">{PROC_LABEL[p.proc]}</td>
                  <td className="d2 mono">
                    {p.rss_bytes === null ? <span className="dim">—</span> : bytes(p.rss_bytes)}
                  </td>
                  <td className="d2 mono">{p.cpu_pct === null ? <span className="dim">—</span> : pct(p.cpu_pct, 1)}</td>
                  <td className="d2 mono">
                    {p.fds === null ? (
                      <span className="dim">—</span>
                    ) : (
                      // 分母显示实际数值。`512k` 是由 524287 近似得到的整数，与 ulimit 中的值不符，
                      // 而调整该上限时需要的正是实际值。
                      <span className={p.fd_limit && p.fds / p.fd_limit >= FD_WARN ? 'hot' : undefined}>
                        {p.fds.toLocaleString()}
                        {p.fd_limit && <span className="dim"> / {p.fd_limit.toLocaleString()}</span>}
                      </span>
                    )}
                  </td>
                  <td className="d2">
                    {p.started_at_unix_secs === null ? (
                      <span className="dim">{p.proc === 'wg' ? '内核模块' : '没在跑'}</span>
                    ) : (
                      <span className="mono">{dur(Math.floor(now / 1000) - p.started_at_unix_secs)}前</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/* ── 判定：跳 ────────────────────────────────────────────────── */

/** 重传率。超过该值即不属于正常波动，而是线路存在丢包。 */
const RETRANS_WARN = 2;
/** 发送缓冲受限的时间占比。这是唯一一条可直接对应处理动作的判定。 */
const SNDBUF_WARN = 25;
/** 对端接收能力不足。该值高表示问题在下一跳的机器上，而非线路上。 */
const RWND_WARN = 30;
/** 瓶颈跳的判定：低于同链中次低的那一跳达到该倍数才判定为瓶颈。
    不设倍数阈值时，五跳中必有一跳最低，每条链都会标记瓶颈，该标记即失去意义。 */
const BOTTLENECK_RATIO = 0.6;

export function hopFindings(view: HopLinkView, sameChain: HopLinkView[]): Finding[] {
  // 拥塞算法位于 view 上（它是机器的属性），其余位于 sample 上（它们是该窗口的属性）。
  const h = view.sample;
  const out: Finding[] = [];

  // 没有任何连接：不是故障，而是该链当前无人使用。
  // 必须与无法测量区分，否则空闲状态会被理解为故障。
  if (h.conns === 0) {
    return [{ tone: 'warn', chip: '没有连接', detailOnly: true, text: <>这一跳当前没有连接。</> }];
  }

  if (!isBbr(view.cc_algo)) {
    out.push({
      tone: 'warn',
      chip: '量不出带宽',
      // 带宽字段已显示算法名称，该行只补充其未表达的部分：带宽为空的原因。
      text: <>{view.cc_algo || '这个算法'} 不维护带宽估计，RTT 和重传不受影响。</>,
    });
  }

  // 瓶颈跳。需要与同链的其他跳比较才有意义——82 Mb/s 本身不表示问题，
  // 同链其他跳都在 300 以上而该跳只有 82 才表示问题。
  // 同链的其他跳按 peer 区分——一条链上不会有两跳指向同一台机器。
  const others = sameChain.filter(o => o.sample.peer_node_id !== h.peer_node_id && o.sample.btlbw_p50_bps !== null);
  if (h.btlbw_p50_bps !== null && others.length > 0) {
    const nextLowest = Math.min(...others.map(o => o.sample.btlbw_p50_bps as number));
    if (h.btlbw_p50_bps < nextLowest * BOTTLENECK_RATIO) {
      out.push({
        tone: 'warn',
        chip: `瓶颈 ${bps(h.btlbw_p50_bps)}`,
        // 与同链其他跳的比较结果是该行的唯一信息——单独的 82 Mb/s 不表示任何问题。
        text: (
          <>
            这条链的瓶颈：<b>{bps(h.btlbw_p50_bps)}</b>，次低的一跳 {bps(nextLowest)}。
          </>
        ),
      });
    }
  }

  if (h.retrans_pct >= RETRANS_WARN) {
    out.push({
      tone: 'bad',
      chip: `重传 ${pct(h.retrans_pct, 1)}`,
      text: (
        <>
          重传 <b>{pct(h.retrans_pct, 1)}</b>。
        </>
      ),
    });
  }

  if (h.sndbuf_limited_pct >= SNDBUF_WARN) {
    out.push({
      tone: 'warn',
      chip: '发送缓冲不够',
      text: (
        <>
          <b>{pct(h.sndbuf_limited_pct)}</b> 的时间卡在发送缓冲上，调 <code>net.core.wmem_max</code> /{' '}
          <code>net.ipv4.tcp_wmem</code>。
        </>
      ),
    });
  }

  if (h.rwnd_limited_pct >= RWND_WARN) {
    out.push({
      tone: 'warn',
      chip: '对端收不动',
      // 需要说明问题位于下一跳：该判定显示在 A→B 这一行上，而需要处理的是 B。
      text: (
        <>
          <b>{pct(h.rwnd_limited_pct)}</b> 的时间等对端接收窗口——要看的是下一跳那台。
        </>
      ),
    });
  }

  return out;
}

/* ── 逐跳链路表 ──────────────────────────────────────────────── */

/** 各跳的线路质量。
 *
 * 数据不需要额外采集：BBR 在每条实际转发连接上、每个 RTT 都会更新其对瓶颈带宽和
 * 最小 RTT 的估计，读取 netlink 即可获取，不发送额外的探测包。
 * 现有的 link_probes（ICMP 测量 MTU，30 分钟一次）和 e2e（TTFB，5 分钟一次）都是主动探测——
 * 频率较低，且探测流量不代表实际流量。
 *
 * 该表回答此前无法回答的三个问题：链路的瓶颈位于哪一跳、速度下降是本端还是线路导致、
 * 应调整哪台机器的缓冲区。 */
export function HopLinkTable({ hops, nodeName }: { hops: HopLinkView[]; nodeName: (id: string) => string }) {
  const byChain = new Map<string, HopLinkView[]>();
  for (const v of hops) {
    const list = byChain.get(v.sample.chain_id) ?? [];
    list.push(v);
    byChain.set(v.sample.chain_id, list);
  }
  if (hops.length === 0) return null;

  /** 一行的键。不使用 hop_label——它是入站标识，与此处测量的出站方向不对应（论证见
      protocol.rs），因此键由起点和终点拼接而成。 */
  const rowKey = (v: HopLinkView) => `${v.node_id}>${v.sample.chain_id}>${v.sample.peer_node_id}`;

  return (
    <div className="panel">
      <header>
        <h4>LINK QUALITY</h4>
        <span className="sp" />
        <span className="hint">来自真实转发流量，不额外发起探测</span>
      </header>
      <table className="t thop">
        <thead>
          <tr>
            <th>跳</th>
            {/* 显示为可测数而非连接数：可用于估算带宽的只有非 app_limited 的样本，
                两个数值都显示是因为其差值本身携带信息——
                差值大表示该跳的多数连接处于空闲状态 */}
            <th className="d2" title="可测 / 总数。只有 delivery_rate 不带 app_limited 标志的连接才算得出带宽">
              连接
            </th>
            <th className="d2" title="BBR 的瓶颈带宽估计（p50 / p90）">
              带宽
            </th>
            <th className="d2" title="min_rtt 取全体连接的最小值——传播时延本就该是最小的那个">
              RTT
            </th>
            <th className="d2">重传</th>
            <th title="拥塞受限 / 对端收不动 / 我方发送缓冲不够">卡在哪</th>
          </tr>
        </thead>
        <tbody>
          {[...byChain.entries()].map(([chainId, list]) => (
            <>
              <tr key={chainId} className="thop-chain">
                <td colSpan={6}>
                  <span className="mono dim">{chainId}</span>
                </td>
              </tr>
              {list.map(v => {
                const h = v.sample;
                const findings = hopFindings(v, list);
                const worst = findings.find(f => f.tone === 'bad') ?? findings[0];
                return (
                  <tr key={rowKey(v)} className={worst?.tone === 'bad' ? 'thop-bad' : undefined}>
                    <td>
                      <span className="thop-arrow">
                        {nodeName(v.node_id)}
                        <i>→</i>
                        {nodeName(h.peer_node_id)}
                      </span>
                      {/* 机器 id 位于名称下方：日常识别依据名称，而检索日志时需要 id */}
                      <span className="dim mono thop-label">
                        {v.node_id} → {h.peer_node_id}
                      </span>
                    </td>
                    <td className="d2 mono">
                      {h.conns === 0 ? (
                        <span className="dim">0</span>
                      ) : (
                        <>
                          {h.conns_measured}
                          <span className="dim">/{h.conns}</span>
                        </>
                      )}
                    </td>
                    <td className="d2">
                      {h.btlbw_p50_bps === null ? (
                        <span className="dim" title={h.conns === 0 ? '没有连接' : `${v.cc_algo} 不维护带宽估计`}>
                          {h.conns === 0 ? '—' : v.cc_algo || '?'}
                        </span>
                      ) : (
                        // 只有单个数值，不绘制火花线：读取端点每跳只返回最新一个窗口
                        // （store 的 DISTINCT ON）。绘制趋势需要先有返回时序数据的端点，
                        // 用单个点绘制曲线会呈现出不存在的历史数据。
                        <span className="mono" title={`p90 ${bps(h.btlbw_p90_bps)}`}>
                          {bps(h.btlbw_p50_bps)}
                        </span>
                      )}
                    </td>
                    <td className="d2 mono">
                      {h.conns === 0 ? (
                        <span className="dim">—</span>
                      ) : (
                        <>
                          {ms(h.min_rtt_us)}
                          {/* p90 只在与 min 差距较大时显示：差距大表示存在排队，
                              而排队时延与传播时延是两项不同的指标 */}
                          {h.rtt_p90_us > h.min_rtt_us * 1.6 && (
                            <span className="dim" title="p90，跟 min 差得远说明路上有排队">
                              {' '}
                              ~{ms(h.rtt_p90_us)}
                            </span>
                          )}
                        </>
                      )}
                    </td>
                    <td className="d2 mono">
                      {h.conns === 0 ? (
                        <span className="dim">—</span>
                      ) : (
                        <span className={h.retrans_pct >= RETRANS_WARN ? 'hot' : undefined}>
                          {pct(h.retrans_pct, 2)}
                        </span>
                      )}
                    </td>
                    <td>
                      {h.conns === 0 ? (
                        <span className="dim">—</span>
                      ) : (
                        <Meter
                          parts={[
                            { cls: 'm1', v: h.busy_pct, label: '拥塞受限' },
                            { cls: 'm2', v: h.rwnd_limited_pct, label: '对端收不动' },
                            { cls: 'm3', v: h.sndbuf_limited_pct, label: '发送缓冲不够' },
                          ]}
                        />
                      )}
                    </td>
                  </tr>
                );
              })}
            </>
          ))}
        </tbody>
      </table>
      {/* 判定显示在表格下方而非行内：行内无法容纳完整的说明，
          而这些判定的作用在于其说明内容——只显示一个标记则无法确定其含义 */}
      {[...byChain.values()].flatMap(list =>
        list.flatMap(v =>
          hopFindings(v, list)
            .filter(f => !f.detailOnly)
            .map((f, i) => (
              <p key={`${rowKey(v)}-${i}`} className={`rt-dx${f.tone === 'bad' ? ' bad' : ''}`}>
                <b className="thop-who">
                  {nodeName(v.node_id)}→{nodeName(v.sample.peer_node_id)}
                </b>{' '}
                {f.text}
              </p>
            )),
        ),
      )}
    </div>
  );
}

/* ── 共用 ───────────────────────────────────────────────────── */

function Findings({ list }: { list: Finding[] }) {
  // chipOnly 的条目只在列表中显示标记，详情页不显示——卡片中的字段已表达该信息。
  const shown = list.filter(f => !f.chipOnly);
  if (shown.length === 0) return null;
  return (
    <>
      {shown.map((f, i) => (
        <p key={i} className={`rt-dx${f.tone === 'bad' ? ' bad' : ''}`}>
          {f.text}
        </p>
      ))}
    </>
  );
}

/** 列表中的标记。正常的行不显示额外内容——该规则在 nodes.tsx 中确立，
 *  常驻的「负载正常」标记会增加每行的宽度而不提供信息。 */
export function LoadChips({ report }: { report: NodeLoadView }) {
  const chips = loadFindings(report).filter(f => !f.detailOnly);
  if (chips.length === 0) return null;
  return (
    <span className="node-artifacts">
      {chips.map(f => (
        <span key={f.chip} className={`node-artifact rt ${f.tone}`} title={f.chip}>
          {f.chip}
        </span>
      ))}
    </span>
  );
}
