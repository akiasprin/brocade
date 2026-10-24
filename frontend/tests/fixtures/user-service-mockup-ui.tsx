// Parts of user-service-mockup: one shared 账期与有效期 editor and the four placements around it.
// Synthetic only; never imported by the app.
import { useEffect, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import type { UserServiceState } from '../../src/api';
import { SegmentedControl } from '../../src/ui/bits';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { bytes } from '../../src/ui/format';
import { Icon } from '../../src/ui/icons';
import {
  DAY,
  daysUntil,
  extend,
  fromInput,
  inputTime,
  md,
  midnight,
  periodOf,
  previewOf,
  resetsBetween,
  stateOf,
  ymd,
  type Policy,
  type PolicyPreview,
} from './user-service-mockup-data';

/* ══ 读数 ══ */

export type Tone = '' | 'gold' | 'err';

/** 一句话的有效期：常态写到期日，临近、未生效和已到期才上色。 */
export function validityOf(policy: Policy, now: number): { text: string; tone: Tone } {
  const state = stateOf(policy, now);
  if (state === 'pending') return { text: `${ymd(policy.start!)} 起生效`, tone: 'gold' };
  if (state === 'expired') return { text: `${ymd(policy.end!)} 已到期`, tone: 'err' };
  if (policy.end === null) return { text: '长期有效', tone: '' };
  const days = daysUntil(policy.end, now);
  return days <= 7 ? { text: `${days} 天后到期`, tone: 'gold' } : { text: `${ymd(policy.end)} 到期`, tone: '' };
}

export const resetText = (policy: Policy) => `每月 ${policy.resetDay} 日重置`;

export const termTitle = (policy: Policy) =>
  [
    resetText(policy),
    policy.start === null ? '立即生效' : `${ymd(policy.start)} 起`,
    policy.end === null ? '长期有效' : `${ymd(policy.end)} 到期`,
  ].join(' · ') + '（UTC+8）';

/* ══ 共用编辑器 ══ */

interface Draft {
  day: string;
  startAt: boolean;
  start: string;
  endAt: boolean;
  end: string;
}

const draftOf = (policy: Policy): Draft => ({
  day: String(policy.resetDay),
  startAt: policy.start !== null,
  start: inputTime(policy.start),
  endAt: policy.end !== null,
  end: inputTime(policy.end),
});

/** 输入框只到分钟；没改动的时间沿用已保存的原值，秒数不丢。 */
function readDraft(
  draft: Draft,
  saved: Policy,
): { policy: Policy | null; problem: string | null; field: string | null } {
  const day = Number(draft.day);
  if (!Number.isInteger(day) || day < 1 || day > 31) return { policy: null, problem: '重置日为 1–31 日', field: 'day' };
  if (draft.startAt && !draft.start) return { policy: null, problem: '填写生效时间', field: 'start' };
  if (draft.endAt && !draft.end) return { policy: null, problem: '填写到期时间', field: 'end' };
  const time = (on: boolean, value: string, kept: number | null) =>
    !on ? null : value === inputTime(kept) ? kept : fromInput(value);
  const start = time(draft.startAt, draft.start, saved.start);
  const end = time(draft.endAt, draft.end, saved.end);
  if (start !== null && end !== null && end <= start) return { policy: null, problem: '到期须晚于生效', field: 'end' };
  return { policy: { resetDay: day, start, end }, problem: null, field: null };
}

const samePolicy = (a: Policy, b: Policy) => a.resetDay === b.resetDay && a.start === b.start && a.end === b.end;

export function PolicyEditor({
  saved,
  served,
  now,
  layout,
  head,
  onCancel,
  onSaved,
}: {
  saved: Policy;
  /** 实际有服务的区间：日账本只在这段时间里有流量，预览按它重算。 */
  served: Pick<Policy, 'start' | 'end'>;
  now: number;
  /** stack：弹出面板与对话框里竖排；band：页头横带里横排。 */
  layout: 'stack' | 'band';
  head?: ReactNode;
  onCancel: () => void;
  onSaved: (policy: Policy) => void;
}) {
  const [draft, setDraft] = useState(() => draftOf(saved));
  const [busy, setBusy] = useState(false);
  const { policy, problem, field } = readDraft(draft, saved);
  const changed = policy !== null && !samePolicy(policy, saved);
  const set = (patch: Partial<Draft>) => setDraft(current => ({ ...current, ...patch }));
  // 续期从现在、生效时间和当前填写的到期时间三者中最晚的一个往后算；连点两次就是两个月。
  const anchor = () =>
    Math.max(
      now,
      draft.startAt && draft.start ? fromInput(draft.start) : 0,
      draft.endAt && draft.end ? fromInput(draft.end) : 0,
    );
  const renew = (unit: 'days' | 'months' | 'years') => set({ endAt: true, end: inputTime(extend(anchor(), unit)) });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!policy || busy) return;
    setBusy(true);
    window.setTimeout(() => onSaved(policy), 420);
  };
  return (
    <form className={`usm-editor ${layout}`} aria-label="账期与有效期" onSubmit={submit}>
      {head}
      <div className="usm-fields">
        <div className="usm-row">
          <span className="usm-k">重置日</span>
          <span className="usm-v">
            每月
            <input
              className="f mono usm-day"
              type="number"
              min={1}
              max={31}
              step={1}
              value={draft.day}
              disabled={busy}
              aria-label="每月流量重置日"
              aria-invalid={field === 'day' || undefined}
              onChange={event => set({ day: event.target.value })}
            />
            日
          </span>
        </div>
        <div className="usm-row">
          <span className="usm-k">生效</span>
          <span className="usm-v">
            <SegmentedControl
              value={draft.startAt}
              options={[
                { value: false, label: '立即' },
                { value: true, label: '指定时间' },
              ]}
              disabled={busy}
              ariaLabel="生效方式"
              onChange={startAt => set({ startAt, start: draft.start || inputTime(midnight(now)) })}
            />
            {draft.startAt && (
              <input
                className="f mono usm-time"
                type="datetime-local"
                value={draft.start}
                disabled={busy}
                aria-label="生效时间（UTC+8）"
                aria-invalid={field === 'start' || undefined}
                onChange={event => set({ start: event.target.value })}
              />
            )}
          </span>
        </div>
        <div className="usm-row">
          <span className="usm-k">到期</span>
          <span className="usm-v">
            <SegmentedControl
              value={draft.endAt}
              options={[
                { value: false, label: '长期' },
                { value: true, label: '指定时间' },
              ]}
              disabled={busy}
              ariaLabel="到期方式"
              onChange={endAt => set({ endAt, end: draft.end || inputTime(extend(anchor(), 'months')) })}
            />
            {draft.endAt && (
              <input
                className="f mono usm-time"
                type="datetime-local"
                value={draft.end}
                disabled={busy}
                aria-label="到期时间（UTC+8）"
                aria-invalid={field === 'end' || undefined}
                onChange={event => set({ end: event.target.value })}
              />
            )}
            <span className="usm-renew" role="group" aria-label="续期">
              <button type="button" disabled={busy} onClick={() => renew('days')}>
                +30 天
              </button>
              <button type="button" disabled={busy} onClick={() => renew('months')}>
                +1 个月
              </button>
              <button type="button" disabled={busy} onClick={() => renew('years')}>
                +1 年
              </button>
            </span>
          </span>
        </div>
      </div>
      {problem && (
        <p className="usm-problem" role="alert">
          {problem}
        </p>
      )}
      {changed && policy && <PreviewLine preview={previewOf(policy, now, served)} before={stateOf(saved, now)} />}
      <footer className="usm-foot">
        {layout === 'band' && <span className="usm-tz">UTC+8</span>}
        <button type="button" className="btn" disabled={busy} onClick={onCancel}>
          取消
        </button>
        {/* 没有改动也能保存：服务端照常重新评估一次，界面不拦。 */}
        <button type="submit" className="btn primary" disabled={!policy || busy}>
          {busy ? '保存中…' : '保存'}
        </button>
      </footer>
    </form>
  );
}

