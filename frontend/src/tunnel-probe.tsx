import { useEffect, useState, useSyncExternalStore, type ReactNode } from 'react';
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  cancelTunnelProbe,
  fetchTunnelProbe,
  fetchTunnelProbeCapability,
  fetchTunnelProbes,
  startTunnelProbe,
  updateTunnelProbePolicy,
  type TunnelProbeHealth,
  type TunnelProbeHealthReason,
  type TunnelProbeListItem,
  type TunnelProbeResultStatus,
  type TunnelProbeRun,
  type TunnelProbeSource,
  type TunnelProbeView,
} from './api';
import { draft } from './draft';
import { ErrorBox, Loading, SegmentedControl } from './ui/bits';
import { DialogClose, DialogLayer } from './ui/dialog';
import { Icon, PanelTitle } from './ui/icons';
import { confirmDiscardChanges, useUnsavedChanges } from './ui/navigation-guard';
import { RegionFlag } from './ui/region-flag';
import { ActionMenu } from './ui/choice-menu';

const INTERVALS = [
  { value: 60, label: '每分钟' },
  { value: 300, label: '每 5 分钟' },
  { value: 900, label: '每 15 分钟' },
  { value: 1800, label: '每 30 分钟' },
  { value: 3600, label: '每小时' },
] as const;

const healthText: Record<TunnelProbeHealth, string> = {
  healthy: '正常',
  degraded: '变慢',
  down: '故障',
  paused: '未监测',
  unknown: '暂无结论',
};

const resultText: Record<TunnelProbeResultStatus, string> = {
  ok: '成功',
  timeout: '超时',
  'connect-failed': '连接失败',
  'target-failed': '落点失败',
  unsupported: '不支持',
  canceled: '已取消',
  interrupted: '执行中断',
};

/** Results that count as a failed run. Canceled and unsupported runs never reached the target. */
const FAILED_RESULTS = new Set<TunnelProbeResultStatus>(['timeout', 'connect-failed', 'target-failed', 'interrupted']);
const COUNTED_RESULTS = new Set<TunnelProbeResultStatus>(['ok', 'timeout', 'connect-failed', 'target-failed']);

const phaseOrder = ['queued', 'preparing', 'starting-xray', 'requesting'] as const;
const phaseText = ['排队', '冻结配置', '启动 Xray', '请求落点'] as const;

const WINDOWS = [
  { value: 86_400, label: '24 小时' },
  { value: 604_800, label: '7 天' },
] as const;
const windowText = (seconds: number) => WINDOWS.find(option => option.value === seconds)?.label ?? `${seconds} 秒`;

/** Records shown before the operator asks for the rest of the retained list. */
const RUNS_SHOWN = 5;

const isActive = (run: TunnelProbeRun | null | undefined) => run?.status === 'queued' || run?.status === 'running';
const intervalText = (seconds: number) => INTERVALS.find(option => option.value === seconds)?.label ?? `${seconds} 秒`;

const two = (value: number) => String(value).padStart(2, '0');
/** HH:MM for today, MM-DD HH:MM otherwise; probe records span at most the retention week. */
export function probeClock(seconds: number, nowMs = Date.now()): string {
  const date = new Date(seconds * 1000);
  const now = new Date(nowMs);
  const time = `${two(date.getHours())}:${two(date.getMinutes())}`;
  return date.toDateString() === now.toDateString()
    ? time
    : `${two(date.getMonth() + 1)}-${two(date.getDate())} ${time}`;
}

/** One decimal, rounded down so a single failure never reads as 100%. */
const successRateText = (rate: number) => {
  const tenths = Math.floor(rate * 1000) / 10;
  return Number.isInteger(tenths) ? String(tenths) : tenths.toFixed(1);
};

function sinceText(seconds: number, nowMs = Date.now()): string {
  const elapsed = Math.max(0, Math.round(nowMs / 1000 - seconds));
  if (elapsed < 60) return '刚刚';
  if (elapsed < 3_600) return `${Math.round(elapsed / 60)} 分钟前`;
  if (elapsed < 86_400) return `${Math.round(elapsed / 3_600)} 小时前`;
  return `${Math.round(elapsed / 86_400)} 天前`;
}

export const tunnelProbeQueryKey = (tenantId: string, outboundId: string, windowSecs: number) =>
  ['tunnel-probe', tenantId, outboundId, windowSecs] as const;

/** The detail page header and the probe panel read one query, so a window switch or a poll is
 * one request. Switching windows keeps the previous window on screen until the next one arrives. */
