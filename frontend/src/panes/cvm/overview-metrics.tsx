import { lazy, Suspense, useId, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchCvmReadings, type CvmInstance, type CvmMonitoringSample } from '../../api';
import { PanelLoading } from '../../ui/loading';
import { observeBpsReadingParts } from '../../ui/observe-chart';
import { observeSeriesData } from '../../ui/observe-series';
import { sparklinePaths } from '../../ui/sparkline';
import { dur } from '../telemetry-format';
import { resourceTone, shareOf } from './cards';
import { cpuPercent, cvmNetworkRate, type CvmOverviewMetric } from './monitoring-data';
import { type useCvmObservation } from './monitoring';
import { capacity } from './queries';

const MetricHistory = lazy(() => import('./monitoring-charts').then(module => ({ default: module.CvmMetricHistory })));

/* 指标带沿用机器详情的 KPI 芯片（.kpi-band / .kpi / .kpi-spark）：等宽小标签、等宽读数加小单位、
   通栏落底的趋势面积。前四块可展开近 1 小时曲线；本月流量与已运行是标量，只画一条平线，
   让六块砖的底部结构一致。 */
const SPARK_H = 24;

function KpiSpark({ values, scale }: { values: (number | null)[]; scale: number }) {
  if (values.length < 2) return <FlatSpark />;
  const paths = sparklinePaths(values, scale, SPARK_H);
  return (
    <svg
      className="kpi-spark"
      viewBox={`0 0 ${values.length - 1} ${SPARK_H}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {paths.areas.map((path, index) => (
        <path key={`a-${index}`} className="kpi-spark-area" d={path} />
      ))}
      {paths.curves.map((path, index) => (
        <path key={`l-${index}`} className="kpi-spark-line" d={path} />
      ))}
    </svg>
  );
}

function FlatSpark() {
  return (
    <svg className="kpi-spark" viewBox={`0 0 100 ${SPARK_H}`} preserveAspectRatio="none" aria-hidden="true">
      <path className="kpi-spark-area" d={`M0 ${SPARK_H - 4}H100V${SPARK_H}H0Z`} />
      <path className="kpi-spark-line" d={`M0 ${SPARK_H - 4}H100`} />
    </svg>
  );
}

/** 读数拆成数字与单位，数字用大号等宽，单位降到 small。 */
function splitReading(text: string) {
  const match = /^([\d.,]+)\s*(.*)$/.exec(text);
  return match ? { number: match[1], unit: match[2] } : { number: text, unit: '' };
}

const toneClass = (share: number | null) => {
  const tone = resourceTone(share, 0.8, 0.92);
  return tone === 'bad' ? ' bad' : tone === 'warn' ? ' warn' : '';
};

export function CvmOverviewMetrics({
  instance,
  observation,
  expanded,
  onExpand,
}: {
  instance: CvmInstance;
  observation: ReturnType<typeof useCvmObservation>;
  expanded: CvmOverviewMetric | null;
  onExpand: (metric: CvmOverviewMetric | null) => void;
}) {
  const { query, latest, current } = observation;
  const detailId = useId();
  const spec = instance.desired.spec;
  const native = spec.execution.kind === 'baremetal';
  const metered = spec.interfaces.some(nic => nic.allocated);
  const meter = useQuery({
    queryKey: ['cvm-readings'],
    queryFn: fetchCvmReadings,
    enabled: metered,
    staleTime: 15_000,
    refetchInterval: 60_000,
    select: readings => readings.instances.find(reading => reading.id === instance.desired.id),
  });
  const reading = current ? latest : null;
  const cpu = cpuPercent(reading?.cpu_usage_basis_points);
  const memory = shareOf(reading?.memory_used_bytes, reading?.memory_total_bytes);
  const disk = shareOf(reading?.root_used_bytes, reading?.root_total_bytes);
  const rx = cvmNetworkRate(reading, 'rx_bps');
  const tx = cvmNetworkRate(reading, 'tx_bps');
  const rateReference = Math.max(rx ?? 0, tx ?? 0);
  const rxReading = rx === null ? null : observeBpsReadingParts(rx, rateReference);
  const txReading = tx === null ? null : observeBpsReadingParts(tx, rateReference);

  const points = query.data?.samples ?? [];
  const times = points.map(sample => sample.observed_at_unix_millis / 1000);
  const starts = points.map(sample => sample.window_start_unix_millis / 1000);
  const series = (pick: (sample: CvmMonitoringSample) => number | null) =>
    observeSeriesData(
      times,
      points.map(sample => (sample.has_gap ? null : pick(sample))),
      { windowStartsUnixSecs: starts },
    ).map(([, value]) => value);
  const rxSeries = series(sample => cvmNetworkRate(sample, 'rx_bps'));
  const rxPeak = Math.max(1_000_000, ...rxSeries.map(value => value ?? 0));

  const monthly = meter.error ? undefined : meter.data;
  const monthUsed = monthly?.month_used_bytes ?? null;
  const monthLimit = monthly?.month_limit_bytes ?? null;
  const monthShare = monthUsed !== null && monthLimit ? Math.min(1, monthUsed / monthLimit) : null;
  const monthText = monthUsed === null ? null : splitReading(capacity(monthUsed));

  const tile = (
    key: CvmOverviewMetric,
    label: string,
    title: string,
    tone: string,
    value: ReactNode,
    spark: ReactNode,
  ) => (
    <button
      type="button"
      className={`kpi kpi-expand${tone}${expanded === key ? ' open' : ''}`}
      aria-expanded={expanded === key}
      aria-controls={expanded === key ? detailId : undefined}
      disabled={!points.length}
      title={points.length ? `${expanded === key ? '收起' : '展开'}近 1 小时${title}曲线` : '近 1 小时没有采样'}
      onClick={() => onExpand(expanded === key ? null : key)}
    >
      <span className="kpi-l">{label}</span>
      <span className="kpi-v">{value}</span>
      {spark}
    </button>
  );
  const percentValue = (share: number | null, sub: string) =>
    share === null ? (
      '—'
    ) : (
      <>
        {Math.round(share * 1000) / 10}
        <small>%</small>
        <span className="kpi-sub">{sub}</span>
      </>
    );
  return (
    <div className="load-dashboard-stack cvm-kpis">
      <div className="kpi-band" aria-label={current ? '当前容器读数' : '容器读数已停止更新'} role="group">
        {tile(
          'cpu',
          'CPU',
          ' CPU ',
          toneClass(cpu === null ? null : cpu / 100),
          percentValue(cpu === null ? null : cpu / 100, `${spec.resources.vcpus} ${native ? '核' : 'vCPU'}`),
          <KpiSpark values={series(sample => cpuPercent(sample.cpu_usage_basis_points))} scale={100} />,
        )}
        {tile(
          'memory',
          '内存',
          '内存',
          toneClass(memory),
          percentValue(memory, capacity(spec.resources.memory_bytes)),
          <KpiSpark
            values={series(sample => sample.memory_used_bytes)}
            scale={latest?.memory_total_bytes ?? spec.resources.memory_bytes}
          />,
        )}
        {tile(
          'disk',
          '系统盘',
          '系统盘',
          toneClass(disk),
          percentValue(disk, native ? '宿主磁盘' : capacity(spec.resources.root_disk_bytes)),
          <KpiSpark
            values={series(sample => sample.root_used_bytes)}
            scale={latest?.root_total_bytes ?? spec.resources.root_disk_bytes}
          />,
        )}
        {tile(
          'network',
          '网络',
          '网络收发',
          '',
          rxReading && txReading ? (
            <>
              {rxReading.number}
              <small>↓</small> {txReading.number}
              <small>↑</small>
              <span className="kpi-sub">{rxReading.unit}</span>
            </>
          ) : (
            '—'
          ),
          <KpiSpark values={rxSeries} scale={rxPeak * 1.15} />,
        )}
        <div
          className={`kpi${toneClass(monthShare)}`}
          title={
            !metered
              ? '未配置计量出站方式'
              : meter.error
                ? '本月流量读取失败'
                : `UTC 自然月${monthly?.month_coverage_gap ? ' · 统计有缺口' : ''}`
          }
        >
          <span className="kpi-l">本月流量</span>
          <span className="kpi-v">
            {monthText ? (
              <>
                {monthText.number}
                <small>{monthText.unit}</small>
                <span className="kpi-sub">{monthLimit ? `/ ${capacity(monthLimit)}` : '不限额'}</span>
              </>
            ) : (
              <span className="cvm-kpi-idle">{!metered ? '未计量' : meter.error ? '读取失败' : '—'}</span>
            )}
          </span>
          {monthShare === null ? <FlatSpark /> : <KpiSpark values={[monthShare * 100, monthShare * 100]} scale={100} />}
        </div>
        <div className="kpi">
          <span className="kpi-l">已运行</span>
          <span className="kpi-v kpi-uptime">{reading ? dur(reading.uptime_millis / 1000) : '—'}</span>
          <FlatSpark />
        </div>
      </div>
      {expanded && (
        <div className="kpi-detail-motion-content" id={detailId}>
          <CvmOverviewHistory observation={observation} metric={expanded} wide />
        </div>
      )}
    </div>
  );
}

export function CvmOverviewHistory({
  observation: { query },
  metric,
  wide = false,
}: {
  observation: ReturnType<typeof useCvmObservation>;
  metric: CvmOverviewMetric;
  wide?: boolean;
}) {
  if (query.isPending) return <PanelLoading label="读取实例监控" />;
  if (!query.data?.samples.length) return <p className="note cvm-chart-empty">近 1 小时没有采样记录。</p>;
  return (
    <Suspense fallback={<PanelLoading label="加载监控曲线" />}>
      <MetricHistory data={query.data} metric={metric} wide={wide} />
    </Suspense>
  );
}
