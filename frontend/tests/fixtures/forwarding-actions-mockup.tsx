// Mockup: 用户详情「转发」页签的授权编辑与独立规则面板。Synthetic data only; never imported by the app.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/forwarding-actions-mockup.html
// Query: v=inline|popover|drawer|current, scene=typical|stopped|fleet|empty|ungranted,
//        open=none|edit|stop|grant|rule|new-rule|probe, ping=<synthetic outcome>,
//        w=<frame width>, theme=light|dark, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import {
  Fragment,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ForwardingProtocol, UserListItem } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { UserForwardingCard } from '../../src/panes/user-forwarding';
import { Confirm, SegmentedControl, SegSwitch } from '../../src/ui/bits';
import { CopyButton, copyStateIcon } from '../../src/ui/copy-button';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { bytes, shareOf } from '../../src/ui/format';
import { endpointText, parseTarget, submittedHost } from '../../src/ui/forwarding-target';
import { Icon, PanelTitle, type IconName } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import { UserTrafficAccountingSetting } from '../../src/user-traffic-accounting';
import {
  GiB,
  jobsOf,
  cycleUsageOf,
  overviewOf,
  SCENE_KEYS,
  SCENES,
  type Candidate,
  type Entry,
  type Job,
  type Rule,
  type Scene,
  type SceneKey,
} from './forwarding-actions-mockup-data';
import {
  createMockProbeTransport,
  MockProbeProvider,
  MockRuleProbe,
  PING_OUTCOMES,
  type PingOutcome,
} from './forwarding-actions-mockup-probe';
import '../../src/styles.css';
import './forwarding-actions-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'dark' ? 'dark' : 'light')) theme.toggle();

type Variant = 'inline' | 'popover' | 'drawer' | 'current';
type Layout = Exclude<Variant, 'current'>;
type Preset = 'none' | 'edit' | 'stop' | 'grant' | 'rule' | 'new-rule' | 'probe';

const VARIANTS: { value: Variant; label: string; note: string }[] = [
  {
    value: 'inline',
    label: 'A 行内展开',
    note: '点哪一行就在那一行下面展开，读数留在原处；同一时间只开一个编辑，打开另一个即切换，不把其他按钮置灰。',
  },
  {
    value: 'popover',
    label: 'B 弹出面板',
    note: '表单浮在按钮下方，列表一点不动；Esc 或点空白处收起。画框窄于 560px 时退回 A 的行内展开。',
  },
  {
    value: 'drawer',
    label: 'C 侧边抽屉',
    note: '从右侧滑出，与 WARP、复用连接的编辑同一形态；抽屉顶部先给本机读数。',
  },
  { value: 'current', label: '现状', note: '生产组件 UserForwardingCard，同一份数据。' },
];

const PRESETS: { value: Preset; label: string }[] = [
  { value: 'none', label: '无' },
  { value: 'edit', label: '编辑授权' },
  { value: 'stop', label: '停止确认' },
  { value: 'grant', label: '授权机器' },
  { value: 'rule', label: '编辑规则' },
  { value: 'new-rule', label: '新建规则' },
  { value: 'probe', label: 'TCP Ping' },
];

const WIDTHS = [
  { value: 1180, label: '1180' },
  { value: 900, label: '900' },
  { value: 620, label: '620' },
  { value: 390, label: '390' },
];

/* ══ 领域小工具（与生产 user-forwarding.tsx 同义） ══ */

const PROTOCOL_LABEL: Record<ForwardingProtocol, string> = { tcp: 'TCP', udp: 'UDP', tcp_udp: 'TCP + UDP' };
const usesTcp = (protocol: ForwardingProtocol) => protocol !== 'udp';
const usesUdp = (protocol: ForwardingProtocol) => protocol !== 'tcp';
const overlap = (a: ForwardingProtocol, b: ForwardingProtocol) =>
  (usesTcp(a) && usesTcp(b)) || (usesUdp(a) && usesUdp(b));
const allowedText = (entry: { tcp: boolean; udp: boolean }) =>
  [entry.tcp && 'TCP', entry.udp && 'UDP'].filter(Boolean).join(' + ');
const protocolAllowed = (entry: Entry, protocol: ForwardingProtocol) =>
  (!usesTcp(protocol) || entry.tcp) && (!usesUdp(protocol) || entry.udp);
const defaultProtocol = (entry: Entry): ForwardingProtocol =>
  entry.tcp && entry.udp ? 'tcp_udp' : entry.tcp ? 'tcp' : 'udp';
const quotaPaused = (entry: Entry) => entry.limit != null && entry.used >= entry.limit;

const entryState = (entry: Entry) =>
  !entry.enabled
    ? { tone: 'off', text: '已停止' }
    : entry.sync === 'error'
      ? { tone: 'bad', text: '收敛失败' }
      : entry.sync === 'pending'
        ? { tone: 'warn', text: '同步中' }
        : quotaPaused(entry)
          ? { tone: 'warn', text: '配额暂停' }
          : { tone: 'ok', text: '已就绪' };

const RULE_STATUS: Record<Rule['status'], { tone: string; text: string } | null> = {
  active: null,
  pending: { tone: 'warn', text: '同步中' },
  error: { tone: 'bad', text: '收敛失败' },
  disabled: { tone: 'off', text: '已停用' },
  'quota-paused': { tone: 'warn', text: '配额暂停' },
};

type RuleFilter = 'all' | 'active' | 'attention' | 'disabled';
const effectiveStatus = (rule: Rule, entry: Entry | undefined): Rule['status'] =>
  !rule.enabled || !entry?.enabled ? 'disabled' : quotaPaused(entry) ? 'quota-paused' : rule.status;
const ruleCategory = (rule: Rule, entry: Entry | undefined): Exclude<RuleFilter, 'all'> => {
  const status = effectiveStatus(rule, entry);
  return status === 'active' || status === 'disabled' ? status : 'attention';
};

const JOB_STATE: Record<Job['status'], string> = {
  queued: '等待节点领取',
  running: '等待节点确认',
  succeeded: '已完成',
  failed: '失败',
  canceled: '已取消',
};

const occupant = (rules: Rule[], nodeId: string, protocol: ForwardingProtocol, port: number, except?: number) =>
  rules.find(
    rule =>
      rule.id !== except &&
      rule.nodeId === nodeId &&
      rule.enabled &&
      rule.port === port &&
      overlap(rule.protocol, protocol),
  );

const freePort = (entry: Entry, rules: Rule[], protocol: ForwardingProtocol, except?: number) => {
  for (let port = entry.start; port <= entry.end; port += 1)
    if (!occupant(rules, entry.id, protocol, port, except)) return port;
  return null;
};

const targetOf = (rule: Rule) => endpointText(rule.host, rule.targetPort);
const customName = (rule: Rule) => (rule.name.trim() && rule.name !== targetOf(rule) ? rule.name : null);
const quotaText = (limit: number | null) => (limit == null ? '' : String(Number((limit / GiB).toFixed(2))));
const parseQuota = (text: string): number | null | undefined => {
  const value = text.trim();
  if (!value) return null;
  if (!/^\d+(\.\d{1,2})?$/.test(value) || Number(value) <= 0) return undefined;
  return Math.round(Number(value) * GiB);
};

const wait = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

/* ══ 授权表单：配额与策略合成一份，三种外壳共用 ══ */

interface EntryDraft {
  nodeId: string;
  quota: string;
  tcp: boolean;
  udp: boolean;
  start: string;
  end: string;
  max: string;
  allowPrivate: boolean;
}

const draftOf = (entry: Entry): EntryDraft => ({
  nodeId: entry.id,
  quota: quotaText(entry.limit),
  tcp: entry.tcp,
  udp: entry.udp,
  start: String(entry.start),
  end: String(entry.end),
  max: String(entry.max),
  allowPrivate: entry.allowPrivate,
});

const GRANT_DRAFT: EntryDraft = {
  nodeId: '',
  quota: '',
  tcp: true,
  udp: true,
  start: '30000',
  end: '59999',
  max: '100',
  allowPrivate: true,
};

function checkEntry(draft: EntryDraft, grant: boolean, own: Rule[]) {
  const enabled = own.filter(rule => rule.enabled);
  const start = Number(draft.start);
  const end = Number(draft.end);
  const max = Number(draft.max);
  let rangeText: string;
  let rangeBad = true;
  if (!/^\d+$/.test(draft.start.trim()) || !/^\d+$/.test(draft.end.trim())) rangeText = '填数字端口';
  else if (start < 1024 || end > 65_535 || end < start) rangeText = '1024–65535，结束不小于起始';
  else {
    const outside = enabled.filter(rule => rule.port < start || rule.port > end);
    rangeBad = outside.length > 0;
    rangeText = rangeBad ? `${outside.map(rule => rule.port).join('、')} 不在新范围内` : `共 ${end - start + 1} 个`;
  }
  let maxText = '1–256';
  let maxBad = true;
  if (/^\d+$/.test(draft.max.trim()) && max >= 1 && max <= 256) {
    maxBad = max < own.length;
    maxText = maxBad ? `不能低于已建的 ${own.length} 条` : grant ? '' : `已建 ${own.length} 条`;
  }
  const quota = parseQuota(draft.quota);
  let problem: string | null = null;
  if (!draft.tcp && !draft.udp) problem = '至少允许一种协议';
  else {
    const blocked = enabled.filter(
      rule => (usesTcp(rule.protocol) && !draft.tcp) || (usesUdp(rule.protocol) && !draft.udp),
    );
    if (blocked.length) problem = `${blocked.length} 条规则使用了要关闭的协议，先停用或修改这些规则`;
  }
  const picked = !grant || !!draft.nodeId;
  return {
    quota,
    quotaBad: quota === undefined,
    rangeText,
    rangeBad,
    maxText,
    maxBad,
    problem,
    ok: picked && quota !== undefined && !rangeBad && !maxBad && !problem,
  };
}