export function useTunnelProbeView(tenantId: string, outboundId: string, windowSecs: number, enabled = true) {
  return useQuery({
    queryKey: tunnelProbeQueryKey(tenantId, outboundId, windowSecs),
    queryFn: () => fetchTunnelProbe(tenantId, outboundId, windowSecs),
    enabled,
    staleTime: 15_000,
    retry: false,
    retryOnMount: false,
    refetchOnMount: false,
    placeholderData: keepPreviousData,
    refetchInterval: query => (isActive(query.state.data?.item.latest_run) ? 2_000 : 15_000),
  });
}

// ECharts stays out of the tunnel list and Settings: only a tunnel detail loads the chart module,
// and it gates its first paint on the module so the page appears complete at once.
const importTunnelProbeChart = () => import('./tunnel-probe-chart');
type TunnelProbeChartComponent = Awaited<ReturnType<typeof importTunnelProbeChart>>['TunnelProbeChart'];
export type TunnelProbeChartState =
  { status: 'pending' } | { status: 'ready'; Chart: TunnelProbeChartComponent } | { status: 'error'; error: unknown };

let loadedTunnelProbeChart: TunnelProbeChartComponent | null = null;
let tunnelProbeChartPromise: Promise<TunnelProbeChartComponent> | null = null;

function loadTunnelProbeChart(): Promise<TunnelProbeChartComponent> {
  if (loadedTunnelProbeChart) return Promise.resolve(loadedTunnelProbeChart);
  tunnelProbeChartPromise ??= importTunnelProbeChart().then(module => {
    loadedTunnelProbeChart = module.TunnelProbeChart;
    return loadedTunnelProbeChart;
  });
  return tunnelProbeChartPromise;
}

export function useTunnelProbeChart(enabled = true): TunnelProbeChartState {
  const [state, setState] = useState<TunnelProbeChartState>(() =>
    loadedTunnelProbeChart ? { status: 'ready', Chart: loadedTunnelProbeChart } : { status: 'pending' },
  );
  useEffect(() => {
    if (!enabled || state.status !== 'pending') return;
    let active = true;
    loadTunnelProbeChart().then(
      Chart => {
        if (active) setState({ status: 'ready', Chart });
      },
      error => {
        if (active) setState({ status: 'error', error });
      },
    );
    return () => {
      active = false;
    };
  }, [enabled, state.status]);
  return state;
}

export type TunnelProbeTone = 'ok' | 'warn' | 'err' | 'idle';
export interface TunnelProbeStatus {
  /** `unpublished`: the tunnel has no Serving copy, so only a draft can be probed. */
  key: TunnelProbeHealth | 'unpublished';
  word: string;
  tone: TunnelProbeTone;
  why: string;
}

const TONE: Record<TunnelProbeHealth, TunnelProbeTone> = {
  healthy: 'ok',
  degraded: 'warn',
  down: 'err',
  paused: 'idle',
  unknown: 'idle',
};

function reasonText(reason: TunnelProbeHealthReason, failure: TunnelProbeRun | undefined, nowMs: number): string {
  const detail = failure?.error_detail ? `：${failure.error_detail}` : '';
  switch (reason.kind) {
    case 'not-scheduled':
      return '定时监测未启用';
    case 'no-samples':
      return '最近 7 天没有 Serving 拨测';
    case 'stale':
      return reason.latest_finished_at_unix_secs === null
        ? '最近一次拨测没有完成'
        : `最近一次拨测在 ${sinceText(reason.latest_finished_at_unix_secs, nowMs)}，已超过 3 个周期`;
    case 'consecutive-failures':
      return `连续 ${reason.count} 次失败，最近一次${reason.latest ? resultText[reason.latest] : '失败'}${detail}`;
    case 'latest-failed':
      return `最近一次${reason.result ? resultText[reason.result] : '失败'}${detail}`;
    case 'low-success-rate':
      return `最近 ${reason.samples} 次成功 ${reason.succeeded} 次`;
    case 'slow':
      return reason.samples === 1
        ? `最近一次拨测首字节 ${reason.max_ttfb_ms} ms，超过 ${reason.threshold_ms} ms`
        : `最近 ${reason.samples} 次拨测首字节最高 ${reason.max_ttfb_ms} ms，超过 ${reason.threshold_ms} ms`;
    case 'healthy':
      return reason.max_ttfb_ms === null
        ? `最近 ${reason.samples} 次全部成功`
        : `最近 ${reason.samples} 次全部成功，首字节最高 ${reason.max_ttfb_ms} ms`;
  }
}

/** Status of a custom tunnel as the Console judged it. `view` is null when the Console has no
 * record of the tunnel at all, which only happens before it is published or first probed. */
