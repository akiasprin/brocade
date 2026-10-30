// Option parts for the 解锁报告 · DNS 回退 mockup. Synthetic data only; never imported by the app.
// The dialog repeats the production markup of `panes/node-unlock.tsx` (same classes, same CSS) and reads the same
// query keys, so only the parts each option changes differ from production. The list table repeats
// `panes/chain-unlock-table.tsx` around a summary that leaves fallback-only families out of the comparison.
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchChainUnlockCapabilities,
  fetchChainUnlockProbe,
  fetchChainUnlockProbes,
  startChainUnlockProbe,
  type ChainUnlockProbeItem,
  type DestMatch,
  type NodeUnlockProbe,
  type SnapshotApp,
  type UnlockIpFamily,
  type UnlockService,
} from '../../src/api';
import { SegmentedControl } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { Icon } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { unlockFlagRegion } from '../../src/ui/unlock-region';
import { ServiceMark } from '../../src/panes/node-unlock';
import { Glyph, type Tone, type Verdict } from '../../src/panes/unlock-status';
import {
  CATEGORIES,
  CATEGORY,
  DISPLAY_SERVICE_NAMES,
  SERVICES,
  unlockReport,
  type DisplayService,
  type Row,
} from '../../src/panes/unlock-report';
import { summarizeChainUnlock, type ChainUnlockCell } from '../../src/panes/chain-unlock-summary';
import { YoutubeMore, youtubeLine } from '../../src/panes/youtube-unlock';
import { MACHINES, e2eProbes } from './list-head-mockup-data';
import { familiesOf, nowSecs, routeOf, type FallbackProbe, type Route, type UnlockDnsPath } from './unlock-fallback-mockup-data';
import '../../src/panes/chain-unlock-table.css';

export type Option = 'a' | 'b' | 'c' | 'd';
/** A family's own report, or D's 实际: each service read in the families its ordinary traffic uses. */
type Lens = UnlockIpFamily | 'real';

const FAMILY = { v4: 'IPv4', v6: 'IPv6' } as const;
const otherFamily = (family: UnlockIpFamily): UnlockIpFamily => (family === 'v4' ? 'v6' : 'v4');
const wire = (service: DisplayService): UnlockService => (service === 'youtube' ? 'youtube_premium' : service);
const machineName = (id: string) => MACHINES.find(machine => machine.id === id)?.name ?? id;
const STRATEGY = {
  use_ip: 'UseIP',
  use_ipv4v6: 'UseIPv4v6',
  use_ipv6v4: 'UseIPv6v4',
  use_ipv4: 'UseIPv4',
  use_ipv6: 'UseIPv6',
} as const;
const FALLBACK = { stop: '停止连接', machine: '回退机器 DNS' } as const;
const SELECTOR_PREFIX: Partial<Record<DestMatch['t'], string>> = {
  geosite: 'geosite',
  domain_suffix: 'domain',
  domain_keyword: 'keyword',
  domain_regex: 'regexp',
};
const selectorText = (match: DestMatch) =>
  'v' in match
    ? `${SELECTOR_PREFIX[match.t] ?? match.t}:${Array.isArray(match.v) ? match.v.join(',') : String(match.v)}`
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

interface FRow extends Row {
  family: UnlockIpFamily;
  path: UnlockDnsPath | undefined;
  route: Route;
}

/** The production rows plus how ordinary traffic treats this family of each service. */
function familyReport(job: FallbackProbe | null | undefined, family: UnlockIpFamily) {
  const report = unlockReport(job);
  const rows = report.rows.map<FRow>(row => {
    const path = job?.dns_paths?.find(item => item.service === wire(row.service));
    const route = routeOf(path, family);
    // 停止连接时这一族没有地址可连：结论仍是「已跳过」，原因换成已知的策略，而不是笼统的「无解析地址」。
    const verdict: Verdict =
      route === 'excluded' && row.result
        ? {
            tone: 'unknown',
            label: '已跳过',
            note: null,
            reason: `DNS 策略只解析 ${FAMILY[otherFamily(family)]}`,
          }
        : row.verdict;
    return { ...row, verdict, family, path, route };
  });
  return { ...report, rows };
}

