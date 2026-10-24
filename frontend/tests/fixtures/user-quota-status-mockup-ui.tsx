// Option parts for the 用量与额度 status mockup. Rows reuse the production quota-row classes (qta-*) so only the
// status part differs; option additions are prefixed qs-.
import { useRef, useState, type ReactNode } from 'react';
import type { UserListItem } from '../../src/api';
import { UserAppTrafficPolicyEditor } from '../../src/panes/user-traffic-policy';
import { DialogLayer } from '../../src/ui/dialog';
import { bytes } from '../../src/ui/format';
import { RegionFlag } from '../../src/ui/region-flag';
import {
  GiB,
  exhaustedOf,
  machineName,
  rateText,
  resetText,
  statusOf,
  viewOf,
  type Line,
  type LineStatus,
  type World,
} from './user-quota-status-mockup-data';

export type Option = 'a' | 'c' | 'd';

const QUOTA_ATTENTION_PCT = 80;
const quotaBytes = (n: number) => (n % GiB === 0 ? `${n / GiB} GiB` : bytes(n));
const policyText = (item: Line) =>
  !item.action ? '' : item.action.kind === 'throttle' ? `用满后限速 ${rateText(item.action.rate_bps)}` : '用满后停用';
const pendingTitle = (item: Line) => `等待 ${item.pending.map(machineName).join('、')} 同步`;
const syncing = (item: Line) => item.action?.kind === 'throttle' && item.pending.length > 0;

/** System-style activity indicator for 「同步中」: eight spokes fading behind the leading one; still under reduced motion. */
const Spin = () => (
  <svg className="qs-spin" viewBox="0 0 12 12" aria-hidden="true">
    {Array.from({ length: 8 }, (_, index) => (
      <line
        key={index}
        x1="6"
        y1="1.3"
        x2="6"
        y2="3.5"
        transform={`rotate(${index * 45} 6 6)`}
        opacity={1 - ((8 - index) % 8) * 0.11}
      />
    ))}
  </svg>
);

/* ══ A 状态字：文字跟在额度后面，常态写超额处理 ══ */

function StatusWordsA({ item, status }: { item: Line; status: LineStatus }) {
  const sync = syncing(item) && (
    <span className="qs-a-sync" title={pendingTitle(item)}>
      <Spin />
      同步中
    </span>
  );
  if (status.kind === 'paused')
    return (
      <span className="qs-a-state idle">
        <i className="qs-dot hollow" aria-hidden="true" />
        服务暂停
      </span>
    );
  if (status.kind === 'suspended')
    return (
      <span className="qs-a-state err">
        <i className="qs-dot" aria-hidden="true" />
        已停用
      </span>
    );
  if (status.kind === 'throttled')
    return (
      <span className="qs-a-state warn">
        <i className="qs-dot" aria-hidden="true" />
        限速中 · {rateText(status.rate)}
        {sync}
      </span>
    );
  const policy = item.quota === null ? '' : policyText(item);
  if (!policy && !sync) return null;
  return (
    <span className="qs-a-policy">
      {policy}
      {sync}
    </span>
  );
}

/* ══ C 状态符号：只在需要注意时出现，点开写明状态 ══ */

/** 限速中：指针落在低档的速度表；停用与服务暂停：两道竖杠。都在实心圆上镂空。 */
function Glyph({ of }: { of: 'gauge' | 'pause' }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      {of === 'gauge' ? (
        <g className="qs-c-stroke">
          <path d="M3.1 11.6 A5.6 5.6 0 1 1 12.9 11.6" />
          <path d="M8 9.6 L4.7 7.4" />
          <circle cx="8" cy="9.6" r="0.9" />
        </g>
      ) : (
        <>
          <rect x="4.6" y="3.8" width="2.4" height="8.4" rx="0.8" />
          <rect x="9" y="3.8" width="2.4" height="8.4" rx="0.8" />
        </>
      )}
    </svg>
  );
}

