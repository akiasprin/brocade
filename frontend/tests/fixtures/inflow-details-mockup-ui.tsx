// Mockup parts: the redesigned 流入明细 dialog in three options (inflow-details-mockup.html). Never imported by the app.
// Each option reads the production inflow-details response plus the proposed local GeoIP decoration
// (source_countries / source_operators, the same fields the 在线设备 card already receives).
import { Fragment, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  fetchNodeInflowDetails,
  type NetworkOperator,
  type NodeInflowDetailRow,
  type NodeInflowDetails,
  type SiteUsagePeriod,
  type TrafficTimeRange,
} from '../../src/api';
import type { TrafficSource } from '../../src/user-traffic-realtime';
import { SegmentedControl } from '../../src/ui/bits';
import { CopyButton } from '../../src/ui/copy-button';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { bytes, shareOf } from '../../src/ui/format';
import { Icon } from '../../src/ui/icons';
import { PanelLoading } from '../../src/ui/loading';
import { useMetricQuery } from '../../src/ui/metric-cadence';
import { RegionFlag } from '../../src/ui/region-flag';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';

export type Option = 'a' | 'b' | 'c';
type Metric = 'total' | 'down' | 'up';

interface Located extends NodeInflowDetails {
  source_countries?: { ip: string; country: string }[];
  source_operators?: { ip: string; operator: NetworkOperator }[];
}

/* ══ 模型：接入链（含端口转发、未按接入链记录）× 来源 ══ */

interface Bytes {
  down: number;
  up: number;
}
const valueOf = (item: Bytes, metric: Metric) =>
  metric === 'down' ? item.down : metric === 'up' ? item.up : item.down + item.up;
type GroupKind = NodeInflowDetailRow['source'];
type LineKind = 'ip' | 'residual' | 'forwarding' | 'unattributed';
interface Line extends Bytes {
  key: string;
  kind: LineKind;
  ip: string | null;
  gap: boolean;
  /** Bytes per group, in group order. */
  parts: Map<string, Bytes>;
  country?: string;
  operator?: NetworkOperator;
}
interface Group extends Bytes {
  key: string;
  kind: GroupKind;
  name: string;
  country: string | null;
  color: string;
  gap: boolean;
  lines: Line[];
}
interface Model extends Bytes {
  groups: Group[];
  /** One line per address across chains; unrecorded parts of all chains merge into one line. */
  sources: Line[];
  ips: number;
  chains: number;
  gap: boolean;
}

const GRAY_FORWARDING = 'var(--nidm-gray-1)';
const GRAY_UNKNOWN = 'var(--nidm-gray-2)';
const groupKeyOf = (row: NodeInflowDetailRow) =>
  row.source === 'chain' ? JSON.stringify([row.app_id, row.chain_id, row.ingress_id]) : row.source;
const lineColor = (line: Line, group: Group) =>
  line.kind === 'ip' ? group.color : line.kind === 'forwarding' ? GRAY_FORWARDING : GRAY_UNKNOWN;

