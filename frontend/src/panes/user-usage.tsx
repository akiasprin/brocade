import type { ReactNode } from 'react';
import type { UserTrafficAccountingMode } from '../api';
import { UserTrafficAccountingSetting } from '../user-traffic-accounting';
import { useNow } from '../ui/clock';
import { bytes, shareOf } from '../ui/format';
import './user-traffic-policy.css';
import './user-quota.css';

export const QUOTA_ATTENTION_PCT = 80;

export type CycleUsageState = 'pending' | 'failed' | 'ready';

export const cycleUsageText = (state: CycleUsageState, rowCount: number, total: number) => {
  if (state === 'pending') return '读取中…';
  if (state === 'failed') return '暂不可用';
  return rowCount > 0 ? bytes(total) : '—';
};

const GiB = 1024 ** 3;
// 卡片读数保持 GiB，不因余额较大而跳成 TiB；小于 GiB 的值仍保留 MiB 等精度。
const lineBytes = (value: number) => (value >= GiB ? `${(value / GiB).toFixed(2)} GiB` : bytes(value));
const quotaBytes = (value: number) => (value % GiB === 0 ? `${value / GiB} GiB` : lineBytes(value));

/** 接入与转发共用卡片、额度分档、读数与圆环；授权及策略状态由各自的调用方提供。 */
export function UserQuotaTile({
  label,
  flags,
  used,
  limit,
  state = 'ready',
  paused = false,
  throttled = false,
  status,
  detail,
  actions,
  editing = false,
}: {
  label: string;
  flags?: ReactNode;
  used: number | null;
  limit: number | null;
  state?: CycleUsageState;
  paused?: boolean;
  throttled?: boolean;
  status?: ReactNode;
  detail?: ReactNode;
  actions?: ReactNode;
  editing?: boolean;
}) {
  const known = state === 'ready' && used !== null;
  const pct = known && limit !== null && limit > 0 ? (used / limit) * 100 : null;
  const over = pct !== null && pct >= 100;
  const tone = !known
    ? 'unknown'
    : limit === null
      ? 'unlimited'
      : over
        ? throttled
          ? 'warn'
          : 'over'
        : pct !== null && pct >= QUOTA_ATTENTION_PCT
          ? 'warn'
          : 'ok';
  return (
    <article
      className={`qta-r ${tone}${paused ? ' paused' : throttled ? ' throttled' : ''}${editing ? ' fwe-open' : ''}${pct === null ? ' no-ring' : ''}`}
      role="listitem"
      aria-label={label}
    >
      <header className="qta-name">
        <span className="qta-app">
          <b>{label}</b>
          {flags}
        </span>
      </header>
      <UserQuotaReading used={used} limit={limit} state={state} />
      {pct !== null && <UserQuotaRing pct={pct} />}
      <footer className="qta-foot">
        {status}
        {detail}
      </footer>
      {actions && <span className="qta-acts">{actions}</span>}
    </article>
  );
}

function UserQuotaReading({
  used,
  limit,
  state,
}: {
  used: number | null;
  limit: number | null;
  state: CycleUsageState;
}) {
  const known = state === 'ready' && used !== null;
  const [figure, unit] = known ? lineBytes(used).split(' ') : [state === 'pending' ? '读取中…' : '—', ''];
  const balance = !known ? (
    state === 'ready' ? (
      '用量未知'
    ) : null
  ) : limit === null ? (
    '不限额度'
  ) : (
    <>
      {used >= limit ? '超出' : '剩余'}
      <b>{lineBytes(Math.abs(limit - used))}</b>
    </>
  );
  return (
    <div className="qta-reading">
      <strong className="qta-figs" aria-label={`已用 ${figure}${unit ? ` ${unit}` : ''}`}>
        {figure}
        {unit && <small>{unit}</small>}
      </strong>
      {limit !== null && <span className="qta-cap">额度 {quotaBytes(limit)}</span>}
      {balance !== null && (
        <span className="qta-left">
          <span className="qta-left-value">{balance}</span>
        </span>
      )}
    </div>
  );
}

