import { useState, type CSSProperties, type ReactNode } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  fetchIpMaintenance,
  fetchIpMaintenanceRun,
  fetchIpMaintenanceRuns,
  type IpMaintenanceReport,
  type IpMaintenanceView,
} from '../../api';
import { ErrorBox } from '../../ui/bits';
import { Icon, type IconName } from '../../ui/icons';
import { RegionFlag } from '../../ui/region-flag';
import { SiteIcon } from '../../ui/site-icon';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Glyph } from '../../panes/unlock-status';
import {
  ACTIVE,
  ACTION_NAME,
  EVIDENCE_SOURCES,
  REASON,
  clock,
  dayClock,
  fmtMiB,
  mmss,
  span,
  useCare,
  roundResult,
  successfulActions,
  actionWasSkipped,
  type Tone,
} from './state';
import { countryName, isoOf } from './regions-zh';

/* ── 基础件 ── */
export function Card({
  title,
  icon,
  meta,
  className,
  children,
}: {
  title?: ReactNode;
  icon?: IconName;
  meta?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={`care-card${className ? ` ${className}` : ''}`}>
      {title && (
        <header className="care-card-head">
          <h3>
            {icon && <Icon of={icon} size={14} />}
            {title}
          </h3>
          {meta && <span className="care-card-meta">{meta}</span>}
        </header>
      )}
      <div className="care-card-body">{children}</div>
    </section>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <label className={`switch${disabled ? ' disabled' : ''}`}>
      <input
        type="checkbox"
        role="switch"
        aria-label={label}
        aria-checked={checked}
        checked={checked}
        disabled={disabled}
        onChange={event => onChange(event.target.checked)}
      />
      <span className="switch-ui" aria-hidden="true">
        <span />
      </span>
    </label>
  );
}

export function Flag({ code, size = 16 }: { code: string | null | undefined; size?: number }) {
  return (
    <span className="care-flag" style={{ width: size, height: size }}>
      <RegionFlag code={code ? isoOf(code) : null} square />
    </span>
  );
}

/* ── 旗帜 + 进度环：身份与节奏放在同一个形状里。进行中画本轮进度，等待时画倒计时。 ── */
export function FlagRing({
  code,
  ring,
  tone,
  size = 64,
}: {
  code: string | null | undefined;
  ring: number | null;
  tone: Tone;
  size?: number;
}) {
  const r = 46;
  const c = 2 * Math.PI * r;
  return (
    <span className="care-flagring" data-tone={tone} style={{ width: size, height: size }}>
      <svg viewBox="0 0 100 100" aria-hidden="true">
        <circle className="track" cx="50" cy="50" r={r} />
        {ring !== null && (
          <circle
            className="arc"
            cx="50"
            cy="50"
            r={r}
            strokeDasharray={`${Math.max(0.001, ring) * c} ${c}`}
            transform="rotate(-90 50 50)"
          />
        )}
      </svg>
      <span className="care-flagring-flag">
        <RegionFlag code={code ? isoOf(code) : null} square />
      </span>
    </span>
  );
}

export function StatusDot({ tone }: { tone: Tone }) {
  return <i className="care-dot" data-tone={tone} aria-hidden="true" />;
}

/* ── 本轮动作：每个动作一段，当前停留的那段按剩余时间填满。 ── */
export function RoundTrack({ report, fill }: { report: IpMaintenanceReport; fill: number }) {
  const total = Math.max(report.actions_total, 1);
  return (
    <span className="care-track" role="presentation" style={{ gridTemplateColumns: `repeat(${total}, 1fr)` }}>
      {Array.from({ length: total }, (_, n) => {
        const index = n + 1;
        const action = report.actions.find(a => a.index === index);
        const skipped = actionWasSkipped(action?.error);
        const failed = !!action && !skipped && (action.error != null || (action.status ?? 0) >= 400);
        const state = skipped
          ? 'skip'
          : failed
            ? 'fail'
            : index < report.action_index
              ? 'done'
              : index === report.action_index
                ? report.phase === 'requesting'
                  ? 'now'
                  : report.phase === 'dwelling'
                    ? 'dwell'
                    : 'done'
                : report.phase === 'postcheck' || report.phase === 'complete'
                  ? 'done'
                  : 'todo';
        return (
          <i
            key={index}
            data-state={state}
            style={state === 'dwell' ? ({ '--p': `${Math.round(fill * 100)}%` } as CSSProperties) : undefined}
          />
        );
      })}
    </span>
  );
}

