import type { ReactNode } from 'react';
import type { NodeAgentStateItem } from '../api';
import { Ago, ErrorBox } from '../ui/bits';
import { Icon, type IconName } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';

/* ══ 发布页驾驶舱：三个页签共用的读数栏与逐台机器表 ══════════════════════════════
   读数栏直接用用量页的 usage-ledger 一组类名（大数字、组成条、组成行、键值行），只补状态配色；
   机器表是 cgc- 前缀的紧凑列表。Agent 与 Xray 用同一套状态词，同一个词在两页表示同一件事：
     ok    已是新版 / 本次已升级   运行的就是可发版本
     run   待升级 / 升级中         已批准，机器正在自行升级，不需要再操作
     warn  可升级                  落后于可发版本，等待批准
     err   失败                    本次升级没有完成
     none  不可升级 / 已取消        当前不满足升级条件，原因写在机器名下 */

export type ReleaseTone = 'ok' | 'warn' | 'run' | 'err' | 'none';

const TONE_ICON: Record<ReleaseTone, IconName> = {
  ok: 'check',
  warn: 'upgrade',
  run: 'clock',
  err: 'close',
  none: 'dash',
};

export interface LedgerPart {
  tone: ReleaseTone;
  label: string;
  /** null 表示还没有读数（例如配置页的逐台对比尚未返回），显示为「—」而不是 0。 */
  count: number | null;
}

