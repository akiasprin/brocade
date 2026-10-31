import { useId, useState, type CSSProperties, type FormEvent } from 'react';
import { InlineField } from '../ui/inline-field';
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import {
  fetchUserTrafficPolicies,
  saveUserTrafficPolicy,
  type UserListItem,
  type UserTrafficPolicy,
  type UserTrafficPolicyView,
  type Whoami,
} from '../api';
import { ErrorBox, SegmentedControl } from '../ui/bits';
import { useSession, isPublic } from '../session';
import { useUnsavedChanges } from '../ui/navigation-guard';
import './user-traffic-policy.css';

export const userTrafficPolicyKey = (user: Pick<UserListItem, 'tenant_id' | 'id'>) =>
  ['user-traffic-policy', user.tenant_id, user.id] as const;
const canReadUserTrafficPolicies = (who: Whoami, user?: UserListItem) =>
  !!user &&
  !isPublic(who) &&
  (who.role !== 'user' || (who.self_user?.tenant_id === user.tenant_id && who.self_user.user_id === user.id));
const userTrafficPolicyQuery = (user?: UserListItem) => ({
  queryKey: user ? userTrafficPolicyKey(user) : ['user-traffic-policy', 'none'],
  queryFn: () => {
    if (!user) throw new Error('没有选择用户');
    return fetchUserTrafficPolicies(user.tenant_id, user.id);
  },
  retry: false as const,
  retryOnMount: false as const,
  refetchInterval: 30_000,
});
export function prefetchUserTrafficPolicies(client: QueryClient, who: Whoami, user: UserListItem) {
  if (!canReadUserTrafficPolicies(who, user)) return;
  return client.prefetchQuery(userTrafficPolicyQuery(user));
}
export function useUserTrafficPolicies(user?: UserListItem) {
  const { who } = useSession();
  const readable = canReadUserTrafficPolicies(who, user);
  const query = useQuery({ ...userTrafficPolicyQuery(user), enabled: readable });
  // Disabling a query does not hide cached administrator data when another identity renders it.
  return { ...query, readable, data: readable ? query.data : undefined, error: readable ? query.error : null };
}
const rateText = (rate: number) => (rate >= 1_000_000 ? `${rate / 1_000_000} Mbps` : `${rate / 1000} Kbps`);
const baseRateText = (view: UserTrafficPolicyView) =>
  view.policy?.base_rate_bps ? `常态限速 ${rateText(view.policy.base_rate_bps)}` : '';
export const userTrafficPolicyState = (view?: UserTrafficPolicyView) => {
  if (!view) return '';
  if (!view.service_available) return '服务已暂停';
  if (!view.policy) return '';
  const action = view.policy.exhausted_action;
  const base = baseRateText(view);
  if (view.policy.quota_bytes === null) return base;
  const rate =
    view.effective?.mode === 'limited'
      ? view.effective.rate_bps
      : action.kind === 'throttle'
        ? Math.min(view.policy.base_rate_bps ?? Infinity, action.rate_bps)
        : 0;
  const quota = view.exhausted
    ? action.kind === 'throttle'
      ? `限速中 · ${rateText(rate)}`
      : '已停用'
    : action.kind === 'throttle'
      ? `用满后限速 ${rateText(action.rate_bps)}`
      : '用满后停用';
  return [base, quota].filter(Boolean).join(' · ');
};