interface EntryEditorProps {
  layout: Layout;
  /** 修改已授权的机器；授权新机器时为空 */
  entry?: Entry;
  rules: Rule[];
  candidates: Candidate[];
  initialConfirm?: 'stop' | 'remove';
  onCancel: () => void;
  onSave: (draft: EntryDraft, quota: number | null) => Promise<void>;
  onStop: () => Promise<void>;
  onRemove: () => Promise<void>;
}

function EntryEditor({
  layout,
  entry,
  rules,
  candidates,
  initialConfirm,
  onCancel,
  onSave,
  onStop,
  onRemove,
}: EntryEditorProps) {
  const id = useId();
  const grant = !entry;
  const initial = useMemo(() => (entry ? draftOf(entry) : GRANT_DRAFT), [entry]);
  const [draft, setDraft] = useState(initial);
  const [busy, setBusy] = useState<'save' | 'stop' | 'remove' | null>(null);
  const [confirming, setConfirming] = useState<'stop' | 'remove' | null>(initialConfirm ?? null);
  const [jobsOpen, setJobsOpen] = useState(false);
  const own = entry ? rules.filter(rule => rule.nodeId === entry.id) : [];
  const enabledCount = own.filter(rule => rule.enabled).length;
  const check = checkEntry(draft, grant, own);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);
  const set = (patch: Partial<EntryDraft>) => {
    setConfirming(null);
    setDraft(current => ({ ...current, ...patch }));
  };
  const protocol = draft.tcp && draft.udp ? 'both' : draft.tcp ? 'tcp' : 'udp';
  const latestJob = entry?.jobs[0];

  const submit = async () => {
    if (!check.ok || !dirty || busy || check.quota === undefined) return;
    setBusy('save');
    await onSave(draft, check.quota);
  };
  const run = async (kind: 'stop' | 'remove') => {
    setBusy(kind);
    await (kind === 'stop' ? onStop() : onRemove());
  };

  const fields = (
    <div className="fwe-fields">
      {grant && (
        <Field label="机器">
          <span className="fwe-pick" role="radiogroup" aria-label="选择机器">
            {candidates.map(node => (
              <button
                key={node.id}
                type="button"
                role="radio"
                aria-checked={draft.nodeId === node.id}
                onClick={() => set({ nodeId: node.id })}
              >
                <RegionFlag code={node.country} />
                {node.name}
              </button>
            ))}
          </span>
        </Field>
      )}
      <Field label="流量配额" htmlFor={`${id}-quota`}>
        <span className="fwe-unit">
          <input
            id={`${id}-quota`}
            className="f mono fwe-num quota"
            inputMode="decimal"
            autoComplete="off"
            autoFocus={!grant && !initialConfirm}
            placeholder="不限"
            value={draft.quota}
            disabled={!!busy}
            aria-invalid={check.quotaBad || undefined}
            onChange={event => set({ quota: event.target.value })}
          />
          <i>GiB</i>
        </span>
        <small className={check.quotaBad ? 'bad' : undefined}>
          {check.quotaBad ? '填正数，最多两位小数' : '留空不限'}
        </small>
        {latestJob && (
          <button
            type="button"
            className={`fwe-link job ${latestJob.status}`}
            aria-expanded={jobsOpen}
            onClick={() => setJobsOpen(open => !open)}
          >
            配额工单 · {JOB_STATE[latestJob.status]}
            <Icon of="chevronDown" size={10} />
          </button>
        )}
      </Field>
      {jobsOpen && entry && (
        <ol className="fwe-jobs" aria-label="配额工单">
          {entry.jobs.map(job => (
            <li key={job.id}>
              <code>#{job.id}</code>
              <span>{job.suspended ? '暂停转发' : '恢复转发'}</span>
              <span className={`fwe-job ${job.status}`}>{JOB_STATE[job.status]}</span>
              <time>{job.at}</time>
            </li>
          ))}
        </ol>
      )}
      <Field label="协议">
        <SegmentedControl
          value={protocol}
          ariaLabel="允许的协议"
          disabled={!!busy}
          options={[
            { value: 'tcp', label: '只 TCP' },
            { value: 'udp', label: '只 UDP' },
            { value: 'both', label: 'TCP + UDP' },
          ]}
          onChange={choice => set({ tcp: choice !== 'udp', udp: choice !== 'tcp' })}
        />
      </Field>
      <Field label="入口端口">
        <span className="fwe-range">
          <input
            className="f mono fwe-num port"
            inputMode="numeric"
            aria-label="端口起始"
            value={draft.start}
            disabled={!!busy}
            aria-invalid={check.rangeBad || undefined}
            onChange={event => set({ start: event.target.value })}
          />
          <i>–</i>
          <input
            className="f mono fwe-num port"
            inputMode="numeric"
            aria-label="端口结束"
            value={draft.end}
            disabled={!!busy}
            aria-invalid={check.rangeBad || undefined}
            onChange={event => set({ end: event.target.value })}
          />
        </span>
        <small className={check.rangeBad ? 'bad' : undefined}>{check.rangeText}</small>
      </Field>
      <Field label="规则上限" htmlFor={`${id}-max`}>
        <input
          id={`${id}-max`}
          className="f mono fwe-num max"
          inputMode="numeric"
          value={draft.max}
          disabled={!!busy}
          aria-invalid={check.maxBad || undefined}
          onChange={event => set({ max: event.target.value })}
        />
        {check.maxText && <small className={check.maxBad ? 'bad' : undefined}>{check.maxText}</small>}
      </Field>
      <Field label="私网目标" title="10.0.0.0/8、172.16.0.0/12、192.168.0.0/16、127.0.0.1 等内部地址">
        <SegSwitch
          checked={draft.allowPrivate}
          off="拒绝"
          on="允许"
          disabled={!!busy}
          ariaLabel="允许转发到私网与本机地址"
          onChange={allowPrivate => set({ allowPrivate })}
        />
      </Field>
      {check.problem && (
        <p className="fwe-problem" role="alert">
          <Icon of="warn" size={12} />
          {check.problem}
        </p>
      )}
    </div>
  );

  const side = grant ? null : entry.enabled ? (
    <button
      type="button"
      className="btn fwe-side"
      disabled={dirty || !!busy}
      title={dirty ? '先保存或取消修改' : undefined}
      onClick={() => setConfirming('stop')}
    >
      <Icon of="pause" size={12} />
      停止转发
    </button>
  ) : (
    <button type="button" className="btn fwe-side" disabled={!!busy} onClick={() => setConfirming('remove')}>
      <Icon of="trash" size={12} />
      移除入口
    </button>
  );

  const foot = confirming ? (
    <footer className="fwe-foot confirming" role="alert">
      <p>
        {confirming === 'stop'
          ? `${enabledCount} 条规则停止监听，经过的连接会断开。`
          : `连同 ${own.length} 条规则一起移除，入口端口释放。`}
      </p>
      <button type="button" className="btn" disabled={!!busy} onClick={() => setConfirming(null)}>
        返回
      </button>
      <button type="button" className="btn danger" disabled={!!busy} autoFocus onClick={() => void run(confirming)}>
        {confirming === 'stop' ? (busy === 'stop' ? '停止中…' : '停止转发') : busy === 'remove' ? '移除中…' : '移除'}
      </button>
    </footer>
  ) : (
    <footer className="fwe-foot">
      {side}
      <span className="sp" />
      {layout !== 'drawer' && (
        <button type="button" className="btn" disabled={!!busy} onClick={onCancel}>
          取消
        </button>
      )}
      <button type="submit" className="btn primary" disabled={!check.ok || !dirty || !!busy}>
        {busy === 'save' ? (grant ? '授权中…' : '保存中…') : grant ? '授权' : '保存'}
      </button>
    </footer>
  );

  return (
    <form
      className={`fwe-form ${layout}`}
      aria-label={grant ? '授权转发入口' : `编辑 ${entry.name} 的转发授权`}
      onKeyDown={event => {
        if (event.key !== 'Escape' || busy || layout === 'drawer') return;
        event.preventDefault();
        event.stopPropagation();
        if (confirming) setConfirming(null);
        else onCancel();
      }}
      onSubmit={event => {
        event.preventDefault();
        void submit();
      }}
    >
      {layout === 'drawer' ? (
        <div className="fwe-drawer-body">
          {entry && <EntrySummary entry={entry} count={own.length} />}
          {fields}
        </div>
      ) : (
        fields
      )}
      {foot}
    </form>
  );
}

