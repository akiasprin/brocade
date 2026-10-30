// Option parts for the 线路页解锁 mockup. Never imported by the app.
// Each part reads the production chain unlock query (same key and fetcher as the 解锁 dialog), so the list and the
// dialog in 链详情 always show the same report. Verdicts, glyphs, service marks and the YouTube projection are the
// production ones; only the arrangement on the list page is new.
import { useRef, useState } from 'react';
import { useQueries, useQuery } from '@tanstack/react-query';
import {
  chainMembers,
  fetchChainUnlockCapabilities,
  fetchChainUnlockProbe,
  fetchE2eProbes,
  fetchNodes,
  fetchSnapshot,
  type E2eProbeItem,
  type NodeUnlockProbe,
  type SnapshotApp,
  type UnlockIpFamily,
  type UnlockResult,
  type UnlockService,
} from '../../src/api';
import { navigate } from '../../src/forge/route';
import { ServiceMark, UNLOCK_SERVICE_NAMES } from '../../src/panes/node-unlock';
import { Glyph, verdictOf, type Tone, type Verdict } from '../../src/panes/unlock-status';
import { youtubeResult } from '../../src/panes/youtube-unlock';
import { SegmentedControl } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { DialogLayer } from '../../src/ui/dialog';
import { byChain, toneOf } from '../../src/ui/probe';
import { RegionFlag } from '../../src/ui/region-flag';
import { unlockFlagRegion } from '../../src/ui/unlock-region';

export type Option = 'a' | 'b' | 'c' | 'd';
export type DisplayService = UnlockService | 'youtube';

/* ══ 服务与分类：与解锁磁贴同序 ══ */

type Category = 'ai' | 'media' | 'social';
export const CATEGORIES: { key: Category; name: string; services: DisplayService[] }[] = [
  { key: 'ai', name: 'AI', services: ['chatgpt', 'claude', 'gemini'] },
  {
    key: 'media',
    name: '流媒体',
    services: ['netflix', 'disney_plus', 'youtube', 'youtube_premium', 'prime_video', 'spotify', 'hbo_max', 'dazn'],
  },
  { key: 'social', name: '社区', services: ['tiktok', 'reddit'] },
];
export const SERVICES = CATEGORIES.flatMap(category => category.services);
const NAMES: Record<DisplayService, string> = { ...UNLOCK_SERVICE_NAMES, youtube: 'YouTube' };
/** Column heads in the table: two lines of 10px must fit a 52px column. */
const SHORT: Record<DisplayService, string> = {
  ...NAMES,
  youtube_premium: 'Premium',
  prime_video: 'Prime',
  hbo_max: 'HBO Max',
};

/** YouTube and YouTube Premium share one brand mark; Premium carries a small P so the two can be told apart. */
export function Mark({ service }: { service: DisplayService }) {
  return (
    <span className="cu-mark">
      <ServiceMark service={service} />
      {service === 'youtube_premium' && <i className="cu-mark-p">P</i>}
    </span>
  );
}

/* ══ 读数：与生产解锁面板相同的推导 ══ */

export interface Cell {
  service: DisplayService;
  verdict: Verdict;
  /** As the service reported it; Gemini's alpha-3 code is shown as alpha-2 so one list reads one code system.
   *  Reddit keeps its Fastly POP: that is a CDN location, not the visitor's region. */
  region: string | null;
  raw: string | null;
  /** 与上一次结果不同时的上次读数，例如「上次 仅自制剧」。 */
  change: string | null;
}

export type Freshness = 'loading' | 'never' | 'fresh' | 'running' | 'stale' | 'obsolete';

export interface Summary {
  freshness: Freshness;
  cells: Cell[];
  available: number;
  hasResults: boolean;
  /** Completion time of the results on screen. */
  at: number | null;
  /** The state in a few words; null when the results are fresh. */
  note: string | null;
  /** The same state for a table cell. */
  short: string | null;
}

const WAITING: Verdict = { tone: 'wait', label: '等待结果', note: null, reason: '检测完成后显示' };
const MISSING: Verdict = { tone: 'unknown', label: '未取得', note: null, reason: '本次没有这项结果' };

function changeOf(now: UnlockResult, before: UnlockResult, service: DisplayService): string | null {
  if (before.status === 'failed') return null;
  const a = verdictOf(now, service);
  const b = verdictOf(before, service);
  const parts: string[] = [];
  if (a.label !== b.label || a.note !== b.note) parts.push(b.note ? `${b.label}（${b.note}）` : b.label);
  if ((now.region ?? null) !== (before.region ?? null)) parts.push(before.region ?? '地区未知');
  return parts.length > 0 ? `上次 ${parts.join(' · ')}` : null;
}

