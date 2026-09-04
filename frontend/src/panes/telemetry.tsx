/** 遥测：LOAD 卡、逐跳链路表，以及它们的判定逻辑。
 *
 * 沿用两条既有规则（均记录在 nodes.tsx 中）：
 *   列表只表示哪台机器存在问题，详情说明具体问题——因此正常的行不显示额外内容
 *   标记写明后果而非字段名——「磁盘剩余 3%，用量记录将开始丢失」优于「disk_free_pct 3」
 *
 * 时间统一使用 unix 秒（服务端两侧共用同一组结构体，见 api.ts 的说明），因此传给
 * `Ago` 之前需要转换为 ISO —— `iso()` 即用于该转换。
 */
import { memo, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import * as echarts from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { GridComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import { bytes } from '../ui/format';
import { Ago } from '../ui/bits';
import {
  OBSERVE_SERIES_COLOR_VARS,
  observeAreaStyle,
  observeAxisLine,
  observeAxisTick,
  observeBpsReading,
  observeBpsUnit,
  observeBytesUnit,
  observeColors,
  observeCountUnit,
  observeMsUnit,
  observeMinorTick,
  observeNumberUnit,
  observeSeriesLine,
  observeTimeInterval,
  observeValueAxis,
} from '../ui/observe-chart';
import type { ObserveAxisUnit, ObserveValueAxis } from '../ui/observe-chart';
import { theme } from '../forge/theme';
import { palette } from '../forge/palette';
import type {
  CpuDetailSample,
  DiskDetailSample,
  HopLinkView,
  LoadSample,
  MemoryDetailSample,
  NetworkDetailSample,
  NodeLoadView,
} from '../api';

// 与 nodes.tsx 的 FleetNetChart 共用同一套注册；echarts.use 对重复注册幂等，
// 但本模块独立使用 echarts，需要自己声明所依赖的组件。
echarts.use([LineChart, GridComponent, MarkLineComponent, TooltipComponent, CanvasRenderer]);

/** canvas 里字体要给具体栈，不能写 var(--mono)。 */
const TP_MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';

/** unix 秒转 ISO。`Ago` 接受 ISO 字符串，而协议中统一使用秒。nodes.tsx 的 HOST 卡复用 */
export const iso = (secs: number) => new Date(secs * 1000).toISOString();

/* ── 格式化 ──────────────────────────────────────────────────
 * 带宽使用 1000 进制（Mbit/s 是网络领域的常用单位），字节使用 ui/format 的 1024 进制。
 * 两套单位并存不是疏漏：将 82 Mbit/s 显示为 78.2 Mibit/s 不符合惯例，
 * 将 184 MB 的 RSS 显示为 193 MB 又与 top 的输出不一致。
 */

/* 孤立读数：KPI 芯片、本月合计、机队汇总——身后没有坐标轴，自己定档。图上的读数一律改走
   observeBpsUnit(...).read，跟本卡的轴同一个单位，见那里的说明。
   精度靠有效数字不靠换单位：原先 ≥1e6 就取整到 Mbit/s，3.42 Mbit/s 显示成「3 Mbit/s」抹掉 13%，
   1 到 10 Mbit/s 之间每个读数都吃这个量级的误差。三位有效数字与 pingLatencyText 同一套规则。 */
export const bps = (n: number | null): string => (n === null ? '—' : observeBpsReading(n));

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
  /** 这条判定归属的可展开指标。KPI 与页签角标必须消费同一条 finding，不能各自再算一遍。 */
  section?: 'cpu' | 'memory' | 'disk';
};

/* ── LOAD 趋势图 ─────────────────────────────────────────────
 * 60 个窗口直接来自现有 node-load 接口。横轴固定为最近 30 分钟；样本不足时右对齐，
 * `has_gap` 时切断路径，不把机器重启前后的两个值连成一条不存在的趋势。
 */

/** 节点详情保留 60 个 30 秒窗口：完整覆盖最近 30 分钟。 */
const LOAD_SLOTS = 60;
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
  /* 上报缺口是否断段。计数类指标缺口表示窗口不可信，必须断；uptime 这类读数
     缺口行仍是真实采样（agent 的 has_gap 只标记窗口不完整），断开反而谎报了一次重启。 */
  bridgeGaps = false,
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
    if ((!bridgeGaps && sample.has_gap) || value === null || !Number.isFinite(value)) {
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
const CPU_PSI_WARN = 1;
const CPU_STEAL_WARN = 5;
const IO_PSI_WARN = 1;
/** 可用内存低于该比例。使用 available 而非 free——free 在有 page cache 的机器上
    始终很小，按其判定会将所有正常机器报为内存不足。 */
const MEM_AVAIL_WARN = 0.12;
const MEM_PSI_WARN = 1;
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

type SampleMaximum = { sample: LoadSample; value: number };

/** 返回有效窗口中的最大值及其发生窗口。只返回数字会导致详情只能展示最新窗口，
 * 页签角标却按历史最大值计数，最终出现“有 1 个问题但找不到是什么”的断裂。 */
function sampleMaximum(series: LoadSample[], valueOf: (sample: LoadSample) => number): SampleMaximum | null {
  let maximum: SampleMaximum | null = null;
  for (const sample of series) {
    if (sample.has_gap) continue;
    const value = valueOf(sample);
    if (!maximum || value > maximum.value) maximum = { sample, value };
  }
  return maximum;
}

/** 后端默认返回 24 个窗口，但刚纳管或存在保留边界时可能不足 24 个；显示真实跨度，
 * 不把不足 12 分钟的数据硬写成“近 12 分钟”。 */
function findingWindow(series: LoadSample[]): string {
  const first = series[0];
  const last = series[series.length - 1];
  return `近 ${dur(Math.max(1, last.window_end_unix_secs - first.window_start_unix_secs))}`;
}

/** 历史窗口触发、最新窗口已经恢复时，字段本身已无法解释页签角标，必须留下可见说明。 */
function rollingFinding(
  finding: Omit<Finding, 'chipOnly' | 'text'>,
  maximum: SampleMaximum,
  latest: LoadSample,
  window: string,
): Finding {
  const historical = maximum.sample.window_end_unix_secs < latest.window_end_unix_secs;
  return {
    ...finding,
    chipOnly: !historical,
    text: historical ? (
      <>
        {window}内 <b>{finding.chip}</b>，<Ago at={iso(maximum.sample.window_end_unix_secs)} />
        达到阈值；{sampleWindow(latest)}已恢复。
      </>
    ) : null,
  };
}

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
  const window = findingWindow(r.series);

  // CPU。峰值和 softirq 分为两条，因为二者对应不同的处理方式。
  const peak = sampleMaximum(r.series, sample => sample.cpu_peak_pct);
  if (peak && peak.value >= CPU_PEAK_WARN) {
    out.push(
      rollingFinding(
        {
          tone: 'warn',
          chip: `CPU 峰值 ${pct(peak.value)}`,
          section: 'cpu',
        },
        peak,
        last,
        window,
      ),
    );
  }
  const softirq = sampleMaximum(r.series, sample => sample.cpu_softirq_pct);
  if (softirq && softirq.value >= SOFTIRQ_WARN) {
    out.push(
      rollingFinding(
        {
          tone: 'warn',
          chip: `软中断 ${pct(softirq.value)}`,
          section: 'cpu',
        },
        softirq,
        last,
        window,
      ),
    );
  }
  const steal = sampleMaximum(r.series, sample => sample.cpu_steal_pct);
  if (steal && steal.value >= CPU_STEAL_WARN) {
    out.push(
      rollingFinding(
        {
          tone: steal.value >= 15 ? 'bad' : 'warn',
          chip: `宿主争抢 ${pct(steal.value, 1)}`,
          section: 'cpu',
        },
        steal,
        last,
        window,
      ),
    );
  }
  const cpuPsi = sampleMaximum(r.series, sample => sample.cpu_detail?.pressure_some_pct ?? 0);
  if (cpuPsi && cpuPsi.value >= CPU_PSI_WARN) {
    out.push(
      rollingFinding(
        {
          tone: cpuPsi.value >= 10 ? 'bad' : 'warn',
          chip: `CPU 等待 ${pct(cpuPsi.value, 1)}`,
          section: 'cpu',
        },
        cpuPsi,
        last,
        window,
      ),
    );
  }
  const ioFull = sampleMaximum(r.series, sample => sample.cpu_detail?.io_pressure_full_pct ?? 0);
  const ioSome = sampleMaximum(r.series, sample => sample.cpu_detail?.io_pressure_some_pct ?? 0);
  if (ioFull && ioFull.value >= IO_PSI_WARN) {
    out.push(
      rollingFinding({ tone: 'bad', chip: `I/O 抖动 ${pct(ioFull.value, 1)}`, section: 'cpu' }, ioFull, last, window),
    );
  } else if (ioSome && ioSome.value >= IO_PSI_WARN) {
    out.push(
      rollingFinding({ tone: 'warn', chip: `I/O 等待 ${pct(ioSome.value, 1)}`, section: 'cpu' }, ioSome, last, window),
    );
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
      section: 'memory',
    });
  }
  const memFull = sampleMaximum(r.series, sample => sample.memory_detail?.pressure_full_pct ?? 0);
  const memSome = sampleMaximum(r.series, sample => sample.memory_detail?.pressure_some_pct ?? 0);
  if (memFull && memFull.value >= MEM_PSI_WARN) {
    out.push(
      rollingFinding(
        { tone: 'bad', chip: `内存抖动 ${pct(memFull.value, 1)}`, section: 'memory' },
        memFull,
        last,
        window,
      ),
    );
  } else if (memSome && memSome.value >= MEM_PSI_WARN) {
    out.push(
      rollingFinding(
        { tone: 'warn', chip: `内存等待 ${pct(memSome.value, 1)}`, section: 'memory' },
        memSome,
        last,
        window,
      ),
    );
  }
  const swapOut = r.series.reduce((sum, sample) => sum + (sample.memory_detail?.swap_out_bytes ?? 0), 0);
  if (swapOut > 0) {
    out.push({ tone: 'warn', chip: `换出 ${bytes(swapOut)}`, chipOnly: true, text: null });
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
      section: 'disk',
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

/** 该机器的当前负载。
 *
 * 位于观测主栏首位：监控页先回答当前资源是否充足，再向下解释 agent、运行时和配置状态。
 * 卡片底部的进程表是主要内容而非附加信息——只有该部分能区分机器整体负载高和本系统
 * 进程负载高，而机器出现问题时首先会怀疑本系统部署的这几个进程。 */
/** 双序列吞吐曲线（echarts）。NETWORK（网卡，按方向）与 XRAY（承载，按角色）共用这一个组件：
 * 两者口径不同、绝不能合并进一张图。用户打开观测页的「同组图表联动」后才传入 group，
 * 用 `echarts.connect(group)` 同步十字线与 Tooltip；默认各图独立。
 *
 * 细节约定（与现网手绘 SVG 一致，逐项对应）：
 * - Y 量程使用 1 / 2 / 2.5 / 5 × 10ⁿ 标准步长，顶部永远保留峰值之后的一整格。
 * - 两条线使用观测分类盘的前两色，与 HistoryChart 和 TCP Ping 保持同一视觉语法。
 * - 两条线都使用「淡化 + 正常 + 中」填充；填充不是流向主次，不改变数据口径。
 * - 缺口（has_gap / null）断开不连接。 */

/** 吞吐值轴的量程与单位。刻度由 ThroughputChart 画，单位由 ThroughputPanel 写在标题栏里
 *  （`网卡流量 (Mbit/s)`），两处都走这个函数取值——峰值只算一遍，量纲不可能对不上。 */
export function throughputAxis(
  rx: (number | null)[],
  tx: (number | null)[],
): { axis: ObserveValueAxis; unit: ObserveAxisUnit } {
  const peak = Math.max(
    ...rx.filter((value): value is number => value !== null),
    ...tx.filter((value): value is number => value !== null),
    1,
  );
  const axis = observeValueAxis(peak);
  return { axis, unit: observeBpsUnit(axis) };
}

export function ThroughputChart({
  timesUnixSecs,
  rangeStartUnixSecs,
  rangeEndUnixSecs,
  rx,
  tx,
  rxName,
  txName,
  group,
}: {
  /** 每个速率点对应的真实窗口结束时间。 */
  timesUnixSecs: number[];
  /** 前端请求并由服务端回显的绝对区间。 */
  rangeStartUnixSecs: number;
  rangeEndUnixSecs: number;
  /** null 表示缺口，折线在此断开。 */
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
    const sig = JSON.stringify([
      timesUnixSecs,
      rangeStartUnixSecs,
      rangeEndUnixSecs,
      rx,
      tx,
      rxName,
      txName,
      group,
      themeName,
      paletteKey,
    ]);
    if (sig === lastSig.current) return;
    lastSig.current = sig;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const colors = observeColors(themeName, cv).slice(0, 2);
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');

    const { axis: valueAxis, unit } = throughputAxis(rx, tx);
    const xMin = rangeStartUnixSecs * 1000;
    const xMax = rangeEndUnixSecs * 1000;
    // x 轴显示墙钟时刻（hh:mm，与全机队镜像图及此前的 mockup 一致），tooltip 到秒。
    const hm = (ms: number) =>
      new Date(ms).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
    const hms = (ms: number) =>
      new Date(ms).toLocaleTimeString('zh-CN', {
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
        hour12: false,
      });
    const xStep = observeTimeInterval(Math.max(30_000, xMax - xMin));

    const mk = (name: string, data: (number | null)[], color: string) => ({
      name,
      type: 'line' as const,
      symbol: 'circle',
      symbolSize: 5,
      showSymbol: false,
      smooth: false,
      connectNulls: false,
      lineStyle: observeSeriesLine(color),
      areaStyle: observeAreaStyle(color, themeName, { count: 2 }),
      itemStyle: { color, borderColor: glass, borderWidth: 1.5 },
      emphasis: { disabled: true },
      data: data.map((value, index) => [timesUnixSecs[index] * 1000, value] as [number, number | null]),
    });

    chart.setOption(
      {
        animation: false,
        color: colors,
        grid: { left: 10, right: 14, top: 10, bottom: 10, containLabel: true },
        textStyle: { fontFamily: TP_MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: TP_MONO },
          extraCssText: 'border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.18); backdrop-filter:blur(8px);',
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' }, z: 0 },
          formatter: (params: unknown) => {
            const arr = params as { seriesName: string; color: string; value: [number, number | null] }[];
            const head = hms(arr[0].value[0]);
            const row = (p: { seriesName: string; color: string; value: [number, number | null] }) =>
              `<div style="display:flex;gap:7px;align-items:center;line-height:1.75">` +
              `<span style="width:8px;height:8px;border-radius:2px;background:${p.color};flex:none"></span>` +
              `<span style="color:${ink3}">${p.seriesName}</span>` +
              `<b style="margin-left:auto;color:${ink};font-weight:500">${p.value[1] === null ? '—' : unit.read(p.value[1])}</b></div>`;
            const rows = [...arr].sort(
              (left, right) =>
                (right.value[1] ?? Number.NEGATIVE_INFINITY) - (left.value[1] ?? Number.NEGATIVE_INFINITY),
            );
            return `<div style="color:${ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${head}</div>${rows.map(row).join('')}`;
          },
        },
        xAxis: {
          // 数值轴而非 time 轴：x 是毫秒时间戳，等比排布即时间轴，但 echarts 6 的 time 轴
          // 无视 interval/minInterval（实测固定 2 分钟一格 → 15 条网格），数值轴才认 interval。
          type: 'value',
          min: xMin,
          max: xMax,
          interval: xStep,
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            hideOverlap: true,
            formatter: (value: number) => hm(value),
          },
        },
        yAxis: {
          type: 'value',
          min: 0,
          max: valueAxis.max,
          interval: valueAxis.interval,
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          // 刻度只写数字，单位由 ThroughputPanel 写在标题栏（`网卡流量 (Mbit/s)`）。原先每格
          // 各调一次 bps()，同一根轴上「1.00 Gbit/s」与「500 Mbit/s」并存，标签列宽在 3 到 9 字
          // 之间跳，containLabel 还按最长那条留白。tooltip 与图例仍用 bps()：那是读数不是量程。
          axisLabel: { color: ink3, fontSize: 9.5, margin: 8, formatter: unit.text },
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        series: [mk(rxName, rx, colors[0]), mk(txName, tx, colors[1])],
      },
      true,
    );
    // connect 按组联动所有已建实例；任一图重设 option 后重连一次，保证最新成员都在组内。
    if (group) echarts.connect(group);
  }, [timesUnixSecs, rangeStartUnixSecs, rangeEndUnixSecs, rx, tx, rxName, txName, group, themeName, paletteKey]);

  return <div ref={elRef} className="ndtp-ec" />;
}