export function useDwellFill(): number {
  const care = useCare();
  const report = care.report;
  if (!report || report.phase !== 'dwelling' || care.live.countdown === null) return 0;
  /* live.ring 已经包含停留比例，这里从它反推当前段的填充。 */
  const total = Math.max(report.actions_total, 1);
  const within = ((care.live.ring ?? 0) * total - (report.action_index - 1) - 0.3) / 0.7;
  return Math.min(1, Math.max(0, within));
}

/* 状态行：大字是结论，小字是位置或原因；倒计时用等宽数字，放在结论之后。 */
export function LiveText({ size = 'm' }: { size?: 'm' | 'l' }) {
  const { live } = useCare();
  return (
    <span className={`care-live-text ${size}`} role="status">
      <b>
        {live.title}
        {live.countdown !== null && live.tone === 'run' && <em>{mmss(live.countdown)}</em>}
      </b>
      {live.detail && <span>{live.detail}</span>}
    </span>
  );
}

export function RunButton({ compact = false }: { compact?: boolean }) {
  const care = useCare();
  if (care.active)
    return (
      <button className="btn care-btn" disabled={!care.actions.canStop} onClick={() => care.actions.stop()}>
        <Icon of="pause" size={13} />
        {care.pending.stop ? '正在停止' : compact ? '停止' : '停止本轮'}
      </button>
    );
  return (
    <button
      className="btn primary care-btn"
      disabled={!care.editable || !!care.actions.runBlocked || care.actions.busy}
      title={care.actions.runBlocked ?? undefined}
      onClick={care.actions.run}
    >
      <Icon of="play" size={12} />
      {compact ? '执行' : '立即执行'}
    </button>
  );
}

/* ── 识别结果：沿用解锁页的「旗 + 两字码 + 国名」读法。 ── */
export function EvidenceRows({ compact = false }: { compact?: boolean }) {
  const care = useCare();
  return (
    <ul className={`care-evidence${compact ? ' compact' : ''}`}>
      {care.evidence.map(item => {
        const code = item.country ? isoOf(item.country) : null;
        const tone = !item.known ? 'unknown' : !code ? 'unknown' : code === care.targetCountry ? 'ok' : 'part';
        return (
          <li key={item.source} data-tone={tone}>
            <span className="care-evidence-mark">
              <SiteIcon siteId={item.mark} />
            </span>
            <span className="care-evidence-name">{item.name}</span>
            <span className="care-evidence-geo">
              {code ? (
                <>
                  <RegionFlag code={code} />
                  <b>{code}</b>
                  {!compact && <small>{countryName(code)}</small>}
                </>
              ) : (
                <small>{item.known ? '未识别' : '—'}</small>
              )}
            </span>
            <Glyph tone={tone === 'ok' ? 'ok' : tone === 'part' ? 'part' : 'unknown'} />
          </li>
        );
      })}
    </ul>
  );
}

export function EvidenceProgress() {
  const care = useCare();
  const matched = care.evidence.filter(item => item.country && isoOf(item.country) === care.targetCountry).length;
  const allKnown = care.evidence.every(item => item.known && item.country);
  const description = allKnown
    ? matched === care.evidence.length
      ? '符合预期'
      : matched === 2
        ? '一步之遥'
        : '积于跬步'
    : null;
  return (
    <div className="care-figure care-location-progress">
      <b>
        {matched}
        <small>/{care.evidence.length}</small>
      </b>
      {description && <span>{description}</span>}
    </div>
  );
}

/* 两个大数字并排：读数在上，细条与上限在下。 */
export function UsageFigures() {
  const { usage } = useCare();
  const figs = [
    {
      key: 'req',
      label: '请求',
      value: usage.known ? usage.requests.toLocaleString() : '—',
      unit: '次',
      limit: `上限 ${usage.requestLimit.toLocaleString()}`,
      ratio: usage.requestLimit ? usage.requests / usage.requestLimit : 0,
    },
    {
      key: 'body',
      label: '流量',
      value: usage.known ? fmtMiB(usage.bytes) : '—',
      unit: 'MiB',
      limit: `上限 ${Math.round(usage.byteLimit / 1048576)} MiB`,
      ratio: usage.byteLimit ? usage.bytes / usage.byteLimit : 0,
    },
  ];
  return (
    <div className="care-usage-figs">
      {figs.map(fig => (
        <div key={fig.key} className="care-usage-fig" data-full={fig.ratio >= 1 || undefined}>
          <span className="care-usage-label">{fig.label}</span>
          <span className="care-usage-number">
            <b>{fig.value}</b>
            <small>{fig.unit}</small>
          </span>
          <span className="care-usage-bar">
            <i style={{ width: `${Math.min(100, Math.max(fig.ratio > 0 ? 1.5 : 0, fig.ratio * 100))}%` }} />
          </span>
          <span className="care-usage-limit">
            {usage.known ? `${Math.round(fig.ratio * 100)}%` : '—'} · {fig.limit}
          </span>
        </div>
      ))}
    </div>
  );
}