export function tunnelProbeStatus(view: TunnelProbeView | null, nowMs = Date.now()): TunnelProbeStatus {
  if (!view || !view.item.supported) {
    return {
      key: 'unpublished',
      word: '未发布',
      tone: 'idle',
      why: view?.item.unsupported_reason ?? '尚未进入 Serving；发布后才能启用定时监测或拨测 Serving',
    };
  }
  const { health, health_reason: reason } = view.item;
  const failure = view.recent_runs.find(
    run => run.source === 'serving' && run.result !== null && COUNTED_RESULTS.has(run.result) && run.result !== 'ok',
  );
  return {
    key: health,
    word: healthText[health],
    tone: TONE[health],
    why: reason ? reasonText(reason, failure, nowMs) : '',
  };
}

/** The newest run that reported where traffic leaves the tunnel. */
export const tunnelExitRun = (view: TunnelProbeView | null | undefined) =>
  view?.recent_runs.find(run => run.exit_ip) ?? (view?.item.latest_run?.exit_ip ? view.item.latest_run : null);

function ProbeProgress({ run, source }: { run: TunnelProbeRun; source: TunnelProbeSource }) {
  const current = Math.max(0, phaseOrder.indexOf(run.phase as (typeof phaseOrder)[number]));
  return (
    <div className="tnd-verdict is-run" role="status">
      <span className="cg-lamp run" aria-hidden />
      <b>拨测中</b>
      <span className="tnd-why">
        {phaseText[current]} · 第 {current + 1} 步，共 {phaseText.length} 步 · {source === 'draft' ? '草稿' : 'Serving'}
      </span>
      <ol className="tnd-steps" aria-label="拨测进度">
        {phaseText.map((label, index) => (
          <li
            className={index < current ? 'done' : index === current ? 'current' : ''}
            key={label}
            title={label}
            aria-label={label}
          />
        ))}
      </ol>
    </div>
  );
}

function Verdict({ status, view }: { status: TunnelProbeStatus; view: TunnelProbeView | null }) {
  const policy = view?.item.policy;
  const lastServing = view?.recent_runs.find(run => run.source === 'serving' && run.finished_at_unix_secs !== null);
  const lastAny = view?.recent_runs.find(run => run.finished_at_unix_secs !== null);
  const timing =
    status.key !== 'unpublished' && policy?.enabled
      ? `上次 ${lastServing?.finished_at_unix_secs ? probeClock(lastServing.finished_at_unix_secs) : '—'} · 下次 ${
          policy.next_run_at_unix_secs ? probeClock(policy.next_run_at_unix_secs) : '—'
        }`
      : lastAny?.finished_at_unix_secs
        ? `上次 ${probeClock(lastAny.finished_at_unix_secs)}`
        : '';
  const healthy = status.key === 'healthy';
  return (
    <div className={`tnd-verdict tone-${status.tone}${healthy ? ' is-quiet' : ''}`}>
      <span className={`cg-lamp ${status.tone}`} role="img" aria-label={status.word} title={status.word} />
      {!healthy && (
        <>
          <b>{status.word}</b>
          <span className="tnd-why" title={status.why}>
            {status.why}
          </span>
        </>
      )}
      {timing && <span className="tnd-when">{timing}</span>}
    </div>
  );
}

interface MetricCell {
  label: string;
  value: ReactNode;
  sub: ReactNode;
  tone?: 'warn' | 'err';
  wide?: boolean;
}

function exitCell(view: TunnelProbeView | null, windowSecs: number): MetricCell {
  const exit = tunnelExitRun(view);
  if (!exit?.exit_ip) return { label: '出口', value: '—', sub: '拨测成功后显示', wide: true };
  const seen = exit.finished_at_unix_secs ?? exit.queued_at_unix_secs;
  const many = view && view.summary.exit_ips > 1 ? ` · ${windowText(windowSecs)}内 ${view.summary.exit_ips} 个 IP` : '';
  return {
    label: '出口',
    value: (
      <>
        <RegionFlag code={exit.exit_loc} />
        <span className="tnd-ip">{exit.exit_ip}</span>
      </>
    ),
    sub: `${exit.exit_loc ?? '未知地区'} · ${probeClock(seen)}${many}`,
    wide: true,
  };
}