/** 只在有改动时出现：新账期的起止、按日账本重算的两项用量，以及服务会不会因此暂停或恢复。 */
function PreviewLine({ preview, before }: { preview: PolicyPreview; before: UserServiceState }) {
  const shift =
    preview.state === before
      ? null
      : preview.state === 'active'
        ? { text: before === 'pending' ? '立即生效' : '恢复服务', tone: 'ok' }
        : { text: '暂停服务', tone: 'err' };
  return (
    <p className="usm-preview" aria-live="polite">
      <span>
        本期 {md(preview.start)} → {md(preview.end)}
      </span>
      <span>接入 {bytes(preview.access)}</span>
      <span>转发 {bytes(preview.forwarding)}</span>
      {shift && <span className={`usm-shift ${shift.tone}`}>{shift.text}</span>}
    </p>
  );
}

function EditorHead({ title, sub, close }: { title: string; sub?: string; close?: ReactNode }) {
  return (
    <header className="usm-head">
      <span>
        <b>{title}</b>
        {sub && <small className="mono">{sub}</small>}
      </span>
      <span className="usm-tz">UTC+8</span>
      {close}
    </header>
  );
}

/* ══ 弹出面板：A、B 共用 ══ */

export function Popover({
  anchor,
  side,
  width = 392,
  label,
  onClose,
  children,
}: {
  anchor: RefObject<HTMLElement | null>;
  /** below：贴在锚点下方左对齐；right：贴在锚点右侧顶对齐，放不下时退回 below。 */
  side: 'below' | 'right';
  width?: number;
  label: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState<{ top: number; left: number; width: number } | null>(null);
  const closeRef = useRef(onClose);
  useLayoutEffect(() => {
    closeRef.current = onClose;
  }, [onClose]);
  useLayoutEffect(() => {
    const measure = () => {
      const target = anchor.current;
      const panel = ref.current;
      if (!target || !panel) return;
      const a = target.getBoundingClientRect();
      const frame = target.closest('.mk-frame')?.getBoundingClientRect() ?? new DOMRect(0, 0, innerWidth, innerHeight);
      const right = Math.min(frame.right, innerWidth) - 8;
      const left = Math.max(frame.left, 0) + 8;
      const w = Math.min(width, right - left);
      const h = panel.offsetHeight;
      if (side === 'right' && a.right + 12 + w <= right) {
        setPlace({ top: Math.max(8, Math.min(a.top - 10, innerHeight - 8 - h)), left: a.right + 12, width: w });
        return;
      }
      const below = a.bottom + 6 + h <= innerHeight - 8 || a.top - 6 - h < 8;
      setPlace({
        top: below ? a.bottom + 6 : a.top - 6 - h,
        left: Math.max(left, Math.min(a.left, right - w)),
        width: w,
      });
    };
    measure();
    addEventListener('resize', measure);
    addEventListener('scroll', measure, true);
    const observer = new ResizeObserver(measure);
    if (ref.current) observer.observe(ref.current);
    return () => {
      removeEventListener('resize', measure);
      removeEventListener('scroll', measure, true);
      observer.disconnect();
    };
  }, [anchor, side, width]);
  const placed = !!place;
  useEffect(() => {
    if (placed) ref.current?.querySelector<HTMLElement>('input:not(:disabled), button:not(:disabled)')?.focus();
  }, [placed]);
  useEffect(() => {
    const away = (event: PointerEvent) => {
      const node = event.target as Node;
      if (ref.current?.contains(node) || anchor.current?.contains(node)) return;
      closeRef.current();
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') closeRef.current();
    };
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('pointerdown', away);
      document.removeEventListener('keydown', escape);
    };
  }, [anchor]);
  useLayoutEffect(() => {
    const target = anchor.current;
    const panel = ref.current;
    return () => {
      if (target?.isConnected && panel?.contains(document.activeElement)) target.focus();
    };
  }, [anchor]);
  return createPortal(
    <div
      ref={ref}
      className="usm-pop"
      role="dialog"
      aria-label={label}
      style={{
        top: place?.top ?? -9999,
        left: place?.left ?? -9999,
        width: place?.width ?? width,
        visibility: place ? 'visible' : 'hidden',
      }}
    >
      {children}
    </div>,
    document.body,
  );
}

