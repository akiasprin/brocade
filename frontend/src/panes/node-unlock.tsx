import { useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { UnlockDnsDetails } from './unlock-dns';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchChainUnlockProbe,
  fetchChainUnlockCapabilities,
  fetchNodeUnlockCapabilities,
  fetchNodeUnlockProbe,
  startChainUnlockProbe,
  startNodeUnlockProbe,
  UNLOCK_DUAL_STACK_PROTOCOL_VERSION,
  UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION,
  type UnlockProbeSelection,
  type NodeAgentStateItem,
  type NodeUnlockProbe,
} from '../api';
import { ErrorBox, SegmentedControl } from '../ui/bits';
import { useNow } from '../ui/clock';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';
import { unlockFlagRegion } from '../ui/unlock-region';
import { SiteIcon } from '../ui/site-icon';
import { Glyph, type Tone } from './unlock-status';
import { CATEGORIES, unlockReport, type DisplayService, type Row } from './unlock-report';
export { UNLOCK_SERVICE_NAMES } from './unlock-report';
import { YoutubeMore, youtubeLine } from './youtube-unlock';
import './node-unlock.css';

/** Monochrome marks already shipped in `ui/site-icon.tsx`; Gemini and Prime Video have none there. */
const BRAND: Partial<Record<DisplayService, string>> = {
  chatgpt: 'openai',
  claude: 'anthropic',
  netflix: 'netflix',
  disney_plus: 'disney',
  youtube: 'youtube',
  youtube_premium: 'youtube',
  tiktok: 'tiktok',
  reddit: 'reddit',
  spotify: 'spotify',
  hbo_max: 'hbomax',
  dazn: 'dazn',
};
const LETTER_MARKS: Partial<Record<DisplayService, string>> = {
  bbc_iplayer: 'BBC',
  crunchyroll: 'C',
  paramount_plus: 'P+',
};