type HistoryLine = {
  name: string;
  values: (number | null)[];
  /** Stable semantic color role. HistoryChart maps it into the categorical observation palette. */
  color?: string;
  /** Lines sharing a stack become a filled composition instead of crossing independent traces. */
  stack?: string;
  area?: boolean;
  dashed?: boolean;
};

const HISTORY_COLOR_SLOT: Record<string, number> = {
  '--observe-user': 0,
  '--observe-system': 1,
  '--observe-softirq': 2,
  '--observe-iowait': 3,
  '--observe-steal': 4,
  '--observe-load': 5,
  '--observe-peak': 5,
  '--observe-free': 9,
};

const historyLineColorVar = (line: HistoryLine, index: number) => {
  const slot = line.color === undefined ? index : (HISTORY_COLOR_SLOT[line.color] ?? index);
  return OBSERVE_SERIES_COLOR_VARS[slot % OBSERVE_SERIES_COLOR_VARS.length];
};

type HistoryChartVariant = 'main' | 'secondary' | 'diagnostic';

type HistoryUnit = (axis: ObserveValueAxis) => ObserveAxisUnit;
type HistoryHeaderReading = ReactNode | ((read: (value: number) => string) => ReactNode);
type HistoryFormatProps =
  { formatValue: (value: number) => string; valueUnit?: never } | { formatValue?: never; valueUnit: HistoryUnit };