interface RealRow {
  row: FRow;
  verdict: Verdict;
  /** Families ordinary traffic uses; one entry when a DNS policy pins the service to it. */
  families: UnlockIpFamily[];
  /** The other family's reading when only one family applies (shown in the path popover). */
  aside: FRow | null;
  /** The IPv6 region when both families apply and it differs. */
  ipv6Region: string | null;
}

/** D 实际：DNS 策略只给一族时只看这一族；两族都可能使用时合并，不一致即金色提示。 */
function realRows(v4: FRow[], v6: FRow[]): RealRow[] {
  return v4.map((a, index) => {
    const b = v6[index];
    const families = familiesOf(a.path ?? b.path);
    if (families.length === 1) {
      const [row, aside] = families[0] === 'v4' ? [a, b] : [b, a];
      return { row, verdict: row.verdict, families, aside, ipv6Region: null };
    }
    const skipped = b.source?.code === 'no_ipv6_dns' || b.source?.code === 'no_ipv6_egress';
    if (!b.result || skipped) return { row: a, verdict: a.verdict, families, aside: null, ipv6Region: null };
    if (!a.result) return { row: b, verdict: b.verdict, families, aside: null, ipv6Region: null };
    const ipv6Region = b.region && b.region !== a.region ? b.region : null;
    if (a.verdict.tone === b.verdict.tone && a.verdict.label === b.verdict.label)
      return { row: a, verdict: a.verdict, families, aside: null, ipv6Region };
    const tone: Tone = a.verdict.tone === 'ok' || b.verdict.tone === 'ok' ? 'part' : a.verdict.tone;
    const label =
      a.verdict.tone === 'ok'
        ? `IPv6 ${b.verdict.label}`
        : b.verdict.tone === 'ok'
          ? `IPv4 ${a.verdict.label}`
          : `IPv4 ${a.verdict.label} · IPv6 ${b.verdict.label}`;
    return {
      row: a,
      verdict: { tone, label, note: null, reason: `IPv4 ${a.verdict.label}；IPv6 ${b.verdict.label}` },
      families,
      aside: null,
      ipv6Region,
    };
  });
}

const active = (job: NodeUnlockProbe | null | undefined) => job?.state === 'queued' || job?.state === 'running';

function useFallbackUnlock(app: string, chain: string, option: Option, initial: Lens, editable: boolean) {
  const [lens, setLens] = useState<Lens>(option === 'd' ? initial : initial === 'real' ? 'v4' : initial);
  const client = useQueryClient();
  const now = useNow();
  useQuery({
    queryKey: ['chain-unlock-capabilities', app, chain],
    queryFn: () => fetchChainUnlockCapabilities(app, chain),
    refetchInterval: 30_000,
  });
  const probe = (family: UnlockIpFamily) => ({
    queryKey: ['chain-unlock-probe', app, chain, family],
    queryFn: () => fetchChainUnlockProbe(app, chain, family) as Promise<FallbackProbe | null>,
    refetchInterval: (query: { state: { data?: FallbackProbe | null } }) =>
      active(query.state.data) ? 2_000 : 30_000,
  });
  const v4 = useQuery(probe('v4'));
  const v6 = useQuery(probe('v6'));
  const start = useMutation({
    mutationFn: (family: UnlockIpFamily) => startChainUnlockProbe(app, chain, family),
    onSuccess: (job, family) => {
      client.setQueryData(['chain-unlock-probe', app, chain, family], job);
      void client.invalidateQueries({ queryKey: ['chain-unlock-probes'] });
    },
  });
  const reports = { v4: familyReport(v4.data, 'v4'), v6: familyReport(v6.data, 'v6') };
  const jobs = { v4: v4.data ?? null, v6: v6.data ?? null };
  const families: UnlockIpFamily[] = lens === 'real' ? ['v4', 'v6'] : [lens];
  const real = realRows(reports.v4.rows, reports.v6.rows);
  const counted =
    lens === 'real'
      ? real.map(item => item.verdict.tone)
      : reports[lens].rows.filter(row => row.route === 'normal').map(row => row.verdict.tone);
  const running = families.some(family => active(jobs[family]));
  const cooldown = Math.max(
    0,
    ...families.map(family => {
      const job = jobs[family];
      return job ? Math.ceil(job.created_at + 60 - now / 1000) : 0;
    }),
  );
  return {
    option,
    lens,
    setLens,
    editable,
    now,
    jobs,
    reports,
    real,
    pending: v4.isPending || v6.isPending,
    hasResults: families.some(family => reports[family].hasResults),
    stale: families.some(family => reports[family].stale),
    running,
    starting: start.isPending,
    cooldown,
    available: counted.filter(tone => tone === 'ok').length,
    total: counted.length,
    run: () => families.forEach(family => start.mutate(family)),
  };
}
type Unlock = ReturnType<typeof useFallbackUnlock>;

