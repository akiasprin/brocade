// 用量。
//
// 本页此前是 `/usage/samples` 的原始输出：100 行五分钟窗口，一屏无法显示完整，且各行多为
// 同一用户同一机器。它可以回答特定窗口内某用户在某机器上的用量——而本页需要回答的是
// 另外四项：本月总量、用量排名、各机器的承载量、是否存在采集缺口。
//
// 另外两个接口此前已存在（`/usage/monthly-summary`、`/usage/node-series`），只是本页
// 未使用——它们此前只服务于用户页和拓扑图。现在按由粗到细排列四块内容：
//
//   ① 本月总量和采集缺口   monthly-summary
//   ② 用量排名             monthly-summary
//   ③ 各机器承载量         node-series
//   ④ 原始样本（折叠）     samples
//
// 无法绘制按天的曲线：三个接口都不提供该数据（node-series 是最近一段时间的等宽窗口，
// monthly-summary 只有整月的总量）。需要趋势数据时需服务端先增加按天聚合。

import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  bucketBytes,
  fetchNodes,
  fetchSnapshot,
  fetchTenants,
  fetchUsage,
  fetchUsageMonthly,
  fetchUsageNodeSeries,
  fetchUsers,
  monthBytes,
  type UsageMonthlyViewRow,
  type UsageNodeSeries,
} from '../api';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { bytes } from '../ui/format';
import { useNodeNames } from '../ui/node-name';

// 柱状图覆盖的时间范围：与机器列表右端使用同一刻度（24 格 × 30s 上报窗口 = 12 分钟）。
// 两个值需要同步修改——格数多于窗口数时左侧会留下始终为空的柱子。
const SERIES_BUCKETS = 24;
const SERIES_WINDOW_SECS = SERIES_BUCKETS * 30;
// 排名列出的名次数量。超出该范围时应在原始样本中按用户筛选，那属于另一种用法
// （查找特定用户，而非查看整体分布）。
const RANK_LIMIT = 12;

const monthLabel = (s: string) => `${s.slice(0, 4)} 年 ${parseInt(s.slice(5, 7), 10)} 月`;
/* 窗口时间只显示时分：一行对应一个采集窗口，日期在 title 中 */
const window_ = (start: string, end: string) => {
  const hm = (t: string) => t.slice(11, 16);
  return `${hm(start)} – ${hm(end)}`;
};
const rowBytes = (r: UsageMonthlyViewRow) => r.uplink_bytes + r.downlink_bytes;
const pct = (part: number, whole: number) => (whole > 0 ? `${((part / whole) * 100).toFixed(1)}%` : '—');

