// Option parts for the 解锁面板整体设计 mockup (round 2). Synthetic data only; never imported by the app.
// Every option keeps the per-family report, the tiles' fixed positions and the fallback reading on the card. What
// changes is the visual system: one status line per service carries the verdict, and a fallback is one of its states
// with its own neutral glyph instead of a faded card. Options differ in the header and in how services are laid out.
// The panel reads the production query keys and `unlockReport` rows.
import { useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchChainUnlockProbe,
  fetchNodeUnlockProbe,
  startChainUnlockProbe,
  startNodeUnlockProbe,
  type DestMatch,
  type NodeUnlockProbe,
  type UnlockDnsPath,
  type UnlockIpFamily,
  egressDnsEndpointText,
} from '../../src/api';
import { SegmentedControl } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { Icon } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { unlockFlagRegion } from '../../src/ui/unlock-region';
import { ServiceMark } from '../../src/panes/node-unlock';
import { Glyph } from '../../src/panes/unlock-status';
import { connectionFailureReason } from '../../src/panes/unlock-dial';
import { CATEGORIES, unlockReport, type Row } from '../../src/panes/unlock-report';
import { YoutubeMore, youtubeLine } from '../../src/panes/youtube-unlock';
import { MACHINES } from './list-head-mockup-data';

export type Option = 'a' | 'b' | 'c' | 'd';
export type Surface =
  { kind: 'chain'; app: string; chain: string; name: string } | { kind: 'node'; node: string; name: string };

const FAMILY = { v4: 'IPv4', v6: 'IPv6' } as const;
const otherFamily = (family: UnlockIpFamily): UnlockIpFamily => (family === 'v4' ? 'v6' : 'v4');
const machineName = (id: string) => MACHINES.find(machine => machine.id === id)?.name ?? id;
const STRATEGY = {
  use_ip: 'UseIP',
  use_ipv4: 'UseIPv4',
  use_ipv6: 'UseIPv6',
  use_ipv4v6: 'UseIPv4v6',
  use_ipv6v4: 'UseIPv6v4',
} as const;
const SELECTOR_PREFIX: Partial<Record<DestMatch['t'], string>> = {
  geosite: 'geosite',
  domain_suffix: 'domain',
  domain_keyword: 'keyword',
  domain_regex: 'regexp',
};
const selectorText = (match: DestMatch) =>
  'v' in match
    ? `${SELECTOR_PREFIX[match.t] ?? match.t}:${Array.isArray(match.v) ? match.v.join(', ') : String(match.v)}`
    : match.t;
const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const regionName = (code: string) => {
  try {
    return REGION_NAMES.of(code) ?? code;
  } catch {
    return code;
  }
};

/* ══ 数据 ══ */

const fellBack = (row: Row) => row.route !== 'normal';
/** Measured DNS facts that leave the result counted: shown after the verdict, not as a separate tag. */
const pathNote = (row: Row) =>
  row.route === 'normal' && row.dnsPart && row.dnsPart.paths.length > 0
    ? row.dnsPart.kind === 'partial'
      ? '部分回退'
      : row.dnsPart.kind === 'unknown'
        ? '回退待确认'
        : null
    : null;

const active = (job: NodeUnlockProbe | null | undefined) => job?.state === 'queued' || job?.state === 'running';