function Field({
  label,
  htmlFor,
  title,
  children,
}: {
  label: string;
  htmlFor?: string;
  title?: string;
  children: ReactNode;
}) {
  return (
    <div className="fwe-field">
      {htmlFor ? (
        <label className="fwe-k" htmlFor={htmlFor} title={title}>
          {label}
        </label>
      ) : (
        <span className="fwe-k" title={title}>
          {label}
        </span>
      )}
      <div className="fwe-v">{children}</div>
    </div>
  );
}

/** 抽屉盖住了列表，顶部补一行本机读数：已用 / 配额、规则数与当前状态。 */
function EntrySummary({ entry, count }: { entry: Entry; count: number }) {
  const state = entryState(entry);
  return (
    <dl className="fwe-summary">
      <div>
        <dt>本月</dt>
        <dd>
          {bytes(entry.used)}
          {entry.limit != null && <small> / {bytes(entry.limit)}</small>}
        </dd>
      </div>
      <div>
        <dt>规则</dt>
        <dd>
          {count}
          <small> / {entry.max}</small>
        </dd>
      </div>
      <div>
        <dt>状态</dt>
        <dd className={`fwd-st ${state.tone}`}>{state.text}</dd>
      </div>
    </dl>
  );
}

/* ══ 规则表单 ══ */

interface RuleDraft {
  nodeId: string;
  target: string;
  port: string;
  protocol: ForwardingProtocol;
  name: string;
}

function checkRule(draft: RuleDraft, entry: Entry | undefined, rules: Rule[], editing: number | undefined) {
  const parsed = parseTarget(draft.target);
  const target = 'error' in parsed ? null : parsed;
  let targetHint = '主机:端口';
  let targetBad = false;
  let portHint = entry ? `授权范围 ${entry.start}–${entry.end}` : '';
  let portBad = false;
  let problem: string | null = null;
  if ('error' in parsed) {
    if (draft.target.trim()) {
      targetHint = parsed.error;
      targetBad = true;
    }
  } else if (parsed.blocked && entry && !entry.allowPrivate) {
    targetHint = '这台机器不允许私网目标';
    targetBad = true;
  } else targetHint = parsed.kind === '域名' ? '域名，在转发入口上解析' : `${parsed.kind} · 端口 ${parsed.port}`;
  const port = Number(draft.port);
  if (entry) {
    if (!/^\d+$/.test(draft.port.trim())) {
      portHint = '填数字';
      portBad = true;
    } else if (port < entry.start || port > entry.end) {
      portHint = `超出 ${entry.start}–${entry.end}`;
      portBad = true;
    } else {
      const taken = occupant(rules, entry.id, draft.protocol, port, editing);
      if (taken) {
        const free = freePort(entry, rules, draft.protocol, editing);
        portHint = `已被「${customName(taken) ?? targetOf(taken)}」占用${free === null ? '' : `，空闲 ${free}`}`;
        portBad = true;
      }
    }
    if (!protocolAllowed(entry, draft.protocol)) problem = `${entry.name} 只授权了 ${allowedText(entry)}`;
  }
  return {
    target,
    targetHint,
    targetBad,
    portHint,
    portBad,
    problem,
    ok: !!target && !targetBad && !portBad && !problem,
  };
}