function modelOf(data: Located, sourceOf: (key: string) => TrafficSource, colorOf: (chainId: string) => string): Model {
  const countries = new Map(data.source_countries?.map(entry => [entry.ip, entry.country]));
  const operators = new Map(data.source_operators?.map(entry => [entry.ip, entry.operator]));
  const groups = new Map<string, Group>();
  for (const row of data.rows) {
    const key = groupKeyOf(row);
    const known = sourceOf(key);
    const group = groups.get(key) ?? {
      key,
      kind: row.source,
      name:
        row.source === 'forwarding'
          ? '端口转发'
          : row.source === 'unattributed'
            ? '未按接入链记录'
            : known.muted
              ? row.chain_id
                ? `接入链 ${row.chain_id}`
                : `历史接入点 ${row.ingress_id ?? ''}`
              : known.name,
      country: row.source === 'chain' ? known.country : null,
      color:
        row.source === 'chain'
          ? colorOf(row.chain_id ?? '')
          : row.source === 'forwarding'
            ? GRAY_FORWARDING
            : GRAY_UNKNOWN,
      down: 0,
      up: 0,
      gap: false,
      lines: [],
    };
    group.down += row.downlink_bytes;
    group.up += row.uplink_bytes;
    group.gap ||= row.has_gap;
    const kind: LineKind = row.source_ip ? 'ip' : row.source === 'chain' ? 'residual' : row.source;
    group.lines.push({
      key: `${key}/${row.source_ip ?? kind}`,
      kind,
      ip: row.source_ip,
      down: row.downlink_bytes,
      up: row.uplink_bytes,
      gap: row.has_gap,
      parts: new Map([[key, { down: row.downlink_bytes, up: row.uplink_bytes }]]),
      country: row.source_ip ? countries.get(row.source_ip) : undefined,
      operator: row.source_ip ? operators.get(row.source_ip) : undefined,
    });
    groups.set(key, group);
  }
  const sources = new Map<string, Line>();
  for (const group of groups.values())
    for (const line of group.lines) {
      const id = line.ip ?? line.kind;
      const source = sources.get(id) ?? { ...line, key: id, down: 0, up: 0, gap: false, parts: new Map() };
      source.down += line.down;
      source.up += line.up;
      source.gap ||= line.gap;
      const part = source.parts.get(group.key) ?? { down: 0, up: 0 };
      part.down += line.down;
      part.up += line.up;
      source.parts.set(group.key, part);
      sources.set(id, source);
    }
  const list = [...groups.values()];
  return {
    groups: list,
    sources: [...sources.values()],
    down: list.reduce((sum, group) => sum + group.down, 0),
    up: list.reduce((sum, group) => sum + group.up, 0),
    ips: [...sources.values()].filter(source => source.kind === 'ip').length,
    chains: list.filter(group => group.kind === 'chain').length,
    gap: list.some(group => group.gap),
  };
}
const byMetric =
  (metric: Metric) =>
  <T extends Bytes & { key: string }>(a: T, b: T) =>
    valueOf(b, metric) - valueOf(a, metric) || a.key.localeCompare(b.key);

/* ══ 来源的写法：与用户页「在线设备」相同 ══ */

const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const OPERATOR_NAMES = new Map<NetworkOperator, string>([
  ['chinanet', '电信'],
  ['cmcc', '移动'],
  ['unicom', '联通'],
  ['cernet', '教育网'],
  ['cstnet', '科技网'],
]);
function networkOf(country: string | undefined, operator: NetworkOperator | undefined) {
  const code = country?.trim().toUpperCase();
  const name = code && /^[A-Z]{2}$/.test(code) && code !== 'ZZ' ? REGION_NAMES.of(code) : undefined;
  const region = name && name !== code ? name : undefined;
  const operatorName = operator ? OPERATOR_NAMES.get(operator) : undefined;
  if (!region) return { code: undefined, label: operatorName ? `位置未知 · ${operatorName}` : '位置未知' };
  if (!operatorName) return { code, label: region };
  return { code, label: code === 'CN' ? `中国${operatorName}` : `${region} · ${operatorName}` };
}
function Network({ line }: { line: Line }) {
  if (line.kind !== 'ip') return <span className="nidm-net" />;
  const network = networkOf(line.country, line.operator);
  return (
    <span className="nidm-net" title="本地 GeoIP · 国家／地区与网络归属参考，非精确位置">
      {network.code && <RegionFlag code={network.code} />}
      <span>{network.label}</span>
    </span>
  );
}
function SourceAddress({ ip }: { ip: string }) {
  const groups = ip.split(':');
  if (groups.length <= 4 || !groups.slice(0, 4).every(Boolean)) return <code>{ip}</code>;
  return (
    <code>
      {groups.slice(0, 4).join(':')}
      <span className="nidm-iid">
        {groups.slice(4).map((group, index) => (
          <Fragment key={index}>
            :<wbr />
            {group}
          </Fragment>
        ))}
      </span>
    </code>
  );
}
const LINE_LABELS: Record<Exclude<LineKind, 'ip'>, string> = {
  residual: '未记录来源',
  forwarding: '不区分来源',
  unattributed: '不区分来源',
};
const LINE_TITLES: Record<Exclude<LineKind, 'ip'>, string> = {
  residual: '用量账本已计入、来源观测没有记录的部分：观测开启前、旧版 Xray 漏采或超出容量。不按 IP 均摊。',
  forwarding: '端口转发按规则记账，不记录来源 IP。',
  unattributed: '来源观测与用量账本的接入归属不一致，无法确认接入链的部分合并在这里。',
};
function Address({ line, label }: { line: Line; label?: string }) {
  if (!line.ip)
    return (
      <span className="nidm-addr" title={line.kind !== 'ip' ? LINE_TITLES[line.kind] : undefined}>
        <span className="nidm-unknown">{label ?? (line.kind !== 'ip' ? LINE_LABELS[line.kind] : '')}</span>
      </span>
    );
  return (
    <span className="nidm-addr">
      <SourceAddress ip={line.ip} />
      <CopyButton
        className="user-fcopy"
        text={line.ip}
        label={`复制来源 IP ${line.ip}`}
        successLabel="来源 IP 已复制"
        failureLabel="来源 IP 复制失败"
        iconOnly
      />
    </span>
  );
}

