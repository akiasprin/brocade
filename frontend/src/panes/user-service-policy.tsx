import { useEffect, useRef, useState, type FormEvent, type RefObject } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  previewUserServicePolicy,
  updateUserServicePolicy,
  type UserListItem,
  type UserServicePolicyPreview,
  type UserServicePolicyWrite,
  type UserServiceState,
} from '../api';
import { ErrorBox, SegmentedControl } from '../ui/bits';
import { useNow } from '../ui/clock';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { bytes } from '../ui/format';
import { FieldLoading } from '../ui/loading';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import './user-service-policy.css';

const DAY = 86_400_000;
const OFFSET = 8 * 3_600_000;
/** UTC+8 wall time as a datetime-local value. */
const localTime = (ms: number) => new Date(ms + OFFSET).toISOString().slice(0, 16);
const inputTime = (value: string | null | undefined) => (value ? localTime(Date.parse(value)) : '');
const wireTime = (value: string) => `${value}:00+08:00`;
const dateText = (value: string) => inputTime(value).slice(0, 10);
const timeText = (value: string) => inputTime(value).replace('T', ' ');

/** Calendar shortcuts clamp the target month's day, just like the billing policy. */
export function extendServiceTime(from: number, unit: 'days' | 'months' | 'years'): number {
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

/** 有效期一句话：常态写到期日；七天内到期、未生效和已到期才上色。 */
function validityOf(user: UserListItem, now: number): { text: string; tone: '' | 'gold' | 'err' } {
  const start = user.service_starts_at;
  const end = user.service_expires_at;
  const state = user.service_state ?? 'active';
  if (state === 'pending') return { text: start ? `${dateText(start)} 起生效` : '未生效', tone: 'gold' };
  if (state === 'expired') return { text: end ? `${dateText(end)} 已到期` : '已到期', tone: 'err' };
  if (!end) return { text: '长期有效', tone: '' };
  const days = Math.ceil((Date.parse(end) - now) / DAY);
  if (days > 7) return { text: `${dateText(end)} 到期`, tone: '' };
  return { text: days > 0 ? `${days} 天后到期` : `${dateText(end)} 到期`, tone: 'gold' };
}

const termsTitle = (user: UserListItem) =>
  [
    `每月 ${user.traffic_reset_day ?? 1} 日重置`,
    user.service_starts_at ? `${timeText(user.service_starts_at)} 起` : '立即生效',
    user.service_expires_at ? `${timeText(user.service_expires_at)} 到期` : '长期有效',
  ].join(' · ') + '（UTC+8）';

/**
 * 读数栏里的「重置日」「有效期」两行。接入与转发共用这份策略，两个读数栏各放一份；
 * 可编辑时整块是一个按钮，在旁边弹出编辑面板。
 */
export function UserServiceTerms({
  user,
  editable,
  onSaved,
}: {
  user: UserListItem;
  editable: boolean;
  onSaved: () => unknown;
}) {
  const now = useNow();
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const validity = validityOf(user, now);
  const terms = [
    { key: '重置日', value: `每月 ${user.traffic_reset_day ?? 1} 日`, tone: '' },
    { key: '有效期', value: validity.text, tone: validity.tone },
  ];
  if (!editable) {
    return (
      <dl className="user-service-terms" title={termsTitle(user)}>
        {terms.map(term => (
          <div key={term.key}>
            <dt className="user-service-term-key">{term.key}</dt>
            <dd className={`user-service-term-value${term.tone ? ` ${term.tone}` : ''}`}>{term.value}</dd>
          </div>
        ))}
      </dl>
    );
  }
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="user-service-terms"
        aria-haspopup="dialog"
        aria-expanded={open}
        title={termsTitle(user)}
        onClick={() => setOpen(true)}
      >
        {terms.map(term => (
          <span key={term.key}>
            <span className="user-service-term-key">{term.key}</span>
            <span className={`user-service-term-value${term.tone ? ` ${term.tone}` : ''}`}>{term.value}</span>
          </span>
        ))}
      </button>
      {open && <PolicyPopover user={user} anchor={anchor} onSaved={onSaved} onClose={() => setOpen(false)} />}
    </>
  );
}

interface Draft {
  day: string;
  startAt: boolean;
  start: string;
  endAt: boolean;
  end: string;
}

const savedPolicyOf = (user: UserListItem): UserServicePolicyWrite => ({
  traffic_reset_day: user.traffic_reset_day ?? 1,
  service_starts_at: user.service_starts_at ?? null,
  service_expires_at: user.service_expires_at ?? null,
});