const active = (job: NodeUnlockProbe | null | undefined) => job?.state === 'queued' || job?.state === 'running';

export function summarize(job: NodeUnlockProbe | null | undefined): Summary {
  const empty = { cells: [], available: 0, hasResults: false, at: null, note: null, short: null };
  if (job === undefined) return { freshness: 'loading', ...empty };
  if (job === null) return { freshness: 'never', ...empty, note: '尚未检测', short: '尚未检测' };
  const running = active(job);
  // 链路级失败时九项都是「失败」，不是各服务的事实：与超时一样改为显示上一次的结果。
  const current = job.results.length > 0 && !job.path_error ? job.results : null;
  const previous = !current && job.previous_results?.length ? job.previous_results : null;
  const shown = current ?? previous;
  const before =
    current && job.state === 'complete' && job.previous_results?.length
      ? new Map(job.previous_results.map(result => [result.service, result]))
      : null;
  const cells = SERVICES.map<Cell>(service => {
    const source = service === 'youtube' ? 'youtube_premium' : service;
    const project = (raw: UnlockResult | undefined) =>
      service === 'youtube'
        ? youtubeResult(raw, 'webpage')
        : service === 'youtube_premium'
          ? youtubeResult(raw, 'premium')
          : (raw ?? null);
    const result = project(shown?.find(item => item.service === source));
    const earlier = project(before?.get(source));
    const raw = result?.region ?? null;
    return {
      service,
      verdict: result ? verdictOf(result, service) : running ? WAITING : MISSING,
      region: raw && service === 'gemini' ? (unlockFlagRegion(service, raw) ?? raw) : raw,
      raw,
      change: result && earlier ? changeOf(result, earlier, service) : null,
    };
  });
  const [freshness, note, short]: [Freshness, string | null, string | null] = running
    ? ['running', job.state === 'queued' ? '等待机器响应' : '检测中', job.state === 'queued' ? '排队中' : '检测中']
    : job.state === 'expired'
      ? ['stale', '最近一次检测超时', '超时']
      : job.path_error === 'client_unavailable'
        ? ['stale', '入口无法启动检测客户端', '无法检测']
        : job.path_error === 'family_unavailable'
          ? ['stale', '链路尚未具备分族检测条件', '需更新配置']
          : job.path_error === 'configuration_changed'
            ? ['stale', '检测期间线路已变更', '线路已变更']
            : job.state === 'obsolete'
              ? [
                  'obsolete',
                  job.configuration_revision != null
                    ? `线路已变更 · 结果来自修订 ${job.configuration_revision}`
                    : '线路已变更',
                  '线路已变更',
                ]
              : ['fresh', null, null];
  return {
    freshness,
    cells,
    available: shown ? cells.filter(cell => cell.verdict.tone === 'ok').length : 0,
    hasResults: shown !== null,
    at: current ? job.completed_at : previous ? (job.previous_completed_at ?? null) : null,
    note,
    short,
  };
}

const unlockKey = (app: string, chain: string, family: UnlockIpFamily) => ['chain-unlock-probe', app, chain, family];
const unlockQuery = (app: string, chain: string, family: UnlockIpFamily) => ({
  queryKey: unlockKey(app, chain, family),
  queryFn: () => fetchChainUnlockProbe(app, chain, family),
  refetchInterval: (query: { state: { data: NodeUnlockProbe | null | undefined } }) =>
    active(query.state.data) ? 2_000 : 30_000,
});

export function useChainUnlock(app: string, chain: string, family: UnlockIpFamily = 'v4') {
  return summarize(useQuery(unlockQuery(app, chain, family)).data);
}

/* ══ 时间：相对值，悬停看具体时刻 ══ */

function span(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分钟`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时`;
  return `${Math.floor(s / 86400)} 天`;
}
const absolute = (secs: number) => new Date(secs * 1000).toLocaleString('zh-CN', { hour12: false });
function Ago({ at }: { at: number }) {
  const now = useNow();
  return (
    <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
      {now / 1000 - at < 45 ? '刚刚' : `${span(now / 1000 - at)}前`}
    </time>
  );
}

const cellTitle = (cell: Cell) =>
  [NAMES[cell.service], cell.verdict.label, cell.raw, cell.change].filter(Boolean).join(' · ');