/* ══ 共同的头部与读数栏 ══ */

const PERIOD_WORDS: Record<SiteUsagePeriod, string> = { day: '当天', week: '当周', month: '当月' };
function rangeLabel(period: SiteUsagePeriod, range: TrafficTimeRange | undefined, start: number, end: number) {
  const parts = (seconds: number) => {
    const wall = new Date((seconds + 8 * 3600) * 1000);
    const two = (value: number) => String(value).padStart(2, '0');
    return {
      day: `${two(wall.getUTCMonth() + 1)}-${two(wall.getUTCDate())}`,
      time: `${two(wall.getUTCHours())}:${two(wall.getUTCMinutes())}`,
    };
  };
  const from = parts(start),
    to = parts(end);
  const span =
    from.day === to.day ? `${from.day} ${from.time}–${to.time}` : `${from.day} ${from.time} – ${to.day} ${to.time}`;
  return `${range ? '自选' : PERIOD_WORDS[period]} · ${span}`;
}

function Readout({ model, label }: { model: Model; label: string }) {
  const total = model.down + model.up;
  const [figure, unit] = bytes(total).split(' ');
  return (
    <section className="nidm-readout" aria-label="流入合计">
      <div className="nidm-hero">
        <span className="nidm-hero-label">{label}</span>
        <strong className="nidm-hero-value">
          {figure} <small>{unit}</small>
        </strong>
      </div>
      <div className="nidm-io">
        <div className="usage-split" aria-hidden="true">
          {total > 0 && (
            <>
              <i className="up" style={{ flexGrow: model.up }} />
              <i className="down" style={{ flexGrow: model.down }} />
            </>
          )}
        </div>
        <dl className="usage-io">
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{bytes(model.up)}</dd>
            <dd className="usage-share">{shareOf(model.up, total)}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{bytes(model.down)}</dd>
            <dd className="usage-share">{shareOf(model.down, total)}</dd>
          </div>
        </dl>
      </div>
      <dl className="nidm-facts">
        <div>
          <dt>来源 IP</dt>
          <dd>{model.ips}</dd>
        </div>
        <div>
          <dt>接入链</dt>
          <dd>{model.chains}</dd>
        </div>
      </dl>
    </section>
  );
}

/** Facts that change how far the numbers can be trusted; one short line, the ledger values behind (!). */
function Notices({ data, model }: { data: Located; model: Model }) {
  const [open, setOpen] = useState(false);
  const noteId = useId();
  const items = [data.limited && '来源记录不完整，仅含已记录部分', model.gap && '所选时段有采集缺口'].filter(
    Boolean,
  ) as string[];
  if (items.length === 0 && !data.accounting_mismatch) return null;
  return (
    <div className="nidm-notices">
      <p className="nidm-notice" role="status">
        <Icon of="warn" size={13} />
        {items.join('；')}
        {data.accounting_mismatch && items.length > 0 && '；'}
        {data.accounting_mismatch && (
          <button
            type="button"
            className="nidm-help"
            aria-expanded={open}
            aria-controls={noteId}
            onClick={() => setOpen(shown => !shown)}
          >
            与用量账本不一致
            <Icon of="info" size={13} />
          </button>
        )}
      </p>
      <p id={noteId} className="dim nidm-note" hidden={!open}>
        用量账本：上行 {bytes(data.accounting_uplink_bytes)}、下行 {bytes(data.accounting_downlink_bytes)}
        。来源观测与账本的接入归属或采样窗口不同，无法确认接入链的部分合并为「未按接入链记录」，不重复计入各链。
      </p>
    </div>
  );
}