const historyBytesUnit: HistoryUnit = observeBytesUnit;
const historyByteRateUnit: HistoryUnit = axis => observeBytesUnit(axis, '/s');
const historyCountUnit: HistoryUnit = observeCountUnit;
const historyCountRateUnit: HistoryUnit = axis => observeCountUnit(axis, '/s');
const historyMsUnit: HistoryUnit = axis => observeMsUnit(axis.interval);
const historyPerSecondUnit: HistoryUnit = axis => observeNumberUnit(axis, '/s');

function historyObservedPeak(lines: HistoryLine[], threshold?: { value: number }): number {
  const stackTotals = new Map<string, number[]>();
  let observedPeak = threshold?.value ?? 0;
  for (const lineSeries of lines) {
    if (lineSeries.stack) {
      const totals = stackTotals.get(lineSeries.stack) ?? Array.from({ length: lineSeries.values.length }, () => 0);
      lineSeries.values.forEach((value, index) => {
        if (value !== null) totals[index] += Math.max(0, value);
      });
      stackTotals.set(lineSeries.stack, totals);
      observedPeak = Math.max(observedPeak, ...totals);
    } else {
      observedPeak = lineSeries.values.reduce<number>(
        (valuePeak, value) => (value === null ? valuePeak : Math.max(valuePeak, value)),
        observedPeak,
      );
    }
  }
  return observedPeak;
}

/** 原始时序曲线。这里不放阈值、状态词或自动结论，只负责把服务端保留的窗口逐点画出。
 * null 与 has_gap 都断线，不用 0 填补；tooltip 显示窗口结束的绝对时间和原始数值。 */