function RuleEditor({
  layout,
  rule,
  entries,
  rules,
  initialNode,
  onCancel,
  onSave,
}: {
  layout: Layout;
  rule?: Rule;
  entries: Entry[];
  rules: Rule[];
  initialNode?: string;
  onCancel: () => void;
  onSave: (draft: RuleDraft, host: string, targetPort: number) => Promise<void>;
}) {
  const id = useId();
  const creating = !rule;
  const roomy = entries.filter(
    entry => entry.enabled && rules.filter(item => item.nodeId === entry.id).length < entry.max,
  );
  const initial = useMemo<RuleDraft>(() => {
    if (rule)
      return {
        nodeId: rule.nodeId,
        target: targetOf(rule),
        port: String(rule.port),
        protocol: rule.protocol,
        name: customName(rule) ?? '',
      };
    const entry = roomy.find(item => item.id === initialNode) ?? roomy[0];
    const protocol = entry ? defaultProtocol(entry) : 'tcp_udp';
    return {
      nodeId: entry?.id ?? '',
      target: '',
      port: entry ? String(freePort(entry, rules, protocol) ?? '') : '',
      protocol,
      name: '',
    };
    // 只在打开时取一次初值
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [draft, setDraft] = useState(initial);
  const [busy, setBusy] = useState(false);
  const entry = entries.find(item => item.id === draft.nodeId);
  const check = checkRule(draft, entry, rules, rule?.id);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);
  const set = (patch: Partial<RuleDraft>) => setDraft(current => ({ ...current, ...patch }));
  const submit = async () => {
    if (!check.ok || !check.target || busy || (!creating && !dirty)) return;
    setBusy(true);
    await onSave(draft, submittedHost(check.target), check.target.port);
  };
  const preview =
    check.target && entry && !check.portBad ? (
      <span className="fwe-preview">
        <code>{endpointText(entry.ipv4, Number(draft.port))}</code>
        <i>→</i>
        <code>{endpointText(check.target.host, check.target.port)}</code>
      </span>
    ) : (
      <span className="fwe-preview" />
    );

  const fields = (
    <div className="fwe-fields">
      {creating && roomy.length > 1 && (
        <Field label="转发入口" htmlFor={`${id}-node`}>
          <select
            id={`${id}-node`}
            className="f fwe-select"
            value={draft.nodeId}
            disabled={busy}
            onChange={event => {
              const next = entries.find(item => item.id === event.target.value);
              if (!next) return;
              const protocol = defaultProtocol(next);
              set({ nodeId: next.id, protocol, port: String(freePort(next, rules, protocol) ?? '') });
            }}
          >
            {roomy.map(item => (
              <option key={item.id} value={item.id}>
                {item.name} · {rules.filter(other => other.nodeId === item.id).length}/{item.max} 条
              </option>
            ))}
          </select>
        </Field>
      )}
      <Field label="目标地址" htmlFor={`${id}-target`}>
        <input
          id={`${id}-target`}
          className="f mono fwe-target"
          autoFocus
          autoComplete="off"
          spellCheck={false}
          placeholder="example.com:443"
          value={draft.target}
          disabled={busy}
          aria-invalid={check.targetBad || undefined}
          onChange={event => set({ target: event.target.value })}
        />
        <small className={check.targetBad ? 'bad' : undefined}>{check.targetHint}</small>
      </Field>
      <Field label="入口端口" htmlFor={`${id}-port`}>
        <input
          id={`${id}-port`}
          className="f mono fwe-num port"
          inputMode="numeric"
          autoComplete="off"
          value={draft.port}
          disabled={busy}
          aria-invalid={check.portBad || undefined}
          onChange={event => set({ port: event.target.value })}
        />
        <small className={check.portBad ? 'bad' : undefined}>{check.portHint}</small>
      </Field>
      <Field label="协议">
        <SegmentedControl
          value={draft.protocol}
          ariaLabel="协议"
          disabled={busy}
          options={(['tcp', 'udp', 'tcp_udp'] as const).map(value => ({
            value,
            label: PROTOCOL_LABEL[value],
            disabled: !entry || !protocolAllowed(entry, value),
          }))}
          onChange={protocol => set({ protocol })}
        />
      </Field>
      <Field label="名称" htmlFor={`${id}-name`}>
        <input
          id={`${id}-name`}
          className="f fwe-name"
          autoComplete="off"
          maxLength={80}
          placeholder={check.target ? endpointText(check.target.host, check.target.port) : '可选'}
          value={draft.name}
          disabled={busy}
          onChange={event => set({ name: event.target.value })}
        />
      </Field>
      {check.problem && (
        <p className="fwe-problem" role="alert">
          <Icon of="warn" size={12} />
          {check.problem}
        </p>
      )}
    </div>
  );
  return (
    <form
      className={`fwe-form rule ${layout}`}
      aria-label={creating ? '新建转发规则' : `编辑入口 ${rule.port} 的规则`}
      onKeyDown={event => {
        if (event.key !== 'Escape' || busy || layout === 'drawer') return;
        event.preventDefault();
        event.stopPropagation();
        onCancel();
      }}
      onSubmit={event => {
        event.preventDefault();
        void submit();
      }}
    >
      {layout === 'drawer' ? <div className="fwe-drawer-body">{fields}</div> : fields}
      <footer className="fwe-foot">
        {preview}
        <span className="sp" />
        {layout !== 'drawer' && (
          <button type="button" className="btn" disabled={busy} onClick={onCancel}>
            取消
          </button>
        )}
        <button type="submit" className="btn primary" disabled={!check.ok || (!creating && !dirty) || busy}>
          {busy ? '保存中…' : creating ? '创建规则' : '保存'}
        </button>
      </footer>
    </form>
  );
}

/* ══ 外壳：弹出面板与抽屉 ══ */

function Popover({
  anchor,
  label,
  width = 432,
  onClose,
  children,
}: {
  anchor: () => HTMLElement | null;
  label: string;
  width?: number;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState<{ top: number; left: number; width: number; above: boolean } | null>(null);
  const closeRef = useRef(onClose);
  useLayoutEffect(() => {
    closeRef.current = onClose;
  }, [onClose]);
  useLayoutEffect(() => {
    let frame = 0;
    const measure = () => {
      const target = anchor();
      const panel = ref.current;
      if (!target || !panel) {
        frame = requestAnimationFrame(measure);
        return;
      }
      const a = target.getBoundingClientRect();
      const bounds = target.closest('.mk-frame')?.getBoundingClientRect() ?? new DOMRect(0, 0, innerWidth, innerHeight);
      const w = Math.min(width, bounds.width - 16);
      const left = Math.max(bounds.left + 8, Math.min(a.right + 6 - w, bounds.right - 8 - w));
      const h = panel.offsetHeight;
      const above = a.bottom + 8 + h > innerHeight - 8 && a.top - 8 - h > 8;
      setPlace({ top: above ? a.top - 8 - h : a.bottom + 8, left, width: w, above });
    };
    measure();
    addEventListener('resize', measure);
    addEventListener('scroll', measure, true);
    const observer = new ResizeObserver(measure);
    if (ref.current) observer.observe(ref.current);
    return () => {
      cancelAnimationFrame(frame);
      removeEventListener('resize', measure);
      removeEventListener('scroll', measure, true);
      observer.disconnect();
    };
  }, [anchor, width]);
  // 定位前面板是隐藏的，表单里的 autoFocus 落不上；放好之后再把焦点交给第一个输入框
  const placed = !!place;
  useEffect(() => {
    if (placed) ref.current?.querySelector<HTMLElement>('input:not(:disabled), button:not(:disabled)')?.focus();
  }, [placed]);
  useEffect(() => {
    const away = (event: PointerEvent) => {
      const node = event.target as Node;
      if (ref.current?.contains(node) || anchor()?.contains(node)) return;
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
  // 收起时把焦点还给打开它的按钮；要在面板从 DOM 移除之前判断焦点是否在面板里
  useLayoutEffect(() => {
    const target = anchor();
    const panel = ref.current;
    return () => {
      if (target?.isConnected && panel?.contains(document.activeElement)) target.focus();
    };
  }, [anchor]);
  return createPortal(
    <div
      ref={ref}
      className="fwe-pop"
      role="dialog"
      aria-label={label}
      data-above={place?.above || undefined}
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

function Drawer({
  title,
  sub,
  closer,
  onClose,
  children,
}: {
  title: string;
  sub: string;
  /** 挂在页头上：保存成功后点页头的「关闭」，走抽屉自己的退场动画 */
  closer: RefObject<HTMLElement | null>;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <DialogLayer label={title} mode="drawer" onClose={onClose}>
      <section className="dialog-surface dialog-drawer external-outbound-drawer fwe-drawer">
        <header ref={closer}>
          <span className="fwe-drawer-title">
            <b>{title}</b>
            <small>{sub}</small>
          </span>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        {children}
      </section>
    </DialogLayer>
  );
}

/* ══ 页签本体 ══ */

type Target =
  | { kind: 'entry'; id: string; confirm?: 'stop' | 'remove' }
  | { kind: 'grant' }
  | { kind: 'rule'; id: number }
  | { kind: 'new-rule' };

const targetKey = (target: Target) =>
  target.kind === 'entry' ? `entry:${target.id}` : target.kind === 'rule' ? `rule:${target.id}` : target.kind;

function presetTarget(preset: Preset, scene: Scene): Target | null {
  const first = [...scene.entries].sort(byEntry)[0];
  switch (preset) {
    case 'edit':
      return first ? { kind: 'entry', id: first.id } : null;
    case 'stop':
      return first ? { kind: 'entry', id: first.id, confirm: 'stop' } : null;
    case 'grant':
      return { kind: 'grant' };
    case 'rule':
      return scene.rules[0] ? { kind: 'rule', id: scene.rules[0].id } : null;
    case 'new-rule':
      return { kind: 'new-rule' };
    default:
      return null;
  }
}

const byEntry = (a: Entry, b: Entry) => Number(b.enabled) - Number(a.enabled) || a.name.localeCompare(b.name, 'zh-CN');

function ForwardingTab({
  scene,
  layout,
  preset,
  pingOutcome,
}: {
  scene: Scene;
  layout: Layout;
  preset: Preset;
  pingOutcome: PingOutcome;
}) {
  const [entries, setEntries] = useState(scene.entries);
  const [rules, setRules] = useState(scene.rules);
  const [candidates, setCandidates] = useState(scene.candidates);
  const [target, setTarget] = useState<Target | null>(() => presetTarget(preset, scene));
  const [filterNode, setFilterNode] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<RuleFilter>('all');
  const [search, setSearch] = useState('');
  const [sort, setSort] = useState<'port' | 'month' | 'created'>('port');
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(
    () => new Set(preset === 'probe' && scene.rules[0] ? [scene.rules[0].id] : []),
  );
  const [menu, setMenu] = useState<number | null>(null);
  const [deleting, setDeleting] = useState<Rule | null>(null);
  const [resuming, setResuming] = useState<string | null>(null);
  const [mode, setMode] = useState<'single' | 'double'>('single');
  useEffect(() => {
    if (preset !== 'probe') return;
    const frame = requestAnimationFrame(() =>
      document.querySelector('.fwr-panel .fwd-tcp-ping')?.scrollIntoView({ block: 'center' }),
    );
    return () => cancelAnimationFrame(frame);
  }, [preset]);
  const anchors = useRef(new Map<string, HTMLElement>());
  const drawerHead = useRef<HTMLElement>(null);
  const anchorRef = useCallback(
    (key: string) => (node: HTMLElement | null) => {
      if (node) anchors.current.set(key, node);
      else anchors.current.delete(key);
    },
    [],
  );
  const openKey = target ? targetKey(target) : null;
  const anchorOf = useMemo(() => () => (openKey ? (anchors.current.get(openKey) ?? null) : null), [openKey]);

  const toggle = (next: Target) =>
    setTarget(current => (current && targetKey(current) === targetKey(next) ? null : next));
  const dismiss = useCallback(() => {
    anchorOf()?.focus();
    setTarget(null);
  }, [anchorOf]);
  /* 保存成功后：抽屉走自己的退场动画，其余直接收起 */
  const finish = useCallback(() => {
    const close = layout === 'drawer' ? drawerHead.current?.querySelector<HTMLButtonElement>('.btn') : null;
    if (close) close.click();
    else dismiss();
  }, [layout, dismiss]);
  const settle = (nodeId: string) =>
    setTimeout(() => {
      setEntries(current =>
        current.map(item => (item.id === nodeId && item.sync === 'pending' ? { ...item, sync: undefined } : item)),
      );
      setRules(current =>
        current.map(item =>
          item.nodeId === nodeId && item.status === 'pending' ? { ...item, status: 'active' } : item,
        ),
      );
    }, 2400);

  const saveEntry = async (entry: Entry | undefined, draft: EntryDraft, quota: number | null) => {
    await wait(650);
    const policy = {
      tcp: draft.tcp,
      udp: draft.udp,
      start: Number(draft.start),
      end: Number(draft.end),
      max: Number(draft.max),
      allowPrivate: draft.allowPrivate,
    };
    const patch = { ...policy, limit: quota, sync: 'pending' as const, lastError: null };
    if (entry) {
      // 只改配额不换代：机器上的转发配置不变，只有跨过额度时规则在「配额暂停」与正常之间切换
      const reconfigured = (Object.keys(policy) as (keyof typeof policy)[]).some(key => policy[key] !== entry[key]);
      const paused = quota != null && entry.used >= quota;
      setEntries(current =>
        current.map(item =>
          item.id === entry.id ? { ...item, ...patch, sync: reconfigured ? 'pending' : item.sync } : item,
        ),
      );
      setRules(current =>
        current.map(item =>
          item.nodeId !== entry.id || !entry.enabled || !item.enabled
            ? item
            : {
                ...item,
                status: paused
                  ? 'quota-paused'
                  : reconfigured || item.status === 'quota-paused'
                    ? 'pending'
                    : item.status,
              },
        ),
      );
      settle(entry.id);
    } else {
      const node = candidates.find(item => item.id === draft.nodeId);
      if (!node) return;
      setEntries(current => [
        ...current,
        {
          id: node.id,
          name: node.name,
          country: node.country,
          ipv4: '192.0.2.200',
          ipv6: null,
          enabled: true,
          used: 0,
          jobs: [],
          ...patch,
        },
      ]);
      setCandidates(current => current.filter(item => item.id !== node.id));
      settle(node.id);
    }
    finish();
  };
  const stopEntry = async (entry: Entry) => {
    await wait(650);
    setEntries(current =>
      current.map(item => (item.id === entry.id ? { ...item, enabled: false, sync: undefined } : item)),
    );
    setRules(current => current.map(item => (item.nodeId === entry.id ? { ...item, status: 'disabled' } : item)));
    finish();
  };
  const removeEntry = async (entry: Entry) => {
    await wait(650);
    setEntries(current => current.filter(item => item.id !== entry.id));
    setRules(current => current.filter(item => item.nodeId !== entry.id));
    setCandidates(current => [...current, { id: entry.id, name: entry.name, country: entry.country }]);
    setFilterNode(current => (current === entry.id ? null : current));
    finish();
  };
  const resume = async (entry: Entry) => {
    setResuming(entry.id);
    await wait(650);
    setEntries(current =>
      current.map(item => (item.id === entry.id ? { ...item, enabled: true, sync: 'pending' } : item)),
    );
    setRules(current =>
      current.map(item => (item.nodeId === entry.id && item.enabled ? { ...item, status: 'pending' } : item)),
    );
    setResuming(null);
    settle(entry.id);
  };
  const saveRule = async (rule: Rule | undefined, draft: RuleDraft, host: string, targetPort: number) => {
    await wait(650);
    const fields = {
      nodeId: draft.nodeId,
      port: Number(draft.port),
      protocol: draft.protocol,
      host,
      targetPort,
      name: draft.name.trim(),
      status: 'pending' as const,
    };
    if (rule) setRules(current => current.map(item => (item.id === rule.id ? { ...item, ...fields } : item)));
    else
      setRules(current => [
        ...current,
        {
          id: Math.max(0, ...current.map(item => item.id)) + 1,
          enabled: true,
          down: 0,
          up: 0,
          created: '2026-10-19 11:00:00+00',
          ...fields,
        },
      ]);
    settle(draft.nodeId);
    finish();
  };
  const toggleRule = (rule: Rule) =>
    setRules(current =>
      current.map(item =>
        item.id === rule.id ? { ...item, enabled: !item.enabled, status: item.enabled ? 'disabled' : 'active' } : item,
      ),
    );

  const sorted = [...entries].sort(byEntry);
  const entryOf = (nodeId: string) => entries.find(item => item.id === nodeId);
  const countOf = (nodeId: string) => rules.filter(rule => rule.nodeId === nodeId).length;
  const down = rules.reduce((sum, rule) => sum + rule.down, 0) * (mode === 'double' ? 2 : 1);
  const up = rules.reduce((sum, rule) => sum + rule.up, 0) * (mode === 'double' ? 2 : 1);
  const [figure, unit] = bytes(down + up).split(' ');
  const maxMonth = Math.max(1, ...rules.map(rule => rule.down + rule.up));
  const scopedRules = rules.filter(rule => !filterNode || rule.nodeId === filterNode);
  const statusCounts = {
    all: scopedRules.length,
    active: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.nodeId)) === 'active').length,
    attention: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.nodeId)) === 'attention').length,
    disabled: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.nodeId)) === 'disabled').length,
  };
  const visible = useMemo(() => {
    const query = search.trim().toLowerCase();
    const shown = rules.filter(
      rule =>
        (!filterNode || rule.nodeId === filterNode) &&
        (statusFilter === 'all' ||
          ruleCategory(
            rule,
            entries.find(entry => entry.id === rule.nodeId),
          ) === statusFilter) &&
        (!query ||
          `:${rule.port} ${rule.name} ${targetOf(rule)} ${entries.find(entry => entry.id === rule.nodeId)?.name ?? ''}`
            .toLowerCase()
            .includes(query)),
    );
    const order = {
      port: (a: Rule, b: Rule) => a.port - b.port || a.nodeId.localeCompare(b.nodeId),
      month: (a: Rule, b: Rule) => b.down + b.up - (a.down + a.up),
      created: (a: Rule, b: Rule) => b.created.localeCompare(a.created),
    }[sort];
    return [...shown].sort((a, b) => order(a, b) || a.id - b.id);
  }, [filterNode, statusFilter, rules, entries, search, sort]);
  const groups = sorted
    .map(entry => ({ entry, rules: visible.filter(rule => rule.nodeId === entry.id) }))
    .filter(group => group.rules.length > 0);
  const selectedEntry = filterNode ? entryOf(filterNode) : undefined;
  const creatable = entries.some(
    entry => (!filterNode || entry.id === filterNode) && entry.enabled && countOf(entry.id) < entry.max,
  );
  const createHint = !entries.length
    ? '先授权一台机器'
    : selectedEntry && !selectedEntry.enabled
      ? '这台机器已停止转发'
      : '规则数已达上限';
  const closeRuleView = () => {
    setMenu(null);
    // 不让已筛走的规则留下悬空弹层；授权表单不受规则筛选影响。
    if (target?.kind === 'rule' || target?.kind === 'new-rule') setTarget(null);
  };
  const changeScope = (next: string | null) => {
    closeRuleView();
    setFilterNode(next);
  };

  /* 当前打开的编辑器（按外壳放到不同位置） */
  const openEntry = target?.kind === 'entry' ? entryOf(target.id) : undefined;
  const openRule = target?.kind === 'rule' ? rules.find(rule => rule.id === target.id) : undefined;
  const entryEditor = (entry: Entry | undefined) => (
    <EntryEditor
      key={entry ? `entry:${entry.id}:${target?.kind === 'entry' ? (target.confirm ?? '') : ''}` : 'grant'}
      layout={layout}
      entry={entry}
      rules={rules}
      candidates={candidates}
      initialConfirm={target?.kind === 'entry' ? target.confirm : undefined}
      onCancel={dismiss}
      onSave={(draft, quota) => saveEntry(entry, draft, quota)}
      onStop={() => (entry ? stopEntry(entry) : Promise.resolve())}
      onRemove={() => (entry ? removeEntry(entry) : Promise.resolve())}
    />
  );
  const ruleEditor = (rule: Rule | undefined) => (
    <RuleEditor
      key={rule ? `rule:${rule.id}` : 'new-rule'}
      layout={layout}
      rule={rule}
      entries={!rule && selectedEntry ? [selectedEntry] : entries}
      rules={rules}
      initialNode={filterNode ?? undefined}
      onCancel={dismiss}
      onSave={(draft, host, targetPort) => saveRule(rule, draft, host, targetPort)}
    />
  );
  const floating =
    target === null
      ? null
      : target.kind === 'entry'
        ? openEntry && { title: openEntry.name, sub: '转发授权', body: entryEditor(openEntry) }
        : target.kind === 'grant'
          ? { title: '授权机器', sub: '转发入口', body: entryEditor(undefined) }
          : target.kind === 'rule'
            ? openRule && {
                title: `${entryOf(openRule.nodeId)?.name ?? ''} · :${openRule.port}`,
                sub: '编辑规则',
                body: ruleEditor(openRule),
              }
            : { title: '新建规则', sub: selectedEntry?.name ?? '转发入口', body: ruleEditor(undefined) };

  return (
    <MockProbeProvider entries={entries} rules={rules} outcome={pingOutcome}>
      <section className="panel config-panel user-dcard user-usage-card user-forwarding-usage">
        <header>
          <PanelTitle of="usage">用量与额度</PanelTitle>
          <span className="rt">
            <b>{entries.length}</b> 个入口
          </span>
        </header>
        <div className="user-dcard-body fwd-body">
          <div className="user-usage panel-full-bleed-body">
            <aside className="fwd-ledger user-usage-ledger" aria-label="本月转发">
              <div className="user-usage-hero">
                <span className="user-usage-hero-label">本月转发</span>
                <strong className="user-usage-hero-value">
                  {figure} <small>{unit}</small>
                </strong>
                <span className="user-usage-reset">
                  10 月 1 日起
                  <span className="usage-dot">·</span>
                  11 月 1 日重置
                </span>
                <div className="usage-split" aria-hidden="true">
                  <i className="down" style={{ flexGrow: down }} />
                  <i className="up" style={{ flexGrow: up }} />
                </div>
                <dl className="usage-io">
                  <div className="down">
                    <dt>
                      <i aria-hidden="true" />
                      下行
                    </dt>
                    <dd>{bytes(down)}</dd>
                    <dd className="usage-share">{shareOf(down, down + up)}</dd>
                  </div>
                  <div className="up">
                    <dt>
                      <i aria-hidden="true" />
                      上行
                    </dt>
                    <dd>{bytes(up)}</dd>
                    <dd className="usage-share">{shareOf(up, down + up)}</dd>
                  </div>
                </dl>
              </div>
              <UserTrafficAccountingSetting mode={mode} disabled={false} onChange={setMode} />
            </aside>
            <div className="qta fwd-quotas fwe-aligned" role="list" aria-label="各转发入口的用量与规则额度">
              {sorted.map(entry => {
                const open = target?.kind === 'entry' && target.id === entry.id;
                return (
                  <EntryRow
                    key={entry.id}
                    entry={entry}
                    count={countOf(entry.id)}
                    open={open}
                    layout={layout}
                    resuming={resuming === entry.id}
                    anchorRef={anchorRef(`entry:${entry.id}`)}
                    onEdit={() => toggle({ kind: 'entry', id: entry.id })}
                    onResume={() => void resume(entry)}
                    sheet={open && layout === 'inline' ? entryEditor(entry) : null}
                  />
                );
              })}
              {layout === 'inline' && target?.kind === 'grant' ? (
                <div className="qta-r fwd-quota-row fwe-row fwe-open fwe-grant" role="listitem">
                  <span className="qta-name">
                    <span className="qta-app">
                      <b>授权机器</b>
                    </span>
                    <span className="qta-sub">转发入口</span>
                  </span>
                  <div className="fwe-sheet">{entryEditor(undefined)}</div>
                </div>
              ) : (
                candidates.length > 0 && (
                  <button
                    ref={anchorRef('grant')}
                    type="button"
                    className="fwd-add"
                    aria-expanded={target?.kind === 'grant'}
                    onClick={() => toggle({ kind: 'grant' })}
                  >
                    <Icon of="plus" size={13} />
                    <b>授权机器</b>
                  </button>
                )
              )}
            </div>
          </div>
        </div>
        <footer className="user-dcard-foot fwd-foot">
          <span>转发流量单独统计，不占用线路额度；规则保存后由转发入口自动生效，不需要变更单。</span>
        </footer>
      </section>

      <section className="panel config-panel user-dcard user-forwarding-card fwr-panel" aria-label="转发入口">
        <header>
          <PanelTitle of="forwarding">转发入口</PanelTitle>
          <span className="rt">
            <b>{rules.length}</b> 条规则
          </span>
          <button
            ref={anchorRef('new-rule')}
            type="button"
            className="btn fwr-create"
            disabled={!creatable}
            title={!creatable ? createHint : undefined}
            aria-expanded={target?.kind === 'new-rule'}
            onClick={() => toggle({ kind: 'new-rule' })}
          >
            <Icon of="plus" size={13} />
            新建规则
          </button>
        </header>
        <div className="user-dcard-body panel-full-bleed-body fwd-body">
          <div className="fwd-main fwr-main">
            <div className="fwr-tools">
              <label className="fwr-machine">
                <span>机器</span>
                <select
                  className="f"
                  aria-label="筛选转发机器"
                  value={filterNode ?? ''}
                  onChange={event => changeScope(event.target.value || null)}
                >
                  <option value="">全部机器 · {entries.length}</option>
                  {sorted.map(entry => (
                    <option key={entry.id} value={entry.id}>
                      {entry.name} · {countOf(entry.id)} 条{!entry.enabled ? ' · 已停止' : ''}
                    </option>
                  ))}
                </select>
              </label>
              <span className="user-list-search fwd-search">
                <Icon of="search" size={13} className="user-list-search-icon" />
                <input
                  type="search"
                  value={search}
                  aria-label="搜索规则"
                  placeholder="搜索端口、目标、名称"
                  autoComplete="off"
                  spellCheck={false}
                  onChange={event => {
                    closeRuleView();
                    setSearch(event.target.value);
                  }}
                />
              </span>
              <select
                className="f fwr-sort"
                aria-label="规则排序"
                value={sort}
                onChange={event => {
                  closeRuleView();
                  setSort(event.target.value as typeof sort);
                }}
              >
                <option value="port">端口升序</option>
                <option value="month">本月流量 ↓</option>
                <option value="created">最近创建</option>
              </select>
            </div>
            <div className="fwr-filterbar">
              <SegmentedControl
                value={statusFilter}
                ariaLabel="筛选规则状态"
                options={(
                  [
                    ['all', '全部'],
                    ['active', '已生效'],
                    ['attention', '未生效'],
                    ['disabled', '已停用'],
                  ] as const
                ).map(([value, label]) => ({
                  value,
                  label: (
                    <span className="fwr-filter-label">
                      {label}
                      <small>{statusCounts[value]}</small>
                    </span>
                  ),
                }))}
                onChange={next => {
                  closeRuleView();
                  setStatusFilter(next);
                }}
              />
              {selectedEntry && !creatable && <span className="fwr-constraint">{createHint}</span>}
              <span className="fwr-result" role="status">
                {visible.length} 条规则
              </span>
            </div>
            {layout === 'inline' && target?.kind === 'new-rule' && (
              <div className="fwe-new">
                <div className="fwe-new-head">新建规则{selectedEntry && ` · ${selectedEntry.name}`}</div>
                {ruleEditor(undefined)}
              </div>
            )}
            <div className="fwr-groups">
              {groups.map(({ entry, rules: groupRules }) => (
                <section className="fwr-group" key={entry.id} aria-label={`${entry.name}的规则`}>
                  <header className="fwr-group-head">
                    <span className="fwr-group-name">
                      <RegionFlag code={entry.country} />
                      <b>{entry.name}</b>
                    </span>
                    <code>{entry.ipv4}</code>
                    {entryState(entry).tone !== 'ok' && (
                      <span className={`fwd-st ${entryState(entry).tone}`}>{entryState(entry).text}</span>
                    )}
                    <span className="fwr-group-count">{groupRules.length} 条</span>
                  </header>
                  <div className="fwr-columns" aria-hidden="true">
                    <span>入口端口</span>
                    <span>转发目标</span>
                    <span>协议</span>
                    <span>本月流量</span>
                    <span>状态</span>
                    <span />
                  </div>
                  <div role="list" aria-label={`${entry.name}转发规则`}>
                    {groupRules.map(rule => {
                      const editing = target?.kind === 'rule' && target.id === rule.id;
                      return (
                        <RuleRow
                          key={rule.id}
                          rule={rule}
                          entry={entry}
                          month={rule.down + rule.up}
                          maxMonth={maxMonth}
                          open={expanded.has(rule.id)}
                          editing={editing}
                          layout={layout}
                          canManage={!!entry?.enabled}
                          menuOpen={menu === rule.id}
                          anchorRef={anchorRef(`rule:${rule.id}`)}
                          onMenu={open => setMenu(open ? rule.id : null)}
                          onToggleOpen={() => {
                            if (editing) {
                              dismiss();
                              return;
                            }
                            setExpanded(current => {
                              const next = new Set(current);
                              if (next.has(rule.id)) next.delete(rule.id);
                              else next.add(rule.id);
                              return next;
                            });
                          }}
                          onEdit={() => {
                            setMenu(null);
                            toggle({ kind: 'rule', id: rule.id });
                          }}
                          onToggleEnabled={() => toggleRule(rule)}
                          onDelete={() => setDeleting(rule)}
                          sheet={editing && layout === 'inline' ? ruleEditor(rule) : null}
                        />
                      );
                    })}
                  </div>
                </section>
              ))}
            </div>
            {visible.length === 0 && (
              <div className="fwr-empty">
                <Icon of="forwarding" size={22} />
                <b>
                  {!entries.length
                    ? '尚未授权转发机器'
                    : !scopedRules.length
                      ? selectedEntry
                        ? '这台机器还没有规则'
                        : '还没有转发规则'
                      : '没有匹配的规则'}
                </b>
                {!entries.length ? (
                  <p>在「用量与额度」中授权机器后即可新建规则。</p>
                ) : search || statusFilter !== 'all' ? (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => {
                      setSearch('');
                      setStatusFilter('all');
                    }}
                  >
                    清除搜索与状态筛选
                  </button>
                ) : (
                  <p>
                    {selectedEntry && !selectedEntry.enabled
                      ? '这台机器已停止转发。'
                      : '点击「新建规则」添加第一个转发目标。'}
                  </p>
                )}
              </div>
            )}
          </div>
        </div>
      </section>

      {layout === 'popover' && floating && openKey && (
        <Popover
          key={openKey}
          anchor={anchorOf}
          label={`${floating.title} · ${floating.sub}`}
          width={target?.kind === 'rule' || target?.kind === 'new-rule' ? 468 : 432}
          onClose={() => setTarget(null)}
        >
          {floating.body}
        </Popover>
      )}
      {layout === 'drawer' && floating && openKey && (
        <Drawer
          key={openKey}
          title={floating.title}
          sub={floating.sub}
          closer={drawerHead}
          onClose={() => setTarget(null)}
        >
          {floating.body}
        </Drawer>
      )}
      {deleting && (
        <Confirm
          title="删除转发规则"
          body={
            <p className="fwd-cpath">
              <code>
                {entryOf(deleting.nodeId)
                  ? endpointText(entryOf(deleting.nodeId)!.ipv4, deleting.port)
                  : `:${deleting.port}`}
              </code>
              <i>→</i>
              <code>{targetOf(deleting)}</code>
            </p>
          }
          confirmLabel="删除规则"
          onConfirm={() => {
            setRules(current => current.filter(item => item.id !== deleting.id));
            setDeleting(null);
          }}
          onCancel={() => setDeleting(null)}
        />
      )}
    </MockProbeProvider>
  );
}