const METRICS = [
  { value: 'total' as const, label: '合计' },
  { value: 'down' as const, label: '下行' },
  { value: 'up' as const, label: '上行' },
];
function ListBar({ title, metric, onMetric }: { title: string; metric: Metric; onMetric: (next: Metric) => void }) {
  return (
    <div className="nidm-listbar">
      <span className="nidm-listbar-title">{title}</span>
      <SegmentedControl value={metric} options={METRICS} ariaLabel="按合计、下行或上行排序与比较" onChange={onMetric} />
    </div>
  );
}

function GroupName({ group }: { group: Pick<Group, 'kind' | 'country' | 'name'> }) {
  return (
    <span className="nt-chain nidm-chain">
      {group.kind === 'forwarding' ? (
        <Icon of="forwarding" size={13} className="traffic-source-icon" />
      ) : (
        group.country && <RegionFlag code={group.country} />
      )}
      <span className="nt-chain-name">{group.name}</span>
    </span>
  );
}

function bytesTitle(line: Bytes, extra?: string) {
  return `${extra ? `${extra} · ` : ''}上行 ${bytes(line.up)} · 下行 ${bytes(line.down)}`;
}

/** A source row: address, network, a bar on the dialog-wide scale, the value and its share. */
function LineRow({
  line,
  value,
  max,
  whole,
  segments,
  label,
  title,
}: {
  line: Line;
  value: number;
  max: number;
  whole: number;
  segments: { key: string; value: number; color: string }[];
  label?: string;
  title?: string;
}) {
  return (
    <li className={`nidm-line ${line.kind}`} title={title ?? bytesTitle(line)}>
      <Address line={line} label={label} />
      <Network line={line} />
      <span className="nidm-track" aria-hidden="true">
        <span className="nidm-meter" style={{ width: `${max > 0 ? Math.max((value / max) * 100, 0.6) : 0}%` }}>
          {segments
            .filter(segment => segment.value > 0)
            .map(segment => (
              <i key={segment.key} style={{ flexGrow: segment.value, background: segment.color }} />
            ))}
        </span>
      </span>
      <span className="nidm-num">{bytes(value)}</span>
      <span className="nidm-pct">{shareOf(value, whole)}</span>
    </li>
  );
}

function MoreRow({
  count,
  value,
  open,
  onToggle,
}: {
  count: number;
  value: number;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <li className="nidm-more-row">
      <button type="button" className="nidm-more" aria-expanded={open} onClick={onToggle}>
        {open ? '收起' : `其余 ${count} 个来源 · ${bytes(value)}`}
        <Icon of="unfold" size={12} />
      </button>
    </li>
  );
}

/* ══ A · 按接入链分组 ══ */

const FOLD_A = 5;
function GroupedBody({ model, metric }: { model: Model; metric: Metric }) {
  const groups = [...model.groups].sort(byMetric(metric));
  const whole = valueOf(model, metric);
  const max = Math.max(0, ...model.groups.flatMap(group => group.lines.map(line => valueOf(line, metric))));
  return (
    <ol className="nidm-groups">
      {groups.map(group => (
        <GroupBlock key={group.key} group={group} metric={metric} max={max} whole={whole} />
      ))}
    </ol>
  );
}
function GroupBlock({ group, metric, max, whole }: { group: Group; metric: Metric; max: number; whole: number }) {
  const [open, setOpen] = useState(false);
  // Only addresses fold; the part observation could not split always stays visible, last.
  const ips = group.lines.filter(line => line.kind === 'ip').sort(byMetric(metric));
  const others = group.lines.filter(line => line.kind !== 'ip');
  const fold = ips.length > FOLD_A + 1 ? FOLD_A : ips.length;
  const hidden = ips.slice(fold);
  const value = valueOf(group, metric);
  const row = (line: Line) => (
    <LineRow
      key={line.key}
      line={line}
      value={valueOf(line, metric)}
      max={max}
      whole={whole}
      segments={[{ key: group.key, value: valueOf(line, metric), color: lineColor(line, group) }]}
    />
  );
  return (
    <li className="nidm-group">
      <div className="nidm-group-head" title={bytesTitle(group, group.name)}>
        <GroupName group={group} />
        {group.kind === 'chain' && (
          <span className="nidm-group-count">{group.lines.filter(line => line.kind === 'ip').length} 个来源</span>
        )}
        <span className="nidm-num">{bytes(value)}</span>
        <span className="nidm-pct">{shareOf(value, whole)}</span>
      </div>
      <ol className="nidm-lines">
        {(open ? ips : ips.slice(0, fold)).map(row)}
        {hidden.length > 0 && (
          <MoreRow
            count={hidden.length}
            value={hidden.reduce((sum, line) => sum + valueOf(line, metric), 0)}
            open={open}
            onToggle={() => setOpen(!open)}
          />
        )}
        {others.map(row)}
      </ol>
    </li>
  );
}