/* ══ 解析路径：命中的出口 DNS 策略 ══ */

function PathFacts({ row, aside }: { row: FRow; aside?: FRow | null }) {
  const path = row.path!;
  const { resolution } = path;
  const families = familiesOf(path);
  return (
    <dl>
      <div>
        <dt>常规流量</dt>
        <dd>{families.map(family => FAMILY[family]).join(' / ')}</dd>
      </div>
      {row.route !== 'normal' && (
        <div>
          <dt>{FAMILY[row.family]} 结果</dt>
          <dd>
            {row.route === 'fallback' ? '经回退机器 DNS 取得' : '不回退，已跳过'}
            {row.route === 'fallback' && <small>自定义 DNS 正常时不使用</small>}
          </dd>
        </div>
      )}
      {aside && (
        <div>
          <dt>{FAMILY[aside.family]}{aside.route === 'fallback' ? ' 回退' : ''}</dt>
          <dd>
            <Glyph tone={aside.verdict.tone} />
            {aside.route === 'excluded' ? '已跳过' : aside.verdict.label}
            {aside.region && <span className="unlock-yt-dim">{aside.region}</span>}
          </dd>
        </div>
      )}
      <div>
        <dt>DNS 策略</dt>
        <dd>
          <code className="fb-code">{selectorText(path.selector)}</code>
        </dd>
      </div>
      <div>
        <dt>解析器</dt>
        <dd>
          <code className="fb-code">
            {resolution.address}:{resolution.port}
          </code>
          <span className="unlock-yt-dim">{resolution.transport.toUpperCase()}</span>
        </dd>
      </div>
      <div>
        <dt>地址策略</dt>
        <dd>{STRATEGY[resolution.address_strategy]}</dd>
      </div>
      <div>
        <dt>失败处理</dt>
        <dd>{FALLBACK[resolution.fallback]}</dd>
      </div>
      <div>
        <dt>出口</dt>
        <dd>{machineName(path.exit)}</dd>
      </div>
    </dl>
  );
}

/** The trigger is a tag, an ⓘ, a chip or a whole compact item; the popover is the YouTube 分项 one. */
function PathButton({
  row,
  aside,
  className,
  label,
  children,
}: {
  row: FRow;
  aside?: FRow | null;
  className: string;
  label: string;
  children: ReactNode;
}) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  if (!row.path) return null;
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
        <DialogLayer mode="popover" anchor={anchor} align="end" label={label} onClose={() => setOpen(false)}>
          <section className="dialog-surface unlock-yt fb-path" aria-label={label}>
            <DialogClose className="sub-dialog-close unlock-yt-close" aria-label="关闭解析路径">
              <Icon of="close" size={14} />
            </DialogClose>
            <h3>
              {row.name}
              <small>{aside ? '解析路径' : `${FAMILY[row.family]} 解析路径`}</small>
            </h3>
            <PathFacts row={row} aside={aside} />
          </section>
        </DialogLayer>
      )}
    </>
  );
}

const Info = ({ row, aside }: { row: FRow; aside?: FRow | null }) => (
  <PathButton row={row} aside={aside} className="unlock-info" label={`${row.name} 解析路径`}>
    <Icon of="info" size={14} />
  </PathButton>
);

/* ══ 磁贴 ══ */

