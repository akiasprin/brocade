// Option parts for the 解锁检测 mockup. Never imported by the app.
// Every option reads the same query keys and endpoints as `panes/node-unlock.tsx`, so the hidden production panel
// and the option share one cache and one polling loop. The options differ only in how a result set is laid out:
//   A 清单  — category groups as inset lists: name on the left, region and verdict on the right.
//   B 磁贴  — one tile per service, a large region code, rows by category.
//   C 结论  — one line per verdict (可用 / 部分可用 / 地区不支持 / 访问受限 / 检测失败) with service chips.
//   D 地区  — one card per region the services reported, so split routing reads directly.
// Chains open either inline (a block under 连通性) or as a dialog from the page head; the body is the same.
import { useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchChainUnlockProbe,
  fetchNodeUnlockProbe,
  startChainUnlockProbe,
  startNodeUnlockProbe,
  UNLOCK_PROBE_PROTOCOL_VERSION,
  type NodeAgentStateItem,
  type NodeUnlockProbe,
  type UnlockResult,
  type UnlockService,
  type YoutubeDiagnostics,
  type YoutubePageResult,
} from '../../src/api';
import { UNLOCK_SERVICE_NAMES } from '../../src/panes/node-unlock';
import { ErrorBox } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { Icon } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { SiteIcon } from '../../src/ui/site-icon';

export type Option = 'a' | 'b' | 'c' | 'd';
export type Variant = 'tab' | 'dialog' | 'inline';
type Tone = 'ok' | 'part' | 'no' | 'unknown' | 'wait';
type Category = 'ai' | 'media' | 'social';

/* ══ 服务 ══ */

const SERVICES = Object.keys(UNLOCK_SERVICE_NAMES) as UnlockService[];
const CATEGORY: Record<UnlockService, Category> = {
  chatgpt: 'ai',
  claude: 'ai',
  gemini: 'ai',
  netflix: 'media',
  disney_plus: 'media',
  youtube_premium: 'media',
  prime_video: 'media',
  tiktok: 'social',
  reddit: 'social',
};
const CATEGORIES: { key: Category; name: string }[] = [
  { key: 'ai', name: 'AI' },
  { key: 'media', name: '流媒体' },
  { key: 'social', name: '社区' },
];
/** Monochrome marks already shipped in `ui/site-icon.tsx`; Gemini and Prime Video have none there. */
const BRAND: Partial<Record<UnlockService, string>> = {
  chatgpt: 'openai',
  claude: 'anthropic',
  netflix: 'netflix',
  disney_plus: 'disney',
  youtube_premium: 'youtube',
  tiktok: 'tiktok',
  reddit: 'reddit',
};

export function ServiceMark({ service }: { service: UnlockService }) {
  const brand = BRAND[service];
  return (
    <span className="ul-mark">
      {brand ? (
        <SiteIcon siteId={brand} />
      ) : (
        /* 本地绘制的识别符，不是官方标志：Gemini 用四角星，Prime Video 用播放键加弧形箭头。 */
        <span className="site-brand-icon" aria-hidden="true">
          {service === 'gemini' ? (
            <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" focusable="false">
              <path d="M12 2c.6 5.3 4.7 9.4 10 10-5.3.6-9.4 4.7-10 10-.6-5.3-4.7-9.4-10-10 5.3-.6 9.4-4.7 10-10Z" />
            </svg>
          ) : (
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" focusable="false">
              <path d="M9.6 4.6v7.2l5.8-3.6Z" fill="currentColor" stroke="none" />
              <path d="M2.8 14.6c5.4 3.7 12.9 3.7 18.4 0" strokeWidth="2.1" strokeLinecap="round" />
              <path d="M17.6 13.3l3.6 1.3-1.1 3.6" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          )}
        </span>
      )}
    </span>
  );
}

/** 开锁：本 mockup 的页签与空状态图标，落地时加入 `ui/icons.tsx`。 */
export function UnlockIcon({ size = 14 }: { size?: number }) {
  return (
    <span className="ui-icon" aria-hidden="true">
      <svg
        width={size}
        height={size}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <rect x="3" y="7.2" width="10" height="6.6" rx="1.6" />
        <path d="M5.4 7.2V5a2.6 2.6 0 0 1 5.1-.8" />
        <path d="M8 9.8v1.6" />
      </svg>
    </span>
  );
}

function RefreshIcon() {
  return (
    <span className="ui-icon ul-run-icon" aria-hidden="true">
      <svg
        width="13"
        height="13"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M13 8a5 5 0 1 1-1.5-3.6" />
        <path d="M11.9 1.9v2.7H9.2" />
      </svg>
    </span>
  );
}