/** Fresh results say nothing; any other state is one quiet line above the readings. */
function StateLine({ s }: { s: Summary }) {
  if (!s.note || s.freshness === 'never') return null;
  const tone = s.freshness === 'obsolete' ? 'warn' : s.freshness === 'running' ? 'run' : 'plain';
  return (
    <span className="cu-state" data-tone={tone}>
      {s.freshness === 'running' && <Glyph tone="wait" />}
      <span>{s.note}</span>
      {s.at && s.freshness !== 'obsolete' && (
        <span className="cu-state-at">
          <Ago at={s.at} />
          的结果
        </span>
      )}
    </span>
  );
}

const dimmed = (s: Summary) => s.freshness === 'running' || s.freshness === 'stale';

/* ══ 链在模型里的事实：缺接入面的链测不了 ══ */

function useChainFacts(app: string, chain: string) {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const model = snapshot.data?.snapshot.apps.find(item => item.id === app);
  return { known: !!model, testable: !!model?.ingresses.some(ingress => ingress.chain === chain) };
}

/* ══ A 标志条 ══ */

export function StripPart({ app, chain }: { app: string; chain: string }) {
  const facts = useChainFacts(app, chain);
  const s = useChainUnlock(app, chain);
  if (!facts.testable || s.freshness === 'loading') return null;
  if (!s.hasResults)
    return (
      <div className="cu-a">
        <span className="cu-quiet">{s.note ?? '未取得结果'}</span>
      </div>
    );
  return (
    <div className="cu-a" aria-label={`解锁 ${s.available} / ${s.cells.length}`}>
      <StateLine s={s} />
      <span className="cu-a-row" data-dim={dimmed(s) || undefined}>
        {CATEGORIES.map(category => (
          <span key={category.key} className="cu-a-seg">
            {s.cells
              .filter(cell => category.services.includes(cell.service))
              .map(cell => (
                <span key={cell.service} className="cu-a-cell" data-tone={cell.verdict.tone} title={cellTitle(cell)}>
                  <Mark service={cell.service} />
                  {cell.change && <i className="cu-dot" />}
                </span>
              ))}
          </span>
        ))}
      </span>
    </div>
  );
}

/* ══ B 常用 ══ */

export const DEFAULT_PINNED: DisplayService[] = [
  'chatgpt',
  'claude',
  'gemini',
  'netflix',
  'disney_plus',
  'youtube_premium',
  'spotify',
  'tiktok',
];

export function PinnedPart({ app, chain, pinned }: { app: string; chain: string; pinned: DisplayService[] }) {
  const facts = useChainFacts(app, chain);
  const s = useChainUnlock(app, chain);
  if (!facts.testable || s.freshness === 'loading' || pinned.length === 0) return null;
  if (!s.hasResults)
    return (
      <div className="cu-b">
        <span className="cu-quiet">{s.note ?? '未取得结果'}</span>
      </div>
    );
  return (
    <div className="cu-b">
      <StateLine s={s} />
      <span className="cu-b-grid" data-dim={dimmed(s) || undefined}>
        {s.cells
          .filter(cell => pinned.includes(cell.service))
          .map(cell => (
            <span key={cell.service} className="cu-b-cell" data-tone={cell.verdict.tone} title={cellTitle(cell)}>
              <Mark service={cell.service} />
              <span className="cu-b-code">{cell.region ?? '—'}</span>
              <Glyph tone={cell.verdict.tone} />
              {cell.change && <i className="cu-dot" />}
            </span>
          ))}
      </span>
    </div>
  );
}

export function PinnedPicker({
  pinned,
  onChange,
}: {
  pinned: DisplayService[];
  onChange: (next: DisplayService[]) => void;
}) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const toggle = (service: DisplayService) =>
    onChange(SERVICES.filter(item => (item === service ? !pinned.includes(service) : pinned.includes(item))));
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="btn cu-b-pick-btn"
        aria-expanded={open}
        onClick={() => setOpen(value => !value)}
      >
        常用服务
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="end" label="常用服务" onClose={() => setOpen(false)}>
          <section className="dialog-surface cu-b-pick" aria-label="常用服务">
            {CATEGORIES.map(category => (
              <fieldset key={category.key}>
                <legend>{category.name}</legend>
                {category.services.map(service => (
                  <label key={service}>
                    <input type="checkbox" checked={pinned.includes(service)} onChange={() => toggle(service)} />
                    <Mark service={service} />
                    <span>{NAMES[service]}</span>
                  </label>
                ))}
              </fieldset>
            ))}
          </section>
        </DialogLayer>
      )}
    </>
  );
}

/* ══ C 筛选 ══ */

/** Every chain on the page, in the list's order. */
function useChains() {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const apps = snapshot.data?.snapshot.apps ?? [];
  return apps.flatMap(app => app.chains.map(chain => ({ app, chain: chain.id })));
}