function HistoryChart({
  title,
  samples,
  lines,
  formatValue,
  valueUnit,
  group,
  variant = 'diagnostic',
  current,
  meta,
  max,
  threshold,
  wide = false,
}: {
  title: string;
  samples: LoadSample[];
  lines: HistoryLine[];
  group?: string;
  variant?: HistoryChartVariant;
  current?: HistoryHeaderReading;
  meta?: HistoryHeaderReading;
  max?: number;
  threshold?: { value: number; label: string };
  wide?: boolean;
} & HistoryFormatProps) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const lastSig = useRef<string | null>(null);
  const observedPeak = historyObservedPeak(lines, threshold);
  const valueAxis = max === undefined ? observeValueAxis(observedPeak) : null;
  // 显式 max 的容量/连接图仍需要一个完整轴描述来选整卡单位；这只决定显示档位，实际轴上界
  // 继续服从调用方传入的 max，不额外抬高容量上限。
  const unitAxis = valueAxis ?? observeValueAxis(max ?? observedPeak);
  const unit = valueUnit?.(unitAxis);
  const axisText = unit?.text ?? formatValue!;
  const readValue = unit?.read ?? formatValue!;

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
    const times = samples.map(sample => sample.window_end_unix_secs);
    const sig = JSON.stringify([times, lines, title, max, threshold, group, unit?.name, themeName, paletteKey]);
    if (sig === lastSig.current) return;
    lastSig.current = sig;
    const css = getComputedStyle(elRef.current ?? document.documentElement);
    const cv = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
    const paletteColors = observeColors(themeName, name => cv(name, ''));
    const colors = lines.map((line, index) => {
      const variable = historyLineColorVar(line, index);
      return paletteColors[OBSERVE_SERIES_COLOR_VARS.indexOf(variable)];
    });
    const ink = cv('--ink', '#20242a');
    const ink3 = cv('--ink-3', '#707780');
    const ink4 = cv('--ink-4', '#9298a1');
    const line = cv('--line', '#d9dde3');
    const lineSoft = cv('--line-soft', '#edf0f3');
    const glass = cv('--glass-strong', '#fff');
    // 只有未堆叠的线会各自填到零线、在底部彼此重叠；填充的墨量按这个数分摊，堆叠段不计。
    const overlapCount = Math.max(1, lines.filter(lineSeries => !lineSeries.stack).length);
    const hasStack = lines.some(lineSeries => lineSeries.stack);
    /** 堆叠段互不重叠，取平涂；纯曲线图的各条都填到零线，取渐变并按条数分摊墨量；
     *  混合图里那条未堆叠的线是画在成分之上的包络（CPU 窗口峰值），填色会盖住成分，只画线。 */
    const areaFill = (lineSeries: HistoryLine, index: number) => {
      if (lineSeries.stack) return observeAreaStyle(colors[index], themeName, { stacked: true });
      if (hasStack) return undefined;
      return observeAreaStyle(colors[index], themeName, { count: overlapCount });
    };
    const lastMs = (times[times.length - 1] ?? 0) * 1000;
    const firstMs = (times[0] ?? 0) * 1000;
    // x 轴显示墙钟时刻（hh:mm），tooltip 到秒；轴起点即首个样本时刻，曲线紧贴 y 轴。
    // 见 ThroughputChart 同处说明：不再向下取整到整分，以免首点秒数变成左端留白。
    const xStep = observeTimeInterval(lastMs - firstMs);
    const xMin = Math.min(firstMs, lastMs - 30_000);
    const hm = (ms: number) =>
      new Date(ms).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false });
    const clockAt = (ms: number) =>
      new Date(ms).toLocaleTimeString('zh-CN', {
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
        hour12: false,
      });
    chart.setOption(
      {
        animation: false,
        color: colors,
        grid: {
          left: 10,
          right: 14,
          top: 12,
          bottom: 10,
          containLabel: true,
        },
        textStyle: { fontFamily: TP_MONO },
        legend: { show: false },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: TP_MONO },
          extraCssText: 'border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.18); backdrop-filter:blur(8px);',
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' }, z: 0 },
          formatter: (params: unknown) => {
            const rows = params as { seriesName: string; color: string; value: [number, number | null] }[];
            const body = [...rows]
              .sort(
                (left, right) =>
                  (right.value[1] ?? Number.NEGATIVE_INFINITY) - (left.value[1] ?? Number.NEGATIVE_INFINITY),
              )
              .map(row => {
                const value = row.value[1] === null ? '—' : readValue(Number(row.value[1]));
                return (
                  `<div style="display:flex;gap:7px;align-items:center;line-height:1.75">` +
                  `<span style="width:8px;height:8px;border-radius:2px;background:${row.color};flex:none"></span>` +
                  `<span style="color:${ink3}">${row.seriesName}</span>` +
                  `<b style="margin-left:auto;color:${ink};font-weight:500">${value}</b></div>`
                );
              })
              .join('');
            return `<div style="color:${ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${clockAt(rows[0]?.value?.[0] ?? lastMs)}</div>${body}`;
          },
        },
        xAxis: {
          // 数值轴承载毫秒时间戳（见 ThroughputChart 同处说明）：echarts 6 的 time 轴不认
          // interval，数值轴才能把主网格钉在稀疏的整分位置，同时保留次刻度。
          type: 'value',
          min: xMin,
          max: lastMs,
          interval: xStep,
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            hideOverlap: true,
            formatter: (value: number) => hm(value),
          },
        },
        yAxis: {
          type: 'value',
          min: 0,
          max: max ?? valueAxis?.max,
          interval: max === undefined ? valueAxis?.interval : undefined,
          scale: true,
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          axisLabel: { color: ink3, fontSize: 9.5, margin: 8, formatter: axisText },
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        series: lines.map((lineSeries, index) => ({
          name: lineSeries.name,
          type: 'line' as const,
          symbol: 'circle',
          symbolSize: 5,
          showSymbol: false,
          smooth: false,
          connectNulls: false,
          stack: lineSeries.stack,
          stackStrategy: lineSeries.stack ? ('all' as const) : undefined,
          areaStyle: areaFill(lineSeries, index),
          lineStyle: {
            ...observeSeriesLine(colors[index]),
            type: lineSeries.dashed ? ('dashed' as const) : ('solid' as const),
            opacity: 1,
          },
          itemStyle: { color: colors[index], borderColor: glass, borderWidth: 1.5 },
          emphasis: { disabled: true },
          data: lineSeries.values.map(
            (value, valueIndex) => [(times[valueIndex] ?? 0) * 1000, value] as [number, number | null],
          ),
          markLine:
            index === 0 && threshold
              ? {
                  silent: true,
                  symbol: ['none', 'none'],
                  label: {
                    show: true,
                    position: 'insideEndTop',
                    formatter: threshold.label,
                    color: cv('--err', '#bd4b4b'),
                    fontSize: 8,
                    fontFamily: TP_MONO,
                  },
                  lineStyle: { color: cv('--err', '#bd4b4b'), width: 1, type: 'dashed', opacity: 0.55 },
                  data: [{ yAxis: threshold.value }],
                }
              : undefined,
        })),
      },
      true,
    );
    if (group) echarts.connect(group);
  }, [
    axisText,
    group,
    lines,
    max,
    paletteKey,
    readValue,
    samples,
    themeName,
    threshold,
    title,
    unit?.name,
    valueAxis,
    variant,
  ]);

  const latest = (values: (number | null)[]) => {
    for (let index = values.length - 1; index >= 0; index -= 1) {
      if (values[index] !== null) return values[index];
    }
    return null;
  };
  const renderHeaderReading = (reading: HistoryHeaderReading | undefined) =>
    typeof reading === 'function' ? reading(readValue) : reading;
  const renderedMeta = renderHeaderReading(meta);
  const renderedCurrent = renderHeaderReading(current);

  return (
    <section className={`history-chart-card history-chart-card-${variant}${wide ? ' history-chart-card-wide' : ''}`}>
      <header className="history-chart-cap">
        <b>{title}</b>
        {unit?.name && <span className="chart-unit">({unit.name})</span>}
        {renderedMeta && <small>{renderedMeta}</small>}
        {renderedCurrent && <strong>{renderedCurrent}</strong>}
      </header>
      <div ref={elRef} className="history-chart" />
      <footer className="history-chart-legend" aria-label={`${title} 图例`}>
        {lines.map((line, index) => {
          const value = latest(line.values);
          const color = `var(${historyLineColorVar(line, index)})`;
          return (
            <span key={line.name} className={line.area ? 'area' : line.dashed ? 'dashed' : undefined}>
              <i style={{ background: color }} />
              {line.name} <b>{value === null ? '—' : readValue(value)}</b>
            </span>
          );
        })}
      </footer>
    </section>
  );
}

const historyValues = (samples: LoadSample[], read: (sample: LoadSample) => number | null): (number | null)[] =>
  samples.map(sample => (sample.has_gap ? null : read(sample)));