export function UsagePane() {
  const monthly = useQuery({ queryKey: ['usage-monthly'], queryFn: () => fetchUsageMonthly() });
  const series = useQuery({
    queryKey: ['usage-node-series', SERIES_WINDOW_SECS],
    queryFn: () => fetchUsageNodeSeries(SERIES_WINDOW_SECS),
  });
  /* 项目名称只存在于快照中；获取失败时回退到 id，不因缺少一个标签而阻断整块渲染。 */
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const labelOf = (appId: string) => snapshot.data?.snapshot.apps.find(a => a.id === appId)?.label ?? appId;
  const nameOf = useNodeNames();

  if (monthly.isPending) return <Loading />;
  if (monthly.error) return <ErrorBox error={monthly.error} />;

  const views = monthly.data.views;
  const total = views.reduce((s, v) => s + rowBytes(v), 0);
  const up = views.reduce((s, v) => s + v.uplink_bytes, 0);
  const dn = views.reduce((s, v) => s + v.downlink_bytes, 0);
  const gaps = views.filter(v => v.has_gap);
  const people = new Set(views.map(v => `${v.tenant_id}/${v.user_id}`)).size;
  const apps = new Set(views.map(v => v.app_id)).size;
  const ranked = [...views].sort((a, b) => rowBytes(b) - rowBytes(a));
  const top = ranked.slice(0, RANK_LIMIT);
  // 进度条的刻度按第一名而非总量：按总量时第一名也只占三成，整列条形都较短而难以比较——
  // 而该列的作用正是不读取数值即可看出用量最大的用户。
  const peak = ranked.length > 0 ? rowBytes(ranked[0]) : 0;

  const nodes = [...(series.data?.nodes ?? [])].sort((a, b) => monthBytes(b) - monthBytes(a));
  const nodeTotal = nodes.reduce((s, n) => s + monthBytes(n), 0);
  // 中继开销单独统计。它与上面的总量不属于同一口径：monthly-summary 统计的是
  // 用户流量（对外提供的部分），中继跳的字节不属于任何用户，只出现在 node-series 中。
  // 不合并进主数值，也不隐藏——两个数值并列显示才能完整反映成本结构：
  // 在一条三跳链上，用户每传输 1 字节，运营方需要转发 2 字节。
  const relay = nodes.reduce((s, n) => s + n.month_relay_uplink_bytes + n.month_relay_downlink_bytes, 0);

  return (
    /* 与设置页、发布页同一版式（.cardpage）：不铺纸、卡片直接落在台面上、两栏并排。
       页首的结论块保持 .blk——它是「本月总量」这一个数，不是一段可操作的配置，
       与链详情页顶部的探测结论同一形态。 */
    <div className="cardpage">
      {/* ── ① 本月总量（存在采集缺口时一并显示） ── */}
      <div className="duo">
        <section className="panel titled">
          <header>
            <h4>本月流量</h4>
            <span className="sp" />
            <span className="st">{monthLabel(monthly.data.month_start)}</span>
          </header>
          <div className="sum">
            <span className="sum-tot">
              <span className="n">{bytes(total)}</span>
            </span>
            <span>
              <span className="sum-split">
                <span>
                  <span className="k">↑ 上行</span>
                  <span className="v">{bytes(up)}</span>
                </span>
                <span>
                  <span className="k">↓ 下行</span>
                  <span className="v">{bytes(dn)}</span>
                </span>
                <span>
                  <span className="k">用户</span>
                  <span className="v">{people}</span>
                </span>
                <span>
                  <span className="k">线路</span>
                  <span className="v">{apps}</span>
                </span>
                {relay > 0 && (
                  <span title="中继跳转掉的字节，不属于任何用户，是运营者的成本">
                    <span className="k">中继开销</span>
                    <span className="v dim">{bytes(relay)}</span>
                  </span>
                )}
              </span>
              <span className="sum-when">
                {monthly.data.month_start.slice(0, 16)} → {monthly.data.month_end.slice(0, 16)}
              </span>
            </span>
          </div>
        </section>

        {gaps.length > 0 && (
          <section className="panel titled">
            <header>
              <h4>有缺口</h4>
              <span className="sp" />
              <span className="st">has_gap</span>
            </header>
            <div className="sum">
              <span className="sum-tot">
                <span className="n">{gaps.length}</span>
                <span className="u">行</span>
              </span>
              <span className="sum-say">
                <span className="l1">
                  {gaps
                    .slice(0, 3)
                    .map(g => `${g.user_id} / ${labelOf(g.app_id)}`)
                    .join('、')}
                  {gaps.length > 3 ? ` 等 ${gaps.length} 行` : ''}
                </span>
                <span className="sum-when">少收了至少一跳，账面只会偏小。翻原始样本按用户筛查。</span>
              </span>
            </div>
          </section>
        )}
      </div>

      {/* ── ② 谁在用 · ③ 哪台机器扛得多 ──
          两段并排。此前是上下相接的两段，各自顶着一条 `h4.sec` 分节线；改为两栏卡片后
          分节线由卡片的标题栏承担，段名不再需要单独占一行。 */}
      <div className="duo">
        <div className="col">
          <section className="panel titled" id="us-who">
            <header>
              <span className="no">01</span>
              <h4>谁在用</h4>
              <span className="hint">用户 × 线路</span>
              <span className="sp" />
              {ranked.length > RANK_LIMIT && (
                <span className="hint">
                  前 {RANK_LIMIT} / 共 {ranked.length}
                </span>
              )}
            </header>
            <p className="cardsub">本月合计，按总量排</p>
            <div>
              {top.length === 0 ? (
                <Empty>这个月还没有用量。</Empty>
              ) : (
                <>
                  <div className="legend">
                    <span>
                      <i className="up" />
                      上行
                    </span>
                    <span>
                      <i className="dn" />
                      下行
                    </span>
                  </div>
                  {top.map((r, i) => (
                    <div className="rank-row" key={`${r.tenant_id}/${r.user_id}/${r.app_id}`}>
                      <span className="rank-no">{String(i + 1).padStart(2, '0')}</span>
                      <span className="rank-who">
                        <b>{r.user_id}</b>
                        <span className="app" title={r.app_id}>
                          {labelOf(r.app_id)}
                        </span>
                        {r.has_gap && <span className="st st-warn">has_gap</span>}
                      </span>
                      {/* 一根条表达两项信息：总长表示与第一名的比例，内部两段表示上下行的构成 */}
                      <span className="bar" title={`↑ ${bytes(r.uplink_bytes)} · ↓ ${bytes(r.downlink_bytes)}`}>
                        <i className="up" style={{ width: peak > 0 ? `${(r.uplink_bytes / peak) * 100}%` : '0' }} />
                        <i className="dn" style={{ width: peak > 0 ? `${(r.downlink_bytes / peak) * 100}%` : '0' }} />
                      </span>
                      <span className="rank-n">{bytes(rowBytes(r))}</span>
                      <span className="rank-pct">{pct(rowBytes(r), total)}</span>
                    </div>
                  ))}
                </>
              )}
            </div>
          </section>
        </div>

        <div className="col">
          <section className="panel titled" id="us-load">
            <header>
              <span className="no">02</span>
              <h4>哪台机器扛得多</h4>
              <span className="hint">最近 {SERIES_WINDOW_SECS / 60} 分钟 · 一格 30 秒</span>
            </header>
            <p className="cardsub">柱图是最近这一段，右边两列是本月</p>
            <div>
              {series.isPending ? (
                <Loading />
              ) : series.error ? (
                <ErrorBox error={series.error} />
              ) : nodes.length === 0 ? (
                <Empty>还没有机器上报用量。</Empty>
              ) : (
                <>
                  <div className="legend">
                    <span>
                      <i className="u" />
                      用户流量（入口）
                    </span>
                    <span>
                      <i className="r" />
                      中继流量（中转跳）
                    </span>
                  </div>
                  {nodes.map(n => (
                    <NodeLoad
                      key={n.node_id}
                      series={n}
                      since={series.data.since}
                      share={pct(monthBytes(n), nodeTotal)}
                      name={nameOf(n.node_id)}
                    />
                  ))}
                </>
              )}
            </div>
          </section>
        </div>
      </div>

      {/* ── ④ 原始样本 ──
          通栏，不进两栏：它是一张八列的表，塞进 420px 的一栏后每列都要换行。
          默认折叠，展开后才请求数据。 */}
      <RawSamples />
    </div>
  );
}

