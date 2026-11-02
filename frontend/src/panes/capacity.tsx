// 管理员的机器流量预测工作面。物理网卡流量是容量硬边界；用户贡献由当前授权和
// 链路拓扑估算，因此与面向用户的自然月用量台账保持独立。
//
// 机器按风险分组排列。状态看预计期末，不只看 P90：本期已用达到配额为「已超额」，
// 预计期末 P50 达到配额为「预计超额」，只有 P90 达到配额为「可能超额」。本期总量缺了只有
// 校准才能补上的部分时，不判「余量充足」，改为「待校准」。
// 计算公式不常驻页面，放在对应读数的悬停说明里。

import { Fragment, useEffect, useId, useRef, useState, type MouseEvent, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchMachineTrafficModel, saveNodeTraffic, type MachineTrafficModelNode } from '../api';
import { navigate } from '../forge/route';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { ListIcon } from '../ui/icons';
import { useNodeCountries } from '../ui/node-name';
import { RegionFlag } from '../ui/region-flag';
import { trafficGapReason } from '../ui/traffic-gap';
import { GeneratedUserAvatar } from '../ui/user-avatar';
import {
  MACHINE_QUOTA_UNITS,
  machineQuotaBytes,
  machineQuotaInput,
  type MachineQuotaUnit,
} from '../ui/machine-traffic-quota';

const DAY = 86_400;
const KiB = 1024;
const MiB = 1024 ** 2;
const GiB = 1024 ** 3;
const TiB = 1024 ** 4;

/* ══ 读数格式 ══ */

/** 三位有效数字：825 GiB、23.5 GiB、1.18 TiB。接近 1000 GiB 时改用 TiB，不出现「1013 GiB」。 */
function volume(n: number): string {
  if (n >= 999.5 * GiB) return `${(n / TiB).toFixed(2)} TiB`;
  if (n >= 99.95 * GiB) return `${(n / GiB).toFixed(0)} GiB`;
  if (n >= 9.995 * GiB) return `${(n / GiB).toFixed(1)} GiB`;
  if (n >= 999.5 * MiB) return `${(n / GiB).toFixed(2)} GiB`;
  if (n >= MiB) return `${(n / MiB).toFixed(0)} MiB`;
  if (n >= KiB) return `${(n / KiB).toFixed(0)} KiB`;
  return `${n.toFixed(0)} B`;
}

