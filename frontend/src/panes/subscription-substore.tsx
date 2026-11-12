import { SubscriptionLogo } from './subscription-logo';
import { createContext, useCallback, useContext, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchSubStore,
  saveSubStore,
  testSubStore,
  removeSubStore,
  startSubStore,
  fetchSubStoreRuns,
  fetchSubStoreRun,
  cancelSubStoreRun,
  type SubStoreView,
  type SubStoreRun,
  type SubStoreWrite,
  type SubStoreRunScope,
  type ClashSubscriptionInfo,
} from '../api';
import { bytes } from '../ui/format';
import { Icon, type IconName } from '../ui/icons';
import { ErrorBox, Loading, Confirm, SegmentedControl } from '../ui/bits';
import { ChoiceSelect, ActionMenu } from '../ui/choice-menu';
import { ConfigSecretInput } from '../ui/config-secret-input';
import {
  activeRun,
  checkpoints,
  dayText,
  resultText,
  reasonText,
  runResultText,
  runSummary,
  SUBSTORE_TITLE,
  syncStatus,
  taskText,
  timeText,
  type Tone,
} from './subscription-substore-state';

type Page = { kind: 'settings' } | { kind: 'records' } | { kind: 'run'; id: number };
export type SubStoreNavigationGuard = (next: () => void) => boolean;
interface Scope {
  tenant: string;
  user: string;
  view: SubStoreView;
  open: (page: Page) => void;
  back: () => void;
  backLabel: string;
  mutate: (operation: () => Promise<unknown>) => Promise<void>;
  busy: boolean;
  error: unknown;
  start: (kind: 'sync' | 'verify' | 'cleanup', parent?: number) => Promise<void>;
  setGuard: (guard: SubStoreNavigationGuard | null) => void;
}
const Context = createContext<Scope | null>(null);
function useConnector() {
  const ctx = useContext(Context);
  if (!ctx) throw new Error('Sub-Store must be inside its user scope');
  return ctx;
}
export function SubStoreShell({
  tenant,
  user,
  children,
  onGuard,
  panel,
}: {
  tenant: string;
  user: string;
  children: ReactNode;
  onGuard: (g: SubStoreNavigationGuard | null) => void;
  panel: { role: 'tabpanel'; id: string; 'aria-labelledby': string };
}) {
  const client = useQueryClient();
  const [stack, setStack] = useState<Page[]>([]);
  const [motion, setMotion] = useState('none');
  const guard = useRef<SubStoreNavigationGuard | null>(null);
  const [bodyHeight, setBodyHeight] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const key = ['subscription-connector', tenant, user];
  const query = useQuery({
    queryKey: key,
    queryFn: ({ signal }) => fetchSubStore(tenant, user, signal),
    refetchInterval: q => (q.state.data?.active ? 1000 : 30_000),
    retry: false,
  });
  const mutation = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSettled: () => client.invalidateQueries({ queryKey: key }),
  });
  const setGuard = useCallback(
    (g: SubStoreNavigationGuard | null) => {
      guard.current = g;
      onGuard(g);
    },
    [onGuard],
  );
  useEffect(() => () => onGuard(null), [onGuard]);
  const open = (page: Page) => {
    const go = () => {
      if (!stack.length) setBodyHeight(root.current?.getBoundingClientRect().height ?? 0);
      setMotion('forward');
      setStack(s => [...s, page]);
    };
    if (!guard.current || guard.current(go)) go();
  };
  const back = () => {
    const go = () => {
      setMotion('back');
      setStack(s => s.slice(0, -1));
    };
    if (!guard.current || guard.current(go)) go();
  };
  const top = stack.at(-1);
  useEffect(() => {
    if (top) root.current?.querySelector<HTMLButtonElement>('.substore-nav-back')?.focus();
  }, [top]);
  // A failed connector read must not hide the user's existing Clash subscription.
  const view = query.data;
  const scope: Scope | null = view
    ? {
        tenant,
        user,
        view,
        open,
        back,
        backLabel: stack.length > 1 ? '记录' : '订阅',
        mutate: async op => {
          await mutation.mutateAsync(op);
        },
        busy: mutation.isPending,
        error: mutation.error,
        setGuard,
        start: async (kind, parent) => {
          try {
            await mutation.mutateAsync(() => startSubStore(tenant, user, kind, parent));
          } catch {
            /* The persistent mutation error is rendered below. */
          }
        },
      }
    : null;
  return (
    <Context.Provider value={scope}>
      <div ref={root} className={`substore-shell substore-enter-${motion}`}>
        {top && view ? (
          <div className="substore-page" {...panel} style={bodyHeight ? { minHeight: bodyHeight } : undefined}>
            {top.kind === 'settings' ? <Settings /> : top.kind === 'records' ? <Records /> : <RunDetails id={top.id} />}
          </div>
        ) : (
          children
        )}
        {query.error && <ErrorBox error={query.error} />}
        {mutation.error && <ErrorBox error={mutation.error} />}
      </div>
    </Context.Provider>
  );
}
// Existing tickets may render while the connector metadata is loading.
export function SubStorePeek({ onSelect, depth, inset }: { onSelect: () => void; depth: number; inset: number }) {
  const ctx = useContext(Context);
  const status = ctx ? syncStatus(ctx.view) : null;
  if (!ctx) return null;
  return (
    <button
      type="button"
      className="sub-pass-peek substore-peek"
      style={{ top: depth * 32, left: 12 + inset * 8, right: 12 + inset * 8 }}
      onClick={onSelect}
      aria-label={`切换到 ${SUBSTORE_TITLE}（${status?.label}）`}
    >
      <SubscriptionLogo card="substore" small />
      <b>{SUBSTORE_TITLE}</b>
      <span className="sub-pass-peek-end">
        <Status tone={status!.tone}>{status!.label}</Status>
      </span>
    </button>
  );
}
export function useSubStoreAvailable() {
  return !!useContext(Context);
}
function Status({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className="substore-status" data-tone={tone}>
      <i aria-hidden="true" />
      {children}
    </span>
  );
}
function AutoField() {
  const { view, tenant, user, mutate, busy } = useConnector();
  const c = view.config;
  if (!c || !view.writable)
    return (
      <span className="sub-pass-field">
        <small>自动维护</small>
        <b>{c?.auto ? '开启' : '关闭'}</b>
      </span>
    );
  return (
    <ChoiceSelect
      className="sub-pass-pick"
      aria-label="自动维护"
      data-label="自动维护"
      menuAlign="below"
      disabled={busy}
      value={c.auto ? 'on' : 'off'}
      onChange={e => {
        void mutate(() =>
          saveSubStore(tenant, user, {
            version: view.version,
            title: c.title,
            auth: c.auth,
            login: c.login,
            auto: e.target.value === 'on',
          }),
        ).catch(() => undefined);
      }}
    >
      <option value="on" data-detail="地址变化时更新来源 · 每 5 分钟核验">
        开启
      </option>
      <option value="off" data-detail="仅手动同步">
        关闭
      </option>
    </ChoiceSelect>
  );
}
const POINTS = ['登记', '拉取', '转换'];
function arc(start: number, end: number) {
  const rad = (d: number) => ((d - 90) * Math.PI) / 180;
  return `M ${60 + 50 * Math.cos(rad(start))} ${60 + 50 * Math.sin(rad(start))} A 50 50 0 0 1 ${60 + 50 * Math.cos(rad(end))} ${60 + 50 * Math.sin(rad(end))}`;
}
export function SubStoreTicket() {
  const { view, open, user } = useConnector();
  const status = syncStatus(view);
  const parts = checkpoints(view);
  const c = view.config;
  const center: IconName =
    status.kind === 'none'
      ? 'plus'
      : (
          {
            ok: 'check',
            warn: 'warn',
            bad: 'close',
            live: 'refresh',
            muted: status.kind === 'paused' ? 'pause' : 'dash',
          } as Record<Tone, IconName>
        )[status.tone];
  return (
    <section className="sub-pass substore-ticket" aria-label={SUBSTORE_TITLE}>
      <div className="sub-pass-top">
        <header className="sub-pass-head">
          <SubscriptionLogo card="substore" />
          <span className="sub-pass-name">
            <b>{SUBSTORE_TITLE}</b>
            <small>{c?.host ?? '未连接'}</small>
          </span>
        </header>
        <div className="sub-pass-primary">
          <small>{c ? '最近同步' : 'Sub-Store'}</small>
          {c?.last_confirmed ? (
            <b className="substore-hero-time">
              {timeText(c.last_confirmed)}
              <span>{dayText(c.last_confirmed)}</span>
            </b>
          ) : (
            <b className="substore-hero-word">{c ? '未同步' : '未连接'}</b>
          )}
        </div>
        <div className="sub-pass-fields">
          {c ? (
            <>
              <AutoField />
              <span className="sub-pass-field">
                <small>下次核验</small>
                <b>{timeText(view.inactive ? null : c.next_check)}</b>
              </span>
            </>
          ) : (
            <>
              <span className="sub-pass-field">
                <small>订阅用户</small>
                <b>{user}</b>
              </span>
              <span className="sub-pass-field">
                <small>同步范围</small>
                <b>全部协议 · 双栈</b>
              </span>
            </>
          )}
        </div>
      </div>
      <div className="sub-pass-code substore-stub substore-ring-stub">
        <button
          type="button"
          className="substore-ring"
          data-tone={status.tone}
          disabled={!view.latest}
          onClick={() => open({ kind: 'records' })}
          aria-label={`${status.label}：${POINTS.map((p, i) => `${p}${({ ok: '已确认', bad: '失败', warn: '未确认', live: '进行中' } as Record<string, string>)[parts[i]] ?? '未开始'}`).join('，')}`}
        >
          <svg viewBox="0 0 120 120" aria-hidden="true">
            {parts.map((part, i) => (
              <g key={i} data-s={part}>
                <path className="substore-ring-track" d={arc(i * 120 + 7, (i + 1) * 120 - 7)} />
                {!['track', 'empty', 'paused'].includes(part) && (
                  <path
                    className="substore-ring-fill"
                    pathLength="100"
                    strokeDasharray={part === 'live' ? '50 100' : undefined}
                    d={arc(i * 120 + 7, (i + 1) * 120 - 7)}
                  />
                )}
              </g>
            ))}
          </svg>
          <span className="substore-ring-center">
            <Icon of={center} size={30} className={status.tone === 'live' ? 'substore-spin' : ''} />
          </span>
        </button>
        <div className="substore-ring-copy" role="status">
          <b data-tone={status.tone}>{status.label}</b>
          <small>{status.detail}</small>
        </div>
        <div className="substore-ring-legend" aria-hidden="true">
          {POINTS.map((name, i) => (
            <span key={name} data-s={parts[i]}>
              <i />
              {name}
            </span>
          ))}
        </div>
      </div>
    </section>
  );
}
export function SubStoreActions() {
  const { view, open, start, busy } = useConnector();
  const c = view.config;
  return (
    <div className="sub-pass-actions">
      <button
        type="button"
        className="btn primary sub-pass-main"
        disabled={!view.writable || busy || (!!c && (!!view.active || view.inactive))}
        onClick={() => (c ? void start('sync') : open({ kind: 'settings' }))}
      >
        <Icon of={c ? 'refresh' : 'link'} size={14} className={view.active ? 'substore-spin' : ''} />
        {c ? (view.active ? '同步中…' : '立即同步') : '连接 Sub-Store'}
      </button>
      {c && (
        <button type="button" className="btn sub-pass-side" onClick={() => open({ kind: 'settings' })}>
          <Icon of="settings" size={14} />
          设置
        </button>
      )}
      {!c && view.latest && (
        <button type="button" className="btn sub-pass-side" onClick={() => open({ kind: 'records' })}>
          记录
        </button>
      )}
    </div>
  );
}
function Group({ title, foot, children }: { title?: string; foot?: ReactNode; children: ReactNode }) {
  return (
    <section className="substore-group-wrap">
      {title && <h3 className="substore-group-title">{title}</h3>}
      <div className="substore-group">{children}</div>
      {foot && <div className="substore-group-foot">{foot}</div>}
    </section>
  );
}
function Chevron() {
  return <Icon of="chevronLeft" size={13} className="substore-chevron" />;
}
function Nav({ title, end }: { title: string; end?: ReactNode }) {
  const s = useConnector();
  return (
    <header className="substore-nav">
      <button type="button" className="substore-nav-back" onClick={s.back}>
        <Icon of="chevronLeft" size={15} />
        {s.backLabel}
      </button>
      <b className="substore-nav-title">{title}</b>
      <span className="substore-nav-end">{end}</span>
    </header>
  );
}
interface Draft {
  endpoint: string;
  title: string;
  auth: boolean;
  login: string;
  password: string;
  auto: boolean;
}
function initial(view: SubStoreView, user: string): Draft {
  const c = view.config;
  return {
    endpoint: '',
    title: c?.title ?? `Brocade · ${user}`,
    auth: c?.auth ?? false,
    login: c?.login ?? '',
    password: '',
    auto: c?.auto ?? true,
  };
}
function Field({
  label,
  value,
  onChange,
  secret = false,
  placeholder,
  disabled,
  invalid = false,
}: {
  label: string;
  value: string;
  onChange: (s: string) => void;
  secret?: boolean;
  placeholder?: string;
  disabled?: boolean;
  invalid?: boolean;
}) {
  const id = useId();
  const [shown, setShown] = useState(false);
  const Input = secret && !shown ? ConfigSecretInput : 'input';
  return (
    <div
      className="substore-float"
      data-invalid={invalid || undefined}
      data-hint={placeholder ? '' : undefined}
      data-secret={secret || undefined}
    >
      <Input
        id={id}
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder ?? ' '}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={invalid || undefined}
      />
      <label htmlFor={id}>{label}</label>
      {secret && value && (
        <button
          type="button"
          className="substore-float-eye"
          aria-label={`${shown ? '隐藏' : '显示'}${label}`}
          aria-pressed={shown}
          onClick={() => setShown(!shown)}
        >
          <Icon of={shown ? 'eyeOff' : 'eye'} size={14} />
        </button>
      )}
    </div>
  );
}
function Settings() {
  const s = useConnector();
  const { view, tenant, user, mutate, setGuard } = s;
  const base = initial(view, user);
  const [draft, setDraft] = useState(base);
  const [problem, setProblem] = useState('');
  const [confirm, setConfirm] = useState<'discard' | 'cleanup' | 'remove' | null>(null);
  const [tested, setTested] = useState<'ok' | 'failed' | null>(null);
  const pending = useRef<(() => void) | null>(null);
  const version = useRef(view.version);
  const dirty = JSON.stringify(base) !== JSON.stringify(draft);
  const readOnly = !view.writable;
  useEffect(() => {
    setGuard(
      dirty
        ? next => {
            pending.current = next;
            setConfirm('discard');
            return false;
          }
        : null,
    );
    return () => setGuard(null);
  }, [dirty, setGuard]);
  const change = (key: keyof Draft) => (value: string | boolean) => {
    setDraft(d => ({ ...d, [key]: value }));
    setProblem('');
    setTested(null);
  };
  const input = (): SubStoreWrite => ({
    version: version.current,
    title: draft.title.trim(),
    auth: draft.auth,
    login: draft.login.trim(),
    auto: draft.auto,
    ...(draft.endpoint.trim() ? { endpoint: draft.endpoint.trim() } : {}),
    ...(draft.password ? { password: draft.password } : {}),
  });
  const valid = () => {
    if (!draft.title.trim()) return '填写来源名称';
    if (!draft.endpoint.trim() && !view.config) return '填写后端地址';
    if (draft.endpoint) {
      try {
        const u = new URL(draft.endpoint.trim());
        if (!['http:', 'https:'].includes(u.protocol) || u.username || u.password || u.search || u.hash)
          throw new Error();
      } catch {
        return '请输入完整的 http(s) 后端地址';
      }
    }
    if (
      draft.auth &&
      ((!draft.login.trim() && !view.config?.has_password) || (!draft.password && !view.config?.has_password))
    )
      return '填写代理用户名和密码';
    return '';
  };
  const save = async () => {
    const err = valid();
    if (err) {
      setProblem(err);
      return;
    }
    try {
      const first = !view.config || !view.config.registered;
      await mutate(() => saveSubStore(tenant, user, input()));
      setGuard(null);
      if (first && !view.inactive) {
        await s.start('sync');
      }
      s.back();
    } catch {
      /* Mutation owns the error; preserve all inputs. */
    }
  };
  const test = async () => {
    const err = valid();
    if (err) {
      setProblem(err);
      return;
    }
    try {
      await mutate(() => testSubStore(tenant, user, input()));
      setTested('ok');
    } catch {
      setTested('failed');
    }
  };
  return (
    <>
      <Nav title="Sub-Store 设置" end={readOnly ? <span className="substore-nav-note">只读</span> : undefined} />
      <div className="substore-settings">
        <Group
          title="连接"
          foot={
            !view.config ? (
              <>
                Sub-Store 页面地址中 <code>api=</code> 后的完整地址
              </>
            ) : undefined
          }
        >
          <Field
            label="后端地址"
            secret
            value={draft.endpoint}
            onChange={change('endpoint')}
            disabled={readOnly || s.busy}
            placeholder={view.config ? `已保存 · ${view.config.host}` : undefined}
          />
          <Field label="来源名称" value={draft.title} onChange={change('title')} disabled={readOnly || s.busy} />
        </Group>
        <Group title="同步">
          <label className="substore-row substore-switch-row switch">
            <span className="substore-row-copy">
              <b>自动维护</b>
              <small>每 5 分钟核验来源</small>
            </span>
            <input
              type="checkbox"
              role="switch"
              checked={draft.auto}
              disabled={readOnly || s.busy}
              onChange={e => change('auto')(e.target.checked)}
            />
            <span className="switch-ui" aria-hidden="true">
              <span />
            </span>
          </label>
          <div className="substore-row">
            <span className="substore-row-copy">
              <b>代理认证</b>
            </span>
            <ChoiceSelect
              className="substore-row-pick"
              aria-label="代理认证"
              value={draft.auth ? 'basic' : 'none'}
              disabled={readOnly || s.busy}
              onChange={e => change('auth')(e.target.value === 'basic')}
            >
              <option value="none">无</option>
              <option value="basic">Basic Auth</option>
            </ChoiceSelect>
          </div>
          {draft.auth && (
            <>
              <Field label="代理用户名" value={draft.login} onChange={change('login')} disabled={readOnly || s.busy} />
              <Field
                label="代理密码"
                secret
                value={draft.password}
                onChange={change('password')}
                placeholder={view.config?.has_password ? '已保存' : undefined}
                disabled={readOnly || s.busy}
              />
            </>
          )}
        </Group>
        {problem && (
          <p role="alert" className="substore-problem">
            {problem}
          </p>
        )}
        <Group>
          {!readOnly && (
            <button
              type="button"
              className="substore-row substore-row-button"
              disabled={s.busy}
              onClick={() => void test()}
            >
              <b>测试连接</b>
              <span className="substore-row-end">
                {tested === 'ok' ? '连接成功' : tested === 'failed' ? '连接失败' : s.busy ? '连接中' : ''}
              </span>
            </button>
          )}
          <button type="button" className="substore-row substore-row-nav" onClick={() => s.open({ kind: 'records' })}>
            <span className="substore-row-copy">
              <b>同步记录</b>
            </span>
            <Chevron />
          </button>
        </Group>
        {view.config && !readOnly && (
          <Group>
            {view.config.registered && (
              <button
                type="button"
                className="substore-row substore-row-button danger"
                disabled={!!view.active || s.busy}
                onClick={() => setConfirm('cleanup')}
              >
                清理远端来源…
              </button>
            )}
            <button
              type="button"
              className="substore-row substore-row-button danger"
              disabled={s.busy}
              onClick={() => setConfirm('remove')}
            >
              移除连接…
            </button>
          </Group>
        )}
      </div>
      {!readOnly && (
        <footer className="substore-page-foot">
          <button
            type="button"
            className="btn primary substore-wide"
            disabled={!dirty || s.busy}
            onClick={() => void save()}
          >
            {!view.config ? '连接并同步' : view.config.registered ? '保存' : '保存并同步'}
          </button>
        </footer>
      )}
      {confirm && (
        <Confirm
          title={
            confirm === 'discard'
              ? '放弃未保存的更改？'
              : confirm === 'cleanup'
                ? '清理远端来源？'
                : '移除 Sub-Store 连接？'
          }
          body={
            confirm === 'discard'
              ? '已保存的设置和后台任务不受影响。'
              : confirm === 'cleanup'
                ? `仅删除由 ${user} 登记的来源；其他来源和组合订阅保持不变。`
                : '停止自动维护并删除已保存的地址，同步记录保留；远端来源不会自动删除。'
          }
          confirmLabel={confirm === 'discard' ? '放弃更改' : confirm === 'cleanup' ? '清理来源' : '移除连接'}
          danger={confirm !== 'discard'}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            const kind = confirm;
            setConfirm(null);
            if (kind === 'discard') {
              setGuard(null);
              pending.current?.();
              return;
            }
            void (kind === 'cleanup' ? s.start('cleanup') : mutate(() => removeSubStore(tenant, user)))
              .then(() => {
                setGuard(null);
                s.back();
              })
              .catch(() => undefined);
          }}
        />
      )}
    </>
  );
}
function RunRow({ run }: { run: SubStoreRun }) {
  const s = useConnector();
  const summary = runSummary(run);
  return (
    <button
      type="button"
      className="substore-row substore-run-row"
      onClick={() => s.open({ kind: 'run', id: run.id })}
      aria-label={`${taskText[run.kind]}，${summary}，${timeText(run.started_at)}`}
    >
      <Glyph
        tone={
          activeRun(run)
            ? 'live'
            : run.status === 'succeeded'
              ? 'ok'
              : run.status === 'partial' || run.status === 'failed'
                ? 'bad'
                : 'muted'
        }
      />
      <span className="substore-row-copy">
        <b>{taskText[run.kind]}</b>
        <small title={summary}>{summary}</small>
      </span>
      <span className="substore-row-end">
        <time>{timeText(run.started_at)}</time>
        <Chevron />
      </span>
    </button>
  );
}
function Glyph({ tone, size = 28 }: { tone: Tone; size?: number }) {
  return (
    <span className="substore-glyph" data-tone={tone} style={{ width: size, height: size }}>
      <Icon
        of={{ ok: 'check', warn: 'warn', bad: 'close', live: 'refresh', muted: 'dash' }[tone] as IconName}
        size={size / 2}
        className={tone === 'live' ? 'substore-spin' : ''}
      />
    </span>
  );
}
function Records() {
  const s = useConnector();
  const [scope, setScope] = useState<SubStoreRunScope>('all');
  const [before, setBefore] = useState<number>();
  const [older, setOlder] = useState<SubStoreRun[]>([]);
  const query = useQuery({
    queryKey: ['subscription-connector', s.tenant, s.user, 'runs', scope, before],
    queryFn: ({ signal }) => fetchSubStoreRuns(s.tenant, s.user, before, scope, signal),
    refetchInterval: s.view.active ? 1000 : 30_000,
  });
  const runs = [...older, ...(query.data?.runs ?? [])];
  const groups = new Map<string, SubStoreRun[]>();
  for (const r of runs) {
    const day = dayText(r.started_at);
    groups.set(day, [...(groups.get(day) ?? []), r]);
  }
  const exportRuns = async () => {
    const all: SubStoreRun[] = [];
    let cursor: number | undefined;
    for (let page = 0; page < 50; page++) {
      const result = await fetchSubStoreRuns(s.tenant, s.user, cursor);
      all.push(...result.runs);
      if (!result.next) break;
      cursor = result.next;
    }
    const url = URL.createObjectURL(
      new Blob([JSON.stringify({ user: s.user, records: all }, null, 2)], { type: 'application/json' }),
    );
    const a = document.createElement('a');
    a.href = url;
    a.download = `${s.user}-substore-records.json`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return (
    <>
      <Nav
        title="同步记录"
        end={
          <ActionMenu
            label="记录操作"
            items={[
              {
                label: '立即验证拉取',
                note: '只检查拉取与转换，不重复写入',
                disabled:
                  !s.view.writable || s.view.inactive || !s.view.config?.registered || !!s.view.active
                    ? '当前不可用'
                    : false,
                onSelect: () => void s.start('verify'),
              },
              {
                label: '导出记录',
                note: 'JSON · 保留期内全部记录',
                disabled: !runs.length,
                onSelect: () => void s.mutate(exportRuns).catch(() => undefined),
              },
            ]}
          />
        }
      />
      <div className="substore-records-scope">
        <SegmentedControl
          value={scope}
          options={[
            { value: 'all', label: '最近结果' },
            { value: 'errors', label: '异常' },
            { value: 'ignored', label: '忽略' },
          ]}
          onChange={v => {
            setScope(v);
            setBefore(undefined);
            setOlder([]);
          }}
          ariaLabel="记录范围"
        />
      </div>
      {query.error ? (
        <ErrorBox error={query.error} />
      ) : query.isPending ? (
        <Loading />
      ) : groups.size ? (
        [...groups].map(([day, list]) => (
          <Group key={day} title={day}>
            {list.map(r => (
              <RunRow key={r.id} run={r} />
            ))}
          </Group>
        ))
      ) : (
        <div className="substore-empty">
          <Icon of="clock" size={22} />
          <b>{scope === 'errors' ? '暂无异常' : scope === 'ignored' ? '暂无忽略记录' : '尚无同步记录'}</b>
        </div>
      )}
      {query.data?.next && (
        <button
          type="button"
          className="substore-load-more"
          onClick={() => {
            setOlder(runs);
            setBefore(query.data.next!);
          }}
        >
          加载更多
        </button>
      )}
      <p className="substore-records-foot">
        保留 30 天 · 最多 1,000 条 · 时间为 {Intl.DateTimeFormat().resolvedOptions().timeZone}
      </p>
    </>
  );
}
function RunDetails({ id }: { id: number }) {
  const s = useConnector();
  const [attempt, setAttempt] = useState(-1);
  const query = useQuery({
    queryKey: ['subscription-connector', s.tenant, s.user, 'run', id],
    queryFn: ({ signal }) => fetchSubStoreRun(s.tenant, s.user, id, signal),
    refetchInterval: q => (activeRun(q.state.data) ? 1000 : false),
  });
  const r = query.data;
  if (!r)
    return (
      <>
        <Nav title="记录" />
        {query.error ? <ErrorBox error={query.error} /> : <Loading />}
      </>
    );
  const live = activeRun(r);
  const index = attempt < 0 ? r.attempts.length - 1 : Math.min(attempt, r.attempts.length - 1);
  const events = r.attempts[index]?.events ?? [];
  const tone: Tone = live
    ? 'live'
    : r.status === 'succeeded'
      ? 'ok'
      : r.status === 'partial' || r.status === 'failed'
        ? 'bad'
        : 'muted';
  return (
    <>
      <Nav title={`#${r.id}`} />
      <section className="substore-run-hero">
        <Glyph tone={tone} size={44} />
        <div>
          <b>{taskText[r.kind]}</b>
          <span className="substore-run-result" data-tone={tone}>
            {runResultText(r)}
          </span>
        </div>
        <small>
          {dayText(r.started_at)} {timeText(r.started_at, true)} ·{' '}
          {r.duration_ms != null ? `${(r.duration_ms / 1000).toFixed(2)} 秒` : resultText[r.status]} · {r.trigger}
        </small>
      </section>
      {r.error && (
        <p className="substore-run-note" data-tone={tone}>
          原因：{reasonText(r.error)}
        </p>
      )}
      {['failed', 'partial', 'canceled'].includes(r.status) && s.view.writable && (
        <button
          type="button"
          className="btn substore-wide substore-run-retry"
          disabled={!!s.view.active || !s.view.config || s.view.inactive || s.busy}
          onClick={() =>
            void s.start(r.status === 'partial' ? 'verify' : r.kind === 'cleanup' ? 'cleanup' : 'sync', r.id)
          }
        >
          <Icon of="refresh" size={14} />
          {r.kind === 'cleanup' ? '重试清理' : '重试同步'}
        </button>
      )}
      {r.attempts.length > 1 && (
        <div className="substore-attempts">
          <SegmentedControl
            value={index}
            options={r.attempts.map((a, i) => ({
              value: i,
              label: `第 ${a.number} 次 · ${resultText[a.status] ?? a.status}`,
            }))}
            onChange={setAttempt}
            ariaLabel="执行尝试"
          />
        </div>
      )}
      <Group title="阶段">
        {events.length ? (
          events.map((e, i) => (
            <div key={i} className="substore-row substore-phase" data-state={e.state}>
              <span className="substore-phase-mark">
                <Icon
                  of={
                    e.state === 'done'
                      ? 'check'
                      : e.state === 'failed'
                        ? 'close'
                        : e.state === 'running'
                          ? 'refresh'
                          : 'pause'
                  }
                  size={12}
                  className={e.state === 'running' ? 'substore-spin' : ''}
                />
              </span>
              <span className="substore-row-copy">
                <b>{e.name}</b>
                {e.detail && <small>{reasonText(e.detail)}</small>}
              </span>
              <span className="substore-row-end substore-phase-meta">
                {e.state === 'running'
                  ? '执行中'
                  : [e.http, e.ms != null ? `${e.ms} ms` : null].filter(v => v != null).join(' · ')}
              </span>
            </div>
          ))
        ) : (
          <div className="substore-row">等待工作线程</div>
        )}
      </Group>
      <Group title="详情">
        {[
          ['来源版本', r.source_version ?? '—'],
          ['设置版本', r.config_version],
          ['输出节点', r.output_count ?? '—'],
          ['开始', new Date(r.started_at).toLocaleString()],
          ['结束', r.ended_at ? new Date(r.ended_at).toLocaleString() : '—'],
          ...(r.parent ? [['重试自', `#${r.parent}`]] : []),
        ].map(([label, value]) => (
          <div className="substore-row substore-kv" key={label}>
            <span className="substore-row-copy">
              <b>{label}</b>
            </span>
            <span className="substore-row-end">{value}</span>
          </div>
        ))}
      </Group>
      {live && s.view.writable && (
        <button
          type="button"
          className="btn substore-wide substore-run-stop"
          disabled={s.busy}
          onClick={() => void s.mutate(() => cancelSubStoreRun(s.tenant, s.user, id)).catch(() => undefined)}
        >
          <Icon of="pause" size={14} />
          停止后续步骤
        </button>
      )}
    </>
  );
}

export function SubStoreContent({ children }: { children: (available: boolean) => ReactNode }) {
  return children(useSubStoreAvailable());
}
export function SubscriptionTicketPeeks({
  front,
  haitun,
  value,
  onSelect,
}: {
  front: 'clash' | 'haitun' | 'substore';
  haitun: ClashSubscriptionInfo['haitun'];
  value: ClashSubscriptionInfo | undefined;
  onSelect: (v: 'clash' | 'haitun' | 'substore') => void;
}) {
  const available = useSubStoreAvailable();
  const cards = (
    ['clash', ...(haitun ? ['haitun'] : []), ...(available ? ['substore'] : [])] as ('clash' | 'haitun' | 'substore')[]
  ).filter(c => c !== front);
  return cards.map((card, i) => {
    if (card === 'substore')
      return <SubStorePeek key={card} depth={i} inset={cards.length - i - 1} onSelect={() => onSelect(card)} />;
    const speed = card === 'haitun';
    const name = speed ? 'Koipy 测速订阅' : 'Clash 订阅';
    const summary = speed
      ? { active: '可用', revoked: '已撤销', 'not-created': '尚未生成' }[haitun!.status]
      : !value
        ? '暂不可用'
        : value.remaining_bytes == null
          ? '不限量'
          : `剩余 ${bytes(value.remaining_bytes)}`;
    return (
      <button
        key={card}
        type="button"
        className="sub-pass-peek substore-peek"
        style={{ top: i * 32, left: 12 + (cards.length - i - 1) * 8, right: 12 + (cards.length - i - 1) * 8 }}
        aria-label={speed ? `切换到 ${name}（${summary}）` : '切换到 Clash 订阅'}
        onClick={() => onSelect(card)}
      >
        <SubscriptionLogo card={speed ? 'haitun' : 'clash'} small />
        <b>{name}</b>
        <span className="sub-pass-peek-end">{summary}</span>
      </button>
    );
  });
}