const CpuHistory = memo(function CpuHistory({
  report,
  label,
  windows,
  linked,
}: {
  report: NodeLoadView;
  label: string;
  windows: number;
  linked: boolean;
}) {
  const samples = report.series.slice(-windows);
  const group = linked ? `nd-cpu-history-${report.node_id}` : undefined;
  const detail = (sample: LoadSample, read: (value: CpuDetailSample) => number | null) =>
    sample.cpu_detail ? read(sample.cpu_detail) : null;
  const coreIds = Array.from(
    new Set(samples.flatMap(sample => sample.cpu_detail?.cores.map(core => core.cpu) ?? [])),
  ).sort((a, b) => a - b);
  const last = samples[samples.length - 1];
  const current =
    last.cpu_user_pct +
    last.cpu_sys_pct +
    last.cpu_softirq_pct +
    (last.cpu_detail?.iowait_pct ?? 0) +
    last.cpu_steal_pct;
  return (
    <section className="observe-history" aria-label={`CPU ${label} 数值`}>
      <div className="history-primary">
        <HistoryChart
          title="CPU 时间占比"
          samples={samples}
          group={group}
          formatValue={value => pct(value, 1)}
          variant="main"
          current={pct(current, 0)}
          meta={`窗口峰值 ${pct(last.cpu_peak_pct, 0)}`}
          max={100}
          threshold={{ value: CPU_PEAK_WARN, label: `${CPU_PEAK_WARN}% 峰值阈值` }}
          lines={[
            {
              name: '用户态',
              color: '--observe-user',
              stack: 'cpu',
              area: true,
              values: historyValues(samples, sample => sample.cpu_user_pct),
            },
            {
              name: '内核态',
              color: '--observe-system',
              stack: 'cpu',
              area: true,
              values: historyValues(samples, sample => sample.cpu_sys_pct),
            },
            {
              name: 'SoftIRQ',
              color: '--observe-softirq',
              stack: 'cpu',
              area: true,
              values: historyValues(samples, sample => sample.cpu_softirq_pct),
            },
            {
              name: 'IOwait',
              color: '--observe-iowait',
              stack: 'cpu',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.iowait_pct)),
            },
            {
              name: 'Steal',
              color: '--observe-steal',
              stack: 'cpu',
              area: true,
              values: historyValues(samples, sample => sample.cpu_steal_pct),
            },
            {
              name: '窗口峰值',
              color: '--observe-peak',
              values: historyValues(samples, sample => sample.cpu_peak_pct),
            },
          ]}
        />
        <HistoryChart
          title="CPU 与 I/O 压力"
          samples={samples}
          group={group}
          formatValue={value => pct(value, 2)}
          variant="secondary"
          lines={[
            {
              name: 'CPU PSI some',
              color: '--observe-softirq',
              values: historyValues(samples, sample => detail(sample, value => value.pressure_some_pct)),
            },
            {
              name: 'I/O PSI some',
              color: '--observe-iowait',
              values: historyValues(samples, sample => detail(sample, value => value.io_pressure_some_pct)),
            },
            {
              name: 'I/O PSI full',
              color: '--observe-steal',
              values: historyValues(samples, sample => detail(sample, value => value.io_pressure_full_pct)),
            },
          ]}
        />
      </div>
      <div className="history-grid">
        <HistoryChart
          title="负载与运行队列"
          samples={samples}
          group={group}
          formatValue={value => value.toFixed(2)}
          lines={[
            { name: 'Load 1m', values: historyValues(samples, sample => sample.load1) },
            { name: 'Load 5m', values: historyValues(samples, sample => detail(sample, value => value.load5)) },
            { name: 'Load 15m', values: historyValues(samples, sample => detail(sample, value => value.load15)) },
            {
              name: '运行进程',
              values: historyValues(samples, sample => detail(sample, value => value.procs_running)),
            },
          ]}
        />
        <HistoryChart
          title="调度与网络事件速率"
          samples={samples}
          group={group}
          valueUnit={historyCountRateUnit}
          lines={[
            {
              name: '上下文切换',
              values: historyValues(samples, sample => detail(sample, value => value.context_switches_per_sec)),
            },
            {
              name: 'NET_RX SoftIRQ',
              values: historyValues(samples, sample => detail(sample, value => value.net_rx_softirqs_per_sec)),
            },
            {
              name: 'NET_TX SoftIRQ',
              values: historyValues(samples, sample => detail(sample, value => value.net_tx_softirqs_per_sec)),
            },
          ]}
        />
        <HistoryChart
          title="Cgroup 限流时间"
          samples={samples}
          group={group}
          valueUnit={historyMsUnit}
          lines={[
            {
              name: '每窗口 throttled',
              values: historyValues(samples, sample =>
                detail(sample, value => (value.throttled_usec === null ? null : value.throttled_usec / 1000)),
              ),
            },
          ]}
        />
        {coreIds.length > 0 && (
          <HistoryChart
            title={`逐核繁忙度 · ${coreIds.length} 核`}
            samples={samples}
            group={group}
            formatValue={value => pct(value, 1)}
            max={100}
            lines={coreIds.map(cpu => ({
              name: `CPU ${cpu}`,
              values: historyValues(samples, sample => {
                const core = sample.cpu_detail?.cores.find(value => value.cpu === cpu);
                return core ? core.user_pct + core.system_pct + core.softirq_pct : null;
              }),
            }))}
          />
        )}
      </div>
    </section>
  );
});

const MemoryHistory = memo(function MemoryHistory({
  report,
  label,
  windows,
  linked,
}: {
  report: NodeLoadView;
  label: string;
  windows: number;
  linked: boolean;
}) {
  const samples = report.series.slice(-windows);
  const group = linked ? `nd-memory-history-${report.node_id}` : undefined;
  const detail = (sample: LoadSample, read: (value: MemoryDetailSample) => number | null) =>
    sample.memory_detail ? read(sample.memory_detail) : null;
  const last = samples[samples.length - 1];
  const memTotal = report.host?.mem_total_bytes ?? 0;
  return (
    <section className="observe-history" aria-label={`内存 ${label} 数值`}>
      <div className="history-primary">
        <HistoryChart
          title="容量构成"
          samples={samples}
          group={group}
          valueUnit={historyBytesUnit}
          variant="main"
          current={read => `可用 ${read(last.mem_available_bytes)}`}
          meta={
            last.memory_detail ? read => `窗口最低可用 ${read(last.memory_detail!.available_min_bytes)}` : undefined
          }
          max={memTotal > 0 ? memTotal : undefined}
          threshold={memTotal > 0 ? { value: memTotal * 0.85, label: '可用 15% 阈值' } : undefined}
          lines={[
            {
              name: '匿名页',
              color: '--observe-user',
              stack: 'memory',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.anon_bytes)),
            },
            {
              name: '共享/tmpfs',
              color: '--observe-softirq',
              stack: 'memory',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.shmem_bytes)),
            },
            {
              name: '内核/其他',
              color: '--observe-iowait',
              stack: 'memory',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.kernel_other_bytes)),
            },
            {
              name: '文件缓存',
              color: '--observe-system',
              stack: 'memory',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.file_cache_bytes)),
            },
            {
              name: '空闲',
              color: '--observe-free',
              stack: 'memory',
              area: true,
              values: historyValues(samples, sample => detail(sample, value => value.free_bytes)),
            },
          ]}
        />
        <HistoryChart
          title="内存压力"
          samples={samples}
          group={group}
          formatValue={value => pct(value, 2)}
          variant="secondary"
          lines={[
            {
              name: 'Memory PSI some',
              color: '--observe-iowait',
              values: historyValues(samples, sample => detail(sample, value => value.pressure_some_pct)),
            },
            {
              name: 'Memory PSI full',
              color: '--observe-steal',
              values: historyValues(samples, sample => detail(sample, value => value.pressure_full_pct)),
            },
          ]}
        />
      </div>
      <div className="history-grid">
        <HistoryChart
          title="内核缓存与固定页"
          samples={samples}
          group={group}
          valueUnit={historyBytesUnit}
          lines={[
            { name: 'Buffers', values: historyValues(samples, sample => detail(sample, value => value.buffers_bytes)) },
            {
              name: 'KReclaimable',
              values: historyValues(samples, sample => detail(sample, value => value.kernel_reclaimable_bytes)),
            },
            {
              name: 'SUnreclaim',
              values: historyValues(samples, sample => detail(sample, value => value.slab_unreclaimable_bytes)),
            },
            {
              name: 'Unevictable',
              values: historyValues(samples, sample => detail(sample, value => value.unevictable_bytes)),
            },
            { name: 'Mlocked', values: historyValues(samples, sample => detail(sample, value => value.mlocked_bytes)) },
            {
              name: 'GUP pinned',
              values: historyValues(samples, sample => detail(sample, value => value.gup_pinned_bytes)),
            },
          ]}
        />
        <HistoryChart
          title="Swap、脏页与回写"
          samples={samples}
          group={group}
          valueUnit={historyBytesUnit}
          lines={[
            { name: 'Swap 已用', values: historyValues(samples, sample => sample.swap_used_bytes) },
            {
              name: 'Swap cache',
              values: historyValues(samples, sample => detail(sample, value => value.swap_cached_bytes)),
            },
            { name: 'Dirty', values: historyValues(samples, sample => detail(sample, value => value.dirty_bytes)) },
            {
              name: 'Writeback',
              values: historyValues(samples, sample => detail(sample, value => value.writeback_bytes)),
            },
          ]}
        />
        <HistoryChart
          title="Swap I/O"
          samples={samples}
          group={group}
          valueUnit={historyBytesUnit}
          lines={[
            { name: '换入', values: historyValues(samples, sample => detail(sample, value => value.swap_in_bytes)) },
            { name: '换出', values: historyValues(samples, sample => detail(sample, value => value.swap_out_bytes)) },
          ]}
        />
        <HistoryChart
          title="缺页与直接回收"
          samples={samples}
          group={group}
          valueUnit={historyCountUnit}
          lines={[
            {
              name: 'Major faults',
              values: historyValues(samples, sample => detail(sample, value => value.major_faults)),
            },
            {
              name: 'Direct reclaim pages',
              values: historyValues(samples, sample => detail(sample, value => value.direct_reclaim_pages)),
            },
          ]}
        />
      </div>
    </section>
  );
});