export function PopoverEditor({
  anchor,
  side,
  saved,
  served,
  now,
  userId,
  onClose,
  onSaved,
}: {
  anchor: RefObject<HTMLElement | null>;
  side: 'below' | 'right';
  saved: Policy;
  served: Pick<Policy, 'start' | 'end'>;
  now: number;
  userId: string;
  onClose: () => void;
  onSaved: (policy: Policy) => void;
}) {
  return (
    <Popover anchor={anchor} side={side} label={`${userId} 的账期与有效期`} onClose={onClose}>
      <PolicyEditor
        saved={saved}
        served={served}
        now={now}
        layout="stack"
        head={<EditorHead title="账期与有效期" />}
        onCancel={onClose}
        onSaved={onSaved}
      />
    </Popover>
  );
}

/* ══ 方案 A：页头徽标 ══ */

export function TermChip({
  policy,
  now,
  editable,
  open,
  chipRef,
  onOpen,
  quietWhenNormal = false,
}: {
  policy: Policy;
  now: number;
  editable: boolean;
  open: boolean;
  chipRef: RefObject<HTMLButtonElement | null>;
  onOpen: () => void;
  /** 方案 D：长期有效与远期到期都不占页头，只在未生效、已到期或七天内到期时出现。 */
  quietWhenNormal?: boolean;
}) {
  const { text, tone } = validityOf(policy, now);
  if (quietWhenNormal && !tone) return null;
  const className = `usm-chip${tone ? ` ${tone}` : ''}`;
  if (!editable) {
    return (
      <span className={`${className} static`} title={termTitle(policy)}>
        <Icon of="calendar" size={12} />
        {text}
      </span>
    );
  }
  return (
    <button
      ref={chipRef}
      type="button"
      className={className}
      aria-haspopup="dialog"
      aria-expanded={open}
      title={termTitle(policy)}
      onClick={onOpen}
    >
      <Icon of="calendar" size={12} />
      {text}
    </button>
  );
}