function EntryRow({
  entry,
  count,
  open,
  layout,
  resuming,
  anchorRef,
  onEdit,
  onResume,
  sheet,
}: {
  entry: Entry;
  count: number;
  open: boolean;
  layout: Layout;
  resuming: boolean;
  anchorRef: (node: HTMLElement | null) => void;
  onEdit: () => void;
  onResume: () => void;
  sheet: ReactNode;
}) {
  const state = entryState(entry);
  const percent = entry.max > 0 ? (count / entry.max) * 100 : 0;
  const tone = percent >= 100 ? 'over' : percent >= 95 ? 'warn' : 'ok';
  return (
    <div
      role="listitem"
      className={`qta-r fwd-quota-row fwe-row ${tone}${open ? (layout === 'inline' ? ' fwe-open' : ' fwe-active') : ''}`}
    >
      <span className="qta-name fwe-entry-name">
        <span className="qta-app">
          <RegionFlag code={entry.country} />
          <b>{entry.name}</b>
        </span>
        <span className="qta-sub">
          {state.tone === 'ok' ? (
            `${allowedText(entry)} · ${entry.start}–${entry.end}`
          ) : (
            <span className={`fwd-st ${state.tone}`}>{state.text}</span>
          )}
        </span>
      </span>
      <span className="qta-figs">
        {bytes(entry.used)}
        {entry.limit != null && <small className="dim"> / {bytes(entry.limit)}</small>}
      </span>
      <span className="qta-left">
        规则
        <b>
          {count} / {entry.max}
        </b>
      </span>
      <span className="qta-meter">
        <span className="qta-t" aria-hidden="true">
          <i style={{ width: `${Math.min(100, percent)}%` }} />
        </span>
        <span className="qta-pct">{Math.round(percent)}%</span>
      </span>
      <span className="qta-acts">
        {!entry.enabled && (
          <button type="button" className="btn fwe-resume" disabled={resuming} onClick={onResume}>
            {resuming ? '恢复中…' : '恢复'}
          </button>
        )}
        <button
          ref={anchorRef}
          type="button"
          className="fwd-ib"
          aria-label={`编辑 ${entry.name} 的转发授权`}
          title="编辑授权"
          aria-expanded={open}
          onClick={onEdit}
        >
          <Icon of="pencil" size={13} />
        </button>
      </span>
      {state.tone === 'bad' && entry.lastError && (
        <p className="fwd-merr">
          <Icon of="warn" size={12} />
          {entry.lastError}
        </p>
      )}
      {sheet && <div className="fwe-sheet">{sheet}</div>}
    </div>
  );
}