/* ── 24 小时节律 ── */
export function useDayRuns() {
  const care = useCare();
  const node = care.node.node_id;
  return useQuery({
    queryKey: ['node-ip-maintenance-day', node, care.baseline?.session_generation ?? 0],
    staleTime: 30_000,
    refetchInterval: 60_000,
    queryFn: async ({ signal }) => {
      const since = Date.now() / 1000 - 86400;
      const runs: IpMaintenanceReport[] = [];
      let cursor: number | null = null;
      // Five pages cover the machine's 100 retained rounds, including both address families.
      // Stop at that bound while an older Console is still retaining more history.
      for (let page = 0; page < 5; page++) {
        const chunk = await fetchIpMaintenanceRuns(node, cursor, signal);
        runs.push(...chunk.runs);
        if (chunk.next_cursor !== null && chunk.next_cursor === cursor) throw new Error('养护记录分页没有前进');
        cursor = chunk.next_cursor;
        const last = chunk.runs.at(-1);
        if (cursor === null || (last && (last.last_started_at_unix_secs ?? last.observed_at_unix_secs) < since))
          return runs.filter(r => (r.last_started_at_unix_secs ?? r.observed_at_unix_secs) >= since);
      }
      return runs.filter(r => (r.last_started_at_unix_secs ?? r.observed_at_unix_secs) >= since);
    },
  });
}

/* 按小时堆叠，读法同「屏幕使用时间」：柱高是这一小时完成的动作数，下段 Google、上段地区站点，
   中断的动作是灰段；冷却覆盖的小时铺一层淡金斜纹。 */
interface HourBucket {
  start: number;
  google: number;
  trust: number;
  stopped: number;
  cool: boolean;
  live: boolean;
}
export function useHourBuckets(): HourBucket[] {
  const care = useCare();
  const day = useDayRuns();
  const top = new Date(care.now);
  top.setMinutes(0, 0, 0);
  const first = top.getTime() / 1000 - 23 * 3600;
  const buckets: HourBucket[] = Array.from({ length: 24 }, (_, i) => ({
    start: first + i * 3600,
    google: 0,
    trust: 0,
    stopped: 0,
    cool: false,
    live: false,
  }));
  const slot = (secs: number) => Math.floor((secs - first) / 3600);
  for (const r of day.data ?? []) {
    const at = r.last_started_at_unix_secs ?? r.observed_at_unix_secs;
    const bucket = buckets[slot(at)];
    if (r.phase === 'cooldown' && r.cooldown_until_unix_secs)
      for (let i = Math.max(0, slot(at)); i <= Math.min(23, slot(r.cooldown_until_unix_secs)); i++)
        buckets[i].cool = true;
    if (!bucket) continue;
    if (ACTIVE.includes(r.phase)) continue;
    if (r.phase === 'complete') bucket[r.module === 'trust' ? 'trust' : 'google'] += successfulActions(r);
    else bucket.stopped += Math.max(1, r.action_index);
  }
  const live = care.active ? care.report : null;
  if (live?.last_started_at_unix_secs) {
    const bucket = buckets[slot(live.last_started_at_unix_secs)];
    if (bucket) {
      bucket[live.module === 'trust' ? 'trust' : 'google'] += live.action_index;
      bucket.live = care.confirmed;
    }
  }
  return buckets;
}