function Metrics({
  view,
  status,
  windowSecs,
}: {
  view: TunnelProbeView | null;
  status: TunnelProbeStatus;
  windowSecs: number;
}) {
  let cells: MetricCell[];
  if (status.key === 'unpublished') {
    const runs = view?.recent_runs.filter(run => run.finished_at_unix_secs !== null) ?? [];
    const latest = runs[0];
    if (!latest) return null;
    cells = [
      {
        label: '最近一次',
        value:
          latest.result === 'ok' ? (
            <>
              {latest.ttfb_ms}
              <span className="u">ms</span>
            </>
          ) : (
            <span className="err">{latest.result ? resultText[latest.result] : '未完成'}</span>
          ),
        sub: `${latest.source === 'draft' ? `草稿 ${latest.draft_sha256?.slice(0, 8) ?? '—'}` : 'Serving'} · ${probeClock(latest.finished_at_unix_secs!)}`,
      },
      {
        label: '草稿拨测',
        value: (
          <>
            {runs.filter(run => run.result === 'ok').length}
            <span className="u">/ {runs.length} 成功</span>
          </>
        ),
        sub: '手动发起',
      },
      exitCell(view, windowSecs),
    ];
  } else {
    const summary = view!.summary;
    const split = [
      ['超时', summary.timeouts],
      ['连接', summary.connect_failures],
      ['落点', summary.target_failures],
    ]
      .filter(([, count]) => Number(count) > 0)
      .map(([label, count]) => `${label} ${count}`)
      .join(' · ');
    cells = [
      {
        label: '可用率',
        value:
          summary.success_rate === null ? (
            '—'
          ) : (
            <>
              {successRateText(summary.success_rate)}
              <span className="u">%</span>
            </>
          ),
        sub: `${summary.succeeded} / ${summary.total} 次 · ${windowText(windowSecs)}`,
      },
      {
        label: '首字节 P50',
        value:
          summary.p50_ms === null ? (
            '—'
          ) : (
            <>
              {summary.p50_ms}
              <span className="u">ms</span>
            </>
          ),
        sub: '中位数',
      },
      {
        label: 'P95',
        value:
          summary.p95_ms === null ? (
            '—'
          ) : (
            <>
              {summary.p95_ms}
              <span className="u">ms</span>
            </>
          ),
        sub: '慢尾',
      },
      {
        label: '失败',
        value: summary.failures,
        sub: split || '—',
        tone: summary.failures === 0 ? undefined : status.key === 'down' ? 'err' : 'warn',
      },
      exitCell(view, windowSecs),
    ];
  }
  return (
    <dl className={`tnd-metrics is-${cells.length}`}>
      {cells.map(cell => (
        <div className={cell.wide ? 'is-wide' : undefined} key={cell.label}>
          <dt>{cell.label}</dt>
          <dd className={cell.tone}>{cell.value}</dd>
          <small>{cell.sub}</small>
        </div>
      ))}
    </dl>
  );
}

export interface TunnelProbeRunGroup {
  runs: TunnelProbeRun[];
  key: string | null;
}

/** Only consecutive scheduled runs of the same published configuration and observable outcome
 * may collapse. Filtering happens afterwards, so a success still separates two failures. */
export function groupTunnelProbeRuns(
  runs: readonly TunnelProbeRun[],
  intervalSecs: number | null,
  slowMs: number,
): TunnelProbeRunGroup[] {
  const groups: TunnelProbeRunGroup[] = [];
  for (const run of runs) {
    const eligible =
      run.source === 'serving' &&
      run.trigger === 'scheduled' &&
      run.finished_at_unix_secs !== null &&
      run.result !== null &&
      (COUNTED_RESULTS.has(run.result) || run.result === 'interrupted');
    const key = eligible
      ? JSON.stringify([
          run.result,
          run.ttfb_ms === null ? null : run.ttfb_ms > slowMs,
          run.serving_generation,
          run.topology_revision,
          run.settings_revision,
          run.http_status,
          run.exit_ip,
          run.exit_loc,
          run.error_code,
          run.error_detail,
        ])
      : null;
    const last = groups.at(-1);
    const previous = last?.runs.at(-1);
    const spacing =
      previous?.finished_at_unix_secs !== null &&
      previous?.finished_at_unix_secs !== undefined &&
      run.finished_at_unix_secs !== null
        ? previous.finished_at_unix_secs - run.finished_at_unix_secs
        : Infinity;
    if (
      key !== null &&
      last?.key === key &&
      intervalSecs !== null &&
      spacing >= 0 &&
      spacing <= intervalSecs * 1.5 + run.timeout_secs
    )
      last.runs.push(run);
    else groups.push({ key, runs: [run] });
  }
  return groups;
}

