// 用量按自然月展示。月份选择只切换整张视图，不把两个月叠在一起比较；汇总与每日柱形
// 来自同一个月汇总端点。

import { useEffect, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchUsageMonthly, type UsageDailyRow, type UsageMonthlySummary, type UsageMonthlyViewRow } from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { bytes } from '../ui/format';
import { ListIcon, PanelTitle } from '../ui/icons';

const monthLabel = (s: string) => `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月`;
const dayLabel = (s: string) =>
  `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月 ${parseInt(s.slice(8, 10), 10)} 日`;
const rowBytes = (r: UsageMonthlyViewRow) => r.uplink_bytes + r.downlink_bytes;

interface CalendarUsageDay extends UsageDailyRow {
  dayNumber: number;
}

/** The response omits empty dates. Filling the short calendar range here makes zero-traffic days
 * visible without turning them into stored accounting facts. UTC is used only for month length;
 * the labels themselves stay on the server's +08 calendar convention. */
function calendarDays(summary: UsageMonthlySummary): CalendarUsageDay[] {
  const [year, month] = summary.month_start.slice(0, 7).split('-').map(Number);
  const validMonth = Number.isInteger(year) && Number.isInteger(month) && month >= 1 && month <= 12;
  const count = validMonth ? new Date(Date.UTC(year, month, 0)).getUTCDate() : 31;
  const prefix = summary.month_start.slice(0, 7);
  const reported = new Map((summary.days ?? []).map(day => [day.day, day]));
  return Array.from({ length: count }, (_, index) => {
    const dayNumber = index + 1;
    const key = `${prefix}-${String(dayNumber).padStart(2, '0')}`;
    return {
      day: key,
      dayNumber,
      uplink_bytes: reported.get(key)?.uplink_bytes ?? 0,
      downlink_bytes: reported.get(key)?.downlink_bytes ?? 0,
      has_gap: reported.get(key)?.has_gap ?? false,
    };
  });
}

export function UsagePane() {
  const [monthOffset, setMonthOffset] = useState<0 | -1>(0);
  const monthly = useQuery({
    queryKey: monthOffset === 0 ? ['usage-monthly'] : ['usage-monthly', monthOffset],
    queryFn: () => fetchUsageMonthly(monthOffset),
  });

  if (monthly.isPending) return <Loading variant="usage" />;

  return (
    <div className="cardpage usage-page">
      <section className="panel titled usage-summary-panel" data-page-title="true">
        <header>
          <ListIcon of="usage" />
          <h4>用量</h4>
          <span className="hint">
            {monthly.data ? monthLabel(monthly.data.month_start) : monthOffset === 0 ? '本月' : '上月'}
          </span>
          <span className="sp" />
          {monthly.isFetching && !monthly.isPending && <span className="usage-refreshing">读取中</span>}
          <div className="segsw usage-month-tabs" role="group" aria-label="用量月份">
            <button type="button" aria-pressed={monthOffset === 0} onClick={() => setMonthOffset(0)}>
              本月
            </button>
            <button type="button" aria-pressed={monthOffset === -1} onClick={() => setMonthOffset(-1)}>
              上月
            </button>
          </div>
        </header>

        {monthly.error ? <ErrorBox error={monthly.error} /> : <UsageMonth summary={monthly.data} />}
      </section>

      {monthly.data && <DailyUsageChart summary={monthly.data} />}
    </div>
  );
}

function UsageMonth({ summary }: { summary: UsageMonthlySummary }) {
  const views = summary.views;
  const total = views.reduce((sum, row) => sum + rowBytes(row), 0);
  const uplink = views.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const downlink = views.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const people = new Set(views.map(row => `${row.tenant_id}/${row.user_id}`)).size;
  const apps = new Set(views.map(row => row.app_id)).size;
  const gaps = views.filter(row => row.has_gap).length;
  return (
    <>
      <section className="usage-overview" aria-label={`${monthLabel(summary.month_start)}用量概览`}>
        <div className="usage-total">
          <span>月累计流量</span>
          <strong>{bytes(total)}</strong>
          <small>自然月口径 · 每日上下行组成</small>
        </div>
        <UsageMetric label="上行" value={bytes(uplink)} tone="up" />
        <UsageMetric label="下行" value={bytes(downlink)} tone="down" />
        <UsageMetric label="活跃用户" value={String(people)} />
        <UsageMetric label="线路" value={String(apps)} />
      </section>

      {gaps > 0 && (
        <div className="callout warn usage-gap" role="status">
          <span />
          {gaps} 条流量明细包含采集缺口，页面按已收到的数据展示。
        </div>
      )}
    </>
  );
}