/** Production `Tile` with slots: `top` after the name, `regionExtra` after the region, `foot` replaces the label. */
function Tile({
  row,
  verdict = row.verdict,
  route,
  top,
  regionExtra,
  foot,
}: {
  row: Row;
  verdict?: Verdict;
  route?: Route;
  top?: ReactNode;
  regionExtra?: ReactNode;
  foot?: ReactNode;
}) {
  const detail =
    row.service === 'youtube' && verdict.tone === 'ok'
      ? null
      : verdict.tone === 'ok'
        ? (verdict.note ?? verdict.label)
        : verdict.tone === 'unknown' && row.result
          ? `${verdict.label} · ${verdict.reason}`
          : verdict.label;
  const youtube = youtubeLine(row.youtube);
  return (
    <article
      className="unlock-b-tile"
      aria-label={row.name}
      data-service={row.service}
      data-tone={verdict.tone}
      data-route={route && route !== 'normal' ? route : undefined}
      title={row.result ? verdict.reason : undefined}
    >
      <div className="unlock-b-top">
        <ServiceMark service={row.service} />
        <span className="unlock-b-name">{row.name}</span>
        {top}
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
            {regionExtra}
          </>
        ) : (
          <b className="none">—</b>
        )}
      </div>
      {(detail || youtube || row.change || foot) && (
        <div className="unlock-b-foot">
          {foot ?? (detail && <span className="unlock-b-label">{detail}</span>)}
          {youtube && <span className="unlock-b-label unlock-b-summary">{youtube}</span>}
          {row.change && <span className="unlock-change">{row.change}</span>}
        </div>
      )}
    </article>
  );
}

/** A：读数原样保留，去掉状态色，名称后标「回退」；停止连接时只写原因。 */
function MarkedTile({ row }: { row: FRow }) {
  return (
    <Tile
      row={row}
      route={row.route}
      top={
        row.route === 'fallback' ? (
          <PathButton row={row} className="fb-tag" label={`${row.name} · 回退结果的解析路径`}>
            回退
          </PathButton>
        ) : (
          <Info row={row} />
        )
      }
    />
  );
}

/** B：主读数让位给常规流量使用的地址族，回退取得的结论降为底行。 */
function YieldTile({ row, onLens }: { row: FRow; onLens: (lens: Lens) => void }) {
  const target = otherFamily(row.family);
  return (
    <article className="unlock-b-tile fb-yield" aria-label={row.name} data-service={row.service} data-route={row.route}>
      <div className="unlock-b-top">
        <ServiceMark service={row.service} />
        <span className="unlock-b-name">{row.name}</span>
        <Info row={row} />
      </div>
      <button type="button" className="fb-yield-go" onClick={() => onLens(target)}>
        走 {FAMILY[target]}
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <path d="M6 3.5 10.5 8 6 12.5" />
        </svg>
      </button>
      <div className="unlock-b-foot">
        <span className="fb-yield-note">
          <Glyph tone={row.route === 'excluded' ? 'unknown' : row.verdict.tone} />
          {row.route === 'excluded' ? (
            '不回退 · 已跳过'
          ) : (
            <>
              回退时 {row.verdict.label}
              {row.region && <b>{row.region}</b>}
            </>
          )}
        </span>
      </div>
    </article>
  );
}

/** C：回退结果收进末尾一行的紧凑条目，点开看解析路径。 */
function AsideItem({ row }: { row: FRow }) {
  return (
    <PathButton row={row} className="fb-chip" label={`${row.name} · ${row.route === 'fallback' ? '回退结果' : '已跳过'}`}>
      <ServiceMark service={row.service} />
      <span className="fb-chip-name">{row.name}</span>
      <Glyph tone={row.verdict.tone} />
      <span className="fb-chip-reading">
        {row.route === 'excluded' ? '已跳过' : `${row.verdict.label}${row.region ? ` · ${row.region}` : ''}`}
      </span>
    </PathButton>
  );
}

/** D 实际：只走一族的服务标出这一族，另一族的回退读数在路径弹层里。 */
function RealTile({ item }: { item: RealRow }) {
  const { row } = item;
  return (
    <Tile
      row={row}
      verdict={item.verdict}
      top={
        item.families.length === 1 && (
          <PathButton row={row} aside={item.aside} className="fb-lone" label={`${row.name} 解析路径`}>
            仅 {FAMILY[item.families[0]]}
          </PathButton>
        )
      }
      regionExtra={item.ipv6Region && <span className="fb-v6">v6 {item.ipv6Region}</span>}
    />
  );
}

function Rows({ children }: { children: (category: (typeof CATEGORIES)[number]) => ReactNode }) {
  return <div className="unlock-b" data-variant="dialog">{CATEGORIES.map(children)}</div>;
}

