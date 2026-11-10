import type { CSSProperties, ReactNode } from 'react';
import type { UserTrafficAccountingMode } from '../api';
import { UserTrafficAccountingSetting } from '../user-traffic-accounting';
import { useNow } from '../ui/clock';
import { bytes, shareOf } from '../ui/format';
import './user-traffic-policy.css';

export type CycleUsageState = 'pending' | 'failed' | 'ready';

export const cycleUsageText = (state: CycleUsageState, rowCount: number, total: number) => {
  if (state === 'pending') return '读取中…';
  if (state === 'failed') return '暂不可用';
  return rowCount > 0 ? bytes(total) : '—';
};

const GiB = 1024 ** 3;
const quotaBytes = (value: number) => (value % GiB === 0 ? `${value / GiB} GiB` : bytes(value));

/** 接入与转发共用读数和余额的文案、精度及单位层级。 */
export function UserQuotaReading({
  used,
  limit,
  state = 'ready',
}: {
  used: number | null;
  limit: number | null;
  state?: CycleUsageState;
}) {
  const [figure, unit] = used === null ? ['', ''] : bytes(used).split(' ');
  const balance =
    state === 'pending' ? null : state === 'failed' ? (
      '用量暂不可用'
    ) : used === null ? (
      '用量未知'
    ) : limit === null ? (
      '不限额度'
    ) : used >= limit ? (
      <>
        超出<b>{bytes(used - limit)}</b>
      </>
    ) : (
      <>
        剩余<b>{bytes(limit - used)}</b>
      </>
    );
  return (
    <>
      <span className="qta-figs">
        {state === 'pending' ? (
          '读取中…'
        ) : used === null ? (
          '—'
        ) : (
          <>
            {figure}
            <small>{unit}</small>
          </>
        )}
        {limit !== null && <span className="qta-cap">/ {quotaBytes(limit)}</span>}
      </span>
      {balance !== null && (
        <span className="qta-left">
          <span className="qta-left-value">{balance}</span>
        </span>
      )}
    </>
  );
}

/** 流量额度轨道只表达流量占比；规则数量等其它限制单独展示。 */
export function UserQuotaMeter({
  used,
  limit,
  status,
  tone = '',
}: {
  used: number | null;
  limit: number | null;
  status?: ReactNode;
  tone?: '' | 'idle' | 'warn' | 'err';
}) {
  if (limit === null || used === null) return status ? <span className="qta-policy-only">{status}</span> : null;
  const pct = (used / limit) * 100;
  const overflow = used > limit;
  const share = overflow ? limit / used : 1;
  const fill = overflow ? 100 : Math.min(100, Math.max(used > 0 ? 1 : 0, pct));
  return (
    <span className="qta-meter qta-quota-meter">
      <span
        className={`qta-meter-run${status ? ' has-policy' : ''} ${tone}`}
        style={{ '--at': `${share * 100}%`, '--at-cqi': `${share * 100}cqi` } as CSSProperties}
      >
        <span className="qta-meter-track" aria-hidden="true">
          <span className="qta-t" style={{ flexGrow: share }}>
            <i style={{ width: `${fill}%` }} />
          </span>
          {overflow && <span className="qta-overflow" style={{ flexGrow: 1 - share }} />}
        </span>
        {status}
      </span>
      <span className="qta-pct">{pct.toFixed(0)}%</span>
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