/** 结论符号：可用实心勾、部分可用半圆、不可用实心叉、失败空心横线、等待转圈。颜色只由 tone 决定。 */
export function Glyph({ tone }: { tone: Tone }) {
  return (
    <svg className="ul-glyph" data-tone={tone} viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      {tone === 'ok' && (
        <>
          <circle cx="8" cy="8" r="7.2" className="fill" />
          <path d="M4.9 8.3 7.1 10.4 11.2 5.9" className="mark" />
        </>
      )}
      {tone === 'no' && (
        <>
          <circle cx="8" cy="8" r="7.2" className="fill" />
          <path d="M5.7 5.7 10.3 10.3 M10.3 5.7 5.7 10.3" className="mark" />
        </>
      )}
      {tone === 'part' && (
        <>
          <circle cx="8" cy="8" r="6.4" className="ring" />
          <path d="M8 1.6A6.4 6.4 0 0 0 8 14.4Z" className="fill" />
        </>
      )}
      {tone === 'unknown' && (
        <>
          <circle cx="8" cy="8" r="6.4" className="ring" />
          <path d="M5.4 8h5.2" className="dash" />
        </>
      )}
      {tone === 'wait' && <circle cx="8" cy="8" r="6" className="spin" />}
    </svg>
  );
}

const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const regionName = (code: string) => {
  try {
    return REGION_NAMES.of(code) ?? code;
  } catch {
    return code;
  }
};

export function Region({ code }: { code: string | null }) {
  if (!code) return <span className="ul-region none">—</span>;
  return (
    <span className="ul-region" title={regionName(code)}>
      <RegionFlag code={code} />
      <span>{code}</span>
    </span>
  );
}

/* ══ YouTube 分项 ══ */

const pageVerdict = (page: YoutubePageResult) => verdictOf({ service: 'youtube_premium', ...page, region: null });
const pageRegion = (page: YoutubePageResult) => page.privacy_region ?? page.topbar_region;

/**
 * 默认只多一行要点：网页结论与 Premium 不同时写出网页结论，再写视频 CDN 的位置。
 * HTTP 状态、页面配置 GL、CDN 节点与用时放进 ⓘ。
 */
function youtubeLine(youtube: YoutubeDiagnostics | null): string | null {
  if (!youtube) return null;
  const parts: string[] = [];
  const web = pageVerdict(youtube.webpage);
  if (web.label !== pageVerdict(youtube.premium).label) parts.push(`网页${web.label}`);
  const { cdn } = youtube;
  if (cdn.node)
    parts.push(`CDN ${cdn.country ? regionName(cdn.country) : ''} ${cdn.airport ?? ''}`.replace(/\s+/g, ' ').trim());
  else if (cdn.error) parts.push(`CDN ${FAILURE[cdn.error] ?? '未取得'}`);
  return parts.length > 0 ? parts.join(' · ') : null;
}

function PageLine({ name, page }: { name: string; page: YoutubePageResult }) {
  const verdict = pageVerdict(page);
  return (
    <>
      <dt>{name}</dt>
      <dd>
        <Glyph tone={verdict.tone} />
        <span>{verdict.label}</span>
        <Region code={pageRegion(page)} />
        <span className="ul-yt-dim">HTTP {page.http_status ?? '—'}</span>
      </dd>
    </>
  );
}

/** ⓘ：YouTube 的完整分项，贴着按钮弹出。 */
export function YoutubeMore({ youtube }: { youtube: YoutubeDiagnostics | null }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  if (!youtube) return null;
  const { cdn } = youtube;
  const gl = youtube.premium.page_region ?? youtube.webpage.page_region;
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="ul-info"
        aria-label="YouTube 分项"
        aria-expanded={open}
        onClick={event => {
          event.stopPropagation();
          setOpen(value => !value);
        }}
      >
        <Icon of="info" size={13} />
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="end" label="YouTube 分项" onClose={() => setOpen(false)}>
          <div className="dialog-surface ul-yt">
            <dl>
              <PageLine name="网页" page={youtube.webpage} />
              <PageLine name="Premium" page={youtube.premium} />
              <dt>视频 CDN</dt>
              <dd>
                {cdn.node ? (
                  <>
                    <span>
                      {cdn.country ? `${regionName(cdn.country)} · ${cdn.airport}` : (cdn.airport ?? '位置未识别')}
                    </span>
                    <code className="ul-yt-dim">{cdn.node}</code>
                  </>
                ) : (
                  <span className="ul-yt-dim">{cdn.error ? (FAILURE[cdn.error] ?? '未取得') : '未取得'}</span>
                )}
                {cdn.elapsed_ms > 0 && (
                  <span className="ul-yt-dim" title="经完整链路的请求用时，不是出口到 CDN 的往返">
                    {cdn.elapsed_ms} ms
                  </span>
                )}
              </dd>
              {gl && (
                <>
                  <dt>页面配置</dt>
                  <dd>
                    <span>GL {gl}</span>
                    <span className="ul-yt-dim">仅页面配置，不作为地区</span>
                  </dd>
                </>
              )}
            </dl>
          </div>
        </DialogLayer>
      )}
    </>
  );
}