export function Rhythm({ height = 72, axis = true }: { height?: number; axis?: boolean }) {
  const buckets = useHourBuckets();
  const day = useDayRuns();
  const max = Math.max(12, ...buckets.map(b => b.google + b.trust + b.stopped));
  const pct = (n: number) => `${(n / max) * 100}%`;
  if (day.error) return <ErrorBox error={day.error} />;
  if (day.isPending) return <p className="care-empty">正在读取记录…</p>;
  return (
    <div className="care-rhythm">
      <div className="care-rhythm-plot" style={{ height }}>
        <span className="care-rhythm-grid" aria-hidden="true">
          <i />
          <i />
          <i />
        </span>
        <div className="care-rhythm-cols">
          {buckets.map(b => (
            <span
              key={b.start}
              className="care-rhythm-col"
              data-cool={b.cool || undefined}
              data-live={b.live || undefined}
              title={`${clock(b.start)}–${clock(b.start + 3600)} · Google ${b.google} · 地区站点 ${b.trust}${b.stopped ? ` · 中断 ${b.stopped}` : ''}${b.cool ? ' · 冷却' : ''}`}
            >
              <span className="care-rhythm-stack" style={{ height: pct(b.google + b.trust + b.stopped) }}>
                {b.stopped > 0 && <i data-kind="stopped" style={{ flexGrow: b.stopped }} />}
                {b.trust > 0 && <i data-kind="trust" style={{ flexGrow: b.trust }} />}
                {b.google > 0 && <i data-kind="google" style={{ flexGrow: b.google }} />}
              </span>
            </span>
          ))}
        </div>
      </div>
      {axis && (
        <div className="care-rhythm-axis" aria-hidden="true">
          {buckets.map((b, i) => (
            <span key={b.start}>
              {i % 6 === 0 ? `${new Date(b.start * 1000).getHours()}时` : i === 23 ? '现在' : ''}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

export function RhythmLegend() {
  return (
    <span className="care-legend">
      <span>
        <i data-module="google" />
        Google
      </span>
      <span>
        <i data-module="trust" />
        地区站点
      </span>
    </span>
  );
}

export function useDayStats() {
  const day = useDayRuns();
  const runs = day.data ?? [];
  return {
    rounds: day.data ? runs.length : null,
  };
}

/* ── 记录 ── */

export function ModuleTile({ module }: { module: IpMaintenanceReport['module'] }) {
  return (
    <span className="care-module" data-module={module ?? 'google'}>
      {module === 'trust' ? <Icon of="region" size={13} /> : <SiteIcon siteId="google" />}
    </span>
  );
}

export function RoundRows({
  runs,
  limit,
  onOpen,
}: {
  runs: IpMaintenanceReport[];
  limit?: number;
  onOpen: (run: IpMaintenanceReport) => void;
}) {
  const care = useCare();
  const shown = limit ? runs.slice(0, limit) : runs;
  if (!shown.length) return <p className="care-empty">还没有执行记录</p>;
  return (
    <ul className="care-rounds">
      {shown.map(run => {
        const result = roundResult(run);
        const startedAt = run.last_started_at_unix_secs ?? run.observed_at_unix_secs;
        return (
          <li key={run.round_id}>
            <button type="button" onClick={() => onOpen(run)}>
              <time>{dayClock(startedAt, care.now)}</time>
              <ModuleTile module={run.module} />
              <span className="care-round-title">
                {run.module === 'trust' ? '地区站点' : 'Google'}
                {run.family && <small>{run.family === 'v6' ? 'IPv6' : 'IPv4'}</small>}
                <small className={result.text ? 'care-round-result' : undefined} data-tone={result.tone}>
                  {result.text ||
                    (run.action_index === run.actions_total
                      ? `${run.actions_total} 个动作`
                      : `${run.action_index} / ${run.actions_total} 个动作`)}
                </small>
              </span>
              <span className="care-round-geo">
                {run.evidence.map(e => (
                  <Flag key={e.source} code={e.country} size={13} />
                ))}
              </span>
              <Icon of="chevronLeft" size={12} className="care-chevron" />
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function RoundSheet({ run: initial, onClose }: { run: IpMaintenanceReport; onClose: () => void }) {
  const care = useCare();
  const client = useQueryClient();
  const session = care.baseline?.session_generation ?? 0;
  const viewKey = ['node-ip-maintenance', care.node.node_id];
  const obsolete = () => {
    const latest = client.getQueryData<IpMaintenanceView>(viewKey);
    return !!latest && latest.desired.session_generation !== session;
  };
  const detail = useQuery({
    queryKey: ['node-ip-maintenance-run', care.node.node_id, session, initial.round_id],
    queryFn: async ({ signal }) => {
      if (obsolete()) return initial;
      try {
        return await fetchIpMaintenanceRun(care.node.node_id, initial.round_id!, signal);
      } catch (error) {
        if (!(error instanceof ApiError) || error.status !== 404) throw error;
        // Another window may reset while this detail refresh is in flight. Confirm that
        // session change before dismissing its deleted round; other 404s remain errors.
        await client.fetchQuery({
          queryKey: viewKey,
          queryFn: ({ signal }) => fetchIpMaintenance(care.node.node_id, signal),
          staleTime: 0,
        });
        if (obsolete()) return initial;
        throw error;
      }
    },
    enabled: !!initial.round_id,
    // Event hints normally refresh this query; polling covers a disconnected event stream.
    refetchInterval: query => (ACTIVE.includes((query.state.data ?? initial).phase) ? 15_000 : false),
  });
  const run = detail.data ?? initial;
  const result = roundResult(run);
  const startedAt = run.last_started_at_unix_secs ?? run.observed_at_unix_secs;
  return (
    <DialogLayer label="养护轮次" onClose={onClose}>
      <section className="dialog-surface care-sheet">
        <header className="care-sheet-head">
          <ModuleTile module={run.module} />
          <div>
            <h2>
              {dayClock(startedAt, care.now)} · {run.module === 'trust' ? '地区站点' : 'Google'}
              {run.family ? ` · ${run.family === 'v6' ? 'IPv6' : 'IPv4'}` : ''}
            </h2>
            <span className="care-round-result" data-tone={result.tone}>
              {result.tone !== 'idle' && <StatusDot tone={result.tone} />}
              {result.text && `${result.text} · `}用时 {span(run.observed_at_unix_secs - startedAt)}
            </span>
          </div>
          <DialogClose className="care-sheet-close" aria-label="关闭">
            <Icon of="close" size={15} />
          </DialogClose>
        </header>
        <div className="care-sheet-body">
          {detail.error && <ErrorBox error={detail.error} />}
          <ol className="care-steps">
            {Array.from({ length: run.actions_total }, (_, n) => {
              const action = run.actions.find(a => a.index === n + 1);
              const skipped = actionWasSkipped(action?.error);
              const failed = !!action && !skipped && (action.error != null || (action.status ?? 0) >= 400);
              return (
                <li
                  key={n}
                  data-state={!action || skipped ? 'skip' : failed ? 'fail' : action.status ? 'done' : 'unknown'}
                >
                  <span className="care-step-dot">{n + 1}</span>
                  <span className="care-step-name">
                    {action ? (ACTION_NAME[action.kind] ?? action.kind) : '未执行'}
                    {action && <small>{action.host}</small>}
                  </span>
                  <span className="care-step-meta">
                    {!action
                      ? ''
                      : action.error != null
                        ? (REASON[action.error] ?? '请求失败')
                        : action.status
                          ? `${action.status} · ${Math.round(action.response_bytes / 1024)} KB`
                          : '结果待确认'}
                  </span>
                </li>
              );
            })}
          </ol>
          {run.evidence.length > 0 && (
            <div className="care-sheet-evidence">
              <h4>地区取证</h4>
              <ul className="care-evidence compact">
                {EVIDENCE_SOURCES.map(item => {
                  const hit = run.evidence.find(e => e.source === item.source);
                  const code = hit?.country ? isoOf(hit.country) : null;
                  const tone = !code ? 'unknown' : code === care.targetCountry ? 'ok' : 'part';
                  return (
                    <li key={item.source} data-tone={tone}>
                      <span className="care-evidence-mark">
                        <SiteIcon siteId={item.mark} />
                      </span>
                      <span className="care-evidence-name">{item.name}</span>
                      <span className="care-evidence-geo">
                        {code ? (
                          <>
                            <RegionFlag code={code} />
                            <b>{code}</b>
                          </>
                        ) : (
                          <small>未识别</small>
                        )}
                      </span>
                      <Glyph tone={tone === 'ok' ? 'ok' : tone === 'part' ? 'part' : 'unknown'} />
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
        <footer className="care-sheet-foot">
          <span>{run.round_id}</span>
          <span>出口 {run.actual_exit_ip ?? '—'}</span>
          <span>策略 v{run.run_generation}</span>
        </footer>
      </section>
    </DialogLayer>
  );
}

export function useRoundSheet() {
  const care = useCare();
  const session = care.baseline?.session_generation ?? 0;
  const [selection, setSelection] = useState<{ run: IpMaintenanceReport; session: number } | null>(null);
  return {
    open: (run: IpMaintenanceReport) => setSelection({ run, session }),
    sheet:
      selection?.session === session ? <RoundSheet run={selection.run} onClose={() => setSelection(null)} /> : null,
  };
}