export function LensBar({ lens, onLens }: { lens: DisplayService; onLens: (next: DisplayService) => void }) {
  const chains = useChains();
  const testable = chains.filter(item => item.app.ingresses.some(ingress => ingress.chain === item.chain));
  const reports = useQueries({ queries: testable.map(item => unlockQuery(item.app.id, item.chain, 'v4')) });
  const summaries = reports.map(report => summarize(report.data));
  const ok = summaries.filter(s => s.cells.find(cell => cell.service === lens)?.verdict.tone === 'ok').length;
  return (
    <div className="cu-c-bar">
      <span className="cu-c-chips" role="group" aria-label="查看服务">
        {CATEGORIES.map(category => (
          <span key={category.key} className="cu-c-seg">
            {category.services.map(service => (
              <button
                key={service}
                type="button"
                className="cu-c-chip"
                aria-pressed={lens === service}
                title={NAMES[service]}
                onClick={() => onLens(service)}
              >
                <Mark service={service} />
                {lens === service && <span>{NAMES[service]}</span>}
              </button>
            ))}
          </span>
        ))}
      </span>
      <span className="cu-c-count">
        <b>{ok}</b> / {testable.length} 条链可用
      </span>
    </div>
  );
}

export function LensLine({ app, chain, lens }: { app: string; chain: string; lens: DisplayService }) {
  const facts = useChainFacts(app, chain);
  const s = useChainUnlock(app, chain);
  if (!facts.known || s.freshness === 'loading') return null;
  if (!facts.testable || !s.hasResults)
    return (
      <div className="cu-c-line" data-dim="">
        <Glyph tone="unknown" />
        <span className="cu-c-verdict">{facts.testable ? (s.note ?? '未取得结果') : '缺接入面'}</span>
      </div>
    );
  const cell = s.cells.find(item => item.service === lens)!;
  const tone: Tone = cell.verdict.tone;
  return (
    <div
      className="cu-c-line"
      data-tone={tone}
      data-dim={tone === 'ok' || tone === 'part' ? undefined : ''}
      data-stale={dimmed(s) || undefined}
      title={s.note ?? cell.verdict.reason}
    >
      <Glyph tone={tone} />
      <span className="cu-c-verdict">{cell.verdict.label}</span>
      {cell.region && (
        <span className="cu-c-region">
          <RegionFlag code={unlockFlagRegion(lens, cell.region)} />
          {cell.region}
        </span>
      )}
      {cell.change && <span className="unlock-change">{cell.change}</span>}
      {s.freshness !== 'fresh' && (
        <span className="cu-c-when" data-tone={s.freshness}>
          {s.short}
        </span>
      )}
    </div>
  );
}

/* ══ D 对照表 ══ */

export type View = 'cards' | 'table';

export function ViewSwitch({
  view,
  onView,
  family,
  onFamily,
}: {
  view: View;
  onView: (next: View) => void;
  family: UnlockIpFamily;
  onFamily: (next: UnlockIpFamily) => void;
}) {
  return (
    <span className="cu-d-switches">
      {view === 'table' && (
        <SegmentedControl
          value={family}
          options={[
            { value: 'v4' as const, label: 'IPv4' },
            { value: 'v6' as const, label: 'IPv6' },
          ]}
          ariaLabel="解锁地址族"
          onChange={onFamily}
        />
      )}
      <SegmentedControl
        value={view}
        options={[
          { value: 'cards' as const, label: '卡片' },
          { value: 'table' as const, label: '解锁' },
        ]}
        ariaLabel="线路视图"
        onChange={onView}
      />
    </span>
  );
}

type Lamp = 'ok' | 'warn' | 'bad' | 'idle';

/** Same order of facts as the production card lamp: model first, then the end-to-end probe. */
function lampOf(app: SnapshotApp, chain: string, retired: Set<string>, probe: E2eProbeItem | undefined): Lamp {
  const ingress = app.ingresses.find(item => item.chain === chain);
  if (!ingress || chainMembers(app, chain).some(node => retired.has(node))) return 'bad';
  const tone = toneOf(probe);
  if (tone === 'down') return 'bad';
  if (tone === 'odd') return 'warn';
  const users = new Set(
    app.grants
      .filter(grant => app.ingresses.some(item => item.chain === chain && item.id === grant.ingress))
      .map(grant => `${grant.tenant}/${grant.user}`),
  ).size;
  return tone === 'none' || users === 0 ? 'idle' : 'ok';
}