function RunRow({ run, slowMs }: { run: TunnelProbeRun; slowMs: number }) {
  const failed = run.result !== null && FAILED_RESULTS.has(run.result);
  const tone =
    run.result === 'ok'
      ? run.ttfb_ms !== null && run.ttfb_ms > slowMs
        ? 'slow'
        : 'ok'
      : failed
        ? 'fail'
        : isActive(run)
          ? 'run'
          : 'muted';
  const revision =
    run.source === 'draft'
      ? `点击时冻结的草稿，基于修订 r${run.topology_revision}`
      : `Serving G${run.serving_generation ?? '—'} · r${run.topology_revision}`;
  return (
    <article className={`tnd-run is-${tone}`} key={run.id} title={revision}>
      <span className="tnd-run-when">{probeClock(run.finished_at_unix_secs ?? run.queued_at_unix_secs)}</span>
      <span className="tnd-run-ic" aria-hidden>
        {tone === 'ok' || tone === 'slow' ? (
          <Icon of="check" size={11} />
        ) : tone === 'fail' ? (
          <Icon of="close" size={11} />
        ) : (
          <Icon of="clock" size={11} />
        )}
      </span>
      <b className="tnd-run-val">
        {run.result === 'ok' && run.ttfb_ms !== null ? (
          <>
            {run.ttfb_ms}
            <span className="u">ms</span>
          </>
        ) : run.result ? (
          resultText[run.result]
        ) : run.status === 'running' ? (
          '运行中'
        ) : (
          '等待执行'
        )}
      </b>
      <span className="tnd-run-detail">
        {run.result === 'ok' ? (
          <>
            {run.http_status !== null && <span>HTTP {run.http_status}</span>}
            {run.exit_ip && (
              <>
                <RegionFlag code={run.exit_loc} />
                <span className="tnd-ip">{run.exit_ip}</span>
              </>
            )}
          </>
        ) : (
          <span className="tnd-run-error" title={run.error_detail ?? undefined}>
            {run.error_detail ?? ''}
          </span>
        )}
      </span>
      <span className="tnd-run-tags">
        {run.trigger === 'manual' && <span className="tnd-tag">手动</span>}
        {run.source === 'draft' && <span className="tnd-tag draft">草稿 {run.draft_sha256?.slice(0, 8) ?? '—'}</span>}
      </span>
    </article>
  );
}

function RunRows({
  runs,
  intervalSecs,
  slowMs,
  retentionDays,
}: {
  runs: TunnelProbeRun[];
  intervalSecs: number | null;
  slowMs: number;
  retentionDays: number;
}) {
  const [all, setAll] = useState(false);
  const [filter, setFilter] = useState<'all' | 'fail'>('all');
  const groups = groupTunnelProbeRuns(runs, intervalSecs, slowMs).filter(
    group => filter === 'all' || group.runs.some(run => run.result !== null && FAILED_RESULTS.has(run.result)),
  );
  const shown = all ? groups : groups.slice(0, RUNS_SHOWN);
  return (
    <>
      <div className="tnd-subhead">
        <b>拨测记录</b>
      </div>
      <div className="tnd-record-controls">
        <span className="tnd-record-scope">
          最近 {runs.length} 次 · 保留 {retentionDays} 天 · 含手动与草稿
        </span>
        <span className="sp" />
        <SegmentedControl
          value={filter}
          options={[
            { value: 'all', label: '全部' },
            { value: 'fail', label: '只看失败' },
          ]}
          onChange={value => {
            setFilter(value);
            setAll(false);
          }}
          ariaLabel="拨测记录筛选"
        />
      </div>
      {groups.length === 0 ? (
        <p className="tnd-none">{filter === 'fail' ? `最近 ${runs.length} 次记录中没有失败。` : '还没有拨测记录。'}</p>
      ) : (
        <div className="tnd-runs">
          {shown.map(group => {
            const first = group.runs[0];
            if (group.runs.length === 1) return <RunRow key={first.id} run={first} slowMs={slowMs} />;
            const failed = first.result !== null && FAILED_RESULTS.has(first.result);
            const slow = first.result === 'ok' && (first.ttfb_ms ?? 0) > slowMs;
            const values = group.runs.flatMap(run => (run.ttfb_ms === null ? [] : [run.ttfb_ms]));
            const oldest = group.runs.at(-1)!;
            return (
              <details className="tnd-run-group" key={first.id}>
                <summary className={`tnd-run is-group is-${failed ? 'fail' : slow ? 'slow' : 'ok'}`}>
                  <span className="tnd-run-when">
                    {probeClock(oldest.finished_at_unix_secs!)}–{probeClock(first.finished_at_unix_secs!)}
                  </span>
                  <span className="tnd-run-ic" aria-hidden>
                    <Icon of={failed ? 'close' : 'check'} size={11} />
                  </span>
                  <b className="tnd-run-val">
                    {group.runs.length} 次{failed ? resultText[first.result!] : slow ? '变慢' : '成功'}
                  </b>
                  <span className="tnd-run-detail">
                    {failed ? (
                      <span className="tnd-run-error" title={first.error_detail ?? undefined}>
                        {first.error_detail}
                      </span>
                    ) : (
                      <>
                        {values.length > 0 && (
                          <span>
                            {Math.min(...values)}–{Math.max(...values)} ms
                          </span>
                        )}
                        {first.http_status !== null && <span>HTTP {first.http_status}</span>}
                        {first.exit_ip && (
                          <>
                            <RegionFlag code={first.exit_loc} />
                            <span className="tnd-ip">{first.exit_ip}</span>
                          </>
                        )}
                      </>
                    )}
                  </span>
                  <span className="tnd-run-tags tnd-group-toggle">
                    明细<span aria-hidden>⌄</span>
                  </span>
                </summary>
                <div className="tnd-group-detail">
                  {group.runs.map(run => (
                    <RunRow key={run.id} run={run} slowMs={slowMs} />
                  ))}
                </div>
              </details>
            );
          })}
        </div>
      )}
      {groups.length > RUNS_SHOWN && (
        <button className="tnd-more" type="button" onClick={() => setAll(value => !value)}>
          {all ? '收起记录' : `显示其余记录 · 最近 ${runs.length} 次`}
        </button>
      )}
    </>
  );
}