const samePolicy = (a: UserServicePolicyWrite, b: UserServicePolicyWrite) =>
  a.traffic_reset_day === b.traffic_reset_day &&
  a.service_starts_at === b.service_starts_at &&
  a.service_expires_at === b.service_expires_at;

/** 输入框只到分钟；没改动的时间沿用已保存的原值，秒数和时区写法都不变。 */
function readDraft(
  draft: Draft,
  saved: UserServicePolicyWrite,
):
  | { policy: UserServicePolicyWrite; problem: null }
  | { policy: null; problem: { field: 'day' | 'start' | 'end'; text: string } } {
  const day = Number(draft.day);
  if (!Number.isInteger(day) || day < 1 || day > 31) {
    return { policy: null, problem: { field: 'day', text: '重置日为 1–31 日' } };
  }
  if (draft.startAt && !draft.start) return { policy: null, problem: { field: 'start', text: '填写生效时间' } };
  if (draft.endAt && !draft.end) return { policy: null, problem: { field: 'end', text: '填写到期时间' } };
  const time = (on: boolean, value: string, kept: string | null) =>
    !on ? null : value === inputTime(kept) ? kept : wireTime(value);
  const start = time(draft.startAt, draft.start, saved.service_starts_at);
  const end = time(draft.endAt, draft.end, saved.service_expires_at);
  if (start && end && Date.parse(end) <= Date.parse(start)) {
    return { policy: null, problem: { field: 'end', text: '到期须晚于生效' } };
  }
  return { policy: { traffic_reset_day: day, service_starts_at: start, service_expires_at: end }, problem: null };
}

/** 停止输入一会儿再取预览：逐位改日期时不必每一步都请求。 */
function useSettled<T>(value: T, delay = 300): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setSettled(value), delay);
    return () => window.clearTimeout(timer);
  }, [value, delay]);
  return settled;
}