function StatusSymbolC({ item, status }: { item: Line; status: LineStatus }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const sync = syncing(item);
  if (status.kind === 'normal' && !sync) return null;
  const tone =
    status.kind === 'throttled' ? 'warn' : status.kind === 'suspended' ? 'err' : status.kind === 'paused' ? 'idle' : '';
  const title =
    status.kind === 'throttled'
      ? `限速中 · ${rateText(status.rate)}`
      : status.kind === 'suspended'
        ? '已停用'
        : status.kind === 'paused'
          ? '服务暂停'
          : '限速设置同步中';
  const facts: [string, string][] = [];
  if (status.kind === 'suspended') facts.push(['接入点', `${status.withdrawn} 个已停用`]);
  if (status.kind === 'throttled' || status.kind === 'suspended') facts.push(['恢复', `${resetText()}重置后`]);
  if (status.kind === 'paused') facts.push(['原因', '有效期已到']);
  if (sync) facts.push(['待同步', item.pending.map(machineName).join('、')]);
  const label = [title, ...facts.map(([key, value]) => `${key} ${value}`)].join(' · ');
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`qs-c-badge${tone ? ` ${tone}` : ' sync'}`}
        title={label}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        {status.kind === 'normal' ? (
          <Spin />
        ) : (
          <span className="qs-c-disc">
            <Glyph of={status.kind === 'throttled' ? 'gauge' : 'pause'} />
          </span>
        )}
        {status.kind !== 'normal' && sync && <span className="qs-c-corner" aria-hidden="true" />}
      </button>
      {open && (
        <DialogLayer
          mode="popover"
          align="end"
          anchor={anchor}
          label={`${item.label} 的额度状态`}
          onClose={() => setOpen(false)}
        >
          <div className="dialog-surface qs-c-pop">
            <b className={tone || undefined}>{title}</b>
            <dl>
              {facts.map(([key, value]) => (
                <div key={key}>
                  <dt>{key}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
          </div>
        </DialogLayer>
      )}
    </>
  );
}

/* ══ D 名称行状态：与名册同一规则，异常时状态顶替接入链 ══ */

function SubtitleD({ item, status, chains }: { item: Line; status: LineStatus; chains: string }) {
  const sync = syncing(item);
  if (status.kind === 'paused')
    return (
      <span className="qta-sub qs-d idle">
        <i className="qs-dot hollow" aria-hidden="true" />
        服务暂停
      </span>
    );
  if (status.kind === 'suspended')
    return (
      <span className="qta-sub qs-d err" title={`${chains} 已被系统停用；补足额度或账期重置后自动恢复`}>
        <i className="qs-dot" aria-hidden="true" />
        已停用 · {status.withdrawn} 个接入点
      </span>
    );
  if (status.kind === 'throttled')
    return (
      <span className="qta-sub qs-d warn" title={sync ? pendingTitle(item) : undefined}>
        <i className="qs-dot" aria-hidden="true" />
        限速中 · {rateText(status.rate)}
        {sync && <> · 同步中</>}
      </span>
    );
  if (sync)
    return (
      <span className="qta-sub qs-d sync" title={pendingTitle(item)}>
        <Spin />
        同步中 · {item.pending.map(machineName).join('、')}
      </span>
    );
  return (
    <span className="qta-sub" title={[chains, policyText(item)].filter(Boolean).join(' · ')}>
      {chains}
    </span>
  );
}

/* ══ 行：生产 QuotaRow 的结构与类名，状态部分按方案替换 ══ */

function MockQuotaRow({
  option,
  world,
  item,
  user,
  editable,
}: {
  option: Option;
  world: World;
  item: Line;
  user: UserListItem;
  editable: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const editButton = useRef<HTMLButtonElement>(null);
  const status = statusOf(world, item);
  const limit = item.quota;
  const used = item.used;
  const over = exhaustedOf(item);
  const pct = limit !== null ? (used / limit) * 100 : null;
  const chains = item.chains.map(chain => chain.name).join('、');
  // 限速中的线路仍可用：额度条、百分比与超出量用注意色，与名册「已限速」不标红点一致。
  const tone =
    limit === null
      ? 'unlimited'
      : over
        ? status.kind === 'throttled'
          ? 'warn qs-throttled'
          : 'over'
        : pct !== null && pct >= QUOTA_ATTENTION_PCT
          ? 'warn'
          : 'ok';
  const [figure, unit] = bytes(used).split(' ');
  const left: ReactNode =
    limit === null ? (
      '不限额度'
    ) : over ? (
      <>
        超出<b>{bytes(used - limit)}</b>
      </>
    ) : (
      <>
        剩余<b>{bytes(limit - used)}</b>
      </>
    );
  const countries = [...new Set(item.chains.map(chain => chain.country))];

  let subtitle: ReactNode = (
    <span className="qta-sub" title={chains}>
      {chains}
    </span>
  );
  if (option === 'd') subtitle = <SubtitleD item={item} status={status} chains={chains} />;
  // C 只换符号，名称行沿用生产写法：系统停用的接入点以告警色写出数量。
  if (option === 'c' && status.kind === 'suspended')
    subtitle = (
      <span className="qta-sub stop" title={`${chains} 已被系统停用；补足额度或账期重置后自动恢复`}>
        系统已停用 {status.withdrawn} 个接入点
      </span>
    );

  // A 的文字跟在额度后面：放进伸缩的数值列，不撑宽右侧余量列，各行余量仍右缘对齐。
  const afterCap = option === 'a' ? <StatusWordsA item={item} status={status} /> : null;
  const before = option === 'c' ? <StatusSymbolC item={item} status={status} /> : null;

  return (
    <div
      className={`qta-r ${tone} qs-row qs-${option}${status.kind === 'paused' ? ' qs-paused' : ''}${editing ? ' fwe-open' : ''}`}
      role="listitem"
    >
      <span className="qta-name">
        <span className="qta-app">
          <b>{item.label}</b>
          <span className="qta-flags">
            {countries.slice(0, 3).map(code => (
              <RegionFlag key={code} code={code} />
            ))}
          </span>
        </span>
        {subtitle}
      </span>
      <span className="qta-figs">
        {figure}
        <small>{unit}</small>
        {limit !== null && <span className="qta-cap">/ {quotaBytes(limit)}</span>}
        {afterCap}
      </span>
      {/* 窄卡里数值与余量已占满一行，A 的文字另起一行（同一内容，按容器宽度二选一显示）。 */}
      {afterCap && <span className="qs-a-line">{afterCap}</span>}
      <span className={`qta-left${before ? ' qs-left-status' : ''}`}>
        {before}
        <span className="qta-left-value">{left}</span>
      </span>
      {pct !== null && (
        <span className="qta-meter">
          <span className="qta-t" aria-hidden="true">
            <i style={{ width: `${used > 0 ? Math.min(100, Math.max(1, pct)) : 0}%` }} />
          </span>
          <span className="qta-pct">{pct.toFixed(0)}%</span>
        </span>
      )}
      {editable && (
        <span className="qta-acts">
          <button
            ref={editButton}
            type="button"
            className="btn qta-pencil"
            disabled={editing}
            aria-label={`${item.label} · 额度与超额处理`}
            title="编辑额度"
            aria-expanded={editing}
            onClick={() => setEditing(true)}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="m3 11.8.6-2.7L10.8 2l2.2 2.2-7.1 7.1-2.9.5Z" />
              <path d="m9.7 3.1 2.2 2.2" />
            </svg>
          </button>
        </span>
      )}
      {editing && (
        <div className="fwe-sheet">
          <UserAppTrafficPolicyEditor
            key={`${item.id}-${item.quota}-${JSON.stringify(item.action)}`}
            user={user}
            view={viewOf(world, item)}
            label={`${item.label} · 额度与超额处理`}
            onClose={() => {
              setEditing(false);
              queueMicrotask(() => editButton.current?.focus());
            }}
          />
        </div>
      )}
    </div>
  );
}

export function MockQuotaRows({
  option,
  world,
  user,
  editable,
}: {
  option: Option;
  world: World;
  user: UserListItem;
  editable: boolean;
}) {
  return (
    <div className="user-access-quotas qs-own">
      <div className="qta" role="list" aria-label="各线路的用量与额度">
        {world.lines.map(item => (
          <MockQuotaRow key={item.id} option={option} world={world} item={item} user={user} editable={editable} />
        ))}
      </div>
    </div>
  );
}