// 一台机器一行。柱状图的刻度按本行自身的峰值而非全表统一——本图表示的是该机器近期的
// 运行情况，跨机器比较总量应看右侧两列。使用统一刻度时，低流量机器的柱子全部贴底，
// 无法看出其自身是否中断。与机器列表右端的柱状图使用同一约定。
function NodeLoad({
  series,
  since,
  share,
  name,
}: {
  series: UsageNodeSeries;
  since?: string;
  share: string;
  name: string;
}) {
  // 服务端只返回有样本的窗口，中间无数据的窗口需要补零——否则五分钟的中断会被绘制为
  // 连续运行。格的位置按服务端返回的 since 计算，不按该机器自身的最后一个样本
  // （那样掉线机器仅有的几根柱子会被排到最右侧，表现为刚刚仍在运行），
  // 也不按浏览器的 now()（本地时钟与服务端可能不同步，相差一格会导致整条错位）。
  const user = new Array<number>(SERIES_BUCKETS).fill(0);
  const relay = new Array<number>(SERIES_BUCKETS).fill(0);
  const t0 = since ? Date.parse(since) : NaN;
  if (!Number.isNaN(t0)) {
    for (const b of series.buckets) {
      const idx = Math.round((Date.parse(b.window_end) - t0) / 30_000) - 1;
      if (idx < 0 || idx >= SERIES_BUCKETS) continue;
      user[idx] += b.user_uplink_bytes + b.user_downlink_bytes;
      relay[idx] += b.relay_uplink_bytes + b.relay_downlink_bytes;
    }
  }
  const peak = Math.max(...series.buckets.map(bucketBytes), 1);
  const month = monthBytes(series);

  return (
    <div className="load-row">
      <span className="load-who">
        <b>{name}</b>
      </span>
      <span className="bars" title={`峰值 ${bytes(peak)}/窗口`}>
        {user.map((u, i) => (
          <i key={i}>
            <span className="r" style={{ height: `${Math.min(100, (relay[i] / peak) * 100)}%` }} />
            {/* 有数值时至少绘制 1.5px：按比例计算时，用户流量比中继流量小两个数量级的情况下
                该段只有半个像素，实际不可见——而该段表示的正是该机器上是否有用户在使用。 */}
            <span
              className="u"
              style={{
                height: `${Math.min(100, (u / peak) * 100)}%`,
                minHeight: u > 0 ? 1.5 : 0,
              }}
            />
          </i>
        ))}
      </span>
      <span className="load-n">{bytes(month)}</span>
      <span className="load-share">{share}</span>
    </div>
  );
}