function usePanel(surface: Surface, initial: UnlockIpFamily, editable: boolean) {
  const [family, setFamily] = useState<UnlockIpFamily>(initial);
  const client = useQueryClient();
  const now = useNow();
  const keyOf = (selected: UnlockIpFamily) =>
    surface.kind === 'chain'
      ? ['chain-unlock-probe', surface.app, surface.chain, selected]
      : ['node-unlock-probe', surface.node, selected];
  const probe = (selected: UnlockIpFamily) => ({
    queryKey: keyOf(selected),
    queryFn: () =>
      surface.kind === 'chain'
        ? fetchChainUnlockProbe(surface.app, surface.chain, selected)
        : fetchNodeUnlockProbe(surface.node, selected),
    refetchInterval: (query: { state: { data?: NodeUnlockProbe | null } }) =>
      active(query.state.data) ? 2_000 : 30_000,
  });
  const v4 = useQuery(probe('v4'));
  const v6 = useQuery(probe('v6'));
  const start = useMutation({
    mutationFn: (selected: UnlockIpFamily) =>
      surface.kind === 'chain'
        ? startChainUnlockProbe(surface.app, surface.chain, selected)
        : startNodeUnlockProbe(surface.node, selected),
    onSuccess: (job, selected) => {
      client.setQueryData(keyOf(selected), job);
      void client.invalidateQueries({ queryKey: ['chain-unlock-probes'] });
    },
  });
  const jobs = { v4: v4.data ?? null, v6: v6.data ?? null };
  const reports = { v4: unlockReport(jobs.v4), v6: unlockReport(jobs.v6) };
  /** Only results ordinary traffic can reach are counted; a fallback reading stays visible but outside the count. */
  const countOf = (selected: UnlockIpFamily) => {
    const counted = reports[selected].rows.filter(row => !fellBack(row));
    return {
      available: counted.filter(row => row.verdict.tone === 'ok').length,
      total: counted.length,
      rows: counted,
      fallback: reports[selected].rows.filter(row => row.route === 'fallback').length,
      excluded: reports[selected].rows.filter(row => row.route === 'excluded').length,
    };
  };
  const job = jobs[family];
  return {
    surface,
    family,
    setFamily,
    editable,
    now,
    jobs,
    reports,
    rows: reports[family].rows,
    countOf,
    job,
    pending: v4.isPending || v6.isPending,
    hasResults: reports[family].hasResults,
    stale: reports[family].stale,
    running: active(job),
    starting: start.isPending && start.variables === family,
    cooldown: job ? Math.max(0, Math.ceil(job.created_at + 60 - now / 1000)) : 0,
    run: () => start.mutate(family),
  };
}
type Panel = ReturnType<typeof usePanel>;

/* ══ 解析路径弹层（只列事实） ══ */

function RuleFacts({ path }: { path: UnlockDnsPath }) {
  return (
    <>
      {path.rules.map((rule, index) => (
        <div key={index}>
          <dt>{index === 0 ? '规则' : ''}</dt>
          <dd>
            <code className="pm-code">{selectorText(rule.selector)}</code>
            <span className="pm-rule">
              <code className="pm-code">{egressDnsEndpointText(rule.resolution)}</code>
              {rule.resolution.transport.toUpperCase()} · {STRATEGY[rule.resolution.address_strategy]} ·{' '}
              {rule.resolution.fallback === 'stop' ? '失败停止' : '失败继续后续 DNS'}
            </span>
          </dd>
        </div>
      ))}
      <div>
        <dt>出口</dt>
        <dd>{machineName(path.exit)}</dd>
      </div>
    </>
  );
}

function PathFacts({ row }: { row: Row }) {
  const paths = row.dnsPart?.paths ?? [];
  const families = [...new Set(paths.flatMap(path => (path.normal_family ? [FAMILY[path.normal_family]] : [])))];
  const connections = (row.source?.connection_observations ?? []).filter(item => item.part === row.dnsPart?.part);
  return (
    <>
      <dl>
        <div>
          <dt>常规流量</dt>
          <dd>
            {row.dnsPart?.kind === 'unknown'
              ? '未完全确定'
              : families.length
                ? `${row.dnsPart?.kind === 'partial' ? '已确认部分走 ' : ''}${families.join(' / ')}`
                : '未能确定'}
          </dd>
        </div>
        <div>
          <dt>{row.family ? `${FAMILY[row.family]} 结果` : '结果'}</dt>
          <dd>
            {row.route === 'excluded' ? '停止连接，未检测' : row.verdict.label}
            {row.region && <span className="unlock-yt-dim">{row.region}</span>}
            <small>
              {row.route === 'excluded'
                ? '不回退，不计入可用统计'
                : row.route === 'fallback'
                  ? '经 DNS 回退取得，不计入可用统计'
                  : row.dnsPart?.kind === 'partial'
                    ? '部分连接经 DNS 回退，保留可用统计'
                    : '常规地址族待确认，保留本族结果'}
            </small>
          </dd>
        </div>
      </dl>
      {paths.map((path, index) => (
        <section className="pm-path-group" key={`${path.exit}/${index}`}>
          {paths.length > 1 && <h4>解析路径 {index + 1}</h4>}
          <dl>
            {path.reason && (
              <div>
                <dt>回退原因</dt>
                <dd>
                  {path.reason === 'resolver_error'
                    ? '规则 DNS 请求失败'
                    : `规则 DNS 未返回${row.family ? ` ${FAMILY[row.family]}` : '该族'} 地址`}
                </dd>
              </div>
            )}
            <div>
              <dt>回答来源</dt>
              <dd>
                {path.fallback === 'stop'
                  ? '停止连接，无回答'
                  : path.answer_from === 'policy'
                    ? '后续自定义 DNS'
                    : path.answer_from === 'mixed'
                      ? '后续自定义 DNS / 机器 DNS'
                      : path.answer_from === 'machine'
                        ? '机器 DNS'
                        : '未确认'}
              </dd>
            </div>
            <RuleFacts path={path} />
          </dl>
        </section>
      ))}
      {connections.length > 0 && (
        <dl className="pm-path-group">
          <div>
            <dt>实际连接</dt>
            <dd className="pm-connections">
              {connections.slice(0, 6).map(item => (
                <span key={item.id}>
                  <code className="pm-code">{item.domain}</code>
                  <small>
                    {machineName(item.exit)} ·{' '}
                    {item.connected
                      ? `${item.family ? FAMILY[item.family] : '地址族未记录'} 已连接`
                      : (connectionFailureReason(item) ?? '未建立连接')}
                  </small>
                </span>
              ))}
              {connections.length > 6 && <small>另有 {connections.length - 6} 条连接</small>}
            </dd>
          </div>
        </dl>
      )}
    </>
  );
}