export function UnlockTable({ family }: { family: UnlockIpFamily }) {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const probes = useQuery({ queryKey: ['e2e-probes'], queryFn: () => fetchE2eProbes() });
  const apps = snapshot.data?.snapshot.apps ?? [];
  const rows = apps.flatMap(app => app.chains.map(chain => ({ app, chain })));
  const reports = useQueries({ queries: rows.map(row => unlockQuery(row.app.id, row.chain.id, family)) });
  const capabilities = useQueries({
    queries: rows.map(row => ({
      queryKey: ['chain-unlock-capabilities', row.app.id, row.chain.id],
      queryFn: () => fetchChainUnlockCapabilities(row.app.id, row.chain.id),
      enabled: family === 'v6',
    })),
  });
  const retired = new Set((nodes.data?.nodes ?? []).filter(node => node.retired_at).map(node => node.node_id));
  const probeOf = byChain(probes.data?.chains);
  let index = 0;
  return (
    <div className="cu-d-scroll">
      <table className="cu-d">
        <thead>
          <tr className="cu-d-cats">
            <th className="cu-d-corner" />
            {CATEGORIES.map(category => (
              <th key={category.key} colSpan={category.services.length} scope="colgroup">
                {category.name}
              </th>
            ))}
            <th className="cu-d-when" />
          </tr>
          <tr className="cu-d-heads">
            <th className="cu-d-corner" scope="col">
              链
            </th>
            {SERVICES.map(service => (
              <th
                key={service}
                scope="col"
                title={NAMES[service]}
                data-first={CATEGORIES.some(category => category.services[0] === service) || undefined}
              >
                <Mark service={service} />
                <span>{SHORT[service]}</span>
              </th>
            ))}
            <th className="cu-d-when" scope="col">
              检测
            </th>
          </tr>
        </thead>
        {apps.map((app, groupIndex) => (
          <tbody key={app.id}>
            <tr className="cu-d-group">
              <th colSpan={SERVICES.length + 2} scope="rowgroup">
                <span className="no">{String(groupIndex + 1).padStart(2, '0')}</span>
                <b>{app.label || app.id}</b>
                <span className="count">{app.chains.length} 条链</span>
              </th>
            </tr>
            {app.chains.map(chain => {
              const at = index++;
              const s = summarize(reports[at]?.data);
              const testable = app.ingresses.some(ingress => ingress.chain === chain.id);
              const noV6 = family === 'v6' && capabilities[at]?.data?.ipv6_available === false;
              const lamp = lampOf(app, chain.id, retired, probeOf.get(chain.id));
              const off = chainMembers(app, chain.id).some(node => retired.has(node));
              const open = () => navigate('chains', { p: 'chain', app: app.id, chain: chain.id });
              const blank = !testable
                ? '缺接入面'
                : noV6
                  ? (capabilities[at]?.data?.ipv6_reason ?? '无 IPv6')
                  : !s.hasResults && s.freshness !== 'loading'
                    ? (s.note ?? '未取得结果')
                    : null;
              return (
                <tr
                  key={chain.id}
                  className="cu-d-row"
                  data-off={off || undefined}
                  tabIndex={0}
                  onClick={open}
                  onKeyDown={event => {
                    if (event.key === 'Enter') open();
                  }}
                >
                  <th scope="row" className="cu-d-chain">
                    <span className="cu-d-chain-in">
                      <i className="cu-lamp" data-tone={lamp} />
                      <RegionFlag code={chain.subscription_country} />
                      <span className="cu-d-name">{chain.name || chain.id}</span>
                    </span>
                  </th>
                  {blank !== null ? (
                    <td className="cu-d-blank" colSpan={SERVICES.length}>
                      {blank}
                    </td>
                  ) : (
                    s.cells.map(cell => (
                      <td
                        key={cell.service}
                        data-tone={cell.verdict.tone}
                        data-dim={dimmed(s) || undefined}
                        data-first={CATEGORIES.some(category => category.services[0] === cell.service) || undefined}
                        title={cellTitle(cell)}
                      >
                        <span className="cu-d-cell">
                          <Glyph tone={cell.verdict.tone} />
                          <span>{cell.region ?? '—'}</span>
                          {cell.change && <i className="cu-dot" />}
                        </span>
                      </td>
                    ))
                  )}
                  <td className="cu-d-when" data-tone={s.freshness}>
                    {blank !== null && s.freshness !== 'stale' ? (
                      '—'
                    ) : s.freshness === 'running' ? (
                      <span className="cu-d-when-in">
                        <Glyph tone="wait" />
                        {s.short}
                      </span>
                    ) : s.freshness === 'fresh' && s.at ? (
                      <Ago at={s.at} />
                    ) : (
                      <span title={s.note ?? undefined}>{s.short}</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        ))}
      </table>
    </div>
  );
}
