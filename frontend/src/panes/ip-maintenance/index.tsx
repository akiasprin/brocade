import { useState, type ReactNode } from 'react';
import type { NodeAgentStateItem } from '../../api';
import { ErrorBox, SegSwitch } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon, PanelTitle } from '../../ui/icons';
import { RhythmLegend, Rhythm, RoundRows, useDayRuns, useDayStats, useRoundSheet } from './kit';
import { PolicyGroups, SaveBar } from './policy';
import { ACTION_NAME, CareProvider, dayClock, fmtMiB, mmss, successfulActions, useCare, useRuns } from './state';
import { countryName } from './regions-zh';
import './styles.css';

function Trail({ children, onClick }: { children: ReactNode; onClick: () => void }) {
  return (
    <button type="button" className="care-config-trail" onClick={onClick}>
      {children}
      <Icon of="chevronLeft" size={11} className="care-chevron" />
    </button>
  );
}

function RecordsDialog({ onClose }: { onClose: () => void }) {
  const care = useCare();
  const day = useDayRuns();
  const stats = useDayStats();
  const runs = useRuns(care.node.node_id, care.baseline?.session_generation ?? 0);
  const sheet = useRoundSheet();
  const list = runs.data?.pages.flatMap(page => page.runs) ?? [];
  const { usage } = care;
  return (
    <>
      <DialogLayer label="养护记录" onClose={onClose}>
        <section className="dialog-surface care-sheet care-all care-records-sheet">
          <header className="care-sheet-head plain">
            <div>
              <h2>养护记录</h2>
              <span className="care-muted">
                24 小时 <b className="care-record-count">{stats.rounds ?? '—'}</b> 轮
                {stats.issues > 0 && ` · ${stats.issues} 轮未完成`}
              </span>
            </div>
            <DialogClose className="care-sheet-close" aria-label="关闭">
              <Icon of="close" size={15} />
            </DialogClose>
          </header>
          <div className="care-sheet-body">
            <div className="care-records-chart">
              <div className="care-records-today">
                <span className="care-records-usage">
                  今日 <b>{usage.known ? usage.requests.toLocaleString() : '—'}</b> /{' '}
                  {usage.requestLimit.toLocaleString()} 次
                  <i />
                  <b>{usage.known ? fmtMiB(usage.bytes) : '—'}</b> / {Math.round(usage.byteLimit / 1048576)} MiB
                </span>
                <RhythmLegend />
              </div>
              <Rhythm height={84} />
            </div>
            {day.error && <ErrorBox error={day.error} />}
            {runs.error ? (
              <ErrorBox error={runs.error} />
            ) : runs.isPending ? (
              <p className="care-empty">正在读取记录…</p>
            ) : (
              <RoundRows runs={list} onOpen={sheet.open} />
            )}
            {runs.hasNextPage && (
              <button className="care-more" disabled={runs.isFetchingNextPage} onClick={() => runs.fetchNextPage()}>
                {runs.isFetchingNextPage ? '读取中…' : '更早的记录'}
              </button>
            )}
          </div>
        </section>
      </DialogLayer>
      {sheet.sheet}
    </>
  );
}