/* ══ 结果的读法 ══ */

interface Verdict {
  tone: Tone;
  /** Short verdict shown everywhere: 可用 / 仅自制剧 / 地区不支持 / 访问受限 / 检测失败. */
  label: string;
  /** A qualifier that changes how the verdict is used, shown next to it (client DNS). */
  note: string | null;
  /** Why the service answered that way; the failure reason is shown, the rest is a tooltip. */
  reason: string;
}

const FAILURE: Record<string, string> = {
  timeout: '请求超时',
  network_error: '网络连接失败',
  unrecognized_response: '未识别服务响应',
};

function verdictOf(result: UnlockResult): Verdict {
  if (result.client_dns)
    return {
      tone: 'ok',
      label: '可用',
      note: '客户端 DNS',
      reason: '将域名交给链路时请求失败，入口解析 IPv4 后经同一链路访问成功',
    };
  switch (result.status) {
    case 'available':
      return { tone: 'ok', label: '可用', note: null, reason: '服务网页可访问' };
    case 'partial':
      return result.code === 'originals_only'
        ? { tone: 'part', label: '仅自制剧', note: null, reason: '测试片目不可访问，可能仅支持自制剧' }
        : result.code === 'web_only'
          ? { tone: 'part', label: '仅网页可用', note: null, reason: '网页端通过，App 端访问受限' }
          : { tone: 'part', label: '仅 App 可用', note: null, reason: 'App 端通过，网页端访问受限' };
    case 'unsupported_region':
      return { tone: 'no', label: '地区不支持', note: null, reason: '服务返回地区限制' };
    case 'restricted':
      return { tone: 'no', label: '访问受限', note: null, reason: '服务拒绝访问或要求验证' };
    case 'failed':
      return { tone: 'unknown', label: '检测失败', note: null, reason: FAILURE[result.code] ?? '检测失败' };
  }
}

interface Row {
  service: UnlockService;
  name: string;
  category: Category;
  result: UnlockResult | null;
  verdict: Verdict;
  region: string | null;
  /** 与上一次结果不同时的上次读数，例如「上次 仅自制剧」「上次 SG」。 */
  change: string | null;
  /** YouTube 分项（Agent v49 起）；旧结果没有，这时不显示分项入口。 */
  youtube: YoutubeDiagnostics | null;
}

const WAITING: Verdict = { tone: 'wait', label: '等待结果', note: null, reason: '检测完成后显示' };
const MISSING: Verdict = { tone: 'unknown', label: '未取得', note: null, reason: '本次没有这项结果' };

function changeOf(now: UnlockResult, before: UnlockResult | undefined): string | null {
  if (!before) return null;
  const a = verdictOf(now);
  const b = verdictOf(before);
  const parts: string[] = [];
  if (a.label !== b.label || a.note !== b.note) parts.push(b.note ? `${b.label}（${b.note}）` : b.label);
  if ((now.region ?? null) !== (before.region ?? null)) parts.push(before.region ?? '地区未知');
  return parts.length > 0 ? `上次 ${parts.join(' · ')}` : null;
}

/* ══ 时间 ══ */