export function ReleaseLedger({
  label,
  value,
  unit,
  period,
  parts,
  facts,
}: {
  label: string;
  value: ReactNode;
  unit: string;
  period: ReactNode;
  parts: LedgerPart[];
  facts: [string, ReactNode][];
}) {
  const total = parts.reduce((sum, part) => sum + (part.count ?? 0), 0);
  const share = (count: number | null) => (count != null && total > 0 ? `${((count / total) * 100).toFixed(1)}%` : '—');
  return (
    <section className="usage-ledger cgc-ledger" aria-label={label}>
      <div className="usage-hero">
        <span className="usage-hero-label">{label}</span>
        <strong className="usage-hero-value">
          {value} <small>{unit}</small>
        </strong>
        <span className="usage-period">{period}</span>
      </div>
      <div className="usage-compose">
        {/* 组成行写着同样的数值，条只表示比例。 */}
        <div className="usage-split" aria-hidden="true">
          {parts
            .filter(part => (part.count ?? 0) > 0)
            .map(part => (
              <i key={part.label} className={`cgc-seg-${part.tone}`} style={{ flexGrow: part.count ?? 0 }} />
            ))}
        </div>
        <dl className="usage-io">
          {parts.map(part => (
            <div key={part.label} className={`cgc-io-${part.tone}`}>
              <dt>
                <i aria-hidden="true" />
                {part.label}
              </dt>
              <dd>{part.count == null ? '—' : `${part.count} 台`}</dd>
              <dd className="usage-share">{share(part.count)}</dd>
            </div>
          ))}
        </dl>
      </div>
      <dl className="usage-facts">
        {facts.map(([name, value]) => (
          <div key={name}>
            <dt>{name}</dt>
            <dd>{value}</dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

export interface MachineRow {
  node: NodeAgentStateItem;
  tone: ReleaseTone;
  status: string;
  /** 运行中的版本或构建；没有上报时为 null。 */
  current: string | null;
  /** 要升级到的版本；已是新版或不能升级时为 null。 */
  target: string | null;
  /** 不能升级或升级失败的原因，写在机器名下。 */
  reason?: string | null;
  /** 状态格里附带的操作，例如失败目标的重试。 */
  action?: ReactNode;
  /** 编辑升级范围时这一行的勾选框。 */
  pick?: { checked: boolean; disabled: boolean; onChange: () => void };
}

export interface MachineGroup {
  key: string;
  label: string;
  rows: MachineRow[];
}

/** 逐台机器表：机器 │ 当前 │ 目标 │ 状态 │ 上次来拉，按状态分组。编辑升级范围时前面多一列勾选，
 *  工具条贴在列头之上，外框换成主题色细线。 */
export function MachineTable({
  label,
  groups,
  editing,
  toolbar,
  pickAll,
}: {
  label: string;
  groups: MachineGroup[];
  editing: boolean;
  toolbar?: ReactNode;
  pickAll?: ReactNode;
}) {
  const visible = groups.filter(group => group.rows.length > 0);
  const columns = editing ? 6 : 5;
  return (
    <div className={`cgc-table${editing ? ' editing' : ''}`}>
      {editing && toolbar}
      <div role="table" aria-label={label} aria-colcount={columns}>
        <div className="cgc-th" role="row">
          {editing && (
            <span className="cgc-pick" role="columnheader">
              {pickAll}
            </span>
          )}
          <span role="columnheader">机器</span>
          <span role="columnheader">当前</span>
          <span role="columnheader">目标</span>
          <span role="columnheader">状态</span>
          <span role="columnheader">上次来拉</span>
        </div>
        {visible.map(group => (
          <div key={group.key} className="cgc-group" role="rowgroup">
            <div className="cgc-tg" role="row">
              <span role="cell" aria-colspan={columns}>
                {group.label}
                <b>{group.rows.length}</b>
              </span>
            </div>
            {group.rows.map(row => (
              <MachineTableRow key={row.node.node_id} row={row} editing={editing} />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function MachineTableRow({ row, editing }: { row: MachineRow; editing: boolean }) {
  const { node } = row;
  const name = node.name || node.node_id;
  return (
    <div className={`cgc-tr ${row.tone}`} role="row">
      {editing && (
        <span className="cgc-pick" role="cell">
          {row.pick && (
            <input
              type="checkbox"
              aria-label={`升级 ${name}`}
              checked={row.pick.checked}
              disabled={row.pick.disabled}
              onChange={row.pick.onChange}
            />
          )}
        </span>
      )}
      <span className="cgc-mach" role="rowheader">
        <span className="cgc-flag">
          <RegionFlag code={node.public_ipv4_country} />
        </span>
        <span>
          <b>{name}</b>
          {row.reason && <small title={row.reason}>{row.reason}</small>}
        </span>
      </span>
      <span className="cgc-ver" role="cell">
        {row.current ?? '—'}
      </span>
      <span className="cgc-ver to" role="cell">
        {row.target ? `→ ${row.target}` : ''}
      </span>
      <span className="cgc-st" role="cell">
        <span className={`cgc-node ${row.tone}`}>
          <Icon of={TONE_ICON[row.tone]} size={10} />
        </span>
        {row.status}
        {row.action}
      </span>
      <span className="cgc-poll" role="cell">
        {node.last_poll_at ? <Ago at={node.last_poll_at} /> : '从未来拉'}
      </span>
    </div>
  );
}

/** 编辑时贴在机器表列头之上的工具条：第一行是范围与提交，第二行是批准后会发生什么。 */
export function ScopeToolbar({ children, note, error }: { children: ReactNode; note: ReactNode; error?: ReactNode }) {
  return (
    <div className="cgc-tbar" role="group" aria-label="升级范围">
      <div className="cgc-tbar-row">
        <span className="cgc-tbar-label">升级范围</span>
        {children}
      </div>
      <p className="cgc-tbar-note">{note}</p>
      {error}
    </div>
  );
}

/** 页签数据读取失败：整页签给出错误，不画空的读数栏和空表，避免读成「没有机器」。 */
export function TabError({ error }: { error: unknown }) {
  return (
    <section className="cgc-main cgc-span">
      <ErrorBox error={error} />
    </section>
  );
}

/** 读数栏里的时刻：今年只写月日与时分，跨年才带年份。 */
export function stamp(at: string): string {
  const time = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(time)) return at;
  const date = new Date(time);
  const pad = (value: number) => String(value).padStart(2, '0');
  const day = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return date.getFullYear() === new Date().getFullYear() ? day : `${date.getFullYear()}-${day}`;
}

/** 键值行里的一串地区旗：最多八面，其余折成「+N」。 */
export function FlagRun({ nodes }: { nodes: (NodeAgentStateItem | undefined)[] }) {
  const shown = nodes.slice(0, 8);
  return (
    <span className="cgc-flags">
      {shown.map((node, index) => (
        <RegionFlag key={node?.node_id ?? index} code={node?.public_ipv4_country} />
      ))}
      {nodes.length > shown.length && <small>+{nodes.length - shown.length}</small>}
    </span>
  );
}