const DiskHistory = memo(function DiskHistory({
  report,
  label,
  windows,
  linked,
}: {
  report: NodeLoadView;
  label: string;
  windows: number;
  linked: boolean;
}) {
  const samples = report.series.slice(-windows);
  const host = report.host;
  const group = linked ? `nd-disk-history-${report.node_id}` : undefined;
  const detail = (sample: LoadSample, read: (value: DiskDetailSample) => number | null) =>
    sample.disk_detail ? read(sample.disk_detail) : null;
  const total = (sample: LoadSample) => sample.disk_detail?.total_bytes ?? host?.disk_total_bytes ?? 0;
  const last = samples[samples.length - 1];
  const totalNow = total(last);
  const diskUsed = totalNow > 0 ? Math.max(0, totalNow - last.disk_free_bytes) : null;
  return (
    <section className="observe-history" aria-label={`磁盘 ${label} 数值`}>
      <div className="history-primary">
        <HistoryChart
          title="容量"
          samples={samples}
          group={group}
          valueUnit={historyBytesUnit}
          variant="main"
          current={diskUsed === null ? '—' : pct((diskUsed / totalNow) * 100, 0)}
          meta={read => `可用 ${read(last.disk_free_bytes)} · inode ${pct(100 - last.disk_inode_free_pct, 0)}`}
          max={totalNow > 0 ? totalNow : undefined}
          threshold={totalNow > 0 ? { value: totalNow * 0.9, label: '90% 容量阈值' } : undefined}
          lines={[
            {
              name: '已用',
              color: '--observe-user',
              stack: 'disk',
              area: true,
              values: historyValues(samples, sample => {
                const capacity = total(sample);
                return capacity > 0 ? capacity - sample.disk_free_bytes : null;
              }),
            },
            {
              name: '可用',
              color: '--observe-free',
              stack: 'disk',
              area: true,
              values: historyValues(samples, sample => sample.disk_free_bytes),
            },
          ]}
        />
        <HistoryChart
          title="设备繁忙与 I/O 压力"
          samples={samples}
          group={group}
          formatValue={value => pct(value, 2)}
          variant="secondary"
          lines={[
            {
              name: '设备繁忙',
              color: '--observe-user',
              values: historyValues(samples, sample => detail(sample, value => value.busy_pct)),
            },
            {
              name: 'I/O PSI some',
              color: '--observe-iowait',
              values: historyValues(samples, sample => detail(sample, value => value.pressure_some_pct)),
            },
            {
              name: 'I/O PSI full',
              color: '--observe-steal',
              values: historyValues(samples, sample => detail(sample, value => value.pressure_full_pct)),
            },
          ]}
        />
      </div>
      <div className="history-grid">
        <HistoryChart
          title="容量与 inode"
          samples={samples}
          group={group}
          formatValue={value => pct(value, 2)}
          lines={[
            {
              name: '容量占用',
              values: historyValues(samples, sample => {
                const capacity = total(sample);
                return capacity > 0 ? (1 - sample.disk_free_bytes / capacity) * 100 : null;
              }),
            },
            {
              name: 'inode 占用',
              values: historyValues(samples, sample => 100 - sample.disk_inode_free_pct),
            },
          ]}
        />
        <HistoryChart
          title="块设备吞吐"
          samples={samples}
          group={group}
          valueUnit={historyByteRateUnit}
          lines={[
            { name: '读取', values: historyValues(samples, sample => detail(sample, value => value.read_bps)) },
            { name: '写入', values: historyValues(samples, sample => detail(sample, value => value.write_bps)) },
          ]}
        />
        <HistoryChart
          title="块设备 IOPS"
          samples={samples}
          group={group}
          valueUnit={historyPerSecondUnit}
          lines={[
            { name: '读取', values: historyValues(samples, sample => detail(sample, value => value.read_iops)) },
            { name: '写入', values: historyValues(samples, sample => detail(sample, value => value.write_iops)) },
          ]}
        />
        <HistoryChart
          title="完成延迟"
          samples={samples}
          group={group}
          valueUnit={historyMsUnit}
          lines={[
            {
              name: '读取 await',
              values: historyValues(samples, sample => detail(sample, value => value.read_await_ms)),
            },
            {
              name: '写入 await',
              values: historyValues(samples, sample => detail(sample, value => value.write_await_ms)),
            },
          ]}
        />
        <HistoryChart
          title="队列"
          samples={samples}
          group={group}
          formatValue={value => value.toFixed(2)}
          lines={[
            {
              name: '平均队列深度',
              values: historyValues(samples, sample => detail(sample, value => value.queue_depth)),
            },
            {
              name: '窗口末 in-flight',
              values: historyValues(samples, sample => detail(sample, value => value.in_flight)),
            },
          ]}
        />
      </div>
    </section>
  );
});

