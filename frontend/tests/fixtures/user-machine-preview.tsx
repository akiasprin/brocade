// Local visual acceptance fixture; production components and tokens, synthetic traffic only.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { SiteUsagePeriod, UserMachineUsage, UserMachineUsageRow } from '../../src/api';
import { UserMachineUsageCard } from '../../src/user-machine-usage';
import { UserSiteUsageCard } from '../../src/user-site-usage';
import { PanelTitle } from '../../src/ui/icons';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import '../../src/styles.css';
import { theme } from '../../src/forge/theme';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const scene = params.get('scene') ?? 'regular';
const now = Date.UTC(2026, 9, 18, 8, 23) / 1000;
const names = [
  '香港示例 01',
  '香港示例 02',
  '日本 NTT',
  '新加坡 SG',
  '台湾住宅入口',
  '美国西部',
  '德国法兰克福',
  '澳洲悉尼',
  '韩国首尔',
  '备用入口',
];
if (scene === 'dense')
  names.splice(
    0,
    names.length,
    ...Array.from({ length: 36 }, (_, i) => `多地域机器 ${i} 东京 Singapore ABCDEFGHIJKLMNOPQRSTUVWXYZ`),
  );
function bounds(period: SiteUsagePeriod) {
  const day = Math.floor((now + 28800) / 86400) * 86400 - 28800;
  const start = period === 'day' ? day : period === 'week' ? day - 6 * 86400 : Date.UTC(2026, 9, 1) / 1000 - 28800;
  const end = period === 'month' ? Date.UTC(2026, 10, 1) / 1000 - 28800 : start + (period === 'week' ? 7 : 1) * 86400;
  return { start, end };
}
function usage(period: SiteUsagePeriod): UserMachineUsage {
  const { start, end } = bounds(period);
  const step = period === 'month' ? 86400 : 3600;
  const count = Math.ceil((now - start) / step);
  const rows: UserMachineUsageRow[] = names.slice(0, scene === 'few' ? 3 : names.length).map((_, index) => {
    const points: UserMachineUsageRow['points'] = Array.from({ length: count }, (_, slot) => {
      const at = start + slot * step;
      const gap = scene === 'gap' && index === 1 && at >= now - 4 * 3600;
      const prior = scene === 'launch' && period === 'month' && slot < 10;
      const amount =
        scene === 'empty' || (scene !== 'dense' && index >= 8) || gap || prior
          ? 0
          : Math.round(((0.1 + ((slot * 7 + index * 13) % 17) / 10) * (step / 3600) * 1024 ** 3) / (index + 1) ** 1.6);
      const up = Math.round(amount * 0.03);
      return [at, up, amount - up, index === 2 ? Math.round(amount * 0.2) : 0, gap, !gap && !prior];
    });
    return {
      node_id: `m${index}`,
      uplink_bytes: points.reduce((sum, p) => sum + p[1], 0),
      downlink_bytes: points.reduce((sum, p) => sum + p[2], 0),
      forwarding_bytes: points.reduce((sum, p) => sum + p[3], 0),
      expected: true,
      latest_report_unix_secs: scene === 'gap' && index === 1 ? now - 4 * 3600 : now - 30,
      has_gap: scene === 'gap' && index === 1,
      points,
    };
  });
  if (scene === 'launch' && period === 'month')
    rows.push({
      node_id: null,
      uplink_bytes: 10 * 1024 ** 3,
      downlink_bytes: 90 * 1024 ** 3,
      forwarding_bytes: 0,
      has_gap: false,
      expected: false,
      latest_report_unix_secs: null,
      points: Array.from({ length: 10 }, (_, slot) => [start + slot * step, 1024 ** 3, 9 * 1024 ** 3, 0, false, true]),
    });
  return {
    period,
    range_start_unix_secs: start,
    range_end_unix_secs: now,
    period_end_unix_secs: end,
    bucket_secs: step,
    expected_nodes: rows.filter(r => r.expected).length,
    reporting_nodes: rows.filter(r => r.expected).length - (scene === 'gap' ? 1 : 0),
    rows,
  };
}
globalThis.fetch = async input => {
  const path = String(input);
  if (path === '/nodes/agent-state')
    return Response.json({
      nodes: names.map((name, index) => ({
        node_id: `m${index}`,
        name,
        public_ipv4_country: index < 2 ? 'HK' : index === 2 ? 'JP' : 'US',
      })),
    });
  const period = (path.match(/period=(day|week|month)/)?.[1] ?? 'day') as SiteUsagePeriod;
  if (path.includes('/machine-usage'))
    return scene === 'error'
      ? Response.json({ error: 'fixture unavailable' }, { status: 503 })
      : Response.json(usage(period));
  const { start, end } = bounds(period);
  const common = {
    collection_enabled: true,
    period,
    range_start_unix_secs: start,
    range_end_unix_secs: now,
    period_end_unix_secs: end,
    bucket_secs: period === 'month' ? 86400 : 3600,
    reporting_nodes: 9,
    expected_nodes: 9,
    limited: false,
    latest_report_unix_secs: now,
    observed_since_unix_secs: start,
  };
  if (path.includes('/site-usage')) {
    const categories = ['youtube', 'openai', 'google', 'github', 'anthropic'];
    const factor = period === 'month' ? 18 : period === 'week' ? 7 : 1;
    const rows = categories.map((site_id, index) => ({
      site_id,
      name: ['YouTube', 'OpenAI / ChatGPT', 'Google', 'GitHub', 'Claude'][index],
      uplink_bytes: 1024 ** 2 * factor,
      downlink_bytes: 1024 ** 3 * (5 - index) * factor,
    }));
    return Response.json({
      ...common,
      catalog_version: 1,
      rows,
      series: rows.map(row => ({ site_id: row.site_id, points: [[start, row.uplink_bytes, row.downlink_bytes]] })),
    });
  }
  if (path.includes('/other-domains') || path.includes('/sniffing-failed-targets'))
    return Response.json({ ...common, top_n: 128, rows: [] });
  return Response.json({ error: 'Unknown fixture request' }, { status: 404 });
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <main style={{ maxWidth: 1100, margin: '16px auto', padding: 12 }}>
      <nav aria-label="验收场景" style={{ display: 'flex', gap: 12, flexWrap: 'wrap', marginBottom: 16 }}>
        <span>本地示例 · 生产组件</span>
        {['regular', 'few', 'empty', 'gap', 'launch', 'error'].map(value => (
          <a key={value} href={`?scene=${value}&theme=${params.get('theme') ?? 'dark'}`}>
            {
              {
                regular: '常规',
                few: '三台机器',
                empty: '无流量',
                gap: '采集缺口',
                launch: '上线当月',
                error: '请求失败',
              }[value]
            }
          </a>
        ))}
        <a href={`?scene=${scene}&theme=${params.get('theme') === 'light' ? 'dark' : 'light'}`}>切换主题</a>
      </nav>
      <section className="panel user-split-detail">
        <div className="user-dhead">
          <div className="user-dhead-main">
            <div className="user-dhead-identity">
              <GeneratedUserAvatar id="alice" variant="plate" />
              <div className="dtitle">
                <h1 className="dname" style={{ margin: 0, font: 'inherit' }}>
                  <b>alice</b>
                </h1>
                <p className="dsub">统计 · 示例数据</p>
              </div>
            </div>
          </div>
        </div>
        <div className="user-dbody">
          <section className="panel config-panel user-dcard">
            <header>
              <PanelTitle of="usage">用户实时流量</PanelTitle>
            </header>
            <p className="note">卡片位置参照 · 机器流量位于实时流量与站点流量之间</p>
          </section>
          <UserMachineUsageCard tenantId="fixture" userId="alice" nameOf={id => names[+id.slice(1)] ?? id} />
          <UserSiteUsageCard tenantId="fixture" userId="alice" />
        </div>
      </section>
    </main>
  </QueryClientProvider>,
);