function Section({ name, children }: { name: string; children: ReactNode }) {
  return (
    <section className="unlock-b-row" aria-label={name}>
      <h3>{name}</h3>
      <div className="unlock-b-tiles">{children}</div>
    </section>
  );
}

function Body({ u }: { u: Unlock }) {
  if (u.lens === 'real')
    return (
      <Rows>
        {category => (
          <Section key={category.key} name={category.name}>
            {u.real
              .filter(item => item.row.category === category.key)
              .map(item => (
                <RealTile key={item.row.service} item={item} />
              ))}
          </Section>
        )}
      </Rows>
    );
  const rows = u.reports[u.lens].rows;
  if (u.option === 'c') {
    const aside = rows.filter(row => row.route !== 'normal');
    return (
      <div className="unlock-b" data-variant="dialog">
        {CATEGORIES.map(category => {
          const shown = rows.filter(row => row.category === category.key && row.route === 'normal');
          return (
            shown.length > 0 && (
              <Section key={category.key} name={category.name}>
                {shown.map(row => (
                  <Tile key={row.service} row={row} />
                ))}
              </Section>
            )
          );
        })}
        {(['fallback', 'excluded'] as const).map(route => {
          const items = aside.filter(row => row.route === route);
          return (
            items.length > 0 && (
              <section key={route} className="unlock-b-row fb-aside" aria-label={route === 'fallback' ? '回退' : '已跳过'}>
                <h3>{route === 'fallback' ? '回退' : '已跳过'}</h3>
                <div className="fb-chips">
                  {items.map(row => (
                    <AsideItem key={row.service} row={row} />
                  ))}
                </div>
              </section>
            )
          );
        })}
      </div>
    );
  }
  return (
    <Rows>
      {category => (
        <Section key={category.key} name={category.name}>
          {rows
            .filter(row => row.category === category.key)
            .map(row =>
              row.route === 'normal' ? (
                <Tile key={row.service} row={row} />
              ) : u.option === 'b' ? (
                <YieldTile key={row.service} row={row} onLens={u.setLens} />
              ) : (
                <MarkedTile key={row.service} row={row} />
              ),
            )}
        </Section>
      )}
    </Rows>
  );
}

/* ══ 计数、状态行与检测按钮 ══ */