export function ServiceMark({ service }: { service: DisplayService }) {
  const brand = BRAND[service];
  return (
    <span className="unlock-mark">
      {brand ? (
        <SiteIcon siteId={brand} />
      ) : LETTER_MARKS[service] ? (
        <span className="site-brand-icon" aria-hidden="true">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" focusable="false">
            <text x="12" y="16" fontSize="10" fontWeight="700" textAnchor="middle">
              {LETTER_MARKS[service]}
            </text>
          </svg>
        </span>
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

const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const regionName = (code: string) => {
  try {
    return REGION_NAMES.of(code) ?? code;
  } catch {
    return code;
  }
};

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
  return (
    <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
      {now / 1000 - at < 45 ? '刚刚' : `${span(now / 1000 - at)}前`}
    </time>
  );
}
function Until({ at, now }: { at: number; now: number }) {
  const left = at - now / 1000;
  return (
    <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
      {left <= 60 ? '即将' : `${span(left)}后`}
    </time>
  );
}
const clockOf = (seconds: number) => {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};

/* ══ 数据：与生产面板同一查询键 ══ */

export type Target = { kind: 'node'; node: NodeAgentStateItem } | { kind: 'chain'; app: string; chain: string };

const active = (job: NodeUnlockProbe | null | undefined) => job?.state === 'queued' || job?.state === 'running';

function useUnlock(target: Target, editable: boolean) {
  const [selectedFamily, setFamily] = useState<UnlockProbeSelection>('v4');
  const capabilities = useQuery({
    queryKey:
      target.kind === 'chain'
        ? ['chain-unlock-capabilities', target.app, target.chain]
        : ['node-unlock-capabilities', target.node.node_id],
    queryFn: () =>
      target.kind === 'chain'
        ? fetchChainUnlockCapabilities(target.app, target.chain)
        : fetchNodeUnlockCapabilities(target.node.node_id),
    refetchInterval: 30_000,
  });
  const automatic = target.kind === 'chain' && capabilities.data?.family_mode === 'automatic';
  const family: UnlockProbeSelection = automatic ? 'auto' : selectedFamily === 'auto' ? 'v4' : selectedFamily;
  const nodeFamily = family === 'auto' ? undefined : family;
  const client = useQueryClient();
  const key =
    target.kind === 'chain'
      ? ['chain-unlock-probe', target.app, target.chain, family]
      : ['node-unlock-probe', target.node.node_id, family];
  const query = useQuery({
    queryKey: key,
    queryFn: () =>
      target.kind === 'chain'
        ? fetchChainUnlockProbe(target.app, target.chain, family)
        : fetchNodeUnlockProbe(target.node.node_id, nodeFamily),
    refetchInterval: current =>
      active(current.state.data) || current.state.data?.state === 'obsolete' ? 2_000 : 30_000,
  });
  const familyKey = (selected: UnlockProbeSelection) =>
    target.kind === 'chain'
      ? ['chain-unlock-probe', target.app, target.chain, selected]
      : ['node-unlock-probe', target.node.node_id, selected];
  const start = useMutation({
    mutationFn: (selected: UnlockProbeSelection) =>
      target.kind === 'chain'
        ? startChainUnlockProbe(target.app, target.chain, selected)
        : startNodeUnlockProbe(target.node.node_id, selected === 'auto' ? undefined : selected),
    onMutate: selected => client.cancelQueries({ queryKey: familyKey(selected) }),
    onSuccess: (job, selected) => {
      client.setQueryData(familyKey(selected), job);
      if (target.kind === 'chain') void client.invalidateQueries({ queryKey: ['chain-unlock-probes'] });
    },
  });
  const starting = start.isPending && start.variables === family;
  const now = useNow();
  const legacy = !automatic && query.data != null && query.data.family == null;
  const job = automatic
    ? query.data?.automatic
      ? query.data
      : null
    : query.data?.family === family
      ? query.data
      : null;
  const running = active(job);
  const { rows, hasResults, shownAt, stale } = unlockReport(job);
  const requiredProtocol = automatic ? UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION : UNLOCK_DUAL_STACK_PROTOCOL_VERSION;
  const supported = automatic
    ? capabilities.data?.automatic_ready === true
    : target.kind === 'chain' || (target.node.agent_protocol_version ?? 0) >= UNLOCK_DUAL_STACK_PROTOCOL_VERSION;
  const inactive = target.kind === 'node' && target.node.lifecycle_phase !== 'active';
  const cooldown = job ? Math.max(0, Math.ceil(job.created_at + 60 - now / 1000)) : 0;
  const count = (tone: Tone) =>
    hasResults ? rows.filter(row => row.route === 'normal' && row.result && row.verdict.tone === tone).length : 0;
  return {
    target,
    family,
    automatic,
    detectionMode: capabilities.data?.detection_mode ?? 'scheduled',
    intervalHours: capabilities.data?.interval_hours ?? 24,
    nextFullRun: capabilities.data?.next_run_at,
    requiredProtocol,
    legacy,
    setFamily,
    editable,
    query,
    start,
    starting,
    now,
    job,
    running,
    rows,
    hasResults,
    /** 线路变更只高亮底部提示；重测和检测失败时仍减淡正在引用的历史结果。 */
    stale: stale || (query.isError && hasResults && job?.state !== 'obsolete'),
    shownAt: shownAt ?? null,
    supported,
    inactive,
    cooldown,
    available: count('ok'),
    total: rows.filter(row => row.route === 'normal').length,
    partial: count('part'),
  };
}
type Unlock = ReturnType<typeof useUnlock>;

/* ══ 共用：状态行与检测按钮 ══ */

function RunButton({ u, compact = false }: { u: Unlock; compact?: boolean }) {
  if (!u.editable) return null;
  const disabled =
    !u.supported ||
    u.legacy ||
    u.inactive ||
    u.running ||
    u.starting ||
    u.query.isPending ||
    u.query.isError ||
    u.cooldown > 0;
  const label = u.starting
    ? '发起中…'
    : u.cooldown > 0 && !u.running
      ? compact
        ? `${u.cooldown}${GAP}秒`
        : `${u.cooldown}${GAP}秒后可重测`
      : u.running
        ? '检测中…'
        : u.job
          ? '重新检测'
          : '开始检测';
  return (
    <button
      type="button"
      className={`btn unlock-run${!u.job && !compact ? ' primary' : ''}`}
      disabled={disabled}
      title={u.cooldown > 0 && !u.running ? '两次检测至少间隔 60 秒' : undefined}
      onClick={() => u.start.mutate(u.family)}
    >
      <Icon of="refresh" size={13} />
      {label}
    </button>
  );
}

/** 一行说清：什么时候测的、下次什么时候、此刻在做什么。只写有的项，没有说明文字。 */
function Meta({ u, short = false }: { u: Unlock; short?: boolean }) {
  const { job, now } = u;
  const parts: ReactNode[] = [];
  const lead = (tone: 'warn' | 'err' | 'run' | 'plain', text: ReactNode) => (
    <span key="lead" className="unlock-meta-lead" data-tone={tone}>
      {tone === 'run' && <Glyph tone="wait" />}
      {text}
    </span>
  );
  if (u.query.isPending) parts.push(lead('plain', '读取中…'));
  else if (!u.supported) parts.push(lead('warn', `需要升级 Agent（协议 v${u.requiredProtocol}）`));
  else if (u.inactive) parts.push(lead('warn', '机器未在运行'));
  else if (!job) {
    /* 没有任务时由空状态说明，状态行不重复。 */
  } else if (job.state === 'queued') parts.push(lead('run', '等待机器响应'));
  else if (job.state === 'running') parts.push(lead('run', `检测中 ${clockOf(now / 1000 - job.created_at)}`));
  else if (job.state === 'expired') parts.push(lead('warn', '最近一次检测超时'));
  else if (job.path_error === 'client_unavailable') parts.push(lead('err', '入口无法启动检测客户端'));
  else if (job.path_error === 'family_unavailable') parts.push(lead('warn', '检测未就绪，稍后重试'));
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
  const next = job?.retry_at ?? (u.detectionMode === 'scheduled' ? (u.nextFullRun ?? job?.next_run_at) : null);
  if (next && !u.running && !short)
    parts.push(
      <span key="next">
        {job?.retry_at ? '重试' : '下次全量'} <Until at={next} now={now} />
      </span>,
    );
  if (job?.protocol && !short) parts.push(<span key="protocol">{job.protocol}</span>);
  return (
    <span className="unlock-meta">
      {parts.map((part, index) => (
        <span key={index} className="unlock-meta-part">
          {part}
        </span>
      ))}
    </span>
  );
}

function Problems({ u }: { u: Unlock }) {
  const error = (u.start.variables === u.family ? u.start.error : null) ?? u.query.error;
  if (!error) return null;
  return (
    <div className="unlock-problem">
      <ErrorBox error={error} />
      {u.query.isError && (
        <button type="button" className="btn" onClick={() => void u.query.refetch()}>
          重试读取
        </button>
      )}
    </div>
  );
}

/** 没有可显示的结果：一处空状态说明原因，不画多行「—」。 */
function Nothing({
  u,
  compact = false,
  showRunButton = true,
}: {
  u: Unlock;
  compact?: boolean;
  showRunButton?: boolean;
}) {
  const [title, detail] = !u.supported
    ? [
        '需要升级 Agent',
        u.automatic
          ? `Agent 协议 v${u.requiredProtocol} 起支持原线路自动检测`
          : `Agent 协议 v${u.requiredProtocol} 起支持 IPv4 / IPv6 分开检测`,
      ]
    : u.legacy
      ? ['旧结果未区分地址族', '请升级 Console 和 Agent 后重新检测，不将旧结果归入 IPv4 或 IPv6']
      : u.inactive
        ? ['机器未在运行', '恢复运行后自动检测']
        : u.job?.state === 'expired'
          ? ['最近一次检测超时', '未取得结果']
          : u.job?.path_error === 'family_unavailable'
            ? ['检测未就绪', '正在等待强制地址族的检测配置就绪，稍后自动重试']
            : u.job?.path_error
              ? ['未取得结果', '']
              : [
                  '尚未检测',
                  (u.detectionMode === 'manual'
                    ? '手动模式，可由管理员发起检测'
                    : `每 ${u.intervalHours} 小时全量检测一次`) + (u.editable ? '' : '，也可联系管理员发起检测'),
                ];
  if (compact) return <p className="unlock-nothing-line">{detail ? `${title}，${detail}` : title}</p>;
  return (
    <div className="empty-state unlock-nothing">
      <span className="empty-state-mark">
        <Icon of="unlock" size={24} />
      </span>
      <b>{title}</b>
      {detail && <p>{detail}</p>}
      {showRunButton && u.editable && u.supported && !u.inactive && !u.job && <RunButton u={u} />}
    </div>
  );
}

function Tile({ row }: { row: Row }) {
  const { verdict } = row;
  const detail =
    row.service === 'youtube' && verdict.tone === 'ok'
      ? null
      : verdict.tone === 'ok'
        ? (verdict.note ?? verdict.label)
        : verdict.tone === 'unknown' && row.result
          ? `${verdict.label} · ${verdict.reason}`
          : verdict.label;
  const youtube = youtubeLine(row.youtube, row.source ?? undefined);
  return (
    <article
      className="unlock-b-tile"
      aria-label={row.name}
      data-service={row.service}
      data-tone={verdict.tone}
      data-route={row.route === 'normal' ? undefined : row.route}
      title={row.result ? verdict.reason : undefined}
    >
      <div className="unlock-b-top">
        <ServiceMark service={row.service} />
        <span className="unlock-b-name">{row.name}</span>
        <UnlockDnsDetails row={row} />
        {(row.service === 'youtube' || (row.service === 'youtube_premium' && !row.result?.youtube)) && (
          <YoutubeMore result={row.result ?? undefined} />
        )}
        <Glyph tone={verdict.tone} />
      </div>
      <div className="unlock-b-region">
        {row.region ? (
          <>
            <RegionFlag code={unlockFlagRegion(row.service, row.region)} />
            <b>{row.region}</b>
            {row.region.length === 2 && <span className="unlock-b-region-name">{regionName(row.region)}</span>}
          </>
        ) : (
          <b className="none">—</b>
        )}
      </div>
      {(detail || youtube || row.change) && (
        <div className="unlock-b-foot">
          {detail && <span className="unlock-b-label">{detail}</span>}
          {youtube && <span className="unlock-b-label unlock-b-summary">{youtube}</span>}
          {row.change && <span className="unlock-change">{row.change}</span>}
        </div>
      )}
    </article>
  );
}

function Strip({ rows }: { rows: Row[] }) {
  return (
    <span className="unlock-b-strip" aria-hidden="true">
      {rows.map(row => (
        <i
          key={row.service}
          data-tone={row.verdict.tone}
          data-route={row.route === 'normal' ? undefined : row.route}
          title={`${row.name} · ${row.route === 'fallback' ? '回退，不计入 · ' : row.route === 'excluded' ? '已跳过，不计入 · ' : ''}${row.verdict.label}`}
        />
      ))}
    </span>
  );
}

function Tiles({ u, variant }: { u: Unlock; variant: 'tab' | 'dialog' }) {
  return (
    <div className="unlock-b" data-variant={variant}>
      {CATEGORIES.map(category => (
        <section key={category.key} className="unlock-b-row" aria-label={category.name}>
          <h3>{category.name}</h3>
          <div className="unlock-b-tiles">
            {u.rows
              .filter(row => row.category === category.key)
              .map(row => (
                <Tile key={row.service} row={row} />
              ))}
          </div>
        </section>
      ))}
    </div>
  );
}

function UnlockContents({
  u,
  variant,
  showRunButton = true,
}: {
  u: Unlock;
  variant: 'tab' | 'dialog';
  showRunButton?: boolean;
}) {
  return (
    <>
      {(u.job || u.query.isPending) && (
        <div className="unlock-bar">
          {u.hasResults && (
            <span
              className="unlock-lead unlock-b-hero"
              data-stale={u.stale ? '' : undefined}
              aria-label={`可用 ${u.available} / ${u.total}`}
            >
              <b>{u.available}</b>
              <span>/ {u.total} 可用</span>
              <Strip rows={u.rows} />
            </span>
          )}
          {showRunButton && u.job && <RunButton u={u} />}
        </div>
      )}
      <Problems u={u} />
      {!u.query.isPending &&
        (u.hasResults || u.running ? (
          <div className="unlock-body" data-stale={u.stale ? '' : undefined} aria-busy={u.running}>
            <Tiles u={u} variant={variant} />
          </div>
        ) : (
          !u.query.isError && <Nothing u={u} showRunButton={showRunButton} />
        ))}
      {variant === 'tab' && <UnlockFooter u={u} variant={variant} />}
    </>
  );
}

function UnlockFooter({ u, variant }: { u: Unlock; variant: 'tab' | 'dialog' }) {
  return (
    <div className="unlock-footer" data-variant={variant}>
      <SegmentedControl
        value={u.family}
        options={
          u.automatic
            ? [{ value: 'auto', label: <span title="不限定地址族，本次连接可能使用 IPv4 或 IPv6">不分族</span> }]
            : [
                { value: 'v4', label: 'IPv4' },
                { value: 'v6', label: 'IPv6' },
              ]
        }
        ariaLabel="解锁地址族"
        onChange={u.setFamily}
      />
      <span role="status" aria-live="polite">
        <Meta u={u} />
      </span>
    </div>
  );
}

export function NodeUnlockPanel({
  node,
  editable,
  actionTarget,
}: {
  node: NodeAgentStateItem;
  editable: boolean;
  actionTarget?: HTMLElement | null;
}) {
  const u = useUnlock({ kind: 'node', node }, editable);
  return (
    <>
      {actionTarget && createPortal(<RunButton u={u} />, actionTarget)}
      <div className="node-unlock unlock-page">
        <UnlockContents u={u} variant="tab" showRunButton={actionTarget === undefined} />
      </div>
    </>
  );
}

const CHAIN_UNLOCK_TITLE = '区域性服务解锁状态';

export function ChainUnlockControl({
  app,
  chain,
  name,
  editable,
}: {
  app: string;
  chain: string;
  name: string;
  editable: boolean;
}) {
  const [open, setOpen] = useState(false);
  const u = useUnlock({ kind: 'chain', app, chain }, editable);
  return (
    <>
      <button
        type="button"
        className="btn unlock-head-btn"
        onClick={() => setOpen(true)}
        aria-label="解锁"
        title={u.stale ? '显示上次检测结果' : undefined}
      >
        <Icon of="unlock" size={14} />
        解锁
        {u.hasResults && (
          <span className="unlock-head-count" data-tone={u.available === u.total ? 'ok' : undefined}>
            {u.available}/{u.total}
          </span>
        )}
        {u.running && <Glyph tone="wait" />}
      </button>
      {open && (
        <DialogLayer label={`${CHAIN_UNLOCK_TITLE} · ${name}`} onClose={() => setOpen(false)}>
          <section className="dialog-surface sub-dialog chain-unlock-dialog unlock-dialog">
            <header className="sub-dialog-head">
              <span className="unlock-dialog-title">
                <Icon of="unlock" size={16} className="unlock-dialog-title-icon" />
                <b>{CHAIN_UNLOCK_TITLE}</b>
                <small title={name}>{name}</small>
              </span>
              <DialogClose className="sub-dialog-close" title="关闭" aria-label="关闭解锁">
                <Icon of="close" size={16} />
              </DialogClose>
            </header>
            <div className="unlock-dialog-body">
              <UnlockContents u={u} variant="dialog" />
            </div>
            <UnlockFooter u={u} variant="dialog" />
          </section>
        </DialogLayer>
      )}
    </>
  );
}