/** 能还原成 GiB/TiB 录入值的配额沿用其单位，其余按流量数量级显示。 */
function quotaText(quota: string): string {
  const { amount, unit } = machineQuotaInput(quota);
  return machineQuotaBytes(amount, unit) === quota ? `${amount} ${unit}` : volume(Number(quota));
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** 周期边界是 UTC 零点，日期按 UTC 显示，与机器详情「流量统计」一致。 */
function monthDay(secs: number): string {
  const date = new Date(secs * 1000);
  return `${pad2(date.getUTCMonth() + 1)}-${pad2(date.getUTCDate())}`;
}

const percent = (ratio: number) => `${Math.round(ratio * 100)}%`;

/** 预测偏差：10% 以下保留一位小数。 */
const accuracyError = (basisPoints: number) => `${(basisPoints / 100).toFixed(basisPoints < 1000 ? 1 : 0)}%`;

function resetText(node: MachineTrafficModelNode): string {
  return node.cycle_kind === 'yearly'
    ? `每年 ${node.reset_month} 月 ${node.reset_day} 日重置`
    : `每月 ${node.reset_day} 日重置`;
}

/* ══ 机器状态 ══ */

type CapacityState = 'over' | 'risk' | 'maybe' | 'uncalibrated' | 'ok' | 'unset' | 'unmeasured';

const STATE_RANK: Record<CapacityState, number> = {
  over: 0,
  risk: 1,
  maybe: 2,
  uncalibrated: 3,
  ok: 4,
  unset: 5,
  unmeasured: 6,
};

const CONFIDENCE_LABEL = { high: '高', medium: '中', low: '低' } as const;

interface Machine {
  node: MachineTrafficModelNode;
  quota: number | null;
  used: number;
  p50: number;
  p90: number;
  elapsedDays: number;
  totalDays: number;
  /** 距期末的秒数 */
  left: number;
  state: CapacityState;
  /** 预计期末占配额的比例 */
  ratio: number | null;
  /** 本期累计：[距期初的天数, 累计字节] */
  series: [number, number][];
  /** 预计超额：从现在起按预计期末的速度线性推进，到达配额的时刻 */
  exhaustAt: number | null;
  /** 已超额：本期累计越过配额的那一天（距期初的天数） */
  overDay: number | null;
}

/** 账本只有 UTC 日粒度：每天的字节记在当日结束处，今天记在现在。记账起点晚于期初时从起点画起。
 *  本期校准过时，总量从校准那天起以校准值为准，差额并入校准日，终点与 used_bytes 一致。 */
export function cumulativeSeries(
  node: Pick<
    MachineTrafficModelNode,
    'period_start_unix_secs' | 'tracking_started_at_unix_secs' | 'calibrated_at_unix_secs' | 'days'
  >,
  used: number,
  elapsedDays: number,
): [number, number][] {
  const start = node.period_start_unix_secs;
  const perDay = new Map<number, number>();
  let ledger = 0;
  for (const day of node.days) {
    const bytes = Number(day.bytes);
    perDay.set(Math.round((day.day_start_unix_secs - start) / DAY), bytes);
    ledger += bytes;
  }
  const dayIndexes = [...perDay.keys()];
  const tracked = node.tracking_started_at_unix_secs;
  const trackedX = tracked !== null && tracked > start ? Math.min((tracked - start) / DAY, elapsedDays) : 0;
  // 账本日早于记账起点那天或晚于今天（控制面与数据库时钟不一致）时，仍把这些字节画进来。
  const earliest = Math.min(...dayIndexes);
  const firstX = earliest < Math.floor(trackedX) ? earliest : trackedX;
  const firstIndex = Math.floor(firstX);
  const lastIndex = Math.max(Math.ceil(elapsedDays) - 1, firstIndex, ...dayIndexes);
  const calibrated = node.calibrated_at_unix_secs;
  const calibrationIndex =
    calibrated === null ? null : Math.min(Math.max(Math.floor((calibrated - start) / DAY), firstIndex), lastIndex);
  const points: [number, number][] = [[firstX, 0]];
  let total = 0;
  for (let index = firstIndex; index <= lastIndex; index += 1) {
    total += perDay.get(index) ?? 0;
    if (index === calibrationIndex) total += used - ledger;
    points.push([Math.min(index + 1, elapsedDays), total]);
  }
  return points;
}

/** 上一窗口与本期等长，按同样的天数对齐到本期横轴。 */
function previousSeries(node: MachineTrafficModelNode): [number, number][] {
  if (node.previous_days.length === 0) return [];
  const from = node.previous_period_start_unix_secs;
  const perDay = new Map(
    node.previous_days.map((day): [number, number] => [
      Math.round((day.day_start_unix_secs - from) / DAY),
      Number(day.bytes),
    ]),
  );
  const days = Math.round((node.period_start_unix_secs - from) / DAY);
  const points: [number, number][] = [[0, 0]];
  let total = 0;
  for (let index = 0; index < days; index += 1) {
    total += perDay.get(index) ?? 0;
    points.push([index + 1, total]);
  }
  return points;
}

function crossingDay(series: [number, number][], quota: number): number | null {
  for (let index = 1; index < series.length; index += 1) {
    const [x0, v0] = series[index - 1];
    const [x1, v1] = series[index];
    if (v1 >= quota && v0 < quota) return x0 + ((quota - v0) / (v1 - v0)) * (x1 - x0);
  }
  return null;
}

function machineOf(node: MachineTrafficModelNode, now: number): Machine {
  const quota = node.quota_bytes === null ? null : Number(node.quota_bytes);
  const used = Number(node.used_bytes);
  const p50 = Number(node.projected_p50_bytes);
  const p90 = Number(node.projected_p90_bytes);
  const start = node.period_start_unix_secs;
  const end = node.period_end_unix_secs;
  const elapsed = Math.min(Math.max(now - start, 0), end - start);
  const left = Math.max(end - now, 0);
  // 与机器详情的判断相同：既没有记账起点也没有校准，说明 Agent 还没有上报过网卡累计。
  const measured = node.tracking_started_at_unix_secs !== null || node.calibrated_at_unix_secs !== null;
  // 总量不完整时预计只会偏低：仍可据此判超额，但不能据此判余量充足。
  const state: CapacityState = !measured
    ? 'unmeasured'
    : quota === null
      ? 'unset'
      : used >= quota
        ? 'over'
        : p50 >= quota
          ? 'risk'
          : p90 >= quota
            ? 'maybe'
            : node.needs_calibration
              ? 'uncalibrated'
              : 'ok';
  const series = cumulativeSeries(node, used, elapsed / DAY);
  return {
    node,
    quota,
    used,
    p50,
    p90,
    elapsedDays: elapsed / DAY,
    totalDays: (end - start) / DAY,
    left,
    state,
    ratio: quota === null ? null : p50 / quota,
    series,
    exhaustAt: state === 'risk' && quota !== null && p50 > used ? now + ((quota - used) / (p50 - used)) * left : null,
    overDay: state === 'over' && quota !== null ? crossingDay(series, quota) : null,
  };
}

const nameOf = (node: MachineTrafficModelNode) => node.name || node.node_id;

const byName = (a: Machine, b: Machine) =>
  nameOf(a.node).localeCompare(nameOf(b.node), 'zh-CN') || a.node.node_id.localeCompare(b.node.node_id);

const needsAttention = (machine: Machine) =>
  machine.state === 'over' || machine.state === 'risk' || machine.state === 'maybe' || machine.state === 'uncalibrated';

interface MachineGroup {
  key: 'attention' | 'ok' | 'unset' | 'unmeasured';
  label: string;
  machines: Machine[];
}

function groupsOf(machines: Machine[]): MachineGroup[] {
  const byRisk = (a: Machine, b: Machine) =>
    STATE_RANK[a.state] - STATE_RANK[b.state] || (b.ratio ?? 0) - (a.ratio ?? 0) || byName(a, b);
  const groups: MachineGroup[] = [
    { key: 'attention', label: '需关注', machines: machines.filter(needsAttention).sort(byRisk) },
    { key: 'ok', label: '余量充足', machines: machines.filter(m => m.state === 'ok').sort(byRisk) },
    {
      key: 'unset',
      label: '未设配额',
      machines: machines.filter(m => m.state === 'unset').sort((a, b) => b.p50 - a.p50 || byName(a, b)),
    },
    { key: 'unmeasured', label: '未上报', machines: machines.filter(m => m.state === 'unmeasured').sort(byName) },
  ];
  return groups.filter(group => group.machines.length > 0);
}

/** 默认展开风险最高的一台，打开页面即看到它的走势；没有需关注的机器时全部收起。 */
function defaultOpen(groups: MachineGroup[]): ReadonlySet<string> {
  const first = groups[0]?.machines[0];
  return new Set(first && needsAttention(first) ? [first.node.node_id] : []);
}

function cycleText(machine: Machine): string {
  const left =
    machine.left < DAY
      ? `剩 ${Math.max(1, Math.round(machine.left / 3600))} 小时`
      : `剩 ${Math.round(machine.left / DAY)} 天`;
  return `${left} · ${resetText(machine.node)}`;
}

/** 行尾结论。只有超额与可能超额着色，余量充足为中性色。 */
function verdictOf(machine: Machine): { tone?: 'bad' | 'warn'; main: string; sub?: string } | null {
  const quota = machine.quota ?? 0;
  switch (machine.state) {
    case 'over':
      return {
        tone: 'bad',
        main: `已超出 ${volume(machine.used - quota)}`,
        sub:
          machine.overDay === null
            ? undefined
            : `${monthDay(machine.node.period_start_unix_secs + machine.overDay * DAY)} 越过配额`,
      };
    case 'risk':
      return {
        tone: 'bad',
        main: machine.exhaustAt === null ? '预计超额' : `预计 ${monthDay(machine.exhaustAt)} 用尽`,
        sub: `期末超出 ${volume(machine.p50 - quota)}`,
      };
    case 'maybe':
      return { tone: 'warn', main: '可能超额', sub: `P90 超出 ${volume(machine.p90 - quota)}` };
    case 'uncalibrated': {
      const node = machine.node;
      return {
        tone: 'warn',
        main: '待校准',
        sub: lateStart(node)
          ? `${monthDay(node.tracking_started_at_unix_secs ?? 0)} 起才有记录`
          : node.gap_reason !== null && node.gap_at_unix_secs !== null
            ? `${monthDay(node.gap_at_unix_secs)} ${trafficGapReason(node.gap_reason)}`
            : undefined,
      };
    }
    case 'ok':
      return { main: `期末余 ${volume(quota - machine.p50)}` };
    case 'unmeasured':
      return { main: '未上报', sub: 'Agent 尚未上报网卡累计' };
    case 'unset':
      return null;
  }
}

/** 记账起点晚于期初与本期校准两者中较晚的那个：之前的流量没有计入，只有校准能补上。 */
function lateStart(node: MachineTrafficModelNode): boolean {
  const tracked = node.tracking_started_at_unix_secs;
  return tracked !== null && tracked > Math.max(node.period_start_unix_secs, node.calibrated_at_unix_secs ?? 0);
}

/** 跨日失联不丢字节，只可能把上期的流量算进本期；其余缺口是前后的流量可能没有计入。 */
const gapEffect = (reason: string) => (reason === 'report-gap' ? '可能计入了上期流量' : '可能有流量未计入');

/** P90 相对 P50 剩余部分放大的比例。 */
const upperError = (node: MachineTrafficModelNode) => `${Math.round(node.band.upper_error_basis_points / 100)}%`;

function bandReason(node: MachineTrafficModelNode): string {
  switch (node.band.basis) {
    case 'node':
      return '误差按本机历史';
    case 'fleet':
      return '误差按全部机器的历史';
    case 'default':
      return `历史不足，误差按 +${upperError(node)} 估计`;
  }
}

function confidenceReasons(machine: Machine): string {
  const node = machine.node;
  const reasons: string[] = [];
  const tracked = node.tracking_started_at_unix_secs;
  if (tracked !== null && tracked > node.period_start_unix_secs) {
    reasons.push(lateStart(node) ? `${monthDay(tracked)} 起记账，此前的流量未计入` : `${monthDay(tracked)} 起记账`);
  }
  if (node.calibrated_at_unix_secs !== null) reasons.push(`${monthDay(node.calibrated_at_unix_secs)} 校准`);
  if (node.gap_reason !== null && node.gap_at_unix_secs !== null) {
    reasons.push(
      `${monthDay(node.gap_at_unix_secs)} ${trafficGapReason(node.gap_reason)}，${gapEffect(node.gap_reason)}`,
    );
  }
  reasons.push(node.rate_days > 0 ? `近 28 天中 ${node.rate_days} 天记录完整` : '记录不满一天');
  reasons.push(bandReason(node));
  return reasons.join(' · ');
}

/* ══ 页面 ══ */

export function CapacityPane({ canEdit = false }: { canEdit?: boolean }) {
  const model = useQuery({
    queryKey: ['usage-machine-model'],
    queryFn: fetchMachineTrafficModel,
    retry: false,
  });
  const countryOf = useNodeCountries();
  // null 表示用户还没有展开或收起过：此时按当前数据取默认展开项。
  const [open, setOpen] = useState<ReadonlySet<string> | null>(null);
  const view = model.data;
  const machines = view ? view.nodes.map(node => machineOf(node, view.generated_at_unix_secs)) : [];
  const groups = groupsOf(machines);
  const openIds = open ?? defaultOpen(groups);
  const setOpenFor = (nodeId: string, expanded: boolean) => {
    const next = new Set(openIds);
    if (expanded) next.add(nodeId);
    else next.delete(nodeId);
    setOpen(next);
  };
  const withQuota = groups.some(group => group.key === 'attention' || group.key === 'ok');
  const unsetScale = Math.max(1, ...machines.filter(machine => machine.state === 'unset').map(machine => machine.p90));

  return (
    <div className="cardpage capacity-page">
      <section className="panel titled cap-panel" data-page-title="true" aria-busy={model.isFetching || undefined}>
        <header>
          <ListIcon of="forecast" />
          <h4>流量预测</h4>
          <span className="sp" />
          {machines.length > 0 && <Readout machines={machines} />}
        </header>
        {model.isPending && <Loading variant="panel" />}
        {model.error && <ErrorBox error={model.error} />}
        {view &&
          (machines.length === 0 ? (
            <Empty>还没有机器流量数据。</Empty>
          ) : (
            <div className="cap-body">
              {groups.map((group, index) => (
                <Fragment key={group.key}>
                  <div className="cap-group">
                    <h5>{group.label}</h5>
                    <small>{group.machines.length}</small>
                    {index === 0 && <RunwayLegend withQuota={withQuota} />}
                  </div>
                  <div role="list" aria-label={group.label}>
                    {group.machines.map(machine => (
                      <MachineRow
                        key={machine.node.node_id}
                        machine={machine}
                        country={countryOf(machine.node.node_id)}
                        open={openIds.has(machine.node.node_id)}
                        onOpenChange={expanded => setOpenFor(machine.node.node_id, expanded)}
                        scale={unsetScale}
                        canEdit={canEdit}
                      />
                    ))}
                  </div>
                </Fragment>
              ))}
            </div>
          ))}
      </section>
    </div>
  );
}

/** 标题栏读数：台数为中性色，只有超额、可能超额与待校准着色。 */
function Readout({ machines }: { machines: Machine[] }) {
  const count = (state: CapacityState) => machines.filter(machine => machine.state === state).length;
  const items = [
    { key: 'over', value: count('over'), label: '已超额', tone: 'bad' },
    { key: 'risk', value: count('risk'), label: '预计超额', tone: 'bad' },
    { key: 'maybe', value: count('maybe'), label: '可能超额', tone: 'warn' },
    { key: 'uncalibrated', value: count('uncalibrated'), label: '待校准', tone: 'warn' },
    { key: 'unset', value: count('unset'), label: '未设配额' },
    { key: 'unmeasured', value: count('unmeasured'), label: '未上报' },
  ].filter(item => item.value > 0);
  return (
    <span className="cap-rd">
      <span className="opt">
        <b>{machines.length}</b> 台
      </span>
      {items.map(item => (
        <Fragment key={item.key}>
          <span className="dot" aria-hidden="true">
            ·
          </span>
          <span>
            <b className={item.tone}>{item.value}</b> {item.label}
          </span>
        </Fragment>
      ))}
    </span>
  );
}

function RunwayLegend({ withQuota }: { withQuota: boolean }) {
  return (
    <span className="cap-legend">
      <span>
        <i className="u" />
        本期已用
      </span>
      <span>
        <i className="p" />
        预计期末 P50
      </span>
      <span>
        <i className="w" />
        P90
      </span>
      {withQuota && (
        <span>
          <i className="q" />
          配额
        </span>
      )}
    </span>
  );
}

/* ══ 列表行 ══ */

function MachineRow({
  machine,
  country,
  open,
  onOpenChange,
  scale,
  canEdit,
}: {
  machine: Machine;
  country: string | null;
  open: boolean;
  onOpenChange: (expanded: boolean) => void;
  scale: number;
  canEdit: boolean;
}) {
  const detailId = useId();
  const [editing, setEditing] = useState(false);
  const node = machine.node;
  const measured = machine.state !== 'unmeasured';
  const verdict = verdictOf(machine);
  const toggle = () => {
    if (open) setEditing(false);
    onOpenChange(!open);
  };
  const startEditing = () => {
    setEditing(true);
    onOpenChange(true);
  };
  // 整行可点；行内另有按钮时由按钮自己处理。键盘与读屏通过名称按钮展开。
  const onRowClick = (event: MouseEvent<HTMLDivElement>) => {
    if ((event.target as Element).closest('button, a, input, label')) return;
    toggle();
  };
  const projectedTone =
    machine.state === 'over' || machine.state === 'risk' ? 'bad' : machine.state === 'maybe' ? 'warn' : undefined;

  return (
    <div className={`cap-item${open ? ' is-open' : ''}`} role="listitem">
      <div className="cap-row" onClick={onRowClick}>
        <button
          type="button"
          className="cap-id"
          aria-expanded={open}
          aria-controls={open ? detailId : undefined}
          title={node.node_id}
          onClick={toggle}
        >
          <span className="cap-flag">
            <RegionFlag code={country} />
          </span>
          <b>{nameOf(node)}</b>
          <small>{cycleText(machine)}</small>
        </button>
        <Runway machine={machine} scale={scale} />
        <span className="cap-num used">
          <b>{measured ? volume(machine.used) : '—'}</b>
          <small>{node.quota_bytes === null ? '本期已用' : `配额 ${quotaText(node.quota_bytes)}`}</small>
        </span>
        <span className="cap-num proj">
          <b>{measured ? volume(machine.p50) : '—'}</b>
          {measured && (
            <small className={projectedTone}>
              {machine.ratio === null ? `P90 ${volume(machine.p90)}` : `配额的 ${percent(machine.ratio)}`}
            </small>
          )}
        </span>
        {verdict ? (
          <span className={`cap-verdict${verdict.tone ? ` ${verdict.tone}` : ''}`}>
            <b>{verdict.main}</b>
            {verdict.sub && <small>{verdict.sub}</small>}
          </span>
        ) : (
          <span className="cap-verdict">
            {canEdit && (
              <button type="button" className="cap-link" onClick={startEditing}>
                设置配额 ›
              </button>
            )}
          </span>
        )}
        <span className="cap-chev" aria-hidden="true" />
      </div>
      {open && (
        <MachineDetail
          id={detailId}
          machine={machine}
          canEdit={canEdit}
          editing={editing}
          onEditingChange={setEditing}
        />
      )}
    </div>
  );
}

/** 配额跑道：整条为 max(配额, P90)。实色为本期已用，浅色到预计期末 P50，细线延伸到 P90，竖线为配额；
 *  超出配额的部分改用错误色。未设配额的机器没有参照线，按本组最大的 P90 取长度并用中性色。 */
function Runway({ machine, scale }: { machine: Machine; scale: number }) {
  if (machine.state === 'unmeasured') {
    return (
      <span className="cap-run unset">
        <span className="cap-bar" />
      </span>
    );
  }
  const { quota, used, p50, p90 } = machine;
  const full = quota === null ? scale : Math.max(quota, p90);
  const at = (value: number) => (full > 0 ? (Math.min(Math.max(value, 0), full) / full) * 100 : 0);
  const segments: ReactNode[] = [];
  const segment = (className: string, from: number, to: number) => {
    if (to <= from) return;
    segments.push(
      <i key={className} className={className} style={{ left: `${at(from)}%`, width: `${at(to) - at(from)}%` }} />,
    );
  };
  if (quota === null) {
    segment('u', 0, used);
    segment('p', used, p50);
    segment('w', p50, p90);
  } else {
    segment('u', 0, Math.min(used, quota));
    segment('u x', quota, used);
    segment('p', used, Math.min(p50, quota));
    segment('p x', Math.max(quota, used), p50);
    segment('w', p50, Math.min(p90, quota));
    segment('w x', Math.max(quota, p50), p90);
  }
  const title = `已用 ${volume(used)} · 预计期末 ${volume(p50)}（P90 ${volume(p90)}）${
    machine.node.quota_bytes === null ? ' · 未设配额' : ` · 配额 ${quotaText(machine.node.quota_bytes)}`
  }`;
  return (
    <span className={`cap-run${quota === null ? ' unset' : ''}`} title={title}>
      <span className="cap-bar">{segments}</span>
      {quota !== null && <i className="cap-q" style={{ left: `${at(quota)}%` }} />}
    </span>
  );
}

/* ══ 展开详情：走势 + 日均与预计 + 可信度 + 用户占用 + 配额 ══ */

function MachineDetail({
  id,
  machine,
  canEdit,
  editing,
  onEditingChange,
}: {
  id: string;
  machine: Machine;
  canEdit: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  const node = machine.node;
  const previousPeriod = Number(node.previous_period_bytes);
  const totalDays = Math.round(machine.totalDays);
  const today = Math.min(Math.max(Math.ceil(machine.elapsedDays), 1), totalDays);
  return (
    <div className="cap-detail" id={id}>
      {machine.state === 'unmeasured' ? (
        <p className="cap-d-empty">Agent 尚未上报网卡累计，本期没有记账数据。</p>
      ) : (
        <>
          <div className="cap-d-main">
            <div className="cap-d-head">
              <h5>本期累计</h5>
              <span>
                {monthDay(node.period_start_unix_secs)} → {monthDay(node.period_end_unix_secs)} · 第 {today} /{' '}
                {totalDays} 天
              </span>
              <ChartLegend withPrevious={node.previous_days.length > 0} />
            </div>
            <BurnChart machine={machine} />
            <ForecastModels machine={machine} />
          </div>
          <div className="cap-d-users">
            <UserShares machine={machine} />
          </div>
        </>
      )}
      <div className="cap-d-foot">
        {editing && canEdit ? (
          <QuotaEditor node={node} onDone={() => onEditingChange(false)} />
        ) : (
          <span className="cap-quota">
            {node.quota_bytes === null ? (
              '未设配额'
            ) : (
              <>
                配额 <b>{quotaText(node.quota_bytes)}</b>
              </>
            )}
            {canEdit && (
              <button className="btn" type="button" onClick={() => onEditingChange(true)}>
                {node.quota_bytes === null ? '设置配额' : '修改'}
              </button>
            )}
          </span>
        )}
        <span>{resetText(node)}（UTC）</span>
        {previousPeriod > 0 && (
          <span>
            上期整期 <b>{volume(previousPeriod)}</b>
          </span>
        )}
        <span className="sp" />
        <button
          className="cap-link"
          type="button"
          title="重置日与校准在机器详情的「流量统计」中设置"
          onClick={() => navigate('nodes', { p: 'node', id: node.node_id })}
        >
          {machine.state === 'uncalibrated' && canEdit ? '去校准 ›' : '机器详情 ›'}
        </button>
      </div>
    </div>
  );
}

function ChartLegend({ withPrevious }: { withPrevious: boolean }) {
  return (
    <span className="cap-legend">
      <span>
        <i className="l" />
        本期累计
      </span>
      <span>
        <i className="l dash" />
        预计
      </span>
      {withPrevious && (
        <span>
          <i className="l prev" />
          上期
        </span>
      )}
    </span>
  );
}

function ForecastModels({ machine }: { machine: Machine }) {
  const node = machine.node;
  const band = node.band;
  const accuracy = node.last_period_accuracy;
  const rateTitle =
    node.rate_days > 0
      ? `最近 28 天中记录完整的 ${node.rate_days} 天加权平均，每早 4 天权重减半`
      : '还没有记录完整的一天，按开始记账以来的平均';
  const bandSource =
    band.basis === 'default'
      ? '历史不足，按默认值估计'
      : `${upperError(node)} 是${band.basis === 'node' ? '本机' : '全部机器'} ${band.samples} 次「预测之后 ${band.horizon_days} 天」的误差 90% 分位`;
  const accuracyTitle = accuracy
    ? `${monthDay(accuracy.period_start_unix_secs)} → ${monthDay(accuracy.period_end_unix_secs)} 每天记录的 ${accuracy.forecasts} 次预测：P50 与期末实际 ${volume(Number(accuracy.actual_bytes))} 平均相差 ${accuracyError(accuracy.p50_error_basis_points)}，${accuracy.p90_covered} 次实际没有超过 P90`
    : '每天记录一次预测，一个周期结束后与它的实际总量对比';
  return (
    <>
      <dl className="cap-models">
        <div title={rateTitle}>
          <dt>近期日均</dt>
          <dd>{volume(Number(node.daily_rate_bytes))}/天</dd>
        </div>
        <div title="本期已用 + 近期日均 × 剩余天数">
          <dt>预计期末 P50</dt>
          <dd>{volume(machine.p50)}</dd>
        </div>
        <div title={`本期已用 + 近期日均 × 剩余天数 × (1 + ${upperError(node)})；${bandSource}`}>
          <dt>P90</dt>
          <dd>{volume(machine.p90)}</dd>
        </div>
        <div title={accuracyTitle}>
          <dt>上期预测偏差</dt>
          <dd className={accuracy ? undefined : 'mute'}>
            {accuracy ? accuracyError(accuracy.p50_error_basis_points) : '暂无记录'}
          </dd>
        </div>
      </dl>
      <p className="cap-conf">
        可信度 <b className={node.confidence === 'low' ? 'low' : undefined}>{CONFIDENCE_LABEL[node.confidence]}</b> ·{' '}
        {confidenceReasons(machine)}
      </p>
    </>
  );
}

function UserShares({ machine }: { machine: Machine }) {
  const node = machine.node;
  const unattributed = Number(node.unattributed_bytes);
  const restShare = machine.used > 0 ? unattributed / machine.used : 0;
  const shares = node.users.map(user => user.share_basis_points / 10_000);
  const largest = Math.max(...shares, restShare, 0.0001);
  // 用户行带与用户页名册同一张身份牌；「未归因」不是用户，留出同宽的空位让名称对齐。
  const row = (
    key: string,
    className: string,
    who: string,
    sub: string,
    share: number,
    current: number,
    projected: number,
    person: boolean,
  ) => (
    <div className={`cap-user${className}`} key={key}>
      <span className="cap-who">
        {person ? (
          <GeneratedUserAvatar id={who} variant="cell" />
        ) : (
          <span className="cap-avatar-slot" aria-hidden="true" />
        )}
        <span className="cap-who-text">
          <b>{who}</b>
          <small>{sub}</small>
        </span>
      </span>
      <span className="cap-share">
        <i aria-hidden="true">
          <i style={{ width: `${(share / largest) * 100}%` }} />
        </i>
        <span>{(share * 100).toFixed(1)}%</span>
      </span>
      <span className="cap-n">{volume(current)}</span>
      <span className="cap-n">{volume(projected)}</span>
    </div>
  );
  return (
    <>
      <div className="cap-d-head">
        <h5>用户占用</h5>
        <span
          className="cap-hint"
          title="用户机器占用 =（上行 + 下行）× 2，按当前授权可经过本机的线路用量比例分摊；合计不超过本机物理流量，差额记为未归因。预计整期 = 预计期末 P50 × 占比。"
        >
          估算 · {node.users.length} 位用户
        </span>
      </div>
      <div className="cap-users">
        <div className="cap-user head">
          <span>用户</span>
          <span>占比</span>
          <span>本期</span>
          <span>预计整期</span>
        </div>
        {node.users.map((user, index) =>
          row(
            `${user.tenant_id}/${user.user_id}`,
            '',
            user.user_id,
            user.apps.join('、') || '当前线路',
            shares[index],
            Number(user.attributed_bytes),
            Number(user.projected_bytes),
            true,
          ),
        )}
        {row(
          'unattributed',
          ' rest',
          '未归因',
          '系统、非代理与拓扑外流量',
          restShare,
          unattributed,
          Number(node.projected_unattributed_bytes),
          false,
        )}
      </div>
    </>
  );
}

function QuotaEditor({ node, onDone }: { node: MachineTrafficModelNode; onDone: () => void }) {
  const qc = useQueryClient();
  const errorId = useId();
  const initial = machineQuotaInput(node.quota_bytes);
  const [draft, setDraft] = useState(initial.amount);
  const [unit, setUnit] = useState(initial.unit);
  const [attempted, setAttempted] = useState(false);
  const parsed = draft === initial.amount && unit === initial.unit ? node.quota_bytes : machineQuotaBytes(draft, unit);
  const save = useMutation({
    mutationFn: (quota: string | null) =>
      saveNodeTraffic(node.node_id, {
        cycle_kind: node.cycle_kind,
        accounting_mode: node.accounting_mode,
        reset_month: node.reset_month,
        reset_day: node.reset_day,
        quota_bytes: quota,
        calibrated_total_bytes: null,
      }),
    onSuccess: async view => {
      qc.setQueryData(['node-traffic'], view);
      await qc.invalidateQueries({ queryKey: ['usage-machine-model'] });
      onDone();
    },
  });
  const invalid = attempted && parsed === undefined;
  return (
    <form
      className="cap-quota"
      onSubmit={event => {
        event.preventDefault();
        setAttempted(true);
        if (parsed !== undefined) save.mutate(parsed);
      }}
    >
      <label>
        机器配额{' '}
        <input
          className="f mono"
          value={draft}
          inputMode="decimal"
          placeholder="不限"
          autoFocus
          aria-label={`${nameOf(node)} 机器配额`}
          aria-invalid={invalid || undefined}
          aria-describedby={invalid ? errorId : undefined}
          onFocus={event => event.currentTarget.select()}
          onChange={event => setDraft(event.target.value)}
          onKeyDown={event => {
            if (event.key === 'Escape' && !save.isPending) onDone();
          }}
        />
      </label>
      <select
        className="f words"
        value={unit}
        aria-label={`${nameOf(node)} 机器配额单位`}
        disabled={save.isPending}
        onChange={event => setUnit(event.target.value as MachineQuotaUnit)}
      >
        {MACHINE_QUOTA_UNITS.map(option => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
      <button className="btn primary" type="submit" disabled={save.isPending}>
        {save.isPending ? '保存中…' : '保存'}
      </button>
      <button className="btn" type="button" disabled={save.isPending} onClick={onDone}>
        取消
      </button>
      {invalid && (
        <span className="cap-quota-err" id={errorId}>
          需为大于 0 的数字，最多两位小数；留空表示不设配额
        </span>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </form>
  );
}

/* ══ 累计走势图 ══
   横轴为本期天数，纵轴为累计字节。实线 + 面积为本期累计，虚线从现在延伸到期末 P50，浅色扇形到 P90；
   点线为上期累计；横线为配额，配额以上的部分改用错误色。按实际像素宽度绘制，刻度文字不随缩放变形。 */

const CHART_HEIGHT = 176;

function BurnChart({ machine }: { machine: Machine }) {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.floor(entry.contentRect.width)));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return (
    <div className="cap-chart" ref={box} style={{ height: CHART_HEIGHT }}>
      {width > 0 && <BurnChartSvg machine={machine} width={width} height={CHART_HEIGHT} />}
    </div>
  );
}

function BurnChartSvg({ machine, width, height }: { machine: Machine; width: number; height: number }) {
  const uid = useId().replace(/[^\w-]/g, '');
  const { node, quota, used, p50, p90, series, totalDays, elapsedDays } = machine;
  const start = node.period_start_unix_secs;
  const padL = 1;
  const padR = 62;
  const padT = 16;
  const padB = 20;
  const plotW = Math.max(40, width - padL - padR);
  const plotH = Math.max(30, height - padT - padB);
  const previous = previousSeries(node);
  const previousTotal = previous.length > 0 ? previous[previous.length - 1][1] : 0;
  const top = Math.max(quota ?? 0, p90, previousTotal, 1) * 1.06;
  const X = (day: number) => padL + (Math.min(Math.max(day, 0), totalDays) / totalDays) * plotW;
  const Y = (value: number) => padT + plotH - (value / top) * plotH;
  const f = (n: number) => n.toFixed(1);
  const path = (points: [number, number][]) =>
    points.map(([day, value], index) => `${index ? 'L' : 'M'}${f(X(day))},${f(Y(value))}`).join('');
  const base = Y(0);
  const firstX = series[0][0];
  const actual = path(series);
  const area = `${actual}L${f(X(elapsedDays))},${f(base)}L${f(X(firstX))},${f(base)}Z`;
  const projection = `M${f(X(elapsedDays))},${f(Y(used))}L${f(X(totalDays))},${f(Y(p50))}`;
  const fan = `${projection}L${f(X(totalDays))},${f(Y(p90))}Z`;
  const layer = (className?: string) => (
    <g className={className}>
      <path className="fan" d={fan} />
      <path className="area" d={area} />
      <path className="act" d={actual} />
      <path className="proj" d={projection} />
    </g>
  );

  let crossing: { x: number; text: string } | null = null;
  if (quota !== null && machine.state === 'risk' && machine.exhaustAt !== null) {
    crossing = {
      x: X(elapsedDays + ((quota - used) / (p50 - used)) * (totalDays - elapsedDays)),
      text: `${monthDay(machine.exhaustAt)} 用尽`,
    };
  } else if (machine.state === 'over' && machine.overDay !== null) {
    crossing = { x: X(machine.overDay), text: `${monthDay(start + machine.overDay * DAY)} 越过` };
  }
  const markerAt = (at: number | null) =>
    at !== null && at >= start && at < node.period_end_unix_secs ? X((at - start) / DAY) : null;
  const gapX = node.gap_reason === null ? null : markerAt(node.gap_at_unix_secs);
  const calibrationX = markerAt(node.calibrated_at_unix_secs);
  const qy = quota === null ? null : Y(quota);
  const y50 = Y(p50);
  const y90 = Y(p90);
  const startX = X(0);
  const endX = X(totalDays);
  const nowX = X(elapsedDays);
  const labelY = height - 4;

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label={`${nameOf(node)} 本期累计流量与预计`}
    >
      <line className="base" x1={padL} x2={f(endX)} y1={f(base)} y2={f(base)} />
      {firstX > 0.02 && (
        <>
          <defs>
            <pattern
              id={`${uid}-hatch`}
              width="5"
              height="5"
              patternUnits="userSpaceOnUse"
              patternTransform="rotate(45)"
            >
              <line className="hatch" x1="0" y1="0" x2="0" y2="5" />
            </pattern>
          </defs>
          <rect x={f(startX)} y={padT} width={f(X(firstX) - startX)} height={f(plotH)} fill={`url(#${uid}-hatch)`} />
          {X(firstX) - startX > 46 && (
            <text className="mute" x={f((startX + X(firstX)) / 2)} y={f(padT + plotH / 2 + 4)} textAnchor="middle">
              未记账
            </text>
          )}
        </>
      )}
      {previous.length > 0 && <path className="prev" d={path(previous)} />}
      <line className="today" x1={f(nowX)} x2={f(nowX)} y1={padT - 4} y2={f(base)} />
      {qy === null ? (
        layer()
      ) : (
        <>
          <defs>
            <clipPath id={`${uid}-below`}>
              <rect x="0" y={f(qy)} width={width} height={height} />
            </clipPath>
            <clipPath id={`${uid}-above`}>
              <rect x="0" y="-20" width={width} height={f(qy + 20)} />
            </clipPath>
          </defs>
          <g clipPath={`url(#${uid}-below)`}>{layer()}</g>
          <g clipPath={`url(#${uid}-above)`}>{layer('x')}</g>
          <line className="quota" x1={padL} x2={f(endX)} y1={f(qy)} y2={f(qy)} />
          <text className="q" x={padL + 2} y={f(qy - 5 < padT - 2 ? qy + 13 : qy - 5)}>
            配额 {node.quota_bytes === null ? '' : quotaText(node.quota_bytes)}
          </text>
          {crossing && (
            <>
              <circle className="ex" cx={f(crossing.x)} cy={f(qy)} r="3.5" />
              <text
                className="bad"
                x={f(crossing.x)}
                y={f(qy - 8)}
                textAnchor={crossing.x > endX - 40 ? 'end' : crossing.x < 60 ? 'start' : 'middle'}
              >
                {crossing.text}
              </text>
            </>
          )}
        </>
      )}
      <circle className={`now${quota !== null && used >= quota ? ' x' : ''}`} cx={f(nowX)} cy={f(Y(used))} r="3" />
      {calibrationX !== null && node.calibrated_at_unix_secs !== null && (
        <path className="cal" d={`M${f(calibrationX)},${f(base - 7)}l4,7h-8z`}>
          <title>{`${monthDay(node.calibrated_at_unix_secs)} 校准`}</title>
        </path>
      )}
      {gapX !== null && node.gap_at_unix_secs !== null && node.gap_reason !== null && (
        <path className="gap" d={`M${f(gapX)},${f(base - 7)}l4,7h-8z`}>
          <title>{`${monthDay(node.gap_at_unix_secs)} ${trafficGapReason(node.gap_reason)}，${gapEffect(node.gap_reason)}`}</title>
        </path>
      )}
      <text className="v" x={f(endX + 7)} y={f(y50 + 4)}>
        {volume(p50)}
      </text>
      {y50 - y90 >= 13 && (
        <text x={f(endX + 7)} y={f(y90 + 4)}>
          P90 {volume(p90)}
        </text>
      )}
      <text x={f(startX)} y={labelY}>
        {monthDay(start)}
      </text>
      <text x={f(endX)} y={labelY} textAnchor="end">
        {monthDay(node.period_end_unix_secs)}
      </text>
      {nowX - startX > 52 && endX - nowX > 52 && (
        <text x={f(nowX)} y={labelY} textAnchor="middle">
          今天
        </text>
      )}
    </svg>
  );
}