function Strip({ u }: { u: Unlock }) {
  const items =
    u.lens === 'real'
      ? u.real.map(item => ({ key: item.row.service, name: item.row.name, verdict: item.verdict, route: undefined }))
      : u.reports[u.lens].rows.map(row => ({
          key: row.service,
          name: row.name,
          verdict: row.verdict,
          route: row.route === 'normal' ? undefined : row.route,
        }));
  return (
    <span className="unlock-b-strip" aria-hidden="true">
      {items.map(item => (
        <i
          key={item.key}
          data-tone={item.verdict.tone}
          data-route={item.route}
          title={`${item.name} · ${item.route ? (item.route === 'fallback' ? '回退，不计入' : '已跳过，不计入') : item.verdict.label}`}
        />
      ))}
    </span>
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

function Meta({ u }: { u: Unlock }) {
  const shown = (u.lens === 'real' ? (['v4', 'v6'] as const) : [u.lens]).map(family => u.jobs[family]);
  const parts: ReactNode[] = [];
  const runningJob = shown.find(active);
  if (u.pending) parts.push('读取中…');
  else if (runningJob)
    parts.push(
      <span className="unlock-meta-lead" data-tone="run">
        <Glyph tone="wait" />
        {runningJob.state === 'queued'
          ? '等待机器响应'
          : `检测中 ${Math.floor((u.now / 1000 - runningJob.created_at) / 60)}:${String(Math.floor(u.now / 1000 - runningJob.created_at) % 60).padStart(2, '0')}`}
      </span>,
    );
  else {
    const completed = shown.flatMap(job => (job?.completed_at ? [job.completed_at] : []));
    if (completed.length) parts.push(<Ago at={Math.min(...completed)} now={u.now} />);
  }
  const next = shown.flatMap(job => (job?.next_run_at ? [job.next_run_at] : []));
  if (next.length && !runningJob)
    parts.push(
      <span>
        下次全量 <Until at={Math.min(...next)} now={u.now} />
      </span>,
    );
  const protocol = shown.find(job => job?.protocol)?.protocol;
  if (protocol) parts.push(<span>{protocol}</span>);
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

function RunButton({ u }: { u: Unlock }) {
  if (!u.editable) return null;
  const label = u.starting
    ? '发起中…'
    : u.cooldown > 0 && !u.running
      ? `${u.cooldown}${GAP}秒后可重测`
      : u.running
        ? '检测中…'
        : '重新检测';
  return (
    <button
      type="button"
      className="btn unlock-run"
      disabled={u.running || u.starting || u.pending || u.cooldown > 0}
      title={u.cooldown > 0 && !u.running ? '两次检测至少间隔 60 秒' : undefined}
      onClick={u.run}
    >
      <Icon of="refresh" size={13} />
      {label}
    </button>
  );
}

function Footer({ u }: { u: Unlock }) {
  const families = [
    { value: 'v4' as Lens, label: 'IPv4' },
    { value: 'v6' as Lens, label: 'IPv6' },
  ];
  return (
    <div className="unlock-footer" data-variant="dialog">
      <SegmentedControl
        value={u.lens}
        options={
          u.option === 'd'
            ? [
                {
                  value: 'real' as Lens,
                  label: <span title="按出口 DNS 策略，取各服务常规流量使用的地址族">实际</span>,
                },
                ...families,
              ]
            : families
        }
        ariaLabel="解锁地址族"
        onChange={u.setLens}
      />
      <span role="status" aria-live="polite">
        <Meta u={u} />
      </span>
    </div>
  );
}

const CHAIN_UNLOCK_TITLE = '区域性服务解锁状态';

/** The head button and its dialog, in place of production `ChainUnlockControl`. */
export function FallbackUnlock({
  option,
  app,
  chain,
  name,
  editable,
  initialOpen,
  initialLens,
}: {
  option: Option;
  app: string;
  chain: string;
  name: string;
  editable: boolean;
  initialOpen: boolean;
  initialLens: Lens;
}) {
  const [open, setOpen] = useState(initialOpen);
  const u = useFallbackUnlock(app, chain, option, initialLens, editable);
  return (
    <>
      <button type="button" className="btn unlock-head-btn" onClick={() => setOpen(true)} aria-label="解锁">
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
          <section className="dialog-surface sub-dialog chain-unlock-dialog unlock-dialog" data-fb-option={option}>
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
              <div className="unlock-bar">
                {u.hasResults && (
                  <span
                    className="unlock-lead unlock-b-hero"
                    data-stale={u.stale ? '' : undefined}
                    aria-label={`可用 ${u.available} / ${u.total}`}
                  >
                    <b>{u.available}</b>
                    <span>/ {u.total} 可用</span>
                    <Strip u={u} />
                  </span>
                )}
                <RunButton u={u} />
              </div>
              {!u.pending && (
                <div className="unlock-body" data-stale={u.stale ? '' : undefined} aria-busy={u.running}>
                  <Body u={u} />
                </div>
              )}
            </div>
            <Footer u={u} />
          </section>
        </DialogLayer>
      )}
    </>
  );
}

/* ══ 线路列表的对照表 ══ */

const displayRegion = (row: Row) =>
  row.region && row.service === 'gemini' ? (unlockFlagRegion(row.service, row.region) ?? row.region) : row.region;
const reading = (row: FRow) =>
  [
    `${FAMILY[row.family]}：${row.verdict.label}`,
    row.region && `地区 ${row.region}`,
    row.route === 'fallback' && '经回退机器 DNS 取得',
  ]
    .filter(Boolean)
    .join(' · ');

type FallbackCell = ChainUnlockCell & { lone: UnlockIpFamily | null };

/** Production summary, except that a family ordinary traffic does not use leaves the dual-stack comparison. */
function summarize(item: ChainUnlockProbeItem) {
  const base = summarizeChainUnlock(item);
  const v4 = familyReport(item.probes.v4 as FallbackProbe | null, 'v4');
  const v6 = familyReport(item.probes.v6 as FallbackProbe | null, 'v6');
  const cells = base.cells.map<FallbackCell>((cell, index) => {
    const a = v4.rows[index];
    const b = v6.rows[index];
    const lone = base.automatic ? null : a.route !== 'normal' ? 'v6' : b.route !== 'normal' ? 'v4' : null;
    if (!lone || (lone === 'v4' ? !a.result : !b.result)) return { ...cell, lone: null };
    const [row, unused, report] = lone === 'v4' ? [a, b, v4] : [b, a, v6];
    const why =
      unused.route === 'fallback'
        ? `${FAMILY[unused.family]} 仅在回退机器 DNS 时使用`
        : `DNS 策略不解析 ${FAMILY[unused.family]}`;
    return {
      ...cell,
      verdict: row.verdict,
      region: displayRegion(row),
      regionFamily: lone,
      ipv6Region: null,
      change: row.change ? `${FAMILY[lone]} ${row.change}` : null,
      title: [row.name, reading(row), unused.result && reading(unused), `${why}，采用 ${FAMILY[lone]} 结果`]
        .filter(Boolean)
        .join('；'),
      dim: report.stale,
      lone,
    };
  });
  return { ...base, cells };
}

const categories = CATEGORIES.map(category => ({
  ...category,
  services: SERVICES.filter(service => CATEGORY[service] === category.key),
}));
const services = categories.flatMap(category => category.services);
const shortNames: Partial<Record<DisplayService, string>> = { youtube_premium: 'Premium', prime_video: 'Prime' };

function TableAgo({ at }: { at: number }) {
  const seconds = Math.max(0, Math.floor(useNow() / 1000 - at));
  const label =
    seconds < 60
      ? '刚刚'
      : seconds < 3600
        ? `${Math.floor(seconds / 60)} 分钟前`
        : seconds < 86400
          ? `${Math.floor(seconds / 3600)} 小时前`
          : `${Math.floor(seconds / 86400)} 天前`;
  return (
    <time dateTime={new Date(at * 1000).toISOString()} title={absolute(at)}>
      {label}
    </time>
  );
}

type Probes = ReturnType<typeof e2eProbes>['chains'];

/** The chain status dot, from the same end-to-end probe the production list reads. */
function chainTone(probes: Probes, app: string, chain: string) {
  const probe = probes.find(item => item.app_id === app && item.chain_id === chain);
  if (!probe) return { tone: 'idle', title: '尚无拨测' };
  if (probe.status !== 'ok') return { tone: 'bad', title: probe.detail ?? '拨测不通' };
  if (probe.exit_verdict === 'mismatch') return { tone: 'warn', title: '出口与预期不符' };
  if ((probe.ttfb_ms ?? 0) > 400) return { tone: 'warn', title: '首字节偏慢' };
  return { tone: 'ok', title: '拨测正常' };
}

export function FallbackChainTable({ apps }: { apps: SnapshotApp[] }) {
  const client = useQueryClient();
  const [probes] = useState(() => e2eProbes(nowSecs()).chains);
  const query = useInfiniteQuery({
    queryKey: ['chain-unlock-probes'],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) => fetchChainUnlockProbes(pageParam, signal),
    getNextPageParam: page => page.next_cursor ?? undefined,
    refetchInterval: 30_000,
  });
  const { hasNextPage, isFetching, fetchNextPage } = query;
  useEffect(() => {
    if (hasNextPage && !isFetching) void fetchNextPage();
  }, [hasNextPage, isFetching, fetchNextPage]);
  const items = new Map(
    query.data?.pages.flatMap(page => page.chains.map(item => [`${item.app_id}/${item.chain_id}`, item] as const)),
  );
  const open = (item: ChainUnlockProbeItem | undefined, app: string, chain: string) => {
    if (item) {
      client.setQueryData(['chain-unlock-capabilities', item.app_id, item.chain_id], item.capabilities);
      for (const selection of ['v4', 'v6', 'auto'] as const)
        client.setQueryData(['chain-unlock-probe', item.app_id, item.chain_id, selection], item.probes[selection]);
    }
    location.hash = `#/chains/chain/${encodeURIComponent(app)}/${encodeURIComponent(chain)}`;
  };
  return (
    <>
      <div className="chain-unlock-scroll" role="region" aria-label="线路解锁对照表，可横向滚动" tabIndex={0}>
        <table className="chain-unlock-table" aria-label="线路服务解锁">
          <thead>
            <tr className="cut-cats">
              <th className="cut-corner" />
              {categories.map(category => (
                <th key={category.key} colSpan={category.services.length} scope="colgroup">
                  {category.name}
                </th>
              ))}
              <th className="cut-when" />
            </tr>
            <tr className="cut-heads">
              <th className="cut-corner" scope="col">
                链
              </th>
              {services.map(service => (
                <th
                  key={service}
                  scope="col"
                  aria-label={DISPLAY_SERVICE_NAMES[service]}
                  title={DISPLAY_SERVICE_NAMES[service]}
                  data-first={categories.some(category => category.services[0] === service) || undefined}
                >
                  <ServiceMark service={service} />
                  <span>{shortNames[service] ?? DISPLAY_SERVICE_NAMES[service]}</span>
                </th>
              ))}
              <th className="cut-when" scope="col">
                检测
              </th>
            </tr>
          </thead>
          {apps.map((app, index) => (
            <tbody key={app.id}>
              <tr className="cut-group">
                <th colSpan={services.length + 2} scope="rowgroup">
                  <span className="cut-group-in">
                    <span className="no">{String(index + 1).padStart(2, '0')}</span>
                    <b>{app.label || app.id}</b>
                    <span className="count">{app.chains.length} 条链</span>
                  </span>
                </th>
              </tr>
              {app.chains.map(chain => {
                const item = items.get(`${app.id}/${chain.id}`);
                const summary = item ? summarize(item) : null;
                const testable = app.ingresses.some(ingress => ingress.chain === chain.id);
                const status = chainTone(probes, app.id, chain.id);
                const blank = !testable
                  ? '缺接入面'
                  : !item
                    ? query.isSuccess && !query.hasNextPage
                      ? '尚未发布或尚未检测'
                      : '读取中…'
                    : !summary?.hasResults
                      ? (summary?.state ?? '未取得结果')
                      : null;
                return (
                  <tr key={chain.id} className="cut-row" onClick={() => open(item, app.id, chain.id)}>
                    <th scope="row" className="cut-chain">
                      <button
                        type="button"
                        className="cut-chain-in"
                        title={status.title}
                        onClick={event => {
                          event.stopPropagation();
                          open(item, app.id, chain.id);
                        }}
                      >
                        <i className="chain-card-status" data-tone={status.tone} aria-label={status.title} />
                        <RegionFlag code={chain.subscription_country} />
                        <span className="cut-name">{chain.name || chain.id}</span>
                      </button>
                    </th>
                    {blank !== null ? (
                      <td className="cut-blank" colSpan={services.length}>
                        {blank}
                      </td>
                    ) : (
                      services.map(service => {
                        const cell = summary!.cells.find(cell => cell.service === service)!;
                        return (
                          <td
                            key={service}
                            aria-label={cell.title}
                            title={cell.title}
                            data-service={service}
                            data-tone={cell.verdict.tone}
                            data-dim={cell.dim || undefined}
                            data-first={categories.some(category => category.services[0] === service) || undefined}
                          >
                            <span className="cut-cell">
                              <Glyph tone={cell.verdict.tone} />
                              <span className="cut-regions">
                                <span>
                                  {cell.regionFamily === 'v6' && 'v6 '}
                                  {cell.region ?? '—'}
                                </span>
                                {cell.ipv6Region && <small className="cut-v6">v6 {cell.ipv6Region}</small>}
                                {cell.lone && <small className="cut-v6 fb-cut-lone">仅 {cell.lone}</small>}
                              </span>
                              {cell.change && <i className="cut-dot" aria-label={cell.change} />}
                            </span>
                          </td>
                        );
                      })
                    )}
                    <td className="cut-when" data-tone={summary?.tone} title={summary?.detail}>
                      {summary?.automatic && <small className="cut-auto">不分族</small>}
                      {!testable ? '—' : (summary?.state ?? (summary?.at != null ? <TableAgo at={summary.at} /> : '—'))}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          ))}
        </table>
      </div>
      <p className="chain-unlock-legend">
        <Glyph tone="ok" />
        可用 <Glyph tone="part" />
        一族受限或异常 <Glyph tone="no" />
        不可用 <Glyph tone="unknown" />
        未取得结果 <i className="cut-dot" />
        较上次变化
        <span>
          地区以 IPv4 为主；不同时另列 v6 地区。IPv6 无解析地址时采用 IPv4 结果。「仅 v4」表示出口 DNS 策略只解析 IPv4，IPv6
          结果不参与比较。Reddit 显示 CDN 节点代码。
        </span>
      </p>
    </>
  );
}
