// 用量按自然月展示。月份选择只切换整张视图，不把两个月叠在一起比较；读数栏与每日柱形
// 来自同一个月汇总端点。整页是一张卡：左侧读数栏回答“这个月用了多少”，右侧柱形图回答
// “每天怎样组成”。

import { useEffect, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchUsageMonthly, type UsageDailyRow, type UsageMonthlySummary } from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { bytes } from '../ui/format';
import { ListIcon } from '../ui/icons';

const monthLabel = (s: string) => `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月`;
const dayLabel = (s: string) =>
  `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月 ${parseInt(s.slice(8, 10), 10)} 日`;
const shortDayLabel = (s: string) => `${parseInt(s.slice(5, 7), 10)} 月 ${parseInt(s.slice(8, 10), 10)} 日`;
const dayBytes = (day: UsageDailyRow) => day.uplink_bytes + day.downlink_bytes;
const share = (part: number, whole: number) => (whole > 0 ? `${((part / whole) * 100).toFixed(1)}%` : '—');

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

/** Today's date on the control plane's calendar. The monthly summary query cuts days at +08
 * (Asia/Hong_Kong), so the daily average uses the same zone to decide which days have ended. */
function controlPlaneToday(now: Date): string {
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone: 'Asia/Hong_Kong',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).formatToParts(now);
  const part = (type: Intl.DateTimeFormatPartTypes) => parts.find(item => item.type === type)?.value ?? '';
  return `${part('year')}-${part('month')}-${part('day')}`;
}

export function UsagePane() {
  const [monthOffset, setMonthOffset] = useState<0 | -1>(0);
  const monthly = useQuery({
    queryKey: monthOffset === 0 ? ['usage-monthly'] : ['usage-monthly', monthOffset],
    queryFn: () => fetchUsageMonthly(monthOffset),
    // Month changes are refreshes inside an already-mounted surface. Keep the current reading in
    // place until the next month is ready so the page title, switch and panel never remount.
    placeholderData: previous => previous,
  });

  if (monthly.isPending) return <Loading variant="usage" />;

  return (
    <div className="cardpage usage-page">
      <section
        className="panel titled usage-summary-panel"
        data-page-title="true"
        aria-busy={monthly.isFetching || undefined}
      >
        <header>
          <ListIcon of="usage" />
          <h4>用量</h4>
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

        {/* A failed refresh keeps the month already on screen; the error explains why it may be stale. */}
        {monthly.error && <ErrorBox error={monthly.error} />}
        {monthly.data && <UsageMonth summary={monthly.data} />}
      </section>
    </div>
  );
}

function UsageMonth({ summary }: { summary: UsageMonthlySummary }) {
  // The ledger's peak day and the chart's columns select the same day, so the selection lives here.
  const [selectedDayKey, setSelectedDayKey] = useState<string | null>(null);
  const days = calendarDays(summary);
  return (
    <div className="usage-cockpit">
      <UsageLedger summary={summary} days={days} onSelectDay={setSelectedDayKey} />
      <DailyUsageChart summary={summary} days={days} selectedDayKey={selectedDayKey} onSelectDay={setSelectedDayKey} />
    </div>
  );
}

