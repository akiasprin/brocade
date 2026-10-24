// 实时流量（改版）for the machine traffic tab mockup. Synthetic data only; never imported by the app.
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from 'react';
import { throughputAxis } from '../../src/panes/telemetry-format';
import { SegmentedControl } from '../../src/ui/bits';
import { shareOf } from '../../src/ui/format';
import { Icon, PanelTitle } from '../../src/ui/icons';
import { sparklinePaths } from '../../src/ui/sparkline';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import { RATE_SUM_NOTE } from '../../src/user-traffic-phase';
import { liveHistory, liveRates, NOW, SLOT_SECS, type Built, type ChainView } from './node-traffic-mockup-data';
import { amount, ChainName, Rate } from './node-traffic-mockup-ui';

const ThroughputChart = lazy(() =>
  import('../../src/panes/telemetry').then(module => ({ default: module.ThroughputChart })),
);

const SPARK_HEIGHT = 26;
const chainNameOrder = new Intl.Collator('zh-CN', { numeric: true });
const FOLD = 12;
const SEARCH_ROWS = 12;
type Sort = 'name' | 'now' | 'hour';
const SORTS: { value: Sort; label: string }[] = [
  { value: 'name', label: '名称' },
  { value: 'now', label: '当前' },
  { value: 'hour', label: '近 1 小时' },
];