function PolicyPopover({
  user,
  anchor,
  onSaved,
  onClose,
}: {
  user: UserListItem;
  anchor: RefObject<HTMLElement | null>;
  onSaved: () => unknown;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState<Draft>(() => ({
    day: String(user.traffic_reset_day ?? 1),
    startAt: !!user.service_starts_at,
    start: inputTime(user.service_starts_at),
    endAt: !!user.service_expires_at,
    end: inputTime(user.service_expires_at),
  }));
  const saved = savedPolicyOf(user);
  const { policy, problem } = readDraft(draft, saved);
  const changed = policy !== null && !samePolicy(policy, saved);
  const guard = `user-service-policy:${user.tenant_id}:${user.id}`;
  const save = useMutation({
    mutationFn: (value: UserServicePolicyWrite) => updateUserServicePolicy(user.tenant_id, user.id, value),
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['user-cycles'] }),
        qc.invalidateQueries({ queryKey: ['quotas'] }),
        qc.invalidateQueries({ queryKey: ['forwarding', user.tenant_id, user.id] }),
        qc.invalidateQueries({ queryKey: ['me-user'] }),
        qc.invalidateQueries({ queryKey: ['clash-subscription', user.tenant_id, user.id] }),
        onSaved(),
      ]);
      onClose();
    },
  });
  const busy = save.isPending;
  useUnsavedChanges((policy === null || changed) && !save.isSuccess, `${user.id} 的账期与有效期`, guard);
  const draftKey = changed ? JSON.stringify(policy) : null;
  const settled = useSettled(draftKey) === draftKey;
  const preview = useQuery({
    queryKey: ['user-service-preview', user.tenant_id, user.id, policy],
    queryFn: () => previewUserServicePolicy(user.tenant_id, user.id, policy!),
    enabled: changed && settled,
    retry: false,
  });
  const set = (patch: Partial<Draft>) => setDraft(current => ({ ...current, ...patch }));
  // 续期从现在、生效时间和当前填写的到期时间三者中最晚的一个往后算，连点两次就延长两次。
  const renewFrom = () =>
    Math.max(
      Date.now(),
      draft.startAt && draft.start ? Date.parse(wireTime(draft.start)) : 0,
      draft.endAt && draft.end ? Date.parse(wireTime(draft.end)) : 0,
    );
  const renew = (unit: 'days' | 'months' | 'years') =>
    set({ endAt: true, end: localTime(extendServiceTime(renewFrom(), unit)) });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (policy && !busy) save.mutate(policy);
  };
  return (
    <DialogLayer
      mode="popover"
      anchor={anchor}
      label={`${user.id} 的账期与有效期`}
      onClose={onClose}
      canClose={() => !busy && confirmDiscardChanges(guard)}
    >
      <form className="dialog-surface user-service-pop" onSubmit={submit}>
        <header>
          <h3>账期与有效期</h3>
          <span className="user-service-tz">UTC+8</span>
        </header>
        <div className="user-service-fields">
          <div className="user-service-row">
            <span className="user-service-key">重置日</span>
            <span className="user-service-value">
              每月
              <input
                className="f mono user-service-day"
                type="number"
                min="1"
                max="31"
                step="1"
                value={draft.day}
                disabled={busy}
                aria-label="每月流量重置日"
                aria-invalid={problem?.field === 'day' || undefined}
                onChange={event => set({ day: event.target.value })}
              />
              日
            </span>
          </div>
          <div className="user-service-row">
            <span className="user-service-key">生效</span>
            <span className="user-service-value">
              <SegmentedControl
                value={draft.startAt}
                options={[
                  { value: false, label: '立即' },
                  { value: true, label: '指定时间' },
                ]}
                disabled={busy}
                ariaLabel="生效方式"
                onChange={startAt =>
                  set({ startAt, start: draft.start || `${localTime(Date.now()).slice(0, 10)}T00:00` })
                }
              />
              {draft.startAt && (
                <input
                  className="f mono user-service-time"
                  type="datetime-local"
                  value={draft.start}
                  disabled={busy}
                  aria-label="生效时间（UTC+8）"
                  aria-invalid={problem?.field === 'start' || undefined}
                  onChange={event => set({ start: event.target.value })}
                />
              )}
            </span>
          </div>
          <div className="user-service-row">
            <span className="user-service-key">到期</span>
            <span className="user-service-value">
              <SegmentedControl
                value={draft.endAt}
                options={[
                  { value: false, label: '长期' },
                  { value: true, label: '指定时间' },
                ]}
                disabled={busy}
                ariaLabel="到期方式"
                onChange={endAt =>
                  set({ endAt, end: draft.end || localTime(extendServiceTime(renewFrom(), 'months')) })
                }
              />
              {draft.endAt && (
                <input
                  className="f mono user-service-time"
                  type="datetime-local"
                  value={draft.end}
                  disabled={busy}
                  aria-label="到期时间（UTC+8）"
                  aria-invalid={problem?.field === 'end' || undefined}
                  onChange={event => set({ end: event.target.value })}
                />
              )}
              <span className="user-service-renew" role="group" aria-label="续期">
                <button type="button" className="btn sm" disabled={busy} onClick={() => renew('days')}>
                  +30 天
                </button>
                <button type="button" className="btn sm" disabled={busy} onClick={() => renew('months')}>
                  +1 个月
                </button>
                <button type="button" className="btn sm" disabled={busy} onClick={() => renew('years')}>
                  +1 年
                </button>
              </span>
            </span>
          </div>
        </div>
        {problem && (
          <p className="user-service-problem" role="alert">
            {problem.text}
          </p>
        )}
        <div className="user-service-preview" aria-live="polite">
          {changed &&
            (preview.data ? (
              <PreviewLine preview={preview.data} before={user.service_state ?? 'active'} />
            ) : preview.error ? (
              <ErrorBox error={preview.error} />
            ) : (
              <FieldLoading label="读取预览" announce={false} />
            ))}
        </div>
        {save.error && <ErrorBox error={save.error} />}
        <footer>
          <DialogClose className="btn" disabled={busy}>
            取消
          </DialogClose>
          {/* 没有改动也能保存：服务端照常立即重新评估该用户的有效期和额度。 */}
          <button type="submit" className="btn primary" disabled={!policy || busy}>
            {busy ? '保存中…' : '保存'}
          </button>
        </footer>
      </form>
    </DialogLayer>
  );
}

/** 新账期的起止、按日账本重算的两项用量，以及服务会不会因此暂停或恢复。 */
function PreviewLine({ preview, before }: { preview: UserServicePolicyPreview; before: UserServiceState }) {
  const day = (value: string) => inputTime(value).slice(5, 10);
  const serving = preview.service_state === 'active';
  const shift =
    serving === (before === 'active')
      ? null
      : serving
        ? { text: before === 'pending' ? '立即生效' : '恢复服务', tone: 'ok' }
        : { text: '暂停服务', tone: 'err' };
  return (
    <>
      <span>
        本期 {day(preview.starts_at)} → {day(preview.ends_at)}
      </span>
      <span>接入 {bytes(preview.access_used_bytes)}</span>
      <span>转发 {bytes(preview.forwarding_used_bytes)}</span>
      {shift && <span className={`user-service-shift ${shift.tone}`}>{shift.text}</span>}
    </>
  );
}