/** Console's view is one probe path, not evidence of failure on every referencing node. */
export function tunnelProbeImpact(view: TunnelProbeView | null): string | null {
  if (view?.item.health === 'down') return 'Console 连续拨测失败；以下线路引用此隧道，可能受影响。';
  if (view?.item.health !== 'degraded') return null;
  return view.item.health_reason?.kind === 'slow'
    ? 'Console 拨测首字节偏慢；以下线路引用此隧道，可能受影响。'
    : 'Console 近期拨测出现失败；以下线路引用此隧道，可能受影响。';
}

const endpointText = (url: string) => url.replace(/^https?:\/\//, '');

function PolicyLine({
  item,
  published,
  endpointUrl,
  editable,
  onEdit,
}: {
  item: TunnelProbeListItem | null;
  published: boolean;
  endpointUrl: string | null;
  editable: boolean;
  onEdit: () => void;
}) {
  const policy = item?.policy;
  return (
    <div className="tnd-policy">
      <span className="tnd-k">定时监测</span>
      {!published ? (
        <span>发布后可启用</span>
      ) : policy?.enabled ? (
        <span>
          {intervalText(policy.interval_secs)} · 超时 {policy.timeout_secs} 秒
        </span>
      ) : (
        <span className="warn">{policy ? '已暂停' : '未启用'}</span>
      )}
      {endpointUrl && (
        <span className="tnd-route" title="拨测从 Console 发起，经这条隧道请求落点">
          Console → {endpointText(endpointUrl)}
        </span>
      )}
      {published && item && (
        <button className="btn tnd-mini" type="button" disabled={!editable} onClick={onEdit}>
          {policy?.enabled ? '修改' : '设置'}
        </button>
      )}
    </div>
  );
}

export function TunnelProbePolicyDialog({
  item,
  onClose,
  onSaved,
}: {
  item: TunnelProbeListItem;
  onClose: () => void;
  onSaved?: () => void;
}) {
  const qc = useQueryClient();
  const [enabled, setEnabled] = useState(item.policy?.enabled ?? false);
  const [interval, setIntervalValue] = useState(item.policy?.interval_secs ?? 300);
  const [timeout, setTimeoutValue] = useState(String(item.policy?.timeout_secs ?? 10));
  const dirty =
    enabled !== (item.policy?.enabled ?? false) ||
    interval !== (item.policy?.interval_secs ?? 300) ||
    timeout !== String(item.policy?.timeout_secs ?? 10);
  const guardScope = `tunnel-probe:${item.tenant_id}:${item.outbound_id}`;
  useUnsavedChanges(dirty, `${item.name} 的定时监测`, guardScope);
  const validTimeout = Number.isInteger(Number(timeout)) && Number(timeout) >= 1 && Number(timeout) <= 120;
  const save = useMutation({
    mutationFn: () =>
      updateTunnelProbePolicy(item.tenant_id, item.outbound_id, {
        enabled,
        interval_secs: interval,
        timeout_secs: Number(timeout),
      }),
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['tunnel-probes'] }),
        qc.invalidateQueries({ queryKey: ['tunnel-probe', item.tenant_id, item.outbound_id] }),
      ]);
      onSaved?.();
      onClose();
    },
  });

  return (
    <DialogLayer
      label={`设置 ${item.name} 定时监测`}
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface tunnel-dialog narrow tunnel-probe-dialog">
        <header>
          <b>定时监测</b>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="tunnel-dialog-body">
          <p className="tunnel-probe-dialog-name">
            <b>{item.name}</b>
            <span className="mono">{item.outbound_id}</span>
          </p>
          <div className="fgrid one tunnel-probe-policy-fields">
            <div className="row">
              <span className="k">状态</span>
              <span className="v">
                <SegmentedControl
                  value={enabled}
                  options={[
                    { value: false, label: '暂停' },
                    { value: true, label: '启用' },
                  ]}
                  onChange={setEnabled}
                  ariaLabel="定时监测状态"
                />
              </span>
            </div>
            <label className="row">
              <span className="k">监测间隔</span>
              <span className="v">
                <select className="f" value={interval} onChange={event => setIntervalValue(Number(event.target.value))}>
                  {INTERVALS.map(option => (
                    <option value={option.value} key={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </span>
            </label>
            <label className="row">
              <span className="k">首字节超时</span>
              <span className="v tunnel-probe-timeout-field">
                <input
                  className="f"
                  type="number"
                  min={1}
                  max={120}
                  value={timeout}
                  aria-invalid={!validTimeout || undefined}
                  onChange={event => setTimeoutValue(event.target.value)}
                />
                <span>秒</span>
              </span>
            </label>
          </div>
          <p className="guard">计划在 Console 机器上执行；采用当前 Serving 修订，不会改动节点配置。</p>
          {save.error && <ErrorBox error={save.error} />}
        </div>
        <footer>
          <span className="hint">间隔带稳定抖动，避免同一秒集中发起。</span>
          <span className="sp" />
          <DialogClose className="btn">取消</DialogClose>
          <button className="btn primary" disabled={!validTimeout || save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? '保存中…' : '保存'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

export function useTunnelProbeControls({
  tenantId,
  outboundId,
  draftSupported,
  editable,
  windowSecs,
  enabled = true,
}: {
  tenantId: string;
  outboundId: string;
  draftSupported: boolean;
  editable: boolean;
  windowSecs: number;
  enabled?: boolean;
}) {
  const qc = useQueryClient();
  const draftEntries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const capability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    staleTime: 60_000,
    enabled: editable && enabled,
    retry: false,
    retryOnMount: false,
    refetchOnMount: false,
  });
  const probe = useTunnelProbeView(tenantId, outboundId, windowSecs, enabled);
  const refresh = () => qc.invalidateQueries({ queryKey: ['tunnel-probe', tenantId, outboundId] });
  const start = useMutation({
    mutationFn: (source: TunnelProbeSource) => startTunnelProbe(tenantId, outboundId, source),
    onSuccess: refresh,
  });
  const cancel = useMutation({ mutationFn: (id: number) => cancelTunnelProbe(id), onSuccess: refresh });
  const available = capability.data?.available ?? false;

  const missing = probe.error instanceof ApiError && probe.error.status === 404;
  const view = missing ? null : (probe.data ?? null);
  const status = tunnelProbeStatus(view);
  const published = status.key !== 'unpublished';
  const active = view?.recent_runs.find(isActive) ?? (isActive(view?.item.latest_run) ? view!.item.latest_run : null);
  // A published tunnel can still carry an unpublished edit; its draft is then worth probing too.
  const draftChanged = draftEntries.some(
    entry =>
      entry.op.op === 'upsert_external_outbound' &&
      entry.op.outbound.tenant_id === tenantId &&
      entry.op.outbound.id === outboundId,
  );
  const blocked = !editable || !available || start.isPending;
  const blockedReason = !editable
    ? undefined
    : !available
      ? (capability.data?.reason ?? 'Console 拨测组件不可用')
      : undefined;

  return {
    probe,
    capability,
    view,
    status,
    published,
    active,
    draftChanged,
    blocked,
    blockedReason,
    start,
    cancel,
    available,
    editable,
    draftSupported,
  };
}

type TunnelProbeControls = ReturnType<typeof useTunnelProbeControls>;

export function TunnelProbeActions({ controls }: { controls: TunnelProbeControls }) {
  const { probe, active, published, draftSupported, blocked, blockedReason, start, cancel, editable } = controls;
  if (probe.isPending || (probe.error && !(probe.error instanceof ApiError && probe.error.status === 404))) return null;
  return (
    <div className="tnd-probe-actions" role="group" aria-label="拨测操作">
      {active ? (
        <button
          className="btn"
          type="button"
          disabled={!editable || cancel.isPending || active.cancel_requested}
          onClick={() => cancel.mutate(active.id)}
        >
          {active.cancel_requested || cancel.isPending ? '取消中…' : '取消'}
        </button>
      ) : (
        <>
          {(published || draftSupported) && (
            <button
              className="btn primary"
              type="button"
              disabled={blocked}
              title={blockedReason}
              onClick={() => start.mutate(published ? 'serving' : 'draft')}
            >
              {start.isPending ? '排队中…' : published ? '立即拨测' : '拨测草稿'}
            </button>
          )}
        </>
      )}
    </div>
  );
}

export function TunnelProbeDraftAction({ controls }: { controls: TunnelProbeControls }) {
  const { active, published, draftChanged, draftSupported, blocked, blockedReason, start } = controls;
  if (active || !published || !draftChanged || !draftSupported) return null;
  return (
    <ActionMenu
      label="更多拨测操作"
      items={[
        {
          label: '拨测草稿',
          note: '拨测点击时冻结的未发布修改',
          disabled: blockedReason || blocked,
          onSelect: () => start.mutate('draft'),
        },
      ]}
    />
  );
}

export function TunnelProbePanel({
  controls,
  windowSecs,
  onWindowChange,
}: {
  controls: TunnelProbeControls;
  windowSecs: number;
  onWindowChange: (windowSecs: number) => void;
}) {
  const { probe, capability, view, status, published, active, start, cancel, available, editable } = controls;
  const [policyOpen, setPolicyOpen] = useState(false);
  // The probe target is Console configuration; only operators who may change policies read it.
  const inventory = useQuery({
    queryKey: ['tunnel-probes'],
    queryFn: fetchTunnelProbes,
    staleTime: 60_000,
    enabled: editable,
    retry: false,
  });
  const chart = useTunnelProbeChart();
  if (probe.isPending) return <Loading variant="panel" />;
  const missing = probe.error instanceof ApiError && probe.error.status === 404;
  if (probe.error && !missing) return <ErrorBox error={probe.error} />;
  return (
    <section className="panel config-panel cg-sec tnd-probe">
      <header>
        <PanelTitle of="observe">拨测</PanelTitle>
      </header>
      {active ? <ProbeProgress run={active} source={active.source} /> : <Verdict status={status} view={view} />}
      {editable && capability.data && !available && (
        <p className="tnd-notice">{capability.data.reason ?? 'Console 拨测组件不可用'}</p>
      )}
      {capability.error && <ErrorBox error={capability.error} />}
      {(start.error || cancel.error) && <ErrorBox error={start.error ?? cancel.error} />}
      {published && (
        <div className="tnd-window-controls">
          <span>统计范围</span>
          <SegmentedControl
            className="tnd-seg"
            value={windowSecs}
            options={WINDOWS}
            onChange={onWindowChange}
            ariaLabel="拨测统计范围"
          />
        </div>
      )}
      <Metrics view={view} status={status} windowSecs={view?.summary.window_secs ?? windowSecs} />
      {!published ? (
        <p className="tnd-chart-note">发布后开始定时拨测</p>
      ) : chart.status === 'ready' ? (
        <chart.Chart
          points={view!.points}
          windowSecs={view!.summary.window_secs}
          endUnixSecs={Math.floor(probe.dataUpdatedAt / 1000)}
          bucketSecs={view!.bucket_secs}
          intervalSecs={view!.item.policy?.interval_secs ?? null}
          p50Ms={view!.summary.p50_ms}
          p95Ms={view!.summary.p95_ms}
          slowMs={view!.slow_ttfb_ms}
        />
      ) : chart.status === 'error' ? (
        <ErrorBox error={chart.error} />
      ) : (
        <div className="tnd-chart" aria-hidden />
      )}
      <RunRows
        runs={view?.recent_runs ?? []}
        intervalSecs={view?.item.policy?.interval_secs ?? null}
        slowMs={view?.slow_ttfb_ms ?? 300}
        retentionDays={view?.retention_days ?? 7}
      />
      <PolicyLine
        item={view?.item ?? null}
        published={published}
        endpointUrl={inventory.data?.endpoint_url ?? null}
        editable={editable}
        onEdit={() => setPolicyOpen(true)}
      />
      {policyOpen && view && <TunnelProbePolicyDialog item={view.item} onClose={() => setPolicyOpen(false)} />}
    </section>
  );
}