function UserQuotaRing({ pct }: { pct: number }) {
  const fill = Math.min(100, Math.max(pct > 0 ? 1 : 0, pct));
  const text = pct > 0 && pct < 1 ? '<1%' : `${pct < 100 ? Math.min(99, Math.round(pct)) : Math.round(pct)}%`;
  return (
    <span className="qta-ring" role="img" aria-label={`已用额度 ${text}`}>
      <svg viewBox="0 0 56 56" aria-hidden="true">
        <circle className="track" cx="28" cy="28" r="25.25" strokeWidth="5.5" />
        {fill > 0 && (
          <circle
            className="value"
            cx="28"
            cy="28"
            r="25.25"
            strokeWidth="5.5"
            pathLength="100"
            strokeDasharray={`${fill} 100`}
            transform="rotate(-90 28 28)"
          />
        )}
      </svg>
      <span className="qta-pct">{text}</span>
    </span>
  );
}

/** 两个页签共用接入读数栏的分组和响应式布局；附加说明留在上下行分组内。 */
export function UserUsageLedger({
  label,
  state,
  rows,
  periodEnd,
  accountingMode,
  accountingDisabled,
  onAccountingModeChange,
  cycleReset,
  servicePolicy,
  note,
  children,
}: {
  label: string;
  state: CycleUsageState;
  rows: ReadonlyArray<{ uplink_bytes: number; downlink_bytes: number }>;
  periodEnd: string | null;
  accountingMode: UserTrafficAccountingMode;
  accountingDisabled: boolean;
  onAccountingModeChange: (mode: UserTrafficAccountingMode) => void;
  cycleReset?: ReactNode;
  servicePolicy?: ReactNode;
  note?: ReactNode;
  children?: ReactNode;
}) {
  const uplink = rows.reduce((sum, row) => sum + row.uplink_bytes, 0);
  const downlink = rows.reduce((sum, row) => sum + row.downlink_bytes, 0);
  const rawTotal = uplink + downlink;
  const total = rawTotal * (accountingMode === 'double' ? 2 : 1);
  const known = state === 'ready' && rows.length > 0;
  const [figure, unit] = bytes(total).split(' ');
  const upShare = rawTotal > 0 ? (uplink / rawTotal) * 100 : 0;
  return (
    <section className="user-usage-ledger" aria-label={label}>
      <div className="user-usage-hero">
        <span className="user-usage-hero-label">{label}</span>
        {cycleReset}
        {known ? (
          <strong className="user-usage-hero-value">
            {figure} <small>{unit}</small>
          </strong>
        ) : (
          <strong className="user-usage-hero-value none">{cycleUsageText(state, rows.length, total)}</strong>
        )}
        {periodEnd && <QuotaReset periodEnd={periodEnd} />}
      </div>
      <div className="user-usage-compose">
        <div className="usage-split" aria-hidden="true">
          {known && total > 0 && (
            <>
              <i className="up" style={{ flexGrow: upShare }} />
              <i className="down" style={{ flexGrow: 100 - upShare }} />
            </>
          )}
        </div>
        <dl className="usage-io">
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{known ? bytes(uplink) : '—'}</dd>
            <dd className="usage-share">{known ? shareOf(uplink, rawTotal) : ''}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{known ? bytes(downlink) : '—'}</dd>
            <dd className="usage-share">{known ? shareOf(downlink, rawTotal) : ''}</dd>
          </div>
        </dl>
        {note}
      </div>
      <UserTrafficAccountingSetting
        mode={accountingMode}
        disabled={accountingDisabled}
        onChange={onAccountingModeChange}
      />
      {servicePolicy}
      {children}
    </section>
  );
}

function QuotaReset({ periodEnd }: { periodEnd: string }) {
  const now = useNow();
  const resetAt = Date.parse(periodEnd);
  if (!Number.isFinite(resetAt) || resetAt <= now) return null;
  const date = new Date(resetAt + 8 * 3_600_000);
  const resetDate = `${date.getUTCMonth() + 1} 月 ${date.getUTCDate()} 日`;
  const days = Math.ceil((resetAt - now) / 86_400_000);
  return (
    <span
      className="user-usage-reset"
      title={`额度按 UTC+8 账期统计，${resetDate} 00:00 重新计算；仅恢复仍符合有效期和手动开关的接入点或转发入口`}
    >
      {resetDate}重置
      <span className="usage-dot" aria-hidden="true">
        ·
      </span>
      {days} 天后
    </span>
  );
}