/* ══ B · 来源排行 ══ */

const FOLD_B = 10;
function RankBody({ model, metric }: { model: Model; metric: Metric }) {
  const [focus, setFocus] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const groups = [...model.groups].sort(byMetric(metric));
  const groupOf = new Map(model.groups.map(group => [group.key, group]));
  const whole = valueOf(model, metric);
  const focused = focus ? groupOf.get(focus) : undefined;
  // With a chain in focus every row counts only its bytes on that chain.
  const rows = model.sources
    .map(source => {
      const parts = [...source.parts].filter(([key]) => !focus || key === focus);
      const value = parts.reduce((sum, [, part]) => sum + valueOf(part, metric), 0);
      return { source, parts, value, key: source.key };
    })
    .filter(row => row.parts.length > 0)
    .sort((a, b) => b.value - a.value || a.key.localeCompare(b.key));
  // Only addresses fold; unrecorded sources, forwarding and unattributed rows always stay visible, last.
  const ips = rows.filter(row => row.source.kind === 'ip');
  const others = rows.filter(row => row.source.kind !== 'ip');
  const fold = ips.length > FOLD_B + 1 ? FOLD_B : ips.length;
  const hidden = ips.slice(fold);
  const max = Math.max(0, ...rows.map(row => row.value));
  const scope = focused ? valueOf(focused, metric) : whole;
  const line = ({ source, parts, value }: (typeof rows)[number]) => (
    <LineRow
      key={source.key}
      line={source}
      value={value}
      max={max}
      whole={scope}
      label={
        source.kind === 'forwarding'
          ? '端口转发 · 不区分来源'
          : source.kind === 'unattributed'
            ? '未按接入链记录'
            : undefined
      }
      title={bytesTitle(
        source,
        parts.length > 1
          ? parts.map(([key, part]) => `${groupOf.get(key)!.name} ${bytes(valueOf(part, metric))}`).join(' · ')
          : undefined,
      )}
      segments={parts.map(([key, part]) => ({
        key,
        value: valueOf(part, metric),
        color: lineColor(source, groupOf.get(key)!),
      }))}
    />
  );
  return (
    <>
      {/* A single chain needs no composition: the readout already holds its total. */}
      {groups.length > 1 && (
        <>
          <div className="nidm-compose" aria-hidden="true">
            {groups
              .filter(group => valueOf(group, metric) > 0)
              .map(group => (
                <i
                  key={group.key}
                  className={focus === group.key ? 'on' : ''}
                  style={{ flexGrow: valueOf(group, metric), background: group.color }}
                />
              ))}
          </div>
          <div className="nti-legend nidm-legend" role="group" aria-label="接入链（点击只看这条链的来源）">
            {groups.map(group => (
              <button
                key={group.key}
                type="button"
                className={`nti-key${focus === group.key ? ' on' : ''}`}
                aria-pressed={focus === group.key}
                onClick={() => {
                  setFocus(focus === group.key ? null : group.key);
                  setOpen(false);
                }}
              >
                <i className="nt-swatch" style={{ background: group.color }} aria-hidden="true" />
                <GroupName group={group} />
                <b>{bytes(valueOf(group, metric))}</b>
                <span className="nti-share">{shareOf(valueOf(group, metric), whole)}</span>
              </button>
            ))}
          </div>
        </>
      )}
      <ol className="nidm-lines flat">
        {(open ? ips : ips.slice(0, fold)).map(line)}
        {hidden.length > 0 && (
          <MoreRow
            count={hidden.length}
            value={hidden.reduce((sum, row) => sum + row.value, 0)}
            open={open}
            onToggle={() => setOpen(!open)}
          />
        )}
        {others.map(line)}
      </ol>
    </>
  );
}