function PathButton({ row, className, children }: { row: Row; className: string; children: ReactNode }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const label = `${row.name} · ${row.family ? FAMILY[row.family] : ''} 解析路径`;
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={className}
        aria-label={label}
        aria-expanded={open}
        onClick={event => {
          event.stopPropagation();
          setOpen(value => !value);
        }}
      >
        {children}
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="start" label={label} onClose={() => setOpen(false)}>
          <section className="dialog-surface unlock-yt pm-path" aria-label={label}>
            <DialogClose className="sub-dialog-close unlock-yt-close" aria-label="关闭解析路径">
              <Icon of="close" size={14} />
            </DialogClose>
            <h3>{label}</h3>
            <PathFacts row={row} />
          </section>
        </DialogLayer>
      )}
    </>
  );
}

/* ══ 状态行：结论只在一个位置表达 ══ */

/** The fallback state: same ring as the neutral glyphs, with a turn-back arrow instead of a mark. */
function FallbackGlyph() {
  return (
    <svg className="unlock-glyph pm-fallback-glyph" viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      <circle cx="8" cy="8" r="6.4" className="ring" />
      <path d="M10.6 10.2V8.4a2.3 2.3 0 0 0-2.3-2.3H5.6" className="dash" fill="none" />
      <path d="M7.1 4.4 5.4 6.1 7.1 7.8" className="dash" fill="none" />
    </svg>
  );
}

/** One line per service: glyph and verdict together. A fallback is a state of this line, never a faded card. */
function StatusLine({ row, compact = false }: { row: Row; compact?: boolean }) {
  if (fellBack(row)) {
    const other = FAMILY[otherFamily(row.family ?? 'v6')];
    return (
      <PathButton row={row} className="pm-status pm-status-fallback">
        <FallbackGlyph />
        {row.route === 'excluded' ? (
          <span className="pm-status-text">只解析 {other}</span>
        ) : (
          <span className="pm-status-text">
            回退<i>{row.verdict.label}</i>
          </span>
        )}
      </PathButton>
    );
  }
  const { verdict } = row;
  const label =
    row.service === 'youtube' && verdict.tone === 'ok'
      ? (youtubeLine(row.youtube) ?? verdict.label)
      : verdict.tone === 'ok'
        ? (verdict.note ?? verdict.label)
        : verdict.tone === 'unknown' && row.result && !compact
          ? `${verdict.label} · ${verdict.reason}`
          : verdict.label;
  const note = pathNote(row);
  return (
    <span className="pm-status" data-tone={verdict.tone} title={row.result ? verdict.reason : undefined}>
      <Glyph tone={verdict.tone} />
      <span className="pm-status-text">{label}</span>
      {note && (
        <PathButton row={row} className="pm-status-note">
          {note}
        </PathButton>
      )}
    </span>
  );
}