function UsageMetric({ label, value, tone }: { label: string; value: string; tone?: 'up' | 'down' }) {
  return (
    <div className={`usage-metric${tone ? ` ${tone}` : ''}`}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function DailyUsageChart({ summary }: { summary: UsageMonthlySummary }) {
  const [selectedDayKey, setSelectedDayKey] = useState<string | null>(null);
  const selectedButtonRef = useRef<HTMLButtonElement>(null);
  const dailyAvailable = Array.isArray(summary.days);
  const days = calendarDays(summary);
  const totals = days.map(day => day.uplink_bytes + day.downlink_bytes);
  const peak = Math.max(...totals, 1);
  const total = totals.reduce((sum, value) => sum + value, 0);
  const latestDayWithData = days.reduce<CalendarUsageDay | undefined>(
    (latest, day) => (day.uplink_bytes + day.downlink_bytes > 0 || day.has_gap ? day : latest),
    undefined,
  );
  // Keep a useful detail visible before interaction. When the month changes, an old key simply
  // stops matching and this falls back to the newest populated day in the newly loaded month.
  const selectedDay = days.find(day => day.day === selectedDayKey) ?? latestDayWithData ?? days[0];
  const selectedDayId = selectedDay?.day;
  useEffect(() => {
    const button = selectedButtonRef.current;
    const scroller = button?.closest<HTMLElement>('.usage-plot');
    if (!button || !scroller || scroller.scrollWidth <= scroller.clientWidth) return;
    // A narrow screen uses 28 px day targets in a horizontal strip. Bring the default/current
    // selection into view so its highlighted column and the detail row never disagree visually.
    scroller.scrollLeft = Math.max(0, button.offsetLeft - (scroller.clientWidth - button.offsetWidth) / 2);
  }, [selectedDayId]);
  return (
    <section className="panel titled usage-daily">
      <header>
        <PanelTitle of="usage">每日流量</PanelTitle>
        <span className="hint">{monthLabel(summary.month_start)} · 每根柱显示当天的上下行组成</span>
        <span className="sp" />
        <span className="usage-legend" aria-label="图例">
          <span>
            <i className="up" />
            上行
          </span>
          <span>
            <i className="down" />
            下行
          </span>
        </span>
      </header>
      {!dailyAvailable ? (
        <div className="usage-chart-empty">当前控制面尚未提供每日流量。</div>
      ) : total === 0 ? (
        <div className="usage-chart-empty">这个月还没有流量。</div>
      ) : (
        <>
          <div className="usage-chart-frame">
            <div className="usage-y-axis" aria-hidden="true">
              <span>{bytes(peak)}</span>
              <span>{bytes(Math.round(peak / 2))}</span>
              <span>0</span>
            </div>
            <div className="usage-plot">
              <span className="usage-grid-line top" />
              <span className="usage-grid-line middle" />
              <span className="usage-grid-line bottom" />
              <div className="usage-day-columns" role="list" aria-label={`${monthLabel(summary.month_start)}每日流量`}>
                {days.map(day => {
                  const dayTotal = day.uplink_bytes + day.downlink_bytes;
                  const height = dayTotal === 0 ? 0 : Math.max(2, (dayTotal / peak) * 100);
                  const upShare = dayTotal === 0 ? 0 : (day.uplink_bytes / dayTotal) * 100;
                  const label = `${day.day}：上行 ${bytes(day.uplink_bytes)}，下行 ${bytes(day.downlink_bytes)}${
                    day.has_gap ? '，包含采集缺口' : ''
                  }`;
                  const isMonthEnd = day.dayNumber === days.length;
                  const isInterval = day.dayNumber % 5 === 0 && days.length - day.dayNumber >= 3;
                  const major = day.dayNumber === 1 || isMonthEnd || isInterval;
                  return (
                    <div className="usage-day" key={day.day} role="listitem">
                      <button
                        type="button"
                        ref={selectedDayId === day.day ? selectedButtonRef : undefined}
                        className="usage-day-button"
                        aria-label={`查看 ${label}`}
                        aria-pressed={selectedDayId === day.day}
                        title={label}
                        onClick={() => setSelectedDayKey(day.day)}
                        onFocus={() => setSelectedDayKey(day.day)}
                      >
                        <span className="usage-day-track" aria-hidden="true">
                          <span className="usage-day-stack" style={{ height: `${height}%` }}>
                            <i className="up" style={{ height: `${upShare}%` }} />
                            <i className="down" style={{ height: `${100 - upShare}%` }} />
                          </span>
                          {day.has_gap && <i className="gap" />}
                        </span>
                        <span className={`usage-day-label${major ? ' major' : ''}`} aria-hidden="true">
                          {major ? day.dayNumber : ''}
                        </span>
                      </button>
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
          {selectedDay && (
            <div
              className="usage-day-detail"
              role="group"
              aria-label={`${selectedDay.day} 流量详情`}
              aria-live="polite"
              aria-atomic="true"
            >
              <div className="usage-day-detail-date">
                <time dateTime={selectedDay.day}>{dayLabel(selectedDay.day)}</time>
                <small>{selectedDay.has_gap ? '包含采集缺口' : '点按柱形切换日期'}</small>
              </div>
              <dl>
                <div className="up">
                  <dt>
                    <i aria-hidden="true" />
                    上行
                  </dt>
                  <dd>{bytes(selectedDay.uplink_bytes)}</dd>
                </div>
                <div className="down">
                  <dt>
                    <i aria-hidden="true" />
                    下行
                  </dt>
                  <dd>{bytes(selectedDay.downlink_bytes)}</dd>
                </div>
                <div>
                  <dt>合计</dt>
                  <dd>{bytes(selectedDay.uplink_bytes + selectedDay.downlink_bytes)}</dd>
                </div>
              </dl>
            </div>
          )}
        </>
      )}
    </section>
  );
}