function RuleRow({
  rule,
  entry,
  month,
  maxMonth,
  open,
  editing,
  layout,
  canManage,
  menuOpen,
  anchorRef,
  onMenu,
  onToggleOpen,
  onEdit,
  onToggleEnabled,
  onDelete,
  sheet,
}: {
  rule: Rule;
  entry: Entry | undefined;
  month: number;
  maxMonth: number;
  open: boolean;
  editing: boolean;
  layout: Layout;
  canManage: boolean;
  menuOpen: boolean;
  anchorRef: (node: HTMLElement | null) => void;
  onMenu: (open: boolean) => void;
  onToggleOpen: () => void;
  onEdit: () => void;
  onToggleEnabled: () => void;
  onDelete: () => void;
  sheet: ReactNode;
}) {
  const name = customName(rule);
  const target = targetOf(rule);
  const v4 = entry ? endpointText(entry.ipv4, rule.port) : null;
  const v6 = entry?.ipv6 ? endpointText(entry.ipv6, rule.port) : null;
  const state = effectiveStatus(rule, entry);
  const status = RULE_STATUS[state] ?? { tone: 'ok', text: '已生效' };
  const expandedNow = (open && !editing) || (editing && layout === 'inline');
  const detailId = useId();
  return (
    <div
      role="listitem"
      className={`fwd-item fwr-item st-${state}${expandedNow ? ' open' : ''}${editing ? (layout === 'inline' ? ' fwe-editing' : ' fwe-active') : ''}`}
    >
      <div className="fwd-row">
        <button
          type="button"
          className="fwr-rmain"
          aria-expanded={expandedNow}
          aria-controls={expandedNow ? detailId : undefined}
          aria-label={`${name ? `${name}，` : ''}入口 ${rule.port} 转发到 ${target}`}
          onClick={onToggleOpen}
        >
          <span className="fwd-port">
            <Icon of="chevronDown" size={10} className="fwd-chev" />:{rule.port}
          </span>
          <span className={`fwd-target${name ? '' : ' plain'}`} title={target}>
            {name && <b>{name}</b>}
            <code>
              <i>→</i>
              {target}
            </code>
          </span>
          <span className="fwd-proto">{PROTOCOL_LABEL[rule.protocol]}</span>
          <span className="fwd-cycle">
            <b>{bytes(month)}</b>
            <span className="fwd-bar" aria-hidden="true">
              <i style={{ width: `${month ? Math.max(2, (month / maxMonth) * 100) : 0}%` }} />
            </span>
          </span>
          <span className="fwd-now">
            <span className={`fwd-st ${status.tone}`}>{status.text}</span>
          </span>
        </button>
        <span className="fwr-acts">
          {canManage && (
            <button
              ref={anchorRef}
              type="button"
              className="btn fwr-edit"
              aria-label={`编辑 ${entry?.name ?? ''} :${rule.port}`}
              title="编辑"
              aria-expanded={editing}
              onClick={onEdit}
            >
              编辑
            </button>
          )}
          {(canManage || v4 || v6) && (
            <MoreMenu label={`${entry?.name ?? ''} :${rule.port} 的更多操作`} open={menuOpen} onOpenChange={onMenu}>
              {v4 && (
                <CopyButton text={v4} role="menuitem" label="复制 IPv4 入口">
                  {(copy, label) => (
                    <>
                      <Icon of={copyStateIcon(copy)} size={14} className="action-menu-icon" />
                      <span>
                        {label}
                        <small className="mono">{v4}</small>
                      </span>
                    </>
                  )}
                </CopyButton>
              )}
              {canManage && (
                <MenuItem
                  icon={rule.enabled ? 'pause' : 'play'}
                  title={rule.enabled ? '停用' : '启用'}
                  note={rule.enabled ? `入口 ${rule.port} 停止监听，规则保留` : `重新监听入口 ${rule.port}`}
                  onSelect={onToggleEnabled}
                />
              )}
              {v6 && (
                <CopyButton text={v6} role="menuitem" label="复制 IPv6 入口">
                  {(state, stateLabel) => (
                    <>
                      <Icon of={copyStateIcon(state)} size={14} className="action-menu-icon" />
                      <span>
                        {stateLabel}
                        <small className="mono">{v6}</small>
                      </span>
                    </>
                  )}
                </CopyButton>
              )}
              {canManage && (
                <MenuItem
                  icon="trash"
                  title="删除规则"
                  note={rule.enabled ? `入口 ${rule.port} 立即停止，端口释放` : '规则与端口一起释放'}
                  danger
                  onSelect={onDelete}
                />
              )}
            </MoreMenu>
          )}
        </span>
      </div>
      {sheet ? (
        <div className="fwe-rule-sheet" id={detailId}>
          {sheet}
        </div>
      ) : (
        open &&
        !editing && (
          <div className="fwd-detail fwr-detail" id={detailId}>
            <dl className="fwd-kv">
              <div className="wide">
                <dt>IPv4</dt>
                <dd>
                  {v4 && (
                    <>
                      <code>{v4}</code>
                      <CopyButton text={v4} label={`复制入口 ${v4}`} iconOnly className="fwd-copy">
                        {copy => <Icon of={copyStateIcon(copy)} size={12} />}
                      </CopyButton>
                    </>
                  )}
                </dd>
              </div>
              {v6 && (
                <div className="wide">
                  <dt>IPv6</dt>
                  <dd>
                    <code>{v6}</code>
                    <CopyButton text={v6} label={`复制入口 ${v6}`} iconOnly className="fwd-copy">
                      {copy => <Icon of={copyStateIcon(copy)} size={12} />}
                    </CopyButton>
                  </dd>
                </div>
              )}
              <div className="wide">
                <dt>目标</dt>
                <dd>
                  <code>{target}</code>
                  <span className="fwd-dim"> · {PROTOCOL_LABEL[rule.protocol]}</span>
                </dd>
              </div>
              <div>
                <dt>本月</dt>
                <dd>
                  ↓ {bytes(rule.down)}
                  <span className="fwr-traffic-up">↑ {bytes(rule.up)}</span>
                </dd>
              </div>
              <div>
                <dt>创建</dt>
                <dd>{rule.created.slice(5, 16)}</dd>
              </div>
            </dl>
            <MockRuleProbe ruleId={rule.id} />
          </div>
        )
      )}
    </div>
  );
}