const NetworkHistory = memo(function NetworkHistory({
  report,
  label,
  windows,
  linked,
}: {
  report: NodeLoadView;
  label: string;
  windows: number;
  linked: boolean;
}) {
  const samples = report.series.slice(-windows);
  const group = linked ? `nd-network-history-${report.node_id}` : undefined;
  const detail = (sample: LoadSample, read: (value: NetworkDetailSample) => number | null | undefined) =>
    sample.network_detail ? (read(sample.network_detail) ?? null) : null;
  const hasDeep = samples.some(sample => sample.network_detail);
  const pressure = (sample: LoadSample, read: (value: NetworkDetailSample) => number | null | undefined) => {
    const value = sample.network_detail;
    const capacity = value?.ephemeral_port_capacity;
    if (!value || typeof capacity !== 'number' || capacity <= 0) return null;
    const occupied = read(value);
    return typeof occupied === 'number' ? (occupied / capacity) * 100 : null;
  };
  const last = samples[samples.length - 1];
  const conntrackMax = report.host?.conntrack_max ?? null;
  const socketPart = (sample: LoadSample, key: 'tcp_curr_estab' | 'tcp_time_wait' | 'tcp_orphan' | 'udp_inuse') =>
    sample.network_detail?.[key] ?? null;
  const otherConntrack = (sample: LoadSample) => {
    if (sample.conntrack_count === null || !sample.network_detail) return null;
    const known =
      (sample.network_detail.tcp_curr_estab ?? 0) +
      (sample.network_detail.tcp_time_wait ?? 0) +
      (sample.network_detail.tcp_orphan ?? 0) +
      (sample.network_detail.udp_inuse ?? 0);
    return Math.max(0, sample.conntrack_count - known);
  };
  const hasPortPressure = samples.some(sample => sample.network_detail?.ephemeral_port_capacity != null);
  return (
    <section className="observe-history" aria-label={`网络 ${label} 数值`}>
      <div className="history-primary">
        <HistoryChart
          title="连接与套接字"
          samples={samples}
          group={group}
          valueUnit={historyCountUnit}
          variant="main"
          current={read =>
            last.conntrack_count === null || conntrackMax === null || conntrackMax <= 0
              ? last.conntrack_count === null
                ? '—'
                : read(last.conntrack_count)
              : pct((last.conntrack_count / conntrackMax) * 100, 0)
          }
          meta={
            last.conntrack_count === null
              ? undefined
              : read => `conntrack ${read(last.conntrack_count!)}${conntrackMax ? ` / ${read(conntrackMax)}` : ''}`
          }
          max={conntrackMax && conntrackMax > 0 ? conntrackMax : undefined}
          threshold={
            conntrackMax && conntrackMax > 0 ? { value: conntrackMax * 0.8, label: 'conntrack 80%' } : undefined
          }
          lines={[
            {
              name: 'TCP 已建立',
              color: '--observe-system',
              stack: 'conntrack',
              area: true,
              values: historyValues(samples, sample => socketPart(sample, 'tcp_curr_estab')),
            },
            {
              name: 'TIME_WAIT',
              color: '--observe-iowait',
              stack: 'conntrack',
              area: true,
              values: historyValues(samples, sample => socketPart(sample, 'tcp_time_wait')),
            },
            {
              name: 'TCP orphan',
              color: '--observe-steal',
              stack: 'conntrack',
              area: true,
              values: historyValues(samples, sample => socketPart(sample, 'tcp_orphan')),
            },
            {
              name: 'UDP',
              color: '--observe-softirq',
              stack: 'conntrack',
              area: true,
              values: historyValues(samples, sample => socketPart(sample, 'udp_inuse')),
            },
            {
              name: '其他',
              color: '--observe-user',
              stack: 'conntrack',
              area: true,
              values: historyValues(samples, otherConntrack),
            },
            ...(conntrackMax && conntrackMax > 0
              ? [
                  {
                    name: '余量',
                    color: '--observe-free',
                    stack: 'conntrack',
                    area: true,
                    values: historyValues(samples, sample =>
                      sample.conntrack_count === null ? null : Math.max(0, conntrackMax - sample.conntrack_count),
                    ),
                  },
                ]
              : []),
          ]}
        />
        {hasPortPressure && (
          <HistoryChart
            title="出站端口压力（估算）· 最繁忙目标"
            samples={samples}
            group={group}
            formatValue={value => pct(value, 2)}
            variant="secondary"
            max={100}
            lines={[
              {
                name: 'IPv4',
                color: '--observe-user',
                values: historyValues(samples, sample => pressure(sample, value => value.tcp_ephemeral_top_target_v4)),
              },
              {
                name: 'IPv6',
                color: '--observe-softirq',
                values: historyValues(samples, sample => pressure(sample, value => value.tcp_ephemeral_top_target_v6)),
              },
            ]}
          />
        )}
      </div>
      <div className="history-grid">
        {hasPortPressure && (
          <HistoryChart
            title="出站临时端口套接字"
            samples={samples}
            group={group}
            valueUnit={historyCountUnit}
            lines={[
              {
                name: 'IPv4 范围内',
                values: historyValues(samples, sample => detail(sample, value => value.tcp_ephemeral_inuse_v4)),
              },
              {
                name: 'IPv4 TIME_WAIT',
                values: historyValues(samples, sample => detail(sample, value => value.tcp_ephemeral_time_wait_v4)),
              },
              {
                name: 'IPv6 范围内',
                values: historyValues(samples, sample => detail(sample, value => value.tcp_ephemeral_inuse_v6)),
              },
              {
                name: 'IPv6 TIME_WAIT',
                values: historyValues(samples, sample => detail(sample, value => value.tcp_ephemeral_time_wait_v6)),
              },
            ]}
          />
        )}
        <HistoryChart
          title="连接快照"
          samples={samples}
          group={group}
          valueUnit={historyCountUnit}
          lines={[
            { name: 'Conntrack', values: historyValues(samples, sample => sample.conntrack_count) },
            { name: 'TCP in-use', values: historyValues(samples, sample => detail(sample, value => value.tcp_inuse)) },
          ]}
        />
        {hasDeep && (
          <>
            <HistoryChart
              title="TCP 连接生命周期"
              samples={samples}
              group={group}
              valueUnit={historyCountUnit}
              lines={[
                {
                  name: '主动建立',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_active_opens)),
                },
                {
                  name: '被动建立',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_passive_opens)),
                },
                {
                  name: '建立失败',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_attempt_fails)),
                },
                {
                  name: '连接复位',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_estab_resets)),
                },
              ]}
            />
            <HistoryChart
              title="TCP 重传与异常"
              samples={samples}
              group={group}
              valueUnit={historyCountUnit}
              lines={[
                {
                  name: '重传段',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_retrans_segs)),
                },
                {
                  name: 'SYN 重传',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_syn_retrans)),
                },
                { name: '超时', values: historyValues(samples, sample => detail(sample, value => value.tcp_timeouts)) },
                {
                  name: '接收错误',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_in_errors)),
                },
                {
                  name: '发出 RST',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_out_resets)),
                },
              ]}
            />
            <HistoryChart
              title="监听队列与 UDP 丢弃"
              samples={samples}
              group={group}
              valueUnit={historyCountUnit}
              lines={[
                {
                  name: '监听溢出',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_listen_overflows)),
                },
                {
                  name: '监听丢弃',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_listen_drops)),
                },
                {
                  name: 'UDP 接收错误',
                  values: historyValues(samples, sample => detail(sample, value => value.udp_in_errors)),
                },
                {
                  name: 'UDP 无端口',
                  values: historyValues(samples, sample => detail(sample, value => value.udp_no_ports)),
                },
                {
                  name: 'UDP 接收缓冲满',
                  values: historyValues(samples, sample => detail(sample, value => value.udp_rcvbuf_errors)),
                },
                {
                  name: 'UDP 发送缓冲满',
                  values: historyValues(samples, sample => detail(sample, value => value.udp_sndbuf_errors)),
                },
              ]}
            />
            <HistoryChart
              title="套接字资源"
              samples={samples}
              group={group}
              valueUnit={historyBytesUnit}
              lines={[
                {
                  name: 'TCP 内存',
                  values: historyValues(samples, sample => detail(sample, value => value.tcp_mem_bytes)),
                },
                {
                  name: 'UDP 内存',
                  values: historyValues(samples, sample => detail(sample, value => value.udp_mem_bytes)),
                },
              ]}
            />
          </>
        )}
      </div>
    </section>
  );
});

/** KPI 芯片右下角的迷你趋势线。复用 trendPaths + metricDomain，与既有的指标卡曲线同口径；
 * 面积铺到 viewBox 底部（小图无网格，收在 PLOT_BOTTOM 会留一条空缝）。 */