const GAP = ' ';
function span(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}${GAP}秒`;
  if (s < 3600) return `${Math.floor(s / 60)}${GAP}分钟`;
  if (s < 86400) return `${Math.floor(s / 3600)}${GAP}小时`;
  return `${Math.floor(s / 86400)}${GAP}天`;
}
const absolute = (secs: number) => new Date(secs * 1000).toLocaleString('zh-CN', { hour12: false });
function Ago({ at, now }: { at: number; now: number }) {
  return <time title={absolute(at)}>{now / 1000 - at < 45 ? '刚刚' : `${span(now / 1000 - at)}前`}</time>;
}
function Until({ at, now }: { at: number; now: number }) {
  const left = at - now / 1000;
  return <time title={absolute(at)}>{left <= 60 ? '即将' : `${span(left)}后`}</time>;
}
const clockOf = (seconds: number) => {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};

/* ══ 数据：与生产面板同一查询键 ══ */

export type Target = { kind: 'node'; node: NodeAgentStateItem } | { kind: 'chain'; app: string; chain: string };

const active = (job: NodeUnlockProbe | null | undefined) => job?.state === 'queued' || job?.state === 'running';

function useUnlock(target: Target, editable: boolean) {
  const client = useQueryClient();
  const key =
    target.kind === 'chain'
      ? ['chain-unlock-probe', target.app, target.chain]
      : ['node-unlock-probe', target.node.node_id];
  const query = useQuery({
    queryKey: key,
    queryFn: () =>
      target.kind === 'chain'
        ? fetchChainUnlockProbe(target.app, target.chain)
        : fetchNodeUnlockProbe(target.node.node_id),
    refetchInterval: current => (active(current.state.data) ? 2_000 : 30_000),
  });
  const start = useMutation({
    mutationFn: () =>
      target.kind === 'chain'
        ? startChainUnlockProbe(target.app, target.chain)
        : startNodeUnlockProbe(target.node.node_id),
    onMutate: () => client.cancelQueries({ queryKey: key }),
    onSuccess: job => client.setQueryData(key, job),
  });
  const now = useNow();
  const job = query.data ?? null;
  const running = active(job);
  // 链路级失败（客户端没起来、检测期间线路变了）时九项都是「失败」，不是各服务的事实：
  // 与超时一样，改为显示上一次的结果并减淡。
  const current = job && job.results.length > 0 && !job.path_error ? job.results : null;
  const previous = !current && job?.previous_results?.length ? job.previous_results : null;
  const shown = current ?? previous;
  const shownAt = current ? job?.completed_at : previous ? job?.previous_completed_at : null;
  // 只给新鲜的结果标「与上次不同」：线路已变更时整块都是旧的，再逐项比较只是噪音。
  const before =
    current && job?.state !== 'obsolete' && job?.previous_results?.length
      ? new Map(job.previous_results.map(r => [r.service, r]))
      : null;
  const rows: Row[] = SERVICES.map(service => {
    const result = shown?.find(item => item.service === service) ?? null;
    return {
      service,
      name: UNLOCK_SERVICE_NAMES[service],
      category: CATEGORY[service],
      result,
      verdict: result ? verdictOf(result) : running ? WAITING : MISSING,
      region: result?.region ?? null,
      change: result && before ? changeOf(result, before.get(service)) : null,
      youtube: result?.youtube ?? null,
    };
  });
  const supported =
    target.kind === 'chain' || (target.node.agent_protocol_version ?? 0) >= UNLOCK_PROBE_PROTOCOL_VERSION;
  const inactive = target.kind === 'node' && target.node.lifecycle_phase !== 'active';
  const cooldown = job ? Math.max(0, Math.ceil(job.created_at + 60 - now / 1000)) : 0;
  const count = (tone: Tone) => (shown ? rows.filter(row => row.result && row.verdict.tone === tone).length : 0);
  return {
    target,
    editable,
    query,
    start,
    now,
    job,
    running,
    rows,
    hasResults: shown !== null,
    /** 显示的是上一次的结果（本次还在跑、超时或失效）：结果整体减淡。 */
    stale: (previous !== null || job?.state === 'obsolete') && shown !== null,
    shownAt: shownAt ?? null,
    supported,
    inactive,
    cooldown,
    available: count('ok'),
    partial: count('part'),
  };
}
type Unlock = ReturnType<typeof useUnlock>;

/* ══ 共用：状态行与检测按钮 ══ */

function RunButton({ u, compact = false }: { u: Unlock; compact?: boolean }) {
  if (!u.editable) return null;
  const disabled =
    !u.supported ||
    u.inactive ||
    u.running ||
    u.start.isPending ||
    u.query.isPending ||
    u.query.isError ||
    u.cooldown > 0;
  const label = u.start.isPending
    ? '发起中…'
    : u.cooldown > 0 && !u.running
      ? compact
        ? `${u.cooldown}${GAP}秒`
        : `${u.cooldown}${GAP}秒后可重测`
      : u.job
        ? '重新检测'
        : '开始检测';
  return (
    <button
      type="button"
      className={`btn ul-run${!u.job && !compact ? ' primary' : ''}`}
      disabled={disabled}
      title={u.cooldown > 0 && !u.running ? '两次检测至少间隔 60 秒' : undefined}
      onClick={() => u.start.mutate()}
    >
      <RefreshIcon />
      {label}
    </button>
  );
}

/** 一行说清：什么时候测的、下次什么时候、此刻在做什么。只写有的项，没有说明文字。 */
function Meta({ u, short = false }: { u: Unlock; short?: boolean }) {
  const { job, now } = u;
  const parts: ReactNode[] = [];
  const lead = (tone: 'warn' | 'err' | 'run' | 'plain', text: ReactNode) => (
    <span key="lead" className="ul-meta-lead" data-tone={tone}>
      {tone === 'run' && <Glyph tone="wait" />}
      {text}
    </span>
  );
  if (u.query.isPending) parts.push(lead('plain', '读取中…'));
  else if (!u.supported) parts.push(lead('warn', `需要升级 Agent（协议 v${UNLOCK_PROBE_PROTOCOL_VERSION}）`));
  else if (u.inactive) parts.push(lead('warn', '机器未在运行'));
  else if (!job) {
    /* 没有任务时由空状态说明，状态行不重复。 */
  } else if (job.state === 'queued') parts.push(lead('run', '等待机器响应'));
  else if (job.state === 'running') parts.push(lead('run', `检测中 ${clockOf(now / 1000 - job.created_at)}`));
  else if (job.state === 'expired') parts.push(lead('warn', '最近一次检测超时'));
  else if (job.path_error === 'client_unavailable') parts.push(lead('err', '入口无法启动检测客户端'));
  else if (job.path_error === 'configuration_changed') parts.push(lead('warn', '检测期间线路已变更'));
  else if (job.state === 'obsolete')
    parts.push(
      lead(
        'warn',
        job.configuration_revision != null ? `线路已变更 · 结果来自修订 ${job.configuration_revision}` : '线路已变更',
      ),
    );
  else if (job.completed_at) parts.push(<Ago key="at" at={job.completed_at} now={now} />);
  if (job && u.stale && u.shownAt && job.state !== 'obsolete')
    parts.push(
      <span key="shown">
        显示 <Ago at={u.shownAt} now={now} />
        的结果
      </span>,
    );
  if (job?.next_run_at && !u.running && !short)
    parts.push(
      <span key="next">
        下次 <Until at={job.next_run_at} now={now} />
      </span>,
    );
  if (job?.protocol && !short) parts.push(<span key="protocol">{job.protocol}</span>);
  return (
    <span className="ul-meta">
      {parts.map((part, index) => (
        <span key={index} className="ul-meta-part">
          {part}
        </span>
      ))}
    </span>
  );
}

function Problems({ u }: { u: Unlock }) {
  const error = u.start.error ?? u.query.error;
  if (!error) return null;
  return (
    <div className="ul-problem">
      <ErrorBox error={error} />
      {u.query.isError && (
        <button type="button" className="btn" onClick={() => void u.query.refetch()}>
          重试读取
        </button>
      )}
    </div>
  );
}

/** 没有可显示的结果：一处空状态说明原因，不画九行「—」。 */
function Nothing({ u, compact = false }: { u: Unlock; compact?: boolean }) {
  const [title, detail] = !u.supported
    ? ['需要升级 Agent', `Agent 协议 v${UNLOCK_PROBE_PROTOCOL_VERSION} 起支持解锁检测`]
    : u.inactive
      ? ['机器未在运行', '恢复运行后自动检测']
      : u.job?.state === 'expired'
        ? ['最近一次检测超时', '未取得结果']
        : u.job?.path_error
          ? ['未取得结果', '']
          : ['尚未检测', '每 24 小时自动检测一次'];
  if (compact) return <p className="ul-nothing-line">{detail ? `${title}，${detail}` : title}</p>;
  return (
    <div className="empty-state ul-nothing">
      <span className="empty-state-mark">
        <UnlockIcon size={24} />
      </span>
      <b>{title}</b>
      {detail && <p>{detail}</p>}
      {u.editable && u.supported && !u.inactive && <RunButton u={u} />}
    </div>
  );
}

/* ══ A 清单 ══ */

function ListRow({ row, dense }: { row: Row; dense: boolean }) {
  const { verdict } = row;
  const sub = verdict.tone === 'unknown' && row.result ? verdict.reason : null;
  // 内嵌时一行放不下第二行：右侧只留符号，非「可用」的结论、限定词与上次读数跟在名称后面。
  const trailing = dense
    ? [
        verdict.tone === 'unknown' ? sub : verdict.tone === 'ok' || verdict.tone === 'wait' ? null : verdict.label,
        verdict.note,
      ]
    : [];
  const youtube = dense ? null : youtubeLine(row.youtube);
  return (
    <li className="ul-a-row" data-tone={verdict.tone} title={row.result ? verdict.reason : undefined}>
      <ServiceMark service={row.service} />
      <span className="ul-a-name">
        <span className="ul-a-title">
          <span className="ul-a-text">{row.name}</span>
          {trailing.map(text => text && <em key={text}>{text}</em>)}
          {dense && row.change && <em className="ul-change">{row.change}</em>}
          <YoutubeMore youtube={row.youtube} />
        </span>
        {!dense && (sub || verdict.note || youtube || row.change) && (
          <span className="ul-a-sub">
            {sub && <span>{sub}</span>}
            {verdict.note && <span>{verdict.note}</span>}
            {youtube && <span>{youtube}</span>}
            {row.change && <span className="ul-change">{row.change}</span>}
          </span>
        )}
      </span>
      <Region code={row.region} />
      <span className="ul-a-verdict">
        <Glyph tone={verdict.tone} />
        <span>{verdict.label}</span>
      </span>
    </li>
  );
}

function OptionA({ u, variant }: { u: Unlock; variant: Variant }) {
  if (variant === 'inline')
    return (
      <ul className="ul-a-list ul-a-flat">
        {u.rows.map(row => (
          <ListRow key={row.service} row={row} dense />
        ))}
      </ul>
    );
  return (
    <div className="ul-a" data-variant={variant}>
      {CATEGORIES.map(category => (
        <section key={category.key} className="ul-a-group">
          <h5>{category.name}</h5>
          <ul className="ul-a-list">
            {u.rows
              .filter(row => row.category === category.key)
              .map(row => (
                <ListRow key={row.service} row={row} dense={false} />
              ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

/* ══ B 磁贴 ══ */

function Tile({ row, dense }: { row: Row; dense: boolean }) {
  const { verdict } = row;
  const detail =
    verdict.tone === 'ok'
      ? verdict.note
      : verdict.tone === 'unknown' && row.result
        ? `${verdict.label} · ${verdict.reason}`
        : verdict.label;
  const youtube = dense ? null : youtubeLine(row.youtube);
  return (
    <div className="ul-b-tile" data-tone={verdict.tone} title={row.result ? verdict.reason : undefined}>
      <div className="ul-b-top">
        <ServiceMark service={row.service} />
        <span className="ul-b-name">{row.name}</span>
        <YoutubeMore youtube={row.youtube} />
        <Glyph tone={verdict.tone} />
      </div>
      <div className="ul-b-region">
        {row.region ? (
          <>
            <RegionFlag code={row.region} />
            <b>{row.region}</b>
            {!dense && <span className="ul-b-region-name">{regionName(row.region)}</span>}
          </>
        ) : (
          <b className="none">—</b>
        )}
      </div>
      {(detail || youtube || row.change) && (
        <div className="ul-b-foot">
          {detail && <span className="ul-b-label">{detail}</span>}
          {youtube && <span className="ul-b-label">{youtube}</span>}
          {row.change && <span className="ul-change">{row.change}</span>}
        </div>
      )}
    </div>
  );
}

function Strip({ rows }: { rows: Row[] }) {
  return (
    <span className="ul-b-strip" aria-hidden="true">
      {rows.map(row => (
        <i key={row.service} data-tone={row.verdict.tone} title={`${row.name} · ${row.verdict.label}`} />
      ))}
    </span>
  );
}

function OptionB({ u, variant }: { u: Unlock; variant: Variant }) {
  if (variant === 'inline')
    return (
      <div className="ul-b-tiles ul-b-flat">
        {u.rows.map(row => (
          <Tile key={row.service} row={row} dense />
        ))}
      </div>
    );
  return (
    <div className="ul-b" data-variant={variant}>
      {CATEGORIES.map(category => (
        <section key={category.key} className="ul-b-row">
          <h5>{category.name}</h5>
          <div className="ul-b-tiles">
            {u.rows
              .filter(row => row.category === category.key)
              .map(row => (
                <Tile key={row.service} row={row} dense={false} />
              ))}
          </div>
        </section>
      ))}
    </div>
  );
}

/* ══ C 结论 ══ */

const OUTCOMES: { key: string; label: string; tone: Tone; match: (row: Row) => boolean }[] = [
  { key: 'ok', label: '可用', tone: 'ok', match: row => row.result?.status === 'available' },
  { key: 'part', label: '部分可用', tone: 'part', match: row => row.result?.status === 'partial' },
  { key: 'region', label: '地区不支持', tone: 'no', match: row => row.result?.status === 'unsupported_region' },
  { key: 'denied', label: '访问受限', tone: 'no', match: row => row.result?.status === 'restricted' },
  { key: 'failed', label: '检测失败', tone: 'unknown', match: row => row.result?.status === 'failed' },
  { key: 'wait', label: '等待结果', tone: 'wait', match: row => !row.result },
];

function Chip({ row, outcome }: { row: Row; outcome: string }) {
  const { verdict } = row;
  const extra = outcome === 'part' ? verdict.label : outcome === 'failed' ? verdict.reason : verdict.note;
  return (
    <span
      className="ul-c-chip"
      title={row.result ? verdict.reason : undefined}
      data-changed={row.change ? '' : undefined}
    >
      <ServiceMark service={row.service} />
      <span className="ul-c-name">{row.name}</span>
      {extra && <span className="ul-c-extra">{extra}</span>}
      {row.region && <Region code={row.region} />}
      {row.change && <span className="ul-change">{row.change}</span>}
      <YoutubeMore youtube={row.youtube} />
    </span>
  );
}

function OptionC({ u, variant }: { u: Unlock; variant: Variant }) {
  return (
    <div className="ul-c" data-variant={variant}>
      {OUTCOMES.map(outcome => {
        const rows = u.rows.filter(outcome.match);
        if (rows.length === 0) return null;
        return (
          <div key={outcome.key} className="ul-c-line" data-tone={outcome.tone}>
            <span className="ul-c-head">
              <Glyph tone={outcome.tone} />
              <span>{outcome.label}</span>
              <em>{rows.length}</em>
            </span>
            <span className="ul-c-chips">
              {rows.map(row => (
                <Chip key={row.service} row={row} outcome={outcome.key} />
              ))}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/* ══ D 地区 ══ */

const TONE_ORDER: Tone[] = ['ok', 'part', 'no', 'unknown', 'wait'];

function regionsOf(rows: Row[]) {
  const groups = new Map<string, Row[]>();
  for (const row of rows) {
    const key = row.region ?? '';
    groups.set(key, [...(groups.get(key) ?? []), row]);
  }
  // 服务多的地区在前；未知地区（没有回报地区码的失败与拦截）排最后。
  return [...groups.entries()]
    .map(([code, list]) => ({
      code: code || null,
      rows: [...list].sort((a, b) => TONE_ORDER.indexOf(a.verdict.tone) - TONE_ORDER.indexOf(b.verdict.tone)),
    }))
    .sort((a, b) => (a.code === null ? 1 : b.code === null ? -1 : b.rows.length - a.rows.length));
}

function RegionCard({ code, rows, dense }: { code: string | null; rows: Row[]; dense: boolean }) {
  const ok = rows.filter(row => row.verdict.tone === 'ok').length;
  const waiting = rows.every(row => row.verdict.tone === 'wait');
  return (
    <section className="ul-d-card" data-unknown={code === null ? '' : undefined}>
      <header>
        {code ? (
          <>
            <RegionFlag code={code} />
            <b>{regionName(code)}</b>
            <code>{code}</code>
          </>
        ) : (
          <>
            <Icon of="region" size={14} className="ul-d-globe" />
            <b>{waiting ? '等待结果' : '地区未知'}</b>
          </>
        )}
        {code && !dense && (
          <span className="ul-d-count">
            {ok} / {rows.length} 可用
          </span>
        )}
      </header>
      <ul>
        {rows.map(row => {
          const label =
            row.verdict.tone === 'wait'
              ? null
              : row.verdict.tone === 'ok'
                ? row.verdict.note
                : row.verdict.tone === 'unknown'
                  ? row.verdict.reason
                  : row.verdict.label;
          const youtube = dense ? null : youtubeLine(row.youtube);
          return (
            <li key={row.service} data-tone={row.verdict.tone} title={row.result ? row.verdict.reason : undefined}>
              <Glyph tone={row.verdict.tone} />
              <ServiceMark service={row.service} />
              <span className="ul-d-name">
                <span className="ul-d-text">{row.name}</span>
                <YoutubeMore youtube={row.youtube} />
              </span>
              {(label || row.change) && (
                <span className="ul-d-end">
                  {label && <span className="ul-d-label">{label}</span>}
                  {row.change && <span className="ul-change">{row.change}</span>}
                </span>
              )}
              {youtube && <span className="ul-d-yt">{youtube}</span>}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function OptionD({ u, variant }: { u: Unlock; variant: Variant }) {
  const groups = regionsOf(u.rows);
  return (
    <div className="ul-d" data-variant={variant} data-count={groups.length}>
      {groups.map(group => (
        <RegionCard key={group.code ?? '?'} code={group.code} rows={group.rows} dense={variant === 'inline'} />
      ))}
    </div>
  );
}

/* ══ 组装 ══ */

const BODIES: Record<Option, (props: { u: Unlock; variant: Variant }) => ReactNode> = {
  a: OptionA,
  b: OptionB,
  c: OptionC,
  d: OptionD,
};

function Body({ u, option, variant }: { u: Unlock; option: Option; variant: Variant }) {
  if (u.query.isPending || u.query.isError) return null;
  if (!u.hasResults && !u.running) return <Nothing u={u} compact={variant === 'inline'} />;
  const Layout = BODIES[option];
  return (
    <div className="ul-body" data-stale={u.stale ? '' : undefined} aria-busy={u.running}>
      <Layout u={u} variant={variant} />
    </div>
  );
}

/** 状态行前的结论：A「可用 4 / 9」，B 大号读数和九格条，C 不写（每行自带数量），D「2 个地区」。 */
function Lead({ u, option }: { u: Unlock; option: Option }) {
  if (!u.hasResults) return null;
  const total = u.rows.length;
  const stale = u.stale ? '' : undefined;
  if (option === 'b')
    return (
      <span className="ul-lead ul-b-hero" data-stale={stale}>
        <b>{u.available}</b>
        <span>/ {total} 可用</span>
        <Strip rows={u.rows} />
      </span>
    );
  if (option === 'a')
    return (
      <span className="ul-lead" data-stale={stale}>
        可用 <b>{u.available}</b> / {total}
        {u.partial > 0 && <span className="ul-lead-part"> · 部分 {u.partial}</span>}
      </span>
    );
  if (option === 'd') {
    const regions = new Set(u.rows.map(row => row.region).filter(Boolean)).size;
    return (
      <span className="ul-lead" data-stale={stale}>
        {regions} 个地区 · 可用 <b>{u.available}</b> / {total}
      </span>
    );
  }
  return null;
}

/** 机器详情「解锁检测」页签：没有卡片标题、副标题和底注，页签本身就是标题。 */
export function MachineUnlock({
  option,
  node,
  editable,
}: {
  option: Option;
  node: NodeAgentStateItem;
  editable: boolean;
}) {
  const u = useUnlock({ kind: 'node', node }, editable);
  return (
    <div className={`ul-page ul-opt-${option}`}>
      {u.job && (
        <div className="ul-bar">
          <Lead u={u} option={option} />
          <Meta u={u} />
          <RunButton u={u} />
        </div>
      )}
      <Problems u={u} />
      <Body u={u} option={option} variant="tab" />
    </div>
  );
}

/** 线路详情 · 内嵌：连通性横幅下面的一块，与它同一结构（.blk 标题条 + 内容）。 */
export function ChainUnlockInline({
  option,
  app,
  chain,
  editable,
}: {
  option: Option;
  app: string;
  chain: string;
  editable: boolean;
}) {
  const u = useUnlock({ kind: 'chain', app, chain }, editable);
  const summary = u.hasResults ? `${u.available} / ${u.rows.length} 可用` : null;
  return (
    <div className={`blk ul-blk ul-opt-${option}`}>
      <div className="blk-hd">
        <span>解锁{summary ? ` · ${summary}` : ''}</span>
        <span className="sp" />
        <Meta u={u} short />
        <RunButton u={u} compact />
      </div>
      <div className="blk-bd">
        <Problems u={u} />
        <Body u={u} option={option} variant="inline" />
      </div>
    </div>
  );
}

/** 线路详情 · 弹窗：页头按钮带上可用数，点开是同一块内容。 */
export function ChainUnlockDialog({
  option,
  app,
  chain,
  name,
  editable,
}: {
  option: Option;
  app: string;
  chain: string;
  name: string;
  editable: boolean;
}) {
  const [open, setOpen] = useState(false);
  const u = useUnlock({ kind: 'chain', app, chain }, editable);
  return (
    <>
      <button type="button" className="btn ul-head-btn" onClick={() => setOpen(true)}>
        <UnlockIcon />
        解锁检测
        {u.hasResults && (
          <span className="ul-head-count" data-tone={u.available === u.rows.length ? 'ok' : undefined}>
            {u.available}/{u.rows.length}
          </span>
        )}
        {u.running && <Glyph tone="wait" />}
      </button>
      {open && (
        <DialogLayer label={`${name} · 解锁检测`} onClose={() => setOpen(false)}>
          <section className={`dialog-surface sub-dialog ul-dialog ul-opt-${option}`}>
            <header className="sub-dialog-head">
              <span className="ul-dialog-title">
                <b>{name}</b>
                <small>解锁检测</small>
              </span>
              <DialogClose className="sub-dialog-close" title="关闭" aria-label="关闭解锁检测">
                <Icon of="close" size={16} />
              </DialogClose>
            </header>
            <div className="ul-dialog-body">
              {u.job && (
                <div className="ul-bar">
                  <Lead u={u} option={option} />
                  <Meta u={u} />
                  <RunButton u={u} />
                </div>
              )}
              <Problems u={u} />
              <Body u={u} option={option} variant="dialog" />
            </div>
          </section>
        </DialogLayer>
      )}
    </>
  );
}