function UsageLedger({
  summary,
  days,
  onSelectDay,
}: {
  summary: UsageMonthlySummary;
  days: CalendarUsageDay[];
  onSelectDay: (day: string) => void;
}) {
  const views = summary.views;
  const uplink = views.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const downlink = views.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const total = uplink + downlink;
  const people = new Set(views.map(row => `${row.tenant_id}/${row.user_id}`)).size;
  const apps = new Set(views.map(row => row.app_id)).size;
  // Today is still accumulating, so the average covers only days that have ended. An older control
  // plane without daily rows cannot answer either daily reading.
  const dailyAvailable = Array.isArray(summary.days);
  const today = controlPlaneToday(new Date());
  const closedDays = dailyAvailable ? days.filter(day => day.day < today) : [];
  const average =
    closedDays.length > 0 ? closedDays.reduce((sum, day) => sum + dayBytes(day), 0) / closedDays.length : null;
  const peak = days.reduce<CalendarUsageDay | undefined>(
    (best, day) => (dayBytes(day) > (best ? dayBytes(best) : 0) ? day : best),
    undefined,
  );
  const upShare = total > 0 ? (uplink / total) * 100 : 0;
  const [figure, unit] = bytes(total).split(' ');
  const month = monthLabel(summary.month_start);
  return (
    <section className="usage-ledger" aria-label={`${month}用量概览`}>
      <div className="usage-hero">
        <span className="usage-hero-label">月累计流量</span>
        <strong className="usage-hero-value">
          {figure} <small>{unit}</small>
        </strong>
        <span className="usage-period">
          {month} 1 日 – {days.length} 日
        </span>
      </div>
      <div className="usage-compose">
        {/* The rows below carry the same values as text; the bar only shows the proportion. */}
        <div className="usage-split" aria-hidden="true">
          {total > 0 && (
            <>
              <i className="up" style={{ flexGrow: upShare }} />
              <i className="down" style={{ flexGrow: 100 - upShare }} />
            </>
          )}
        </div>
        <dl className="usage-io">
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{bytes(uplink)}</dd>
            <dd className="usage-share">{share(uplink, total)}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{bytes(downlink)}</dd>
            <dd className="usage-share">{share(downlink, total)}</dd>
          </div>
        </dl>
      </div>
      <dl className="usage-facts">
        <div>
          <dt>日均</dt>
          <dd title={closedDays.length > 0 ? `按 ${closedDays.length} 个完整日计算` : undefined}>
            {average === null ? '—' : bytes(average)}
          </dd>
        </div>
        <div>
          <dt>峰值日</dt>
          <dd>
            {peak ? (
              <>
                <button
                  type="button"
                  className="usage-peak"
                  aria-label={`在每日流量中查看 ${shortDayLabel(peak.day)}`}
                  onClick={() => onSelectDay(peak.day)}
                >
                  {shortDayLabel(peak.day)}
                </button>
                <span className="usage-dot" aria-hidden="true">
                  ·
                </span>
                {bytes(dayBytes(peak))}
              </>
            ) : (
              '—'
            )}
          </dd>
        </div>
        <div>
          <dt>活跃用户</dt>
          <dd>{people}</dd>
        </div>
        <div>
          <dt>线路</dt>
          <dd>{apps}</dd>
        </div>
      </dl>
    </section>
  );
}

function DailyUsageChart({
  summary,
  days,
  selectedDayKey,
  onSelectDay,
}: {
  summary: UsageMonthlySummary;
  days: CalendarUsageDay[];
  selectedDayKey: string | null;
  onSelectDay: (day: string) => void;
}) {
  const selectedButtonRef = useRef<HTMLButtonElement>(null);
  const dailyAvailable = Array.isArray(summary.days);
  const totals = days.map(dayBytes);
  const peak = Math.max(...totals, 1);
  const total = totals.reduce((sum, value) => sum + value, 0);
  const latestDayWithData = days.reduce<CalendarUsageDay | undefined>(
    (latest, day) => (dayBytes(day) > 0 || day.has_gap ? day : latest),
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
    <section className="usage-chart">
      <div className="usage-chart-head">
        <h5>每日流量</h5>
        <span className="usage-chart-hint">每根柱显示当天的上下行组成</span>
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
      </div>
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
                  const dayTotal = dayBytes(day);
                  const height = dayTotal === 0 ? 0 : Math.max(2, (dayTotal / peak) * 100);
                  const upShare = dayTotal === 0 ? 0 : (day.uplink_bytes / dayTotal) * 100;
                  const label = `${day.day}：上行 ${bytes(day.uplink_bytes)}，下行 ${bytes(day.downlink_bytes)}`;
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
                        onClick={() => onSelectDay(day.day)}
                        onFocus={() => onSelectDay(day.day)}
                      >
                        <span className="usage-day-track" aria-hidden="true">
                          <span className="usage-day-stack" style={{ height: `${height}%` }}>
                            <i className="up" style={{ height: `${upShare}%` }} />
                            <i className="down" style={{ height: `${100 - upShare}%` }} />
                          </span>
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
                <small>点按柱形切换日期</small>
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
