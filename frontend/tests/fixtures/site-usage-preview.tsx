// Local visual fixture, never imported by the production application.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { UserSiteUsageCard } from '../../src/user-site-usage';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import { theme } from '../../src/forge/theme';
import type { SiteUsagePeriod, UserSiteUsageRow } from '../../src/api';
import '../../src/styles.css';

if (theme.snapshot() !== (new URLSearchParams(location.search).get('theme') === 'light' ? 'light' : 'dark'))
  theme.toggle();
const now = Math.floor(Date.now() / 1000);
const dailyRows: UserSiteUsageRow[] = [
  { site_id: 'youtube', name: 'YouTube', uplink_bytes: 300_000_000, downlink_bytes: 24_700_000_000 },
  { site_id: 'twitter', name: 'X / Twitter', uplink_bytes: 100_000_000, downlink_bytes: 900_000_000 },
  { site_id: 'facebook', name: 'Facebook', uplink_bytes: 50_000_000, downlink_bytes: 800_000_000 },
  { site_id: 'instagram', name: 'Instagram', uplink_bytes: 50_000_000, downlink_bytes: 700_000_000 },
  { site_id: 'bytedance', name: '字节跳动', uplink_bytes: 20_000_000, downlink_bytes: 600_000_000 },
  { site_id: 'baidu', name: '百度', uplink_bytes: 20_000_000, downlink_bytes: 500_000_000 },
  { site_id: 'tencent', name: '腾讯', uplink_bytes: 20_000_000, downlink_bytes: 400_000_000 },
  { site_id: 'netflix', name: 'Netflix', uplink_bytes: 85_000_000, downlink_bytes: 8_215_000_000 },
  { site_id: 'sniffing-failed', name: '嗅探失败', uplink_bytes: 200_000_000, downlink_bytes: 3_000_000_000 },
  { site_id: 'other', name: '其他站点', uplink_bytes: 400_000_000, downlink_bytes: 2_100_000_000 },
  { site_id: 'openai', name: 'OpenAI / ChatGPT', uplink_bytes: 100_000_000, downlink_bytes: 900_000_000 },
  { site_id: 'telegram', name: 'Telegram', uplink_bytes: 60_000_000, downlink_bytes: 840_000_000 },
  { site_id: 'disney', name: 'Disney+', uplink_bytes: 10_000_000, downlink_bytes: 790_000_000 },
  { site_id: 'anthropic', name: 'Claude', uplink_bytes: 80_000_000, downlink_bytes: 620_000_000 },
  { site_id: 'github', name: 'GitHub', uplink_bytes: 40_000_000, downlink_bytes: 560_000_000 },
  { site_id: 'tiktok', name: 'TikTok', uplink_bytes: 15_000_000, downlink_bytes: 485_000_000 },
  { site_id: 'steam', name: 'Steam', uplink_bytes: 10_000_000, downlink_bytes: 390_000_000 },
  { site_id: 'discord', name: 'Discord', uplink_bytes: 100_000_000, downlink_bytes: 200_000_000 },
  { site_id: 'spotify', name: 'Spotify', uplink_bytes: 10_000_000, downlink_bytes: 190_000_000 },
  { site_id: 'google', name: 'Google', uplink_bytes: 30_000_000, downlink_bytes: 120_000_000 },
  { site_id: 'microsoft', name: 'Microsoft', uplink_bytes: 20_000_000, downlink_bytes: 80_000_000 },
  { site_id: 'apple', name: 'Apple', uplink_bytes: 10_000_000, downlink_bytes: 40_000_000 },
  { site_id: 'unidentified', name: '未识别', uplink_bytes: 5_000_000, downlink_bytes: 5_000_000 },
];
function bounds(period: SiteUsagePeriod) {
  const shifted = new Date((now + 8 * 3600) * 1000);
  const day = Math.floor((now + 8 * 3600) / 86_400) * 86_400 - 8 * 3600;
  const start =
    period === 'day'
      ? day
      : period === 'week'
        ? day - ((shifted.getUTCDay() + 6) % 7) * 86_400
        : Date.UTC(shifted.getUTCFullYear(), shifted.getUTCMonth(), 1) / 1000 - 8 * 3600;
  const end =
    period === 'month'
      ? Date.UTC(shifted.getUTCFullYear(), shifted.getUTCMonth() + 1, 1) / 1000 - 8 * 3600
      : start + (period === 'week' ? 7 : 1) * 86_400;
  return { start, end, detailStart: Math.max(start, (Math.floor(now / 3600) - 23) * 3600) };
}
function detailBytes(index: number, period: SiteUsagePeriod) {
  const elapsed = (now - bounds(period).detailStart) / 86_400;
  return {
    uplink_bytes: Math.round((10_000_000 * elapsed) / (index + 1)),
    downlink_bytes: Math.round((400_000_000 * elapsed) / (index + 1)),
  };
}
globalThis.fetch = async input => {
  const failed = String(input).match(/^\/users\/fixture\/alice\/sniffing-failed-targets\?period=(day|week|month)$/);
  if (failed)
    return Response.json({
      collection_enabled: true,
      top_n: 128,
      range_start_unix_secs: bounds(failed[1] as SiteUsagePeriod).detailStart,
      range_end_unix_secs: now,
      reporting_nodes: 4,
      expected_nodes: 4,
      limited: false,
      latest_report_unix_secs: now,
      observed_since_unix_secs: now - 86400,
      first_record_unix_secs: Math.min(
        Math.floor(now / 3600) * 3600,
        bounds(failed[1] as SiteUsagePeriod).detailStart + 2 * 3600,
      ),
      rows: ['tcp://192.0.2.1:443', 'udp://[2001:db8::1]:443', 'tcp://198.51.100.20:8443', 'udp://203.0.113.8:53'].map(
        (target, index) => ({
          target,
          ...detailBytes(index, failed[1] as SiteUsagePeriod),
        }),
      ),
    });
  const domains = String(input).match(/^\/users\/fixture\/alice\/other-domains\?period=(day|week|month)$/);
  if (domains)
    return Response.json({
      collection_enabled: true,
      top_n: 128,
      range_start_unix_secs: bounds(domains[1] as SiteUsagePeriod).detailStart,
      range_end_unix_secs: now,
      reporting_nodes: 4,
      expected_nodes: 4,
      limited: false,
      latest_report_unix_secs: now,
      observed_since_unix_secs: now - 86400,
      first_record_unix_secs: Math.min(
        Math.floor(now / 3600) * 3600,
        bounds(domains[1] as SiteUsagePeriod).detailStart + 2 * 3600,
      ),
      rows: [
        'video.cinema.example',
        'updates.software.example',
        'images.gallery.example',
        'news.journal.example',
        'docs.reference.example',
        'audio.radio.example',
      ].map((domain, index) => ({
        domain,
        ...detailBytes(index, domains[1] as SiteUsagePeriod),
      })),
    });
  const match = String(input).match(/^\/users\/fixture\/alice\/site-usage\?period=(day|week|month)$/);
  if (!match) return Response.json({ error: '本地示例只提供 alice 的站点数据。' }, { status: 404 });
  const period = match[1] as SiteUsagePeriod;
  const { start, end } = bounds(period);
  const bucket = period === 'month' ? 86_400 : 3600;
  const elapsed = now - start;
  const rows = dailyRows.map(row => ({
    ...row,
    uplink_bytes: Math.round((row.uplink_bytes * elapsed) / 86_400),
    downlink_bytes: Math.round((row.downlink_bytes * elapsed) / 86_400),
  }));
  return Response.json({
    collection_enabled: true,
    catalog_version: 1,
    period,
    range_start_unix_secs: start,
    range_end_unix_secs: now,
    period_end_unix_secs: end,
    bucket_secs: bucket,
    // Every category total is distributed over elapsed buckets, including the partial current one.
    series: rows.map(row => ({
      site_id: row.site_id,
      points: Array.from({ length: Math.ceil(elapsed / bucket) }, (_, index) => {
        const from = index * bucket;
        const until = Math.min(elapsed, from + bucket);
        const part = (total: number) =>
          Number((BigInt(total) * BigInt(until)) / BigInt(elapsed) - (BigInt(total) * BigInt(from)) / BigInt(elapsed));
        return [start + from, part(row.uplink_bytes), part(row.downlink_bytes)];
      }),
    })),
    reporting_nodes: 4,
    expected_nodes: 4,
    limited: false,
    latest_report_unix_secs: now,
    observed_since_unix_secs: new URLSearchParams(location.search).has('resumed')
      ? bounds('day').start + 105 * 60
      : now - 31 * 86400,
    rows,
  });
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient()}>
    <main style={{ maxWidth: 1000, margin: '32px auto', padding: 16 }}>
      <p className="note">用户详情预览 · 以下为 alice 的示例数据，不代表真实用户流量</p>
      <section className="panel user-split-detail" aria-labelledby="fixture-user-name">
        <div className="user-dhead">
          <div className="user-dhead-main">
            <div className="user-dhead-identity">
              <GeneratedUserAvatar id="alice" variant="plate" />
              <div className="dtitle">
                <h1 className="dname" id="fixture-user-name" style={{ margin: 0, font: 'inherit' }}>
                  <b>alice</b>
                </h1>
                <p className="dsub">示例用户</p>
              </div>
            </div>
          </div>
        </div>
        <div className="user-dbody">
          <UserSiteUsageCard tenantId="fixture" userId="alice" />
        </div>
      </section>
    </main>
  </QueryClientProvider>,
);