/** The quota boundary stays at the allowance when the track expands to include overflow. */
export function UserTrafficQuotaMeter({
  used,
  limit,
  view,
}: {
  used: number | null;
  limit: number | null;
  view?: UserTrafficPolicyView;
}) {
  const paused = view?.service_available === false;
  const action = view?.policy?.exhausted_action;
  const pending = view?.pending_nodes?.length ?? 0;
  const label = paused ? '服务暂停' : userTrafficPolicyState(view);
  const tone = paused ? 'idle' : view?.exhausted ? (action?.kind === 'throttle' ? 'warn' : 'err') : '';
  const status = (label || pending > 0) && (
    <span className={`qta-policy-flag ${tone}`}>
      {label}
      {pending > 0 && (
        <span className="qta-policy-sync" title={`等待 ${pending} 台机器同步`}>
          <svg className="qta-policy-spin" viewBox="0 0 12 12" aria-hidden="true">
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
          同步中
        </span>
      )}
    </span>
  );
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
export function UserAppTrafficPolicyEditor({
  user,
  view,
  label,
  onClose,
}: {
  user: UserListItem;
  view: UserTrafficPolicyView;
  label: string;
  onClose: () => void;
}) {
  return <PolicyEditor user={user} view={view} label={label} onClose={onClose} />;
}
function PolicyEditor({
  user,
  view,
  label,
  onClose,
}: {
  user: UserListItem;
  view: UserTrafficPolicyView;
  label: string;
  onClose: () => void;
}) {
  const id = useId();
  const client = useQueryClient();
  const original = view.policy;
  const initialQuota = original?.quota_bytes ? String(Number(original.quota_bytes) / 1024 ** 3) : '';
  const initialAction = original?.exhausted_action.kind ?? 'suspend';
  const initialRate =
    original?.exhausted_action.kind === 'throttle' ? String(original.exhausted_action.rate_bps / 1_000_000) : '5';
  const [quota, setQuota] = useState(initialQuota);
  const [base, setBase] = useState(rateDraft(original?.base_rate_bps));
  const [excess, setExcess] = useState(
    rateDraft(original?.exhausted_action.kind === 'throttle' ? original.exhausted_action.rate_bps : 5_000_000),
  );
  const [action, setAction] = useState<'suspend' | 'throttle'>(initialAction);
  const rate = String(rateBps(excess) / 1_000_000);
  const baseChanged = (base.value.trim() === '' ? null : rateBps(base)) !== (original?.base_rate_bps ?? null);
  const hasQuota = quota.trim() !== '';
  const baseSupported = view.base_rate_supported === true;
  const guard = `user-traffic-policy:${user.tenant_id}:${user.id}:${view.kind}:${view.app_id ?? ''}`;
  const dirty =
    quota !== initialQuota ||
    baseChanged ||
    action !== initialAction ||
    (hasQuota && action === 'throttle' && rate !== initialRate) ||
    !original;
  const clear = useUnsavedChanges(
    quota !== initialQuota ||
      baseChanged ||
      action !== initialAction ||
      (hasQuota && action === 'throttle' && rate !== initialRate),
    label,
    guard,
  );
  const quotaValid =
    quota === initialQuota ||
    quota.trim() === '' ||
    (/^\d+(?:\.\d{1,2})?$/.test(quota.trim()) &&
      Number(quota) > 0 &&
      Number(quota) * 1024 ** 3 <= Number.MAX_SAFE_INTEGER);
  const rateValid = validRate(excess);
  const baseValid = base.value.trim() === '' || validRate(base);
  const unsupported =
    (base.value.trim() !== '' || (action === 'throttle' && hasQuota)) && view.unsupported_nodes.length > 0;
  const save = useMutation({
    mutationFn: () => {
      const policy: UserTrafficPolicy = {
        ...(baseSupported ? { base_rate_bps: base.value.trim() === '' ? null : rateBps(base) } : {}),
        quota_bytes:
          quota === initialQuota && original
            ? original.quota_bytes
            : quota.trim() === ''
              ? null
              : String(Math.round(Number(quota) * 1024 ** 3)),
        exhausted_action:
          action === 'suspend'
            ? { kind: 'suspend' }
            : { kind: 'throttle', rate_bps: Math.round(Number(rate) * 1_000_000) },
      };
      return saveUserTrafficPolicy(user.tenant_id, user.id, view, policy);
    },
    onSuccess: async data => {
      client.setQueryData(userTrafficPolicyKey(user), data);
      await Promise.all([
        client.invalidateQueries({ queryKey: ['quotas'] }),
        client.invalidateQueries({ queryKey: ['user-cycles'] }),
        client.invalidateQueries({ queryKey: ['snapshot'] }),
      ]);
      clear();
      onClose();
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (
      dirty &&
      quotaValid &&
      baseValid &&
      (!hasQuota || action !== 'throttle' || rateValid) &&
      !unsupported &&
      !save.isPending
    )
      save.mutate();
  };
  const cancel = () => {
    if (!save.isPending) {
      clear();
      onClose();
    }
  };
  const quotaInput = (
    <input
      id={`${id}-quota`}
      className="f mono fwe-num quota"
      aria-label="本期流量额度（GiB）"
      autoFocus
      inputMode="decimal"
      autoComplete="off"
      value={quota}
      placeholder="不限"
      disabled={save.isPending}
      aria-invalid={!quotaValid || undefined}
      onChange={event => setQuota(event.target.value)}
    />
  );
  const actionControl = (
    <SegmentedControl
      value={action}
      ariaLabel="额度用满后的处理"
      disabled={save.isPending}
      options={[
        { value: 'suspend', label: '停用' },
        { value: 'throttle', label: '限速' },
      ]}
      onChange={setAction}
    />
  );
  const saveButton = (
    <button
      className="btn primary"
      disabled={
        !dirty ||
        !quotaValid ||
        !baseValid ||
        (hasQuota && action === 'throttle' && !rateValid) ||
        unsupported ||
        save.isPending
      }
    >
      {save.isPending ? '保存中…' : '保存'}
    </button>
  );
  return (
    <form
      className="fwe-form inline user-traffic-policy-editor"
      aria-label={label}
      onSubmit={submit}
      onKeyDown={event => {
        if (event.key === 'Escape' && !save.isPending) {
          event.preventDefault();
          event.stopPropagation();
          cancel();
        }
      }}
    >
      <div className="fwe-fields">
        <InlineField label="流量配额" htmlFor={`${id}-quota`}>
          <span className="fwe-unit">
            {quotaInput}
            <i>GiB</i>
          </span>
          <small className={!quotaValid ? 'bad' : undefined} role={!quotaValid ? 'alert' : undefined}>
            {quotaValid ? '留空表示不限' : '请输入正数，最多两位小数'}
          </small>
        </InlineField>
        <InlineField label="常态限速" htmlFor={`${id}-base`}>
          <SpeedInput
            id={`${id}-base`}
            label="常态"
            draft={base}
            onChange={setBase}
            disabled={save.isPending || !baseSupported}
            optional
          />
          <small className={!baseValid ? 'bad' : undefined} role={!baseValid ? 'alert' : undefined}>
            {!baseSupported
              ? '请升级 Console，以支持常态限速。'
              : baseValid
                ? '立即限速，上下行跨机器合计；留空表示不限。'
                : '速度需在 1 Kbps–1 Tbps 之间，精确到 1 bps。'}
          </small>
        </InlineField>
        {hasQuota && <InlineField label="用满后">{actionControl}</InlineField>}
        {hasQuota && action === 'throttle' && (
          <InlineField label="超额速度" htmlFor={`${id}-rate`}>
            <SpeedInput id={`${id}-rate`} label="超额" draft={excess} onChange={setExcess} disabled={save.isPending} />
            <small className={!rateValid ? 'bad' : undefined} role={!rateValid ? 'alert' : undefined}>
              {rateValid ? '用满后与常态上限取较低值。' : '速度需在 1 Kbps–1 Tbps 之间，精确到 1 bps。'}
            </small>
          </InlineField>
        )}
      </div>
      {unsupported && (
        <p className="fwe-problem" role="alert">
          请先升级相关机器的 Agent 和 Xray，等待用户限速能力确认：{view.unsupported_nodes.join('、')}。
        </p>
      )}
      {save.error && <ErrorBox error={save.error} />}
      <footer className="fwe-foot">
        <span className="sp" />
        <button type="button" className="btn" disabled={save.isPending} onClick={cancel}>
          取消
        </button>
        {saveButton}
      </footer>
    </form>
  );
}

type RateDraft = { value: string; unit: 'Mbps' | 'Kbps' };
const rateDraft = (bps?: number | null): RateDraft => ({
  value: bps == null ? '' : String(bps / (bps >= 1_000_000 ? 1_000_000 : 1000)),
  unit: bps != null && bps < 1_000_000 ? 'Kbps' : 'Mbps',
});
const rateBps = (draft: RateDraft) => Math.round(Number(draft.value) * (draft.unit === 'Mbps' ? 1_000_000 : 1000));
const validRate = (draft: RateDraft) =>
  (draft.unit === 'Mbps' ? /^\d+(?:\.\d{1,6})?$/ : /^\d+(?:\.\d{1,3})?$/).test(draft.value.trim()) &&
  rateBps(draft) >= 1000 &&
  rateBps(draft) <= 1_000_000_000_000;
function SpeedInput({
  id,
  label,
  draft,
  onChange,
  disabled,
  optional = false,
}: {
  id: string;
  label: string;
  draft: RateDraft;
  onChange: (value: RateDraft) => void;
  disabled: boolean;
  optional?: boolean;
}) {
  return (
    <span className="fwe-unit">
      <input
        id={id}
        className="f mono fwe-num"
        aria-label={`${label}速度上限（${draft.unit}）`}
        inputMode="decimal"
        autoComplete="off"
        placeholder={optional ? '不限' : undefined}
        disabled={disabled}
        value={draft.value}
        aria-invalid={(!(optional && draft.value.trim() === '') && !validRate(draft)) || undefined}
        onChange={event => onChange({ ...draft, value: event.target.value })}
      />
      <select
        className="f"
        aria-label={`${label}速度单位`}
        value={draft.unit}
        disabled={disabled}
        onChange={event => {
          const unit = event.target.value as RateDraft['unit'];
          const value =
            draft.value.trim() === '' || !Number.isFinite(Number(draft.value))
              ? draft.value
              : String(Number((Number(draft.value) * (unit === 'Kbps' ? 1000 : 0.001)).toPrecision(15)));
          onChange({ value, unit });
        }}
      >
        <option value="Mbps">Mbps</option>
        <option value="Kbps">Kbps</option>
      </select>
    </span>
  );
}