/* ══ C · 流向图：来源 → 接入链，窄宽度回到 B ══ */

const FLOW_LEFT = 8;
const FLOW_MIN_WIDTH = 640;
function shortAddress(ip: string) {
  const groups = ip.split(':');
  if (groups.length <= 5) return ip;
  return `${groups.slice(0, 4).join(':')}:…:${groups[groups.length - 1]}`;
}
function FlowBody({ model, metric, onMetric }: { model: Model; metric: Metric; onMetric: (next: Metric) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [hot, setHot] = useState<{ source: string | null; group: string | null } | null>(null);
  const [all, setAll] = useState(false);
  useLayoutEffect(() => {
    const element = box.current;
    if (!element) return;
    const measure = () => setWidth(element.getBoundingClientRect().width);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const groups = [...model.groups].sort(byMetric(metric)).filter(group => valueOf(group, metric) > 0);
  const visible = [...model.sources].filter(source => valueOf(source, metric) > 0).sort(byMetric(metric));
  // Only addresses fold into 「其余」; parts without an address keep their own row at the bottom.
  const ranked = visible.filter(source => source.kind === 'ip');
  const extras = visible.filter(source => source.kind !== 'ip');
  const head = all || ranked.length <= FLOW_LEFT + 1 ? ranked : ranked.slice(0, FLOW_LEFT);
  const tail = ranked.slice(head.length);
  const restLine: Line | null =
    tail.length > 0
      ? tail.reduce<Line>(
          (rest, line) => {
            rest.down += line.down;
            rest.up += line.up;
            for (const [key, part] of line.parts) {
              const prior = rest.parts.get(key) ?? { down: 0, up: 0 };
              rest.parts.set(key, { down: prior.down + part.down, up: prior.up + part.up });
            }
            return rest;
          },
          { key: 'rest', kind: 'ip', ip: null, down: 0, up: 0, gap: false, parts: new Map() },
        )
      : null;
  const left = [...head, ...(restLine ? [restLine] : []), ...extras];
  const whole = valueOf(model, metric);
  const labelLeft = 316,
    labelRight = 236,
    node = 8,
    gap = 8;
  const plot = Math.max(120, width - labelLeft - labelRight);
  const height = Math.max(180, Math.max(left.length, groups.length) * 34);
  const usable = height - gap * (Math.max(left.length, groups.length) - 1);
  const scale = whole > 0 ? (usable * 0.8) / whole : 0;
  const slot = (value: number) => Math.max(value * scale, 22);
  const place = (values: number[]) => {
    const slots = values.map(slot);
    const sum = slots.reduce((a, b) => a + b, 0) + gap * (slots.length - 1);
    let cursor = (height - sum) / 2;
    return values.map((value, i) => {
      const h = value * scale;
      const out = { y: cursor + (slots[i] - h) / 2, h };
      cursor += slots[i] + gap;
      return out;
    });
  };
  const lp = place(left.map(line => valueOf(line, metric)));
  const rp = place(groups.map(group => valueOf(group, metric)));
  const usedLeft = left.map(() => 0),
    usedRight = groups.map(() => 0),
    mid = plot / 2;
  const links = left.flatMap((line, i) =>
    groups.flatMap((group, j) => {
      const part = line.parts.get(group.key);
      const value = part ? valueOf(part, metric) : 0;
      if (value <= 0) return [];
      const h = value * scale,
        y0 = lp[i].y + usedLeft[i],
        y1 = rp[j].y + usedRight[j];
      usedLeft[i] += h;
      usedRight[j] += h;
      return [
        {
          line,
          group,
          value,
          d: `M${node} ${y0}C${mid} ${y0} ${mid} ${y1} ${plot - node} ${y1}L${plot - node} ${y1 + h}C${mid} ${y1 + h} ${mid} ${y0 + h} ${node} ${y0 + h}Z`,
        },
      ];
    }),
  );
  const state = (source: string | null, group: string | null) =>
    !hot
      ? ''
      : (hot.source === null || hot.source === source) && (hot.group === null || hot.group === group)
        ? ' on'
        : ' off';
  const groupOf = new Map(model.groups.map(group => [group.key, group]));
  const narrow = width > 0 && width < FLOW_MIN_WIDTH;
  return (
    <div ref={box}>
      <ListBar title={narrow ? '来源' : '来源 → 接入链'} metric={metric} onMetric={onMetric} />
      {narrow ? (
        <RankBody model={model} metric={metric} />
      ) : (
        <>
          <div className="nidm-flow" style={{ height }} onMouseLeave={() => setHot(null)}>
            <div className="nidm-flow-labels left" style={{ width: labelLeft }}>
              {left.map((line, i) => {
                const network = networkOf(line.country, line.operator);
                return (
                  <span
                    key={line.key}
                    className={`nidm-flow-label left${state(line.key, null)}`}
                    style={{ top: lp[i].y + lp[i].h / 2 }}
                    title={bytesTitle(line, line.ip ?? undefined)}
                    onMouseEnter={() => setHot({ source: line.key, group: null })}
                  >
                    {line.kind === 'ip' && line.ip ? (
                      <>
                        {network.code ? <RegionFlag code={network.code} /> : <i className="nidm-flag-none" />}
                        <code>{shortAddress(line.ip)}</code>
                        <CopyButton
                          className="user-fcopy"
                          text={line.ip}
                          label={`复制来源 IP ${line.ip}`}
                          successLabel="来源 IP 已复制"
                          failureLabel="来源 IP 复制失败"
                          iconOnly
                          onFocus={() => setHot({ source: line.key, group: null })}
                        />
                      </>
                    ) : line.key === 'rest' ? (
                      <button type="button" className="nidm-flow-rest" onClick={() => setAll(true)}>
                        其余 {tail.length} 个来源
                      </button>
                    ) : (
                      <span className="nidm-unknown">
                        {line.kind === 'forwarding'
                          ? '端口转发'
                          : line.kind === 'unattributed'
                            ? '未按接入链记录'
                            : '未记录来源'}
                      </span>
                    )}
                    <b>{bytes(valueOf(line, metric))}</b>
                  </span>
                );
              })}
            </div>
            <svg
              className="nidm-flow-svg"
              width={plot}
              height={height}
              style={{ left: labelLeft }}
              role="img"
              aria-label="来源到接入链的流向，带宽与流量成正比"
            >
              {links.map(({ line, group, value, d }) => (
                <path
                  key={`${line.key}/${group.key}`}
                  className={`nidm-flow-link${state(line.key, group.key)}`}
                  d={d}
                  style={{ fill: lineColor(line, group) }}
                  onMouseEnter={() => setHot({ source: line.key, group: group.key })}
                >
                  <title>{`${line.ip ?? '未记录来源'} → ${group.name} · ${bytes(value)}`}</title>
                </path>
              ))}
              {left.map((line, i) => (
                <rect
                  key={line.key}
                  className={`nidm-flow-node${state(line.key, null)}`}
                  x={0}
                  y={lp[i].y}
                  width={node}
                  height={Math.max(lp[i].h, 1)}
                  rx={2}
                />
              ))}
              {groups.map((group, i) => (
                <rect
                  key={group.key}
                  className={`nidm-flow-node${state(null, group.key)}`}
                  x={plot - node}
                  y={rp[i].y}
                  width={node}
                  height={Math.max(rp[i].h, 1)}
                  rx={2}
                  style={{ fill: group.color }}
                />
              ))}
            </svg>
            <div className="nidm-flow-labels right" style={{ width: labelRight }}>
              {groups.map((group, i) => (
                <button
                  type="button"
                  key={group.key}
                  className={`nidm-flow-label right${state(null, group.key)}`}
                  style={{ top: rp[i].y + rp[i].h / 2 }}
                  title={bytesTitle(group, group.name)}
                  onMouseEnter={() => setHot({ source: null, group: group.key })}
                  onFocus={() => setHot({ source: null, group: group.key })}
                  onBlur={() => setHot(null)}
                  onClick={() =>
                    setHot(hot?.group === group.key && hot.source === null ? null : { source: null, group: group.key })
                  }
                >
                  <GroupName group={groupOf.get(group.key)!} />
                  <b>{bytes(valueOf(group, metric))}</b>
                  <span className="nti-share">{shareOf(valueOf(group, metric), whole)}</span>
                </button>
              ))}
            </div>
          </div>
          {all && ranked.length > FLOW_LEFT + 1 && (
            <button type="button" className="btn sm nidm-flow-fold" onClick={() => setAll(false)}>
              收起来源
            </button>
          )}
        </>
      )}
    </div>
  );
}

/* ══ 弹窗 ══ */

export function InflowDetailsMockup({
  option,
  nodeId,
  nodeName,
  tenantId,
  userId,
  period,
  range,
  sourceOf,
  colorOf,
  onClose,
}: {
  option: Option;
  nodeId: string;
  nodeName: string;
  tenantId: string;
  userId: string;
  period: SiteUsagePeriod;
  range?: TrafficTimeRange;
  sourceOf: (key: string) => TrafficSource;
  colorOf: (chainId: string) => string;
  onClose: () => void;
}) {
  const titleId = useId();
  const [metric, setMetric] = useState<Metric>('total');
  const query = useMetricQuery({
    queryKey: ['node-inflow-details', nodeId, tenantId, userId, period, range],
    queryFn: () => fetchNodeInflowDetails(nodeId, tenantId, userId, period, range) as Promise<Located>,
    staleTime: 15_000,
    refetchInterval: 30_000,
    metricFacts: data => [data.limited, data.accounting_mismatch],
  });
  const model = useMemo(
    () => (query.data ? modelOf(query.data, sourceOf, colorOf) : null),
    [query.data, sourceOf, colorOf],
  );
  return (
    <DialogLayer label={`${userId} · 流入明细`} onClose={onClose}>
      <section className={`dialog-surface nidm-dialog option-${option}`} aria-labelledby={titleId}>
        <header className="node-usage-header nidm-head">
          <div className="nidm-who">
            <GeneratedUserAvatar id={userId} variant="mini" />
            <h2 id={titleId}>{userId}</h2>
            <span className="nidm-where">{nodeName} · 流入明细</span>
          </div>
          <DialogClose className="btn sm node-usage-close" aria-label="关闭流入明细">
            <Icon of="close" size={16} />
          </DialogClose>
        </header>
        <div className="nidm-body" aria-busy={query.isFetching}>
          {query.isPending && <PanelLoading label="加载来源流量明细" />}
          {query.isError && (
            <p role="alert" className="nidm-state">
              {query.data ? '明细刷新失败，当前显示上次读取的数据。' : '明细读取失败。'}
              <button type="button" className="btn text sm" onClick={() => query.refetch()}>
                重试
              </button>
            </p>
          )}
          {/* Nothing recorded: one line with the range, no readout of zeros. */}
          {query.data && model && model.groups.length === 0 && (
            <p className="nidm-state">
              {rangeLabel(period, range, query.data.range_start_unix_secs, query.data.range_end_unix_secs)}
              ，没有已记录的流入。
            </p>
          )}
          {query.data && model && model.groups.length > 0 && (
            <>
              <Readout
                model={model}
                label={rangeLabel(period, range, query.data.range_start_unix_secs, query.data.range_end_unix_secs)}
              />
              <Notices data={query.data} model={model} />
              <div className="nidm-list">
                {option !== 'c' && (
                  <ListBar title={option === 'a' ? '按接入链' : '来源'} metric={metric} onMetric={setMetric} />
                )}
                {option === 'a' && <GroupedBody model={model} metric={metric} />}
                {option === 'b' && <RankBody model={model} metric={metric} />}
                {option === 'c' && <FlowBody model={model} metric={metric} onMetric={setMetric} />}
              </div>
            </>
          )}
        </div>
      </section>
    </DialogLayer>
  );
}