/* ══ 方案 B：读数栏 ══ */

export function LedgerTerms({
  policy,
  now,
  editable,
  open,
  termsRef,
  onOpen,
}: {
  policy: Policy;
  now: number;
  editable: boolean;
  open: boolean;
  termsRef: RefObject<HTMLButtonElement | null>;
  onOpen: () => void;
}) {
  const { text, tone } = validityOf(policy, now);
  const rows = (
    <>
      <span className="usm-term">
        <span className="usm-term-k">重置日</span>
        <span className="usm-term-v">每月 {policy.resetDay} 日</span>
      </span>
      <span className="usm-term">
        <span className="usm-term-k">有效期</span>
        <span className={`usm-term-v${tone ? ` ${tone}` : ''}`}>{text}</span>
      </span>
    </>
  );
  if (!editable) return <div className="usm-terms static">{rows}</div>;
  return (
    <button
      ref={termsRef}
      type="button"
      className="usm-terms"
      aria-haspopup="dialog"
      aria-expanded={open}
      title={termTitle(policy)}
      onClick={onOpen}
    >
      {rows}
      <Icon of="pencil" size={12} className="usm-terms-edit" />
    </button>
  );
}

/* ══ 方案 C：页头时间轴 ══ */

interface Track {
  span: [number, number];
  elapsed: number;
  ticks: number[];
  now: number;
  openStart: boolean;
  openEnd: boolean;
  startMark: number | null;
  endMark: number | null;
}

/** 横轴：从生效（或本期起点）到到期；长期有效时画到两期之后并淡出。刻度是每次重置。 */
function trackOf(policy: Policy, now: number): Track {
  const period = periodOf(policy.resetDay, now);
  const from = Math.min(policy.start ?? period.start, period.start, now);
  const later = periodOf(policy.resetDay, periodOf(policy.resetDay, period.end).end).end;
  const to = policy.end === null ? later : Math.max(policy.end, now + (policy.end <= now ? 3 * DAY : 0));
  const x = (t: number) => Math.max(0, Math.min(100, ((t - from) / (to - from)) * 100));
  const spanStart = policy.start ?? from;
  const spanEnd = policy.end ?? to;
  return {
    span: [x(spanStart), x(spanEnd)],
    elapsed: x(Math.max(spanStart, Math.min(now, spanEnd))),
    ticks: resetsBetween(policy.resetDay, from, to)
      .filter(t => t < to)
      .map(x),
    now: x(now),
    openStart: policy.start === null,
    openEnd: policy.end === null,
    startMark: policy.start !== null && policy.start > now ? x(policy.start) : null,
    endMark: policy.end === null ? null : x(policy.end),
  };
}