function Spark({
  series,
  valueOf,
  percent = false,
  domain,
  bridgeGaps = false,
}: {
  series: LoadSample[];
  valueOf: (sample: LoadSample) => number | null;
  percent?: boolean;
  /* 调用方自定量程：uptime 这类大基数标量不能用 metricDomain 的 |max|·8% 余量，
     那会把窗口内的变化压成平线。 */
  domain?: [number, number];
  /* uptime 用 true：缺口行的读数仍真实，线段只在真重启（值回落）处断开。 */
  bridgeGaps?: boolean;
}) {
  const values = series
    .filter(sample => !sample.has_gap)
    .map(valueOf)
    .filter((n): n is number => n !== null);
  const paths = trendPaths(series, valueOf, domain ?? metricDomain(values, percent), PLOT_H, bridgeGaps);
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

function sampleWindow(sample: LoadSample): string {
  const seconds = Math.max(1, sample.window_end_unix_secs - sample.window_start_unix_secs);
  return seconds === 30 ? '最近 30 秒' : `实际窗口 ${dur(seconds)}`;
}

function uptimeLabel(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const days = Math.floor(whole / 86_400);
  const hours = Math.floor((whole % 86_400) / 3_600);
  const minutes = Math.floor((whole % 3_600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分钟`;
  if (minutes > 0) return `${minutes} 分钟`;
  return '不足 1 分钟';
}

const LoadDashboard = memo(function LoadDashboard({
  report,
  historyLabel,
  historyWindows,
  linked,
}: {
  report: NodeLoadView;
  historyLabel: string;
  historyWindows: number;
  linked: boolean;
}) {
  const series = report.series;
  const last = series[series.length - 1];
  const host = report.host;
  const [openDetail, setOpenDetail] = useState<'cpu' | 'memory' | 'disk' | 'network' | null>(null);
  const cpu = (sample: LoadSample) => sample.cpu_user_pct + sample.cpu_sys_pct + sample.cpu_softirq_pct;
  const mem = (sample: LoadSample) =>
    host && host.mem_total_bytes > 0 ? (1 - sample.mem_available_bytes / host.mem_total_bytes) * 100 : null;
  const disk = (sample: LoadSample) => {
    const total = sample.disk_detail?.total_bytes ?? host?.disk_total_bytes ?? 0;
    return total > 0 ? (1 - sample.disk_free_bytes / total) * 100 : null;
  };
  const cpuNow = cpu(last);
  const memNow = mem(last);
  const diskNow = disk(last);
  const uptime = uptimeLabel(last.uptime_secs);
  const ctMax = host?.conntrack_max ?? null;
  const ctRatio = last.conntrack_count !== null && ctMax !== null && ctMax > 0 ? last.conntrack_count / ctMax : null;
  const hasNetworkHistory = series.some(sample => sample.conntrack_count !== null || sample.network_detail);

  return (
    <>
      {/* KPI 芯片带：CPU/内存/磁盘/负载 征收成标题下一条带（各带迷你趋势线）；已运行、连接表
          是标量，只给读数不给趋势线。金/红语气由阈值算出，spark 用 currentColor 随之变色。 */}
      <div className="kpi-band">
        <button
          type="button"
          className={`kpi kpi-expand ${openDetail === 'cpu' ? 'open' : ''}`}
          aria-expanded={openDetail === 'cpu'}
          disabled={!last.cpu_detail}
          title={last.cpu_detail ? `展开 ${historyLabel} CPU 曲线` : '当前 Agent 尚未上报 CPU 深度数据'}
          onClick={() => setOpenDetail(value => (value === 'cpu' ? null : 'cpu'))}
        >
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
        </button>
        <button
          type="button"
          className={`kpi kpi-expand ${openDetail === 'memory' ? 'open' : ''}`}
          aria-expanded={openDetail === 'memory'}
          disabled={!last.memory_detail}
          title={last.memory_detail ? `展开 ${historyLabel} 内存曲线` : '当前 Agent 尚未上报内存深度数据'}
          onClick={() => setOpenDetail(value => (value === 'memory' ? null : 'memory'))}
        >
          <span className="kpi-l">内存</span>
          <span className="kpi-v">
            {memNow === null ? '—' : pct(memNow).replace('%', '')}
            {memNow !== null && <small>%</small>}
          </span>
          <Spark series={series} valueOf={mem} percent />
        </button>
        <button
          type="button"
          className={`kpi kpi-expand ${openDetail === 'disk' ? 'open' : ''}`}
          aria-expanded={openDetail === 'disk'}
          disabled={!last.disk_detail}
          title={last.disk_detail ? `展开 ${historyLabel} 磁盘曲线` : '当前 Agent 尚未上报磁盘深度数据'}
          onClick={() => setOpenDetail(value => (value === 'disk' ? null : 'disk'))}
        >
          <span className="kpi-l">磁盘</span>
          <span className="kpi-v">
            {diskNow === null ? '—' : pct(diskNow).replace('%', '')}
            {diskNow !== null && <small>%</small>}
          </span>
          <Spark series={series} valueOf={disk} percent />
        </button>
        <button
          type="button"
          className={`kpi kpi-expand ${openDetail === 'network' ? 'open' : ''}`}
          aria-expanded={openDetail === 'network'}
          disabled={!hasNetworkHistory}
          title={hasNetworkHistory ? `展开 ${historyLabel} 网络曲线` : '当前尚无连接表或网络深度数据'}
          onClick={() => setOpenDetail(value => (value === 'network' ? null : 'network'))}
        >
          <span className="kpi-l">连接表</span>
          <span className="kpi-v">
            {ctRatio === null ? '—' : pct(ctRatio * 100, 1).replace('%', '')}
            {ctRatio !== null && <small>%</small>}
          </span>
          <Spark series={series} valueOf={sample => sample.conntrack_count} />
        </button>
        <div className="kpi">
          <span className="kpi-l">Load 1m</span>
          <span className="kpi-v">{last.load1.toFixed(2)}</span>
          <Spark series={series} valueOf={sample => sample.load1} />
        </div>
        <div className="kpi">
          <span className="kpi-l">已运行</span>
          <span className="kpi-v kpi-uptime">{uptime}</span>
          {/* 标量没有趋势可画：uptime 的曲线恒为斜线，信息量配不上占据的面积。
              画一条恒 0 的平线（等同趋势恒为 0 的形状），让该砖底部结构与其余砖一致。 */}
          <svg className="kpi-spark" viewBox={`0 0 ${PLOT_W} ${PLOT_H}`} preserveAspectRatio="none" aria-hidden="true">
            <path className="kpi-spark-area" d={`M0 ${PLOT_BOTTOM}H${PLOT_W}V${PLOT_H}H0Z`} />
            <path className="kpi-spark-line" d={`M0 ${PLOT_BOTTOM}H${PLOT_W}`} />
          </svg>
        </div>
      </div>

      {openDetail === 'cpu' && last.cpu_detail && (
        <CpuHistory report={report} label={historyLabel} windows={historyWindows} linked={linked} />
      )}
      {openDetail === 'memory' && last.memory_detail && (
        <MemoryHistory report={report} label={historyLabel} windows={historyWindows} linked={linked} />
      )}
      {openDetail === 'disk' && last.disk_detail && (
        <DiskHistory report={report} label={historyLabel} windows={historyWindows} linked={linked} />
      )}
      {openDetail === 'network' && hasNetworkHistory && (
        <NetworkHistory report={report} label={historyLabel} windows={historyWindows} linked={linked} />
      )}
    </>
  );
});

export function LoadCard({
  report,
  historyLabel = '30 MINUTES',
  historyWindows = LOAD_SLOTS,
  linked = false,
}: {
  report: NodeLoadView;
  historyLabel?: string;
  historyWindows?: number;
  linked?: boolean;
}) {
  if (report.series.length === 0) {
    return (
      <div className="panel" aria-label="观测数据为空">
        <p className="note">还没有负载读数。</p>
      </div>
    );
  }

  return (
    <div className="load-cluster">
      <LoadDashboard report={report} historyLabel={historyLabel} historyWindows={historyWindows} linked={linked} />
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

  // 瓶颈跳。需要与同链的其他跳比较才有意义——82 Mbit/s 本身不表示问题，
  // 同链其他跳都在 300 以上而该跳只有 82 才表示问题。
  // 同链的其他跳按 peer 区分——一条链上不会有两跳指向同一台机器。
  const others = sameChain.filter(o => o.sample.peer_node_id !== h.peer_node_id && o.sample.btlbw_p50_bps !== null);
  if (h.btlbw_p50_bps !== null && others.length > 0) {
    const nextLowest = Math.min(...others.map(o => o.sample.btlbw_p50_bps as number));
    if (h.btlbw_p50_bps < nextLowest * BOTTLENECK_RATIO) {
      out.push({
        tone: 'warn',
        chip: `瓶颈 ${bps(h.btlbw_p50_bps)}`,
        // 与同链其他跳的比较结果是该行的唯一信息——单独的 82 Mbit/s 不表示任何问题。
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
