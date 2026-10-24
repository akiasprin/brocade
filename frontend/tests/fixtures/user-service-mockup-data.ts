// Synthetic data for user-service-mockup: billing periods, service windows and daily usage in UTC+8.
// Times are relative to the real clock so the production pane's countdowns agree with the mockup.
import type { UserServicePolicyWrite, UserServiceState } from '../../src/api';

export const GiB = 1024 ** 3;
export const DAY = 86_400_000;
const OFFSET = 8 * 3_600_000;

export interface Policy {
  resetDay: number;
  start: number | null;
  end: number | null;
}

/** UTC+8 midnight of the day containing `ms`, moved by `days`. */
export function midnight(ms: number, days = 0): number {
  return Math.floor((ms + OFFSET) / DAY) * DAY - OFFSET + days * DAY;
}

/** Same clamping as the store: a month without the chosen day resets on its last day. */
function boundary(year: number, month: number, day: number): number {
  const last = new Date(Date.UTC(year, month + 1, 0)).getUTCDate();
  return Date.UTC(year, month, Math.min(day, last)) - OFFSET;
}

export function periodOf(resetDay: number, now: number): { start: number; end: number } {
  const local = new Date(now + OFFSET);
  let year = local.getUTCFullYear();
  let month = local.getUTCMonth();
  if (boundary(year, month, resetDay) > now) {
    month -= 1;
    if (month < 0) {
      month = 11;
      year -= 1;
    }
  }
  return { start: boundary(year, month, resetDay), end: boundary(year, month + 1, resetDay) };
}

/** Reset boundaries inside [from, to], for the timeline ticks. */
export function resetsBetween(resetDay: number, from: number, to: number): number[] {
  const out: number[] = [];
  let cursor = periodOf(resetDay, from).end;
  while (cursor <= to && out.length < 48) {
    out.push(cursor);
    cursor = periodOf(resetDay, cursor).end;
  }
  return out;
}

export function stateOf(policy: Pick<Policy, 'start' | 'end'>, now: number): UserServiceState {
  if (policy.start !== null && now < policy.start) return 'pending';
  if (policy.end !== null && now >= policy.end) return 'expired';
  return 'active';
}

export const iso = (ms: number | null) => (ms === null ? null : new Date(ms).toISOString().replace('.000Z', 'Z'));
export const wireOf = (policy: Policy): UserServicePolicyWrite => ({
  traffic_reset_day: policy.resetDay,
  service_starts_at: iso(policy.start),
  service_expires_at: iso(policy.end),
});
export const policyOf = (write: UserServicePolicyWrite): Policy => ({
  resetDay: write.traffic_reset_day,
  start: write.service_starts_at ? Date.parse(write.service_starts_at) : null,
  end: write.service_expires_at ? Date.parse(write.service_expires_at) : null,
});

/* ── 格式（UTC+8） ── */

const local = (ms: number) => new Date(ms + OFFSET).toISOString();
export const ymd = (ms: number) => local(ms).slice(0, 10);
export const md = (ms: number) => local(ms).slice(5, 10);
export const cnDate = (ms: number) => {
  const date = new Date(ms + OFFSET);
  return `${date.getUTCMonth() + 1} 月 ${date.getUTCDate()} 日`;
};
/** datetime-local value. */
export const inputTime = (ms: number | null) => (ms === null ? '' : local(ms).slice(0, 16));
export const fromInput = (value: string) => Date.parse(`${value}:00+08:00`);
export const daysUntil = (ms: number, now: number) => Math.ceil((ms - now) / DAY);

/** Calendar shortcuts clamp the target month's day, like the production editor. */
export function extend(from: number, unit: 'days' | 'months' | 'years'): number {
  const date = new Date(from + OFFSET);
  if (unit === 'days') date.setUTCDate(date.getUTCDate() + 30);
  else {
    const day = date.getUTCDate();
    date.setUTCDate(1);
    date.setUTCMonth(date.getUTCMonth() + (unit === 'years' ? 12 : 1));
    const last = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0)).getUTCDate();
    date.setUTCDate(Math.min(day, last));
  }
  return date.getTime() - OFFSET;
}

/* ── 场景 ── */

export type SceneKey = 'typical' | 'term' | 'soon' | 'pending' | 'expired';
export const SCENES: Record<SceneKey, { label: string; policy: (now: number) => Policy }> = {
  typical: { label: '常规 · 每月 1 日 · 长期有效', policy: () => ({ resetDay: 1, start: null, end: null }) },
  term: {
    label: '包年 · 每月 15 日 · 85 天后到期',
    policy: now => ({ resetDay: 15, start: midnight(now, -68), end: midnight(now, 85) }),
  },
  soon: {
    label: '即将到期 · 5 天后',
    policy: now => ({ resetDay: 15, start: null, end: midnight(now, 5) }),
  },
  pending: {
    label: '未生效 · 10 天后开始',
    policy: now => ({ resetDay: 1, start: midnight(now, 10), end: extend(midnight(now, 10), 'years') }),
  },
  expired: {
    label: '已到期 · 21 天前',
    policy: now => ({ resetDay: 20, start: midnight(now, -386), end: midnight(now, -21) }),
  },
};
export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];

/* ── 用量：按 UTC+8 自然日记账，改重置日只改变汇总范围 ── */

function dailyBytes(day: number, base: number, swing: number): number {
  // 每天一个确定的伪随机数，周末略高。
  const index = Math.floor((day + OFFSET) / DAY);
  const noise = (((Math.sin(index * 12.9898) * 43758.5453) % 1) + 1) % 1;
  const weekday = new Date(day + OFFSET).getUTCDay();
  const weekend = weekday === 0 || weekday === 6 ? 1.35 : 1;
  return Math.round((base + swing * noise) * weekend * GiB);
}

/**
 * Usage summed over [start, min(end, now)). `served` is the window the user actually had service in, so a paused
 * user has no traffic on those days no matter which policy is being previewed. The current day counts its elapsed part.
 */
export function usageBetween(start: number, end: number, now: number, served: Pick<Policy, 'start' | 'end'>) {
  let access = 0;
  let forwarding = 0;
  const from = Math.max(start, served.start ?? start);
  const stop = Math.min(end, now, served.end ?? end);
  for (let day = midnight(from); day < stop; day += DAY) {
    const share = Math.min(1, Math.max(0, (stop - Math.max(day, from)) / DAY));
    access += dailyBytes(day, 2.6, 2.4) * share;
    forwarding += dailyBytes(day + 7 * DAY, 0.18, 0.3) * share;
  }
  return { access: Math.round(access), forwarding: Math.round(forwarding) };
}

export interface PolicyPreview {
  start: number;
  end: number;
  access: number;
  forwarding: number;
  state: UserServiceState;
}

export function previewOf(policy: Policy, now: number, served: Pick<Policy, 'start' | 'end'>): PolicyPreview {
  const period = periodOf(policy.resetDay, now);
  return { ...period, ...usageBetween(period.start, period.end, now, served), state: stateOf(policy, now) };
}