export function TermBand({
  policy,
  now,
  editable,
  editing,
  onEdit,
  editor,
}: {
  policy: Policy;
  now: number;
  editable: boolean;
  editing: boolean;
  onEdit: () => void;
  editor: ReactNode;
}) {
  const { text, tone } = validityOf(policy, now);
  const track = trackOf(policy, now);
  const state = stateOf(policy, now);
  const left = policy.end !== null && state === 'active' ? daysUntil(policy.end, now) : null;
  // 未生效时上色的是生效竖条，到期竖条保持中性。
  const endTone = state === 'pending' ? '' : tone;
  return (
    <div className={`usm-band${editing ? ' editing' : ''}`}>
      <div className="usm-band-row">
        <span className="usm-band-rule">{resetText(policy)}</span>
        <div className="usm-track" role="img" aria-label={termTitle(policy)} title={termTitle(policy)}>
          <i
            className={`usm-track-span${track.openEnd ? ' open-end' : ''}${track.openStart ? ' open-start' : ''}`}
            style={{ left: `${track.span[0]}%`, width: `${track.span[1] - track.span[0]}%` }}
          />
          <i
            className="usm-track-elapsed"
            style={{ left: `${track.span[0]}%`, width: `${track.elapsed - track.span[0]}%` }}
          />
          {track.ticks.map(tick => (
            <i key={tick} className="usm-track-tick" style={{ left: `${tick}%` }} />
          ))}
          {track.startMark !== null && <i className="usm-track-mark gold" style={{ left: `${track.startMark}%` }} />}
          {track.endMark !== null && (
            <i className={`usm-track-mark${endTone ? ` ${endTone}` : ''}`} style={{ left: `${track.endMark}%` }} />
          )}
          <i className="usm-track-now" style={{ left: `${track.now}%` }} />
        </div>
        <span className={`usm-band-end${tone ? ` ${tone}` : ''}`}>
          {text}
          {left !== null && left > 7 && <small>剩 {left} 天</small>}
        </span>
        {editable && !editing && (
          <button type="button" className="btn usm-band-edit" onClick={onEdit}>
            <Icon of="pencil" size={12} />
            调整
          </button>
        )}
      </div>
      {editing && editor}
    </div>
  );
}

export function BandEditor(props: {
  saved: Policy;
  served: Pick<Policy, 'start' | 'end'>;
  now: number;
  onCancel: () => void;
  onSaved: (policy: Policy) => void;
}) {
  return <PolicyEditor {...props} layout="band" />;
}

/* ══ 方案 D：「更多」菜单 + 对话框 ══ */

export function MenuEntry({ policy, now, onOpen }: { policy: Policy; now: number; onOpen: () => void }) {
  const { text, tone } = validityOf(policy, now);
  return (
    <>
      <button role="menuitem" className="usm-menu-entry" onClick={onOpen}>
        <Icon of="calendar" size={14} className="user-action-menu-icon" />
        <span>
          账期与有效期
          <small>
            每月 {policy.resetDay} 日重置 · <span className={tone || undefined}>{text}</span>
          </small>
        </span>
      </button>
      <hr />
    </>
  );
}

export function PolicyDialog({
  userId,
  saved,
  served,
  now,
  onClose,
  onSaved,
}: {
  userId: string;
  saved: Policy;
  served: Pick<Policy, 'start' | 'end'>;
  now: number;
  onClose: () => void;
  onSaved: (policy: Policy) => void;
}) {
  return (
    <DialogLayer label={`${userId} 的账期与有效期`} onClose={onClose}>
      <section className="dialog-surface usm-dialog">
        <PolicyEditor
          saved={saved}
          served={served}
          now={now}
          layout="stack"
          head={
            <EditorHead
              title="账期与有效期"
              sub={userId}
              close={
                <DialogClose aria-label="关闭" title="关闭">
                  <Icon of="close" size={14} />
                </DialogClose>
              }
            />
          }
          onCancel={onClose}
          onSaved={onSaved}
        />
      </section>
    </DialogLayer>
  );
}