function MoreMenu({
  label,
  open,
  onOpenChange,
  children,
}: {
  label: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (event: PointerEvent) => {
      if (!ref.current?.contains(event.target as Node)) onOpenChange(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onOpenChange(false);
    };
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('pointerdown', away);
      document.removeEventListener('keydown', escape);
    };
  }, [open, onOpenChange]);
  return (
    <span className="fwd-menuwrap" ref={ref}>
      <button
        type="button"
        className="fwd-ib"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        title={label}
        onClick={() => onOpenChange(!open)}
      >
        <Icon of="more" size={14} />
      </button>
      {open && (
        <div
          className="fg-menu action-menu fwd-menu"
          role="menu"
          onClick={event => {
            if (!(event.target as HTMLElement).closest('.copy-button')) onOpenChange(false);
          }}
        >
          {children}
        </div>
      )}
    </span>
  );
}

function MenuItem({
  icon,
  title,
  note,
  danger = false,
  onSelect,
}: {
  icon: IconName;
  title: string;
  note: string;
  danger?: boolean;
  onSelect: () => void;
}) {
  return (
    <button type="button" role="menuitem" className={danger ? 'dg' : undefined} onClick={onSelect}>
      <Icon of={icon} size={14} className="action-menu-icon" />
      <span>
        {title}
        <small>{note}</small>
      </span>
    </button>
  );
}