function Region({ row, size }: { row: Row; size: 'large' | 'small' }) {
  if (!row.region) return <b className="pm-region none">—</b>;
  return (
    <span className="pm-region" data-size={size}>
      <RegionFlag code={unlockFlagRegion(row.service, row.region)} />
      <b>{row.region}</b>
      {size === 'large' && row.region.length === 2 && <span className="pm-region-name">{regionName(row.region)}</span>}
    </span>
  );
}

const youtubeInfo = (row: Row) =>
  (row.service === 'youtube' || (row.service === 'youtube_premium' && !row.result?.youtube)) && (
    <YoutubeMore result={row.result ?? undefined} />
  );

/* ══ A / B：磁贴（卡面不变，状态并入底行） ══ */

function Tile({ row }: { row: Row }) {
  return (
    <article
      className="unlock-b-tile pm-tile"
      aria-label={row.name}
      data-service={row.service}
      data-tone={row.verdict.tone}
      data-route={fellBack(row) ? row.route : undefined}
    >
      <div className="unlock-b-top">
        <ServiceMark service={row.service} />
        <span className="unlock-b-name">{row.name}</span>
        {youtubeInfo(row)}
      </div>
      <Region row={row} size="large" />
      <div className="pm-foot">
        <StatusLine row={row} />
        {row.change && <span className="unlock-change">{row.change}</span>}
      </div>
    </article>
  );
}