function MaintenanceSummary() {
  const care = useCare();
  const [policy, setPolicy] = useState(false);
  const [records, setRecords] = useState(false);
  const saved = care.baseline?.policy;
  const city = care.place?.city ?? (care.first && care.defaultCountry ? countryName(care.defaultCountry) : null);
  const interval = saved?.interval_minutes ?? 45;
  const frequency = interval >= 60 && interval % 60 === 0 ? `${interval / 60} 小时` : `${interval} 分钟`;
  const plan = !saved?.region
    ? (care.defaultRegionNotice ?? '尚未选择目标地区')
    : `${city ?? '—'} · 每 ${frequency}一轮`;
  const describe = (state: (typeof care.familyStates)[number]) => {
    const { live, report, active, first, confirmed } = state;
    let status = live.title;
    let detail = live.detail;
    // Offline and pending receipts retain their own status even when the last report is active.
    if (active && report && live.tone === 'run') {
      const current = report.actions.find(action => action.index === report.action_index);
      const name = current ? (ACTION_NAME[current.kind] ?? current.kind) : null;
      status = `正在养护 ${report.action_index} / ${report.actions_total}`;
      detail =
        report.phase === 'dwelling' && name && live.countdown !== null
          ? `${name} · 停留 ${mmss(live.countdown)}`
          : report.phase === 'requesting' && name
            ? `正在访问 ${name}`
            : live.title;
    } else if (!first && !care.enabled && confirmed && (live.tone === 'off' || live.tone === 'idle')) {
      status = report?.last_started_at_unix_secs
        ? `上一轮 ${dayClock(report.last_started_at_unix_secs, care.now)}`
        : '空闲';
      detail = null;
    }

    return { status, detail };
  };
  const summary = (() => {
    const states = care.familyStates;
    const current = states.find(item => item.family === care.displayFamily)!;
    const show = (state: typeof current) => ({ ...describe(state), tone: state.live.tone });
    if (!care.dual) return show(current);
    // Pending and unconfirmed receipts take precedence over apparent progress in the other family.
    const uncertain = states.find(
      state => state.live.tone === 'unknown' || ['机器已停用', '需要升级 Agent'].includes(state.live.title),
    );
    if (uncertain) return show(uncertain);
    if (care.pending.stop) return { status: '正在停止', detail: '等待节点确认', tone: 'wait' } as const;
    const pending = states.find(state => state.sessionPending || state.policyPending);
    if (pending || care.pending.manual || care.actions.busy) return show(pending ?? current);
    const running = states.filter(state => state.active);
    if (running.length)
      return {
        status: '正在养护',
        detail: running.length > 1 ? '双栈并发执行中' : describe(running[0]).detail,
        tone: 'run',
      } as const;
    const issue = states.find(state => state.live.tone === 'err' || state.live.tone === 'warn');
    if (issue) {
      const partial =
        issue.live.tone === 'err' && states.some(state => state.report && successfulActions(state.report) > 0);
      return partial
        ? ({ status: '部分成功', detail: '部分动作未完成，请查看记录', tone: 'warn' } as const)
        : show(issue);
    }
    if (states.every(state => state.first)) return show(current);
    if (!care.enabled && care.confirmed) return { status: '空闲', detail: null, tone: 'off' } as const;
    const next = states
      .filter(state => state.live.countdown !== null)
      .sort((a, b) => a.live.countdown! - b.live.countdown!)[0];
    return show(next ?? current);
  })();
  const { status, detail, tone } = summary;
  const runButton = (
    <button
      type="button"
      className="btn"
      disabled={!care.editable || !!care.actions.runBlocked || care.actions.busy}
      title={care.actions.runBlocked ?? undefined}
      onClick={care.actions.run}
    >
      立即执行
    </button>
  );

  return (
    <section className="panel config-panel care-config" aria-busy={!care.view && !care.error}>
      <header>
        <PanelTitle of="maintenance">IP 养护</PanelTitle>
      </header>
      {(care.error || care.actions.error || care.edit.error) && (
        <ErrorBox error={care.error ?? care.actions.error ?? care.edit.error} />
      )}
      <div className="fgrid one">
        <div className="row">
          <span className="k">自动养护</span>
          <span className="v">
            <span className="care-config-line">
              <SegSwitch
                checked={care.actions.enabledShown}
                disabled={!care.editable || !care.view?.supported || !saved?.region || care.actions.busy}
                onChange={care.actions.setEnabled}
                off="关闭"
                on="开启"
                ariaLabel="自动养护"
              />
              <span className="sp" />
              <Trail onClick={() => setPolicy(true)}>策略</Trail>
            </span>
            <span className="sub">{plan}</span>
          </span>
        </div>
        <div className="row">
          <span className="k">状态</span>
          <span className="v">
            <span className="care-config-line care-config-state" data-tone={tone}>
              {care.anyActive ? (
                <button
                  type="button"
                  className="btn"
                  disabled={!care.actions.canStop}
                  onClick={() => care.actions.stop()}
                >
                  {care.pending.stop ? '正在停止' : '停止'}
                </button>
              ) : (
                runButton
              )}
              <b role="status">{status}</b>
              <span className="sp" />
              <Trail onClick={() => setRecords(true)}>记录</Trail>
            </span>
            {detail && <span className="sub">{detail}</span>}
          </span>
        </div>
      </div>
      {policy && (
        <DialogLayer label="养护策略" onClose={() => setPolicy(false)}>
          <section className="dialog-surface care-sheet care-policy-sheet">
            <header className="care-sheet-head plain">
              <div>
                <h2>养护策略</h2>
                {!care.first && <span className="care-muted">{care.pending.policy ? '等待节点确认' : '已应用'}</span>}
              </div>
              <DialogClose className="care-sheet-close" aria-label="关闭">
                <Icon of="close" size={15} />
              </DialogClose>
            </header>
            <div className="care-sheet-body">
              <PolicyGroups />
            </div>
            <SaveBar />
          </section>
        </DialogLayer>
      )}
      {records && <RecordsDialog onClose={() => setRecords(false)} />}
    </section>
  );
}

export function IpMaintenanceCard({ node, canEdit }: { node: NodeAgentStateItem; canEdit: boolean }) {
  return (
    <CareProvider node={node} readOnly={!canEdit}>
      <MaintenanceSummary />
    </CareProvider>
  );
}