/* ══ 现状：生产组件挂同一份数据 ══ */

function installCurrentFetch(scene: Scene, pingOutcome: PingOutcome) {
  const original = globalThis.fetch;
  let overview = overviewOf(scene);
  const probes = createMockProbeTransport(overview, pingOutcome);
  globalThis.fetch = async (input, init) => {
    const path = String(input);
    probes.update(overview, pingOutcome);
    const probeResponse = probes.handle(input, init);
    if (probeResponse) return probeResponse;
    if (path === '/nodes/agent-state')
      return Response.json({
        nodes: scene.entries.map(entry => ({ node_id: entry.id, public_ipv4_country: entry.country })),
      });
    if (path === '/usage/user-cycles') return Response.json(cycleUsageOf(scene));
    const jobs = path.match(/forwarding-nodes\/([^/]+)\/quota\/jobs/);
    if (jobs) return Response.json(jobsOf(scene, decodeURIComponent(jobs[1])));
    if (init?.method === 'PUT' && path.includes('/forwarding-nodes/')) {
      const nodeId = decodeURIComponent(path.split('/forwarding-nodes/')[1].split('/')[0]);
      const body = JSON.parse(String(init.body));
      overview = {
        ...overview,
        entitlements: overview.entitlements.map(item =>
          item.node_id !== nodeId
            ? item
            : path.endsWith('/quota')
              ? { ...item, cycle_limit_bytes: body.limit_bytes }
              : { ...item, ...body },
        ),
      };
      return Response.json(overview);
    }
    if (path.endsWith('/forwarding')) return Response.json(overview);
    return new Response('mockup: unhandled request', { status: 404 });
  };
  return () => {
    globalThis.fetch = original;
  };
}

function Current({ scene, pingOutcome }: { scene: Scene; pingOutcome: PingOutcome }) {
  const client = useMemo(() => new QueryClient({ defaultOptions: { queries: { retry: false } } }), []);
  useLayoutEffect(() => {
    const restore = installCurrentFetch(scene, pingOutcome);
    return () => {
      restore();
      client.clear();
    };
  }, [scene, client, pingOutcome]);
  return (
    <QueryClientProvider client={client}>
      <UserForwardingCard
        user={{ tenant_id: 'ops', id: 'alice', traffic_accounting_mode: 'single' } as UserListItem}
        nodes={scene.candidates.map(node => ({ id: node.id, name: node.name, api_port: 7443 }))}
        canAuthorize
        canManageRules
        accountingDisabled={false}
        onAccountingModeChange={() => undefined}
      />
    </QueryClientProvider>
  );
}

/* ══ 审阅外壳 ══ */

function DetailFrame({ children }: { children: ReactNode }) {
  const uuid = '5b1f7c2e-8d4a-4f0e-9b6c-2a71d0e3c9f4';
  return (
    <section className="panel user-split-detail mk-detail">
      <div className="user-dhead">
        <div className="user-dhead-main">
          <div className="user-dhead-identity">
            <GeneratedUserAvatar id="alice" variant="plate" />
            <div className="dtitle">
              <div className="dname">
                <b className="mono">alice</b>
                <span className="user-account-state" data-tone="ok">
                  可用
                </span>
              </div>
              <p className="dsub">
                <CopyButton className="user-uuid-copy" text={uuid} label="复制完整 UUID" iconOnly>
                  {state => (
                    <>
                      <span className="user-uuid-label">UUID</span>
                      <span className="mono">5b1f7c2e…c9f4</span>
                      <Icon of={copyStateIcon(state)} size={12} />
                    </>
                  )}
                </CopyButton>
              </p>
            </div>
          </div>
          <div className="nd-tabs user-detail-tabs" role="tablist" aria-label="alice 详情页签">
            <div className="nd-tabs-seg">
              {(
                [
                  ['接入', 'access'],
                  ['转发', 'forwarding'],
                  ['统计', 'observe'],
                ] as const
              ).map(([label, icon]) => (
                <button key={label} type="button" role="tab" aria-selected={label === '转发'} tabIndex={-1}>
                  <Icon of={icon} size={14} className="nd-tab-ic" />
                  {label}
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>
      <div className="user-dbody" role="tabpanel" aria-label="转发">
        {children}
      </div>
    </section>
  );
}

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

function App() {
  const [variant, setVariant] = useState<Variant>(
    pick(
      params.get('v'),
      VARIANTS.map(item => item.value),
      'inline',
    ),
  );
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'typical'));
  const [preset, setPreset] = useState<Preset>(
    pick(
      params.get('open'),
      PRESETS.map(item => item.value),
      'none',
    ),
  );
  const [width, setWidth] = useState(Number(params.get('w')) || 1180);
  const [pingOutcome, setPingOutcome] = useState<PingOutcome>(
    pick(
      params.get('ping'),
      PING_OUTCOMES.map(item => item.value),
      'connected',
    ),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [round, setRound] = useState(0);
  const frameRef = useRef<HTMLDivElement>(null);
  const [frameWidth, setFrameWidth] = useState(width);
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const node = frameRef.current;
    if (!node) return;
    const observer = new ResizeObserver(([item]) => setFrameWidth(item.contentRect.width));
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('v', variant);
    next.set('scene', sceneKey);
    next.set('open', preset);
    next.set('w', String(width));
    next.set('theme', mode);
    next.set('ping', pingOutcome);
    history.replaceState(null, '', `?${next}`);
  }, [variant, sceneKey, preset, width, mode, pingOutcome]);
  const scene = useMemo(() => SCENES[sceneKey](), [sceneKey, round]); // eslint-disable-line react-hooks/exhaustive-deps
  const option = VARIANTS.find(item => item.value === variant)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              用户详情 · 转发 · 授权与规则<small>MOCKUP</small>
            </span>
            <div className="mk-controls">
              <span className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  ariaLabel="方案"
                  value={variant}
                  options={VARIANTS.map(item => ({ value: item.value, label: item.label }))}
                  onChange={setVariant}
                />
              </span>
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <select value={sceneKey} onChange={event => setSceneKey(event.target.value as SceneKey)}>
                  {SCENE_KEYS.map(key => (
                    <option key={key} value={key}>
                      {SCENES[key]().label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">打开</span>
                <select value={preset} onChange={event => setPreset(event.target.value as Preset)}>
                  {PRESETS.map(item => (
                    <option key={item.value} value={item.value}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">模拟 TCP Ping</span>
                <select
                  aria-label="模拟 TCP Ping 结果"
                  value={pingOutcome}
                  onChange={event => setPingOutcome(event.target.value as PingOutcome)}
                >
                  {PING_OUTCOMES.map(item => (
                    <option key={item.value} value={item.value}>
                      {item.label}
                    </option>
                  ))}
                </select>
              </label>
              <button type="button" className="btn mk-reset" onClick={() => setRound(value => value + 1)}>
                重置数据
              </button>
              <span className="mk-ctl">
                <span className="mk-ctl-label">宽度</span>
                <SegmentedControl ariaLabel="画框宽度" value={width} options={WIDTHS} onChange={setWidth} />
              </span>
              <span className="mk-ctl">
                <span className="mk-ctl-label">主题</span>
                <SegmentedControl
                  ariaLabel="主题"
                  value={mode}
                  options={[
                    { value: 'light', label: '亮' },
                    { value: 'dark', label: '暗' },
                  ]}
                  onChange={next => {
                    if (theme.snapshot() !== next) theme.toggle();
                    setMode(next);
                  }}
                />
              </span>
            </div>
          </div>
          <p className="mk-scene-note">
            <b>{option.label}</b>
            {option.note}
            <span className="mk-dot">·</span>
            {scene.note}
          </p>
        </div>
      )}
      <div className="mk-stage">
        <div ref={frameRef} className="mk-frame" style={{ width: `min(100%, ${width}px)` }}>
          <DetailFrame>
            <Fragment key={`${variant}/${sceneKey}/${preset}/${round}`}>
              {variant === 'current' ? (
                <Current key={pingOutcome} scene={scene} pingOutcome={pingOutcome} />
              ) : (
                <ForwardingTab
                  scene={scene}
                  layout={variant === 'popover' && frameWidth < 560 ? 'inline' : variant}
                  preset={preset}
                  pingOutcome={pingOutcome}
                />
              )}
            </Fragment>
          </DetailFrame>
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