function Tiles({ p, variant }: { p: Panel; variant: 'tab' | 'dialog' }) {
  return (
    <div className="unlock-b" data-variant={variant}>
      {CATEGORIES.map(category => (
        <section key={category.key} className="unlock-b-row" aria-label={category.name}>
          <h3>{category.name}</h3>
          <div className="unlock-b-tiles">
            {p.rows
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

/* ══ C：分类卡（一类一张卡，格子之间细线分隔） ══ */

function Cell({ row }: { row: Row }) {
  return (
    <article
      className="pm-cell"
      aria-label={row.name}
      data-service={row.service}
      data-tone={row.verdict.tone}
      data-route={fellBack(row) ? row.route : undefined}
    >
      <div className="pm-cell-top">
        <ServiceMark service={row.service} />
        <span className="pm-cell-name">{row.name}</span>
        {youtubeInfo(row)}
      </div>
      <Region row={row} size="large" />
      <div className="pm-foot">
        <StatusLine row={row} />
        {row.change && <span className="unlock-change">{row.change}</span>}
      </div>
    </article>
  );
}

function CategoryCards({ p }: { p: Panel }) {
  return (
    <div className="pm-cats">
      {CATEGORIES.map(category => {
        const rows = p.rows.filter(row => row.category === category.key);
        return (
          <section key={category.key} className="pm-cat-sec" aria-label={category.name}>
            <h3>{category.name}</h3>
            <div className="pm-cat" data-count={rows.length}>
              {rows.map(row => (
                <Cell key={row.service} row={row} />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

/* ══ D：紧凑条目（名称与状态一行，地区靠右） ══ */

function Item({ row }: { row: Row }) {
  return (
    <article
      className="pm-item"
      aria-label={row.name}
      data-service={row.service}
      data-tone={row.verdict.tone}
      data-route={fellBack(row) ? row.route : undefined}
    >
      <ServiceMark service={row.service} />
      <span className="pm-item-main">
        <span className="pm-item-head">
          <span className="pm-item-name">
            {row.name}
            {youtubeInfo(row)}
          </span>
          <Region row={row} size="small" />
        </span>
        <StatusLine row={row} compact />
      </span>
      {row.change && <i className="pm-dot" title={row.change} aria-label={row.change} />}
    </article>
  );
}

function Items({ p }: { p: Panel }) {
  return (
    <div className="pm-items-wrap">
      {CATEGORIES.map(category => (
        <section key={category.key} className="pm-items-sec" aria-label={category.name}>
          <h3>{category.name}</h3>
          <div className="pm-items">
            {p.rows
              .filter(row => row.category === category.key)
              .map(row => (
                <Item key={row.service} row={row} />
              ))}
          </div>
        </section>
      ))}
    </div>
  );
}

/* ══ 统计与底部地址族切换 ══ */

function Strip({ rows }: { rows: Row[] }) {
  return (
    <span className="unlock-b-strip" aria-hidden="true">
      {rows.map(row => (
        <i key={row.service} data-tone={row.verdict.tone} title={`${row.name} · ${row.verdict.label}`} />
      ))}
    </span>
  );
}

function FamilyTabs({ p }: { p: Panel }) {
  return (
    <div className="pm-fam" role="group" aria-label="解锁地址族">
      {(['v4', 'v6'] as const).map(family => {
        const count = p.countOf(family);
        const has = p.reports[family].hasResults;
        return (
          <button
            key={family}
            type="button"
            aria-pressed={p.family === family}
            className="pm-fam-tab"
            data-stale={p.reports[family].stale || undefined}
            onClick={() => p.setFamily(family)}
          >
            <span className="pm-fam-name">{FAMILY[family]}</span>
            {has ? (
              <>
                <span className="pm-fam-count">
                  <b>{count.available}</b>
                  <span>/ {count.total} 可用</span>
                </span>
                <Strip rows={count.rows} />
                <CountNote p={p} family={family} />
              </>
            ) : (
              <span className="pm-fam-count">
                <span>尚未检测</span>
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

const GAP = ' ';
function span(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}${GAP}秒`;
  if (s < 3600) return `${Math.floor(s / 60)}${GAP}分钟`;
  if (s < 86400) return `${Math.floor(s / 3600)}${GAP}小时`;
  return `${Math.floor(s / 86400)}${GAP}天`;
}
const absolute = (secs: number) => new Date(secs * 1000).toLocaleString('zh-CN', { hour12: false });
const Ago = ({ at, now }: { at: number; now: number }) => (
  <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
    {now / 1000 - at < 45 ? '刚刚' : `${span(now / 1000 - at)}前`}
  </time>
);
const Until = ({ at, now }: { at: number; now: number }) => (
  <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
    {at - now / 1000 <= 60 ? '即将' : `${span(at - now / 1000)}后`}
  </time>
);

function Meta({ p }: { p: Panel }) {
  const { job, now } = p;
  const parts: ReactNode[] = [];
  if (p.pending) parts.push('读取中…');
  else if (job && active(job)) {
    const seconds = Math.max(0, Math.floor(now / 1000 - job.created_at));
    parts.push(
      <span className="unlock-meta-lead" data-tone="run">
        <Glyph tone="wait" />
        {job.state === 'queued'
          ? '等待机器响应'
          : `检测中 ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`}
      </span>,
    );
  } else if (job?.completed_at) parts.push(<Ago at={job.completed_at} now={now} />);
  if (job?.next_run_at && !active(job))
    parts.push(
      <span>
        下次全量 <Until at={job.next_run_at} now={now} />
      </span>,
    );
  if (job?.protocol) parts.push(<span>{job.protocol}</span>);
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

function RunButton({ p }: { p: Panel }) {
  if (!p.editable) return null;
  const label = p.starting
    ? '发起中…'
    : p.cooldown > 0 && !p.running
      ? `${p.cooldown}${GAP}秒后可重测`
      : p.running
        ? '检测中…'
        : '重新检测';
  return (
    <button
      type="button"
      className="btn unlock-run"
      disabled={p.running || p.starting || p.pending || p.cooldown > 0}
      title={p.cooldown > 0 && !p.running ? '两次检测至少间隔 60 秒' : undefined}
      onClick={p.run}
    >
      <Icon of="refresh" size={13} />
      {label}
    </button>
  );
}

function Header({ p, option }: { p: Panel; option: Option }) {
  const count = p.countOf(p.family);
  return (
    <div className="unlock-bar">
      {option !== 'b' && p.hasResults && (
        <div className="pm-summary">
          <span
            className="unlock-lead unlock-b-hero"
            data-stale={p.stale ? '' : undefined}
            aria-label={`可用 ${count.available} / ${count.total}`}
          >
            <b>{count.available}</b>
            <span>/ {count.total} 可用</span>
            <Strip rows={count.rows} />
          </span>
          <CountNote p={p} family={p.family} />
        </div>
      )}
      <RunButton p={p} />
    </div>
  );
}

function CountNote({ p, family }: { p: Panel; family: UnlockIpFamily }) {
  if (!p.reports[family].hasResults) return null;
  const count = p.countOf(family);
  const parts = [
    count.fallback ? `${count.fallback} 项回退` : '',
    count.excluded ? `${count.excluded} 项停止连接` : '',
  ].filter(Boolean);
  return parts.length ? <span className="pm-count-note">另 {parts.join('、')}，不计入统计</span> : null;
}

function Footer({ p, option, variant }: { p: Panel; option: Option; variant: 'tab' | 'dialog' }) {
  return (
    <div className="unlock-footer" data-variant={variant}>
      {option === 'b' ? (
        <FamilyTabs p={p} />
      ) : (
        <SegmentedControl
          value={p.family}
          options={[
            { value: 'v4' as UnlockIpFamily, label: 'IPv4' },
            { value: 'v6' as UnlockIpFamily, label: 'IPv6' },
          ]}
          ariaLabel="解锁地址族"
          onChange={p.setFamily}
        />
      )}
      <span role="status" aria-live="polite">
        <Meta p={p} />
      </span>
    </div>
  );
}

function Body({ p, option, variant }: { p: Panel; option: Option; variant: 'tab' | 'dialog' }) {
  if (p.pending) return null;
  return (
    <div className="unlock-body" data-stale={p.stale ? '' : undefined} aria-busy={p.running}>
      {option === 'c' ? <CategoryCards p={p} /> : option === 'd' ? <Items p={p} /> : <Tiles p={p} variant={variant} />}
    </div>
  );
}

const CHAIN_UNLOCK_TITLE = '区域性服务解锁状态';

/** The chain head button and its dialog, in place of production `ChainUnlockControl`. */
export function PanelDialog({
  option,
  surface,
  editable,
  initialOpen,
  initialFamily,
}: {
  option: Option;
  surface: Extract<Surface, { kind: 'chain' }>;
  editable: boolean;
  initialOpen: boolean;
  initialFamily: UnlockIpFamily;
}) {
  const [open, setOpen] = useState(initialOpen);
  const p = usePanel(surface, initialFamily, editable);
  const count = p.countOf(p.family);
  return (
    <>
      <button type="button" className="btn unlock-head-btn" onClick={() => setOpen(true)} aria-label="解锁">
        <Icon of="unlock" size={14} />
        解锁
        {p.hasResults && (
          <span className="unlock-head-count" data-tone={count.available === count.total ? 'ok' : undefined}>
            {count.available}/{count.total}
          </span>
        )}
        {p.running && <Glyph tone="wait" />}
      </button>
      {open && (
        <DialogLayer label={`${CHAIN_UNLOCK_TITLE} · ${surface.name}`} onClose={() => setOpen(false)}>
          <section className="dialog-surface sub-dialog chain-unlock-dialog unlock-dialog" data-pm-option={option}>
            <header className="sub-dialog-head">
              <span className="unlock-dialog-title">
                <Icon of="unlock" size={16} className="unlock-dialog-title-icon" />
                <b>{CHAIN_UNLOCK_TITLE}</b>
                <small title={surface.name}>{surface.name}</small>
              </span>
              <DialogClose className="sub-dialog-close" title="关闭" aria-label="关闭解锁">
                <Icon of="close" size={16} />
              </DialogClose>
            </header>
            <div className="unlock-dialog-body">
              <Header p={p} option={option} />
              <Body p={p} option={option} variant="dialog" />
            </div>
            <Footer p={p} option={option} variant="dialog" />
          </section>
        </DialogLayer>
      )}
    </>
  );
}

/** The machine 解锁 tab, in place of production `NodeUnlockPanel`. */
export function PanelTab({
  option,
  surface,
  editable,
  initialFamily,
}: {
  option: Option;
  surface: Extract<Surface, { kind: 'node' }>;
  editable: boolean;
  initialFamily: UnlockIpFamily;
}) {
  const p = usePanel(surface, initialFamily, editable);
  return (
    <div className="node-unlock unlock-page pm-tab" data-pm-option={option}>
      <Header p={p} option={option} />
      <Body p={p} option={option} variant="tab" />
      <Footer p={p} option={option} variant="tab" />
    </div>
  );
}