// 原始样本。默认折叠——它用于排查而非日常查看，且一百行五分钟窗口展开后一屏无法显示完整。
//
// 用户流量和链路开销合并为一张表：此前是两张表头只差一列的表格叠放，需要先分辨当前
// 查看的是哪一张。中继跳对应的行中「用户」一格为 —，位置一格显示为「链 · 第几跳」。
function RawSamples() {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState({ tenant_id: '', user_id: '', node_id: '' });
  /* 未展开时不发送请求：四个查询中有三个用于填充筛选器的选项，折叠时都不需要 */
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants(), enabled: open });
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true), enabled: open });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), enabled: open });
  const usage = useQuery({
    queryKey: ['usage', filter],
    queryFn: () => fetchUsage(filter),
    enabled: open,
  });
  const nameOf = useNodeNames();

  // 选择具体用户时不列出链路开销：该筛选表示的是某个用户的用量，
  // 而链路开销不属于任何用户，显示在筛选结果中会导致其被计入该用户的用量。
  const rows = [
    ...(usage.data?.samples ?? []).map(s => ({
      key: `u${s.id}`,
      window_start: s.window_start,
      window_end: s.window_end,
      node_id: s.node_id,
      who: s.user_id as string | null,
      where: s.ingress_id,
      whereTitle: s.grant_label,
      up: s.uplink_bytes,
      dn: s.downlink_bytes,
      gap: s.has_gap,
      rev: s.revision_id,
    })),
    ...(filter.user_id ? [] : (usage.data?.chain_samples ?? [])).map(s => ({
      key: `c${s.id}`,
      window_start: s.window_start,
      window_end: s.window_end,
      node_id: s.node_id,
      who: null as string | null,
      // 只显示链 id：`hop_label` 的格式是 `<链>@<机器>`，两部分都已在其他列中显示
      // （节点列显示机器），合并显示会形成「app-hk-01.c3 · app-hk-01.c3@nz-01」这样的重复。
      where: s.chain_id,
      whereTitle: s.hop_label,
      up: s.uplink_bytes,
      dn: s.downlink_bytes,
      gap: s.has_gap,
      rev: s.revision_id,
    })),
  ].sort((a, b) => (a.window_end < b.window_end ? 1 : -1));

  return (
    <details className="raw" onToggle={e => setOpen(e.currentTarget.open)}>
      <summary>
        <span className="caret">{open ? '▾' : '▸'}</span>
        原始样本
        <span className="sp" />
        <span className="src">最近 100 条 · 含中继跳</span>
      </summary>
      <div className="raw-bd">
        {(tenants.error || users.error || nodes.error) && (
          <ErrorBox error={tenants.error ?? users.error ?? nodes.error} />
        )}
        <div className="toolbar">
          <select
            className="f"
            value={filter.tenant_id}
            onChange={e => setFilter({ ...filter, tenant_id: e.target.value })}
          >
            <option value="">租户：全部</option>
            {tenants.data?.tenants.map(t => (
              <option key={t.id} value={t.id}>
                {t.id}
              </option>
            ))}
          </select>
          <select
            className="f"
            value={filter.user_id}
            onChange={e => setFilter({ ...filter, user_id: e.target.value })}
          >
            <option value="">用户：全部</option>
            {users.data?.users.map(u => (
              <option key={`${u.tenant_id}/${u.id}`} value={u.id}>
                {u.id}
              </option>
            ))}
          </select>
          <select
            className="f"
            value={filter.node_id}
            onChange={e => setFilter({ ...filter, node_id: e.target.value })}
          >
            <option value="">节点：全部</option>
            {nodes.data?.nodes.map(n => (
              <option key={n.node_id} value={n.node_id}>
                {n.name || n.node_id}（{n.node_id}）
              </option>
            ))}
          </select>
          <span className="sp" />
          <span className="note">仅可见自身子树内的样本</span>
        </div>

        {usage.isPending ? (
          <Loading />
        ) : usage.error ? (
          <ErrorBox error={usage.error} />
        ) : rows.length === 0 ? (
          <Empty>还没有用量样本。</Empty>
        ) : (
          <table className="tbl cards">
            <thead>
              <tr>
                <th>窗口</th>
                <th>节点</th>
                <th>用户</th>
                <th>接入面 / 链</th>
                <th>↑ 上行</th>
                <th>↓ 下行</th>
                <th>缺口</th>
                <th>修订</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(r => (
                <tr key={r.key}>
                  <td className="mono dim" title={`${r.window_start} → ${r.window_end}`}>
                    {window_(r.window_start, r.window_end)}
                  </td>
                  <td data-label="节点" title={r.node_id}>
                    {nameOf(r.node_id)}
                  </td>
                  <td className="mono" data-label="用户">
                    {r.who ?? <span className="dim">—</span>}
                  </td>
                  <td className="mono dim" data-label="接入面 / 链" title={r.whereTitle}>
                    {r.where}
                  </td>
                  <td className="mono" data-label="↑ 上行">
                    {bytes(r.up)}
                  </td>
                  <td className="mono" data-label="↓ 下行">
                    {bytes(r.dn)}
                  </td>
                  <td data-label="缺口">
                    {r.gap ? <span className="st st-warn">has_gap</span> : <span className="dim">—</span>}
                  </td>
                  <td className="mono dim" data-label="修订">
                    {r.rev ?? '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </details>
  );
}
