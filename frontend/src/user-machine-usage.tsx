import { useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { fetchUserMachineUsage, type SiteUsagePeriod, type UserMachineUsage, type UserMachineUsageRow } from './api';
import { NodeTrafficDialog } from './node-traffic';
import { SegmentedControl } from './ui/bits';
import { bytes, shareOf } from './ui/format';
import { PanelTitle } from './ui/icons';
import { useMetricQuery } from './ui/metric-cadence';
import { useNodeCountries } from './ui/node-name';
import { RegionFlag } from './ui/region-flag';
import './user-machine-usage.css';

const PERIODS = [
  { value: 'day', label: '当天' },
  { value: 'week', label: '当周' },
  { value: 'month', label: '当月' },
] as const;
const SCOPE =
  '统计用户连接的入口机器：认证代理接入与端口转发，不含中继、落地。按 UTC+8 自然日、周一起的自然周和自然月；与用量账本一致，按记账窗口起点归属。';
const local = (at: number) => new Date((at + 8 * 3600) * 1000);
const md = (at: number) => `${local(at).getUTCMonth() + 1}/${local(at).getUTCDate()}`;
const hm = (at: number) =>
  `${String(local(at).getUTCHours()).padStart(2, '0')}:${String(local(at).getUTCMinutes()).padStart(2, '0')}`;
const totalOf = (row: UserMachineUsageRow) => row.uplink_bytes + row.downlink_bytes;

type Cell = {
  at: number;
  up: number;
  down: number;
  forwarding: number;
  gap: boolean;
  observed: boolean;
  future: boolean;
  historical: boolean;
};
export function machineCells(data: UserMachineUsage, row: UserMachineUsageRow, narrow: boolean): Cell[] {
  const per = data.period === 'week' && narrow ? 24 : 1;
  const step = data.bucket_secs * per;
  const count = Math.round((data.period_end_unix_secs - data.range_start_unix_secs) / step);
  const points = new Map(row.points.map(point => [point[0], point]));
  const historicalDays = new Set(
    data.rows
      .find(item => item.node_id === null)
      ?.points.filter(point => point[1] + point[2] > 0)
      .map(point => point[0]) ?? [],
  );
  return Array.from({ length: count }, (_, index) => {
    const at = data.range_start_unix_secs + index * step;
    const cell: Cell = {
      at,
      up: 0,
      down: 0,
      forwarding: 0,
      gap: false,
      observed: row.node_id === null && data.period === 'month',
      future: at >= data.range_end_unix_secs,
      historical: row.node_id !== null && data.period === 'month' && historicalDays.has(at),
    };
    for (let hour = 0; hour < per; hour++) {
      const start = at + hour * data.bucket_secs;
      if (start >= data.range_end_unix_secs) continue;
      const point = points.get(start);
      cell.up += point?.[1] ?? 0;
      cell.down += point?.[2] ?? 0;
      cell.forwarding += point?.[3] ?? 0;
      cell.observed ||= point?.[5] ?? false;
      // An absent bucket is unknown, never an observed zero. Legacy remainder rows have
      // deliberately no hourly placement, so they do not manufacture a midnight spike.
      cell.gap ||= (point?.[4] ?? false) || (row.node_id !== null && !point?.[5]);
    }
    if (cell.historical && cell.up + cell.down === 0) {
      cell.observed = false;
      cell.gap = false;
    }
    return cell;
  });
}

export function machineScale(cells: Cell[]) {
  const peak = Math.max(0, ...cells.filter(cell => !cell.future).map(cell => cell.up + cell.down));
  const raw = Math.max(peak / 128, 64 * 1024);
  const unit = raw >= 1024 ** 3 ? 1024 ** 3 : raw >= 1024 ** 2 ? 1024 ** 2 : 1024;
  const magnitude = 10 ** Math.floor(Math.log10(raw / unit));
  const lead = raw / unit / magnitude;
  const floor = (lead >= 5 ? 5 : lead >= 2 ? 2 : 1) * magnitude * unit;
  return {
    floor,
    peak,
    level: (value: number) =>
      value <= 0 ? 'z' : value < floor ? 't' : `l${1 + Math.min(3, Math.floor(Math.log(value / floor) / Math.log(4)))}`,
  };
}

function Blocks({
  children,
  week,
  count,
  className,
}: {
  children: ReactNode[];
  week: boolean;
  count: number;
  className: string;
}) {
  return (
    <span className={`${className}${week ? ' wk' : ''}`} style={{ '--n': count } as CSSProperties}>
      {week
        ? Array.from({ length: 7 }, (_, day) => (
            <span className="uxm-blk" key={day}>
              {children.slice(day * 24, day * 24 + 24)}
            </span>
          ))
        : children}
    </span>
  );
}

export function UserMachineUsageCard({
  tenantId,
  userId,
  nameOf = id => id,
}: {
  tenantId: string;
  userId: string;
  nameOf?: (id: string) => string;
}) {
  const [period, setPeriod] = useState<SiteUsagePeriod>('day');
  const [narrow, setNarrow] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [usage, setUsage] = useState<{
    tenantId: string;
    userId: string;
    nodeId: string;
    name: string;
    country: string | null;
    period: SiteUsagePeriod;
  } | null>(null);
  const [selection, setSelection] = useState<{
    identity: string;
    row: number;
    index: number;
    anchor: HTMLElement;
    pinned: boolean;
  } | null>(null);
  if (usage && (usage.tenantId !== tenantId || usage.userId !== userId)) setUsage(null);
  const [focused, setFocused] = useState(0);
  const panel = useRef<HTMLElement>(null);
  const tooltipId = useId();
  const countries = useNodeCountries();
  useLayoutEffect(() => {
    const element = panel.current;
    if (!element) return;
    const measure = () => setNarrow(element.getBoundingClientRect().width < 620);
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const query = useMetricQuery({
    queryKey: ['user-machine-usage', tenantId, userId, period],
    queryFn: () => fetchUserMachineUsage(tenantId, userId, period),
    staleTime: 15_000,
    refetchInterval: 30_000,
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[1] === tenantId && previousQuery.queryKey[2] === userId ? previous : undefined,
  });
  const data = query.data;
  const rows = data?.rows ?? [];
  const active = rows.filter(
    row =>
      totalOf(row) > 0 ||
      row.has_gap ||
      (row.expected && (!row.latest_report_unix_secs || row.latest_report_unix_secs < data!.range_end_unix_secs - 300)),
  );
  const idle = rows.filter(row => !active.includes(row));
  const machines = active.filter(row => row.node_id !== null);
  const shown = expanded ? active : [...machines.slice(0, 12), ...active.filter(row => row.node_id === null)];
  const identity = JSON.stringify([
    tenantId,
    userId,
    data?.period,
    data?.range_start_unix_secs,
    narrow,
    shown.map(row => row.node_id),
  ]);
  const selected = selection?.identity === identity ? selection : null;
  const allCells = data ? rows.map(row => machineCells(data, row, narrow)) : [];
  const cells = shown.map(row => allCells[rows.indexOf(row)]);
  const totals =
    allCells[0]?.map((first, index) => ({
      ...first,
      up: allCells.reduce((sum, lane) => sum + lane[index].up, 0),
      down: allCells.reduce((sum, lane) => sum + lane[index].down, 0),
      forwarding: allCells.reduce((sum, lane) => sum + lane[index].forwarding, 0),
      gap: allCells.some(lane => lane[index].gap),
      observed: allCells.some(lane => lane[index].observed),
    })) ?? [];
  const scale = machineScale(allCells.flat());
  const peak = Math.max(1, ...totals.map(cell => cell.up + cell.down));
  const total = rows.reduce((sum, row) => sum + totalOf(row), 0);
  const up = rows.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const down = rows.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const placed = totals.reduce((sum, cell) => sum + cell.up + cell.down, 0);
  const week = data?.period === 'week' && !narrow;
  const step = (data?.bucket_secs ?? 3600) * (data?.period === 'week' && narrow ? 24 : 1);
  const label = (cell: Cell) => (step === 86400 ? md(cell.at) : `${md(cell.at)} ${hm(cell.at)}–${hm(cell.at + step)}`);
  const name = (row: UserMachineUsageRow) => (row.node_id === null ? '未按机器记录' : nameOf(row.node_id));
  const selectedCell = selected ? (selected.row === -1 ? totals : cells[selected.row])?.[selected.index] : undefined;
  const selectedName = selected?.row === -1 ? '合计' : selected && shown[selected.row] ? name(shown[selected.row]) : '';
  const nowAt = data ? (data.range_end_unix_secs - data.range_start_unix_secs) / step : 0;
  const position = (index: number) =>
    week
      ? `calc((100% - 24px) / 7 * ${index / 24} + ${Math.floor(index / 24) * 4}px)`
      : `calc((100% - ${(totals.length - 1) * 2}px) / ${totals.length} * ${index} + ${Math.floor(index) * 2}px)`;
  const nowLine =
    data && nowAt < totals.length ? (
      <i className="uxm-now" style={{ left: position(nowAt) }} aria-hidden="true" />
    ) : null;
  const reveal = (element: HTMLElement, row: number, index: number, pinned = false) => {
    setSelection({
      identity,
      row,
      index,
      anchor: element,
      pinned,
    });
  };
  const buttons = (lane: Cell[], rowIndex: number, bars = false) =>
    lane.map((cell, index) => {
      const value = cell.up + cell.down;
      const unknown = !cell.observed && !cell.future;
      const classes =
        cell.future || (cell.historical && !cell.observed)
          ? 'f'
          : unknown || (cell.gap && value === 0)
            ? 'g'
            : `${scale.level(value)}${cell.gap ? ' gp' : ''}${rowIndex >= 0 && shown[rowIndex].node_id === null ? ' u' : ''}`;
      const machine = rowIndex === -1 ? '合计' : name(shown[rowIndex]);
      const nodeId = rowIndex === -1 ? null : shown[rowIndex].node_id;
      const reading = cell.future
        ? '尚未到来'
        : unknown
          ? cell.historical
            ? '此日流量未完整记录机器归属'
            : '无采集记录'
          : `${bytes(value)}${cell.gap ? '，采集有缺口' : ''}`;
      return (
        <button
          key={cell.at}
          type="button"
          className={bars ? 'uxm-slot' : `uxm-cell ${classes}`}
          tabIndex={index === Math.min(focused, lane.length - 1) ? 0 : -1}
          aria-label={`${machine} · ${label(cell)} · ${reading}${nodeId !== null ? '，查看机器用量' : ''}`}
          aria-haspopup={nodeId !== null ? 'dialog' : undefined}
          aria-describedby={selected?.row === rowIndex && selected.index === index ? tooltipId : undefined}
          onMouseEnter={event => {
            if (!selected?.pinned) reveal(event.currentTarget, rowIndex, index);
          }}
          onMouseLeave={() => {
            if (!selected?.pinned) setSelection(null);
          }}
          onFocus={event => {
            setFocused(index);
            reveal(event.currentTarget, rowIndex, index);
          }}
          onBlur={() => {
            if (!selected?.pinned) setSelection(null);
          }}
          onClick={event => {
            if (nodeId === null) {
              reveal(event.currentTarget, rowIndex, index, true);
              return;
            }
            setSelection(null);
            setUsage({ tenantId, userId, nodeId, name: machine, country: countries(nodeId), period: data!.period });
          }}
          onKeyDown={event => {
            if (event.key === 'Escape') {
              setSelection(null);
              return;
            }
            const next =
              event.key === 'ArrowRight'
                ? Math.min(index + 1, lane.length - 1)
                : event.key === 'ArrowLeft'
                  ? Math.max(0, index - 1)
                  : event.key === 'Home'
                    ? 0
                    : event.key === 'End'
                      ? lane.length - 1
                      : null;
            if (next === null) return;
            event.preventDefault();
            const strip = event.currentTarget.closest('.uxm-strip, .uxm-bars');
            strip?.querySelectorAll<HTMLButtonElement>('button')[next]?.focus();
          }}
        >
          {bars && value > 0 && <b style={{ height: `${Math.max(2, (value / peak) * 100)}%` }} />}
        </button>
      );
    });
  return (
    <section
      ref={panel}
      className="panel config-panel user-dcard uxm-card"
      aria-label="机器流量"
      onKeyDown={event => {
        if (event.key === 'Escape') setSelection(null);
      }}
    >
      <header>
        <span title={SCOPE}>
          <PanelTitle of="nodes">机器流量</PanelTitle>
        </span>
        {data && (
          <span
            className={`rut-status${data.reporting_nodes < data.expected_nodes ? ' warn' : ''}`}
            title="最近 5 分钟有用量上报的入口 / 当前已授权入口；不代表整个范围采集完整"
          >
            <i />
            {data.reporting_nodes} / {data.expected_nodes} 台入口已上报
          </span>
        )}
      </header>
      <div
        className={`uxm-body panel-full-bleed-body uxm-opt-b${narrow ? ' narrow' : ''}`}
        aria-busy={query.isFetching}
      >
        <div className="uxm-b">
          <div className="site-usage-head">
            {data && (
              <>
                <span className="site-usage-total">{bytes(total)}</span>
                <span className="site-usage-label">
                  {md(data.range_start_unix_secs)}
                  {data.period !== 'day' && `–${md(data.range_end_unix_secs)}`} ·{' '}
                  {rows.filter(row => row.node_id !== null && totalOf(row) > 0).length} 台机器
                </span>
                <span className="site-usage-meta">
                  下行 <b>{bytes(down)}</b> · 上行 <b>{bytes(up)}</b>
                </span>
              </>
            )}
            <SegmentedControl ariaLabel="机器统计范围" value={period} options={PERIODS} onChange={setPeriod} />
          </div>
          {query.isPending && <p role="status">正在读取机器统计…</p>}
          {query.isError && (
            <p role="alert">
              {data ? '刷新失败，当前显示上次读取的数据。' : '机器统计读取失败，请稍后重试。'}
              <button className="btn sm" onClick={() => void query.refetch()}>
                重试
              </button>
            </p>
          )}
          {query.isPlaceholderData && (
            <p role="status" className="uxm-idle">
              正在切换范围，暂时显示{PERIODS.find(option => option.value === data?.period)?.label}数据…
            </p>
          )}
          {data && total === 0 && (
            <p className="uxm-empty">
              {data.reporting_nodes > 0 ? '本范围没有记录到流量。' : '尚无用量上报，无法确认本范围是否有流量。'}
            </p>
          )}
          {data && (total > 0 || shown.length > 0) && (
            <div className="uxm-mx" aria-label="机器与时间分布">
              <div className="uxm-row total">
                <span className="uxm-who">
                  <span className="uxm-name">合计</span>
                </span>
                <span className="uxm-lane">
                  <Blocks week={week} count={totals.length} className="uxm-bars">
                    {buttons(totals, -1, true)}
                  </Blocks>
                  {nowLine}
                  <span className="uxm-now-label" style={{ left: position(nowAt) }}>
                    现在
                  </span>
                </span>
                <span />
                <span />
              </div>
              <div className="uxm-row axis">
                <span />
                <span className="uxm-lane">
                  <span className="uxm-axis">
                    {totals.flatMap((cell, index) => {
                      const date = local(cell.at);
                      const show =
                        data.period === 'day'
                          ? date.getUTCHours() % 6 === 0
                          : data.period === 'week'
                            ? date.getUTCHours() === 0
                            : date.getUTCDate() === 1 || date.getUTCDate() % (narrow ? 10 : 5) === 0;
                      return show
                        ? [
                            <span key={cell.at} className="uxm-tick" style={{ left: position(index) }}>
                              {data.period === 'day'
                                ? hm(cell.at)
                                : data.period === 'week' && narrow
                                  ? `周${'日一二三四五六'[date.getUTCDay()]}`
                                  : md(cell.at)}
                            </span>,
                          ]
                        : [];
                    })}
                  </span>
                </span>
                <span />
                <span />
              </div>
              {shown.map((row, index) => (
                <div key={row.node_id ?? 'unassigned'} className={`uxm-row${row.node_id === null ? ' unsplit' : ''}`}>
                  <span className="uxm-who" title={name(row)}>
                    {row.node_id && countries(row.node_id) ? (
                      <span className="nd-idplate uxm-plate">
                        <span className="nd-idplate-clip">
                          <RegionFlag code={countries(row.node_id)!} square />
                        </span>
                      </span>
                    ) : (
                      <span className="uxm-plate-slot" aria-hidden="true" />
                    )}
                    <span className="uxm-name">{name(row)}</span>
                    {row.forwarding_bytes > 0 && <span className="uxm-tag">含转发</span>}
                    {row.expected &&
                      (!row.latest_report_unix_secs ||
                        row.latest_report_unix_secs < data.range_end_unix_secs - 300) && (
                        <span className="uxm-tag warn">未上报</span>
                      )}
                  </span>
                  <span className="uxm-lane">
                    <Blocks week={week} count={cells[index].length} className="uxm-strip">
                      {buttons(cells[index], index)}
                    </Blocks>
                    {nowLine}
                  </span>
                  <span className="uxm-num">{bytes(totalOf(row))}</span>
                  <span className="uxm-pct">{shareOf(totalOf(row), total)}</span>
                </div>
              ))}
            </div>
          )}
          {machines.length > 12 && (
            <button className="btn sm" aria-expanded={expanded} onClick={() => setExpanded(value => !value)}>
              {expanded ? '收起机器' : `展开其余 ${machines.length - 12} 台机器`}
            </button>
          )}
          {idle.length > 0 && (
            <p className="uxm-idle">
              另有 {idle.length} 台机器本范围没有记录到流量：{idle.map(name).join('、')}
            </p>
          )}
          {data && total > placed && (
            <p className="uxm-idle">其中 {bytes(total - placed)} 没有保留小时明细，仅计入合计，不分配到时间格。</p>
          )}
          {rows.some(row => row.node_id === null) && (
            <p className="uxm-idle">“未按机器记录”保留上线前或明细保留期外的账本差额，无法追溯到入口机器。</p>
          )}
          {data && total > 0 && (
            <div className="uxm-legend">
              <span>{step === 86400 ? '每天' : '每小时'}一格 · 各机器同一刻度</span>
              <span className="uxm-scale">
                <span>
                  <i className="uxm-cell z" />
                  未记录流量
                </span>
                <span>
                  <i className="uxm-cell t" />
                  不足 {bytes(scale.floor)}
                </span>
                <span className="uxm-ramp" title="对数刻度，每档 4 倍">
                  {bytes(scale.floor)}
                  {[1, 2, 3, 4].map(level => (
                    <i key={level} className={`uxm-cell l${level}`} />
                  ))}
                  {bytes(scale.peak)}
                </span>
                <span>
                  <i className="uxm-cell g" />
                  无记录 / 缺口
                </span>
              </span>
            </div>
          )}
        </div>
      </div>
      {usage?.tenantId === tenantId && usage.userId === userId && (
        <NodeTrafficDialog
          key={JSON.stringify([tenantId, userId, usage.nodeId])}
          nodeId={usage.nodeId}
          name={usage.name}
          country={usage.country}
          initialPeriod={usage.period}
          onClose={() => setUsage(null)}
        />
      )}
      {selected && selectedCell && (
        <MachineUsageTooltip id={tooltipId} anchor={selected.anchor}>
          <div className="uxm-tip-head">{label(selectedCell)} · UTC+8</div>
          <div className="uxm-tip-name">{selectedName}</div>
          {selectedCell.future ? (
            <p>尚未到来</p>
          ) : !selectedCell.observed ? (
            <p className="uxm-tip-warn">
              {selectedCell.historical ? '此日流量未完整记录机器归属，无法确认本机用量' : '无采集记录，不能视为零流量'}
            </p>
          ) : (
            <>
              <div className="uxm-tip-value">{bytes(selectedCell.up + selectedCell.down)}</div>
              <div className="uxm-tip-row">
                下行 <b>{bytes(selectedCell.down)}</b>
              </div>
              <div className="uxm-tip-row">
                上行 <b>{bytes(selectedCell.up)}</b>
              </div>
              {selectedCell.forwarding > 0 && (
                <div className="uxm-tip-row">
                  含转发 <b>{bytes(selectedCell.forwarding)}</b>
                </div>
              )}
              {selectedCell.gap && <p className="uxm-tip-warn">采集有缺口，仅显示已记录的流量</p>}
              {selectedCell.historical && <p className="uxm-tip-dim">同日另有未按机器记录的流量；此处仅含已归属部分</p>}
              <div className="uxm-tip-dim">按记账窗口归属；有记录不代表持续采集完整</div>
            </>
          )}
          {selected.pinned && (
            <button type="button" className="btn sm" onClick={() => setSelection(null)}>
              关闭读数
            </button>
          )}
        </MachineUsageTooltip>
      )}
    </section>
  );
}

function MachineUsageTooltip({ id, anchor, children }: { id: string; anchor: HTMLElement; children: ReactNode }) {
  const tip = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = tip.current;
    if (!element) return;
    const place = () => {
      const bounds = anchor.getBoundingClientRect();
      const width = element.offsetWidth;
      const height = element.offsetHeight;
      const below = bounds.bottom + 6;
      const top = below + height <= innerHeight - 8 ? below : bounds.top - height - 6;
      element.style.left = `${Math.max(8, Math.min(bounds.left, innerWidth - width - 8))}px`;
      element.style.top = `${Math.max(8, Math.min(top, innerHeight - height - 8))}px`;
    };
    place();
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(place);
    observer?.observe(element);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      observer?.disconnect();
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [anchor]);
  return createPortal(
    <div ref={tip} id={id} role="tooltip" className="uxm-tip uxm-live-tip">
      {children}
    </div>,
    document.body,
  );
}