/** 一秒一拍（同 ui/clock 的节拍）；run=0 时冻结，便于截图。 */
function useSecond(run: boolean) {
  const [second, setSecond] = useState(0);
  useEffect(() => {
    if (!run) return;
    const timer = window.setInterval(() => setSecond(value => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [run]);
  return second;
}

function Spark({ down, up, peak }: { down: number[]; up: number[]; peak: number }) {
  const own = Math.max(0, ...down.map((value, index) => value + up[index]));
  const scale = Math.max(own, peak * 0.06, 1024) * 1.06;
  const paths = { down: sparklinePaths(down, scale, SPARK_HEIGHT), up: sparklinePaths(up, scale, SPARK_HEIGHT) };
  return (
    <svg
      className="rut-spark ntl-spark"
      viewBox={`0 0 ${down.length - 1} ${SPARK_HEIGHT}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {paths.down.bases.map((path, index) => (
        <path key={`b${index}`} className="rut-spark-base" d={path} />
      ))}
      {paths.down.areas.map((path, index) => (
        <path key={`a${index}`} className="rut-spark-area" d={path} />
      ))}
      {paths.up.lines.map((path, index) => (
        <path key={`u${index}`} className="rut-spark-up" d={path} />
      ))}
      {paths.down.lines.map((path, index) => (
        <path key={`d${index}`} className="rut-spark-line" d={path} />
      ))}
    </svg>
  );
}

/* 改版要点：
   - 读数栏（同「用量」「机队实时流量」的家族）：当前合计、上下行、按接入链的当前速率。
   - 右侧是本机合计的近 1 小时走势（30 秒窗口）；秒级速率只作读数，不画成时间序列。
   - 用户从卡片网格改成一行一位：当前接入链、近 1 小时走势、近 1 小时用量、当前速率。
     默认按名称，数字每秒更新而行不跳；按「当前」排序时名次只在点「重排」时更新。 */
export function LiveCard({ built, views, run }: { built: Built; views: ChainView[]; run: boolean }) {
  const second = useSecond(run);
  const history = useMemo(() => liveHistory(built), [built]);
  const rates = useMemo(() => liveRates(built, history, second), [built, history, second]);
  const [sort, setSort] = useState<Sort>('name');
  const [order, setOrder] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState(false);
  const [seenChains, setSeenChains] = useState<ReadonlySet<string>>(() => new Set());
  const listRef = useRef<HTMLOListElement>(null);
  const now = NOW + second;
  const viewOf = (col: string) => views.find(view => view.id === col);
  const down = rates.reduce((sum, rate) => sum + rate.down, 0);
  const up = rates.reduce((sum, rate) => sum + rate.up, 0);
  const total = down + up;
  const active = rates.filter(rate => rate.down + rate.up > 0);
  const chainRates = views.map(view => ({
    view,
    value: rates.reduce(
      (sum, rate) =>
        sum + rate.chains.filter(chain => chain.col === view.id).reduce((acc, chain) => acc + chain.down + chain.up, 0),
      0,
    ),
  }));
  const nextSeenChains = new Set(seenChains);
  chainRates.filter(item => item.value > 0).forEach(item => nextSeenChains.add(item.view.id));
  if (nextSeenChains.size !== seenChains.size) setSeenChains(nextSeenChains);
  const byChain = chainRates
    .filter(item => nextSeenChains.has(item.view.id))
    .sort((a, b) => chainNameOrder.compare(a.view.name, b.view.name) || a.view.id.localeCompare(b.view.id));
  const chart = useMemo(() => {
    const rx = history.totalDown.map(value => (value * 8) / SLOT_SECS);
    const tx = history.totalUp.map(value => (value * 8) / SLOT_SECS);
    return {
      rx,
      tx,
      unit: throughputAxis(rx, tx).unit.name,
      bytes:
        history.totalDown.reduce((sum, value) => sum + value, 0) +
        history.totalUp.reduce((sum, value) => sum + value, 0),
    };
  }, [history]);
  const peak = Math.max(
    0,
    ...[...history.users.values()].flatMap(user => user.down.map((value, index) => value + user.up[index])),
  );
  const ranked = (list: typeof rates) =>
    [...list].sort((a, b) => b.down + b.up - (a.down + a.up) || a.id.localeCompare(b.id)).map(rate => rate.id);
  const rows = [...rates]
    .filter(rate => !query || rate.id.includes(query.trim().toLowerCase()))
    .sort((a, b) => {
      if (sort === 'hour')
        return history.users.get(b.id)!.hourBytes - history.users.get(a.id)!.hourBytes || a.id.localeCompare(b.id);
      if (sort === 'now') {
        const rankA = order.indexOf(a.id);
        const rankB = order.indexOf(b.id);
        return (rankA < 0 ? 1e9 : rankA) - (rankB < 0 ? 1e9 : rankB) || a.id.localeCompare(b.id);
      }
      return a.id.localeCompare(b.id);
    });
  const shown = expanded || rows.length <= FOLD + 2 ? rows : rows.slice(0, FOLD);
  const stale = sort === 'now' && ranked(rates).slice(0, shown.length).join() !== shown.map(rate => rate.id).join();
  const chooseSort = (next: Sort) => {
    setSort(next);
    if (next === 'now') setOrder(ranked(rates));
  };

  return (
    <section className="panel config-panel realtime-user-traffic ntl-card" aria-label="实时流量">
      <header className="realtime-panel-header">
        <span title="统计入口 Xray 的接入与转发；中继不提供最终用户身份">
          <PanelTitle of="usage">实时流量</PanelTitle>
        </span>
        <span className="rut-status" role="status">
          <i className="node-lamp" />
          实时 · 每秒更新
        </span>
      </header>
      <div className="panel-full-bleed-body ntl-body">
        <div className="ntl-top">
          <section className="rut-now ntl-now" aria-label="当前速率">
            <div className="user-usage-hero">
              <span className="user-usage-hero-label" title={RATE_SUM_NOTE}>
                最近速率合计
              </span>
              <strong className="user-usage-hero-value">
                <Rate value={total} />
              </strong>
              <span className="rut-hero-sub">
                <b>{active.length}</b> 位传输中 · 近 1 小时 <b>{rates.length}</b> 位
              </span>
            </div>
            <div>
              <div className="usage-split" aria-hidden="true">
                {down > 0 && <i className="down" style={{ flexGrow: down }} />}
                {up > 0 && <i className="up" style={{ flexGrow: up }} />}
              </div>
              <dl className="usage-io">
                <div className="down">
                  <dt>
                    <i aria-hidden="true" />
                    下行
                  </dt>
                  <dd>
                    <Rate value={down} reference={Math.max(down, up)} />
                  </dd>
                  <dd className="usage-share">{total > 0 ? shareOf(down, total) : ''}</dd>
                </div>
                <div className="up">
                  <dt>
                    <i aria-hidden="true" />
                    上行
                  </dt>
                  <dd>
                    <Rate value={up} reference={Math.max(down, up)} />
                  </dd>
                  <dd className="usage-share">{total > 0 ? shareOf(up, total) : ''}</dd>
                </div>
              </dl>
            </div>
            <div className="ntl-bychain">
              <span className="ntl-label">当前经过的接入链</span>
              {byChain.length === 0 ? (
                <span className="ntl-none">没有正在传输的连接</span>
              ) : (
                <ol>
                  {byChain.map(({ view, value }) => (
                    <li key={view.id} title={`${view.name} · ${view.via} · 占当前 ${shareOf(value, total)}`}>
                      <ChainName view={view} />
                      <b>
                        <Rate value={value} />
                      </b>
                    </li>
                  ))}
                </ol>
              )}
            </div>
          </section>
          <section className="rut-hour ntl-hour" aria-label="近 1 小时">
            <div className="rut-hour-head">
              <span title="按真实采样窗口计算平均速率；空闲是 0，未采集的窗口断开">
                近 1 小时 · 本机合计 · 每 30 秒
              </span>
              <span className="chart-unit">({chart.unit})</span>
              <span className="rut-legend" aria-hidden="true">
                <i className="d" />
                下行
                <i className="u" />
                上行
              </span>
              <b>{amount(chart.bytes)}</b>
            </div>
            {chart.bytes > 0 ? (
              <Suspense fallback={<div className="rut-chart-wait" />}>
                <ThroughputChart
                  timesUnixSecs={history.slots}
                  windowStartsUnixSecs={history.slots.map(end => end - SLOT_SECS)}
                  rangeStartUnixSecs={history.slots[0] - SLOT_SECS}
                  rangeEndUnixSecs={history.slots[history.slots.length - 1]}
                  rx={chart.rx}
                  tx={chart.tx}
                  rxName="下行"
                  txName="上行"
                />
              </Suspense>
            ) : (
              <div className="rut-chart-wait">
                <span className="rut-chart-note">近 1 小时没有用户流量，本机上报正常</span>
              </div>
            )}
          </section>
        </div>
        {rates.length > 0 && (
          <section className="ntl-users" aria-label="用户当前速率">
            <div className="ntl-tools">
              <span className="ntl-label">
                用户 · <b>{rates.length}</b> 位
              </span>
              {rates.length > SEARCH_ROWS && (
                <span className="user-list-search ntl-search">
                  <Icon of="search" size={13} className="user-list-search-icon" />
                  <input
                    type="search"
                    value={query}
                    placeholder="搜索用户"
                    aria-label="搜索用户"
                    onChange={event => setQuery(event.target.value)}
                  />
                </span>
              )}
              {stale && (
                <button type="button" className="btn text sm ntl-reorder" onClick={() => setOrder(ranked(rates))}>
                  名次有变化 · 重排
                </button>
              )}
              <SegmentedControl
                ariaLabel="排序"
                value={sort}
                options={SORTS}
                onChange={chooseSort}
                className="ntl-sort"
              />
            </div>
            <div className="ntl-head" aria-hidden="true">
              <span>用户</span>
              <span>当前接入链</span>
              <span>近 1 小时</span>
              <span />
              <span>当前</span>
            </div>
            <ol ref={listRef} className="ntl-list">
              {shown.map(rate => {
                const past = history.users.get(rate.id)!;
                const idle = rate.down + rate.up === 0;
                const minutes = rate.idleSince === null ? null : Math.max(1, Math.round((now - rate.idleSince) / 60));
                return (
                  <li key={rate.id} className={`ntl-row${idle ? ' idle' : ''}`}>
                    <span className="ntl-who">
                      <GeneratedUserAvatar id={rate.id} variant="mini" />
                      <span className="ntl-name">{rate.id}</span>
                      <span className="ntl-tenant">{rate.tenant}</span>
                    </span>
                    <span className="ntl-chains">
                      {idle ? (
                        <span className="ntl-none">—</span>
                      ) : (
                        [...rate.chains]
                          .sort((a, b) => b.down + b.up - (a.down + a.up))
                          .map(chain => {
                            const view = viewOf(chain.col);
                            return view ? (
                              <span key={chain.col} className="ntl-chip">
                                <ChainName view={view} />
                                <b>
                                  <Rate value={chain.down + chain.up} />
                                </b>
                              </span>
                            ) : null;
                          })
                      )}
                    </span>
                    <Spark down={past.down} up={past.up} peak={peak} />
                    <span className="ntl-hourbytes">{amount(past.hourBytes)}</span>
                    <span className="ntl-rate">
                      {idle ? (
                        <span className="ntl-idle">{minutes === null ? '空闲' : `空闲 ${minutes} 分钟`}</span>
                      ) : (
                        <>
                          <span className="ntl-d">
                            ↓ <Rate value={rate.down} />
                          </span>
                          <span className="ntl-u">
                            ↑ <Rate value={rate.up} />
                          </span>
                        </>
                      )}
                    </span>
                  </li>
                );
              })}
            </ol>
            {rows.length > FOLD + 2 && (
              <button
                type="button"
                className="btn sm ntl-more"
                aria-expanded={expanded}
                onClick={() => setExpanded(v => !v)}
              >
                {expanded ? '收起' : `显示其余 ${rows.length - FOLD} 位`}
              </button>
            )}
          </section>
        )}
      </div>
    </section>
  );
}
