import { InlineField } from '../ui/inline-field';
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import {
  createForwardingRule,
  deleteForwardingEntitlement,
  deleteForwardingRule,
  fetchForwardingOverview,
  fetchForwardingProbes,
  fetchNodes,
  fetchUserCycleUsage,
  setForwardingEntitlement,
  setForwardingQuota,
  updateForwardingRule,
  type ForwardingEntitlement,
  type ForwardingEntitlementWrite,
  type ForwardingOverview,
  type ForwardingPort,
  type ForwardingProtocol,
  type ForwardingProbe,
  type ForwardingRule,
  type ForwardingRuleWrite,
  type UserListItem,
  type UserTrafficAccountingMode,
} from '../api';
import { Confirm, EmptyState, ErrorBox, SegmentedControl } from '../ui/bits';
import { CopyButton, copyStateIcon } from '../ui/copy-button';
import { bytes, shareOf } from '../ui/format';
import { blockedAddress, endpointText, parseTarget, submittedHost, type ParsedTarget } from '../ui/forwarding-target';
import { Icon, PanelTitle, type IconName } from '../ui/icons';
import { FieldLoading } from '../ui/loading';
import { observeBpsReading } from '../ui/observe-chart';
import { RegionFlag } from '../ui/region-flag';
import { useUnsavedChanges } from '../ui/navigation-guard';
import { forwardingRuleRates, useUserTrafficSource } from '../user-traffic-source';
import { UserTrafficAccountingSetting } from '../user-traffic-accounting';
import {
  ForwardingQuotaHistory,
  forwardingQuotaJobsKey,
  forwardingQuotaText,
  forwardingQuotaValue,
} from './forwarding-quota';
import { ForwardingRuleProbe, forwardingProbeKey, forwardingProbePending } from './forwarding-probe';

/* 用户详情「转发」页签：共用服务端查询，规则面板独立筛选机器。
 * 本期转发独立于接入账本，已删除规则仍计入合计和所属机器；规则额度是数量上限。
 * 授权与规则都在原行下展开；停止/移除授权原位确认，删除规则使用公共确认框。 */

export interface ForwardingNodeOption {
  id: string;
  name?: string;
  public_ipv4?: string | null;
  public_ipv6?: string | null;
  api_port?: number | null;
}

type RuleSort = 'port' | 'cycle' | 'created';
type RuleFilter = 'all' | 'active' | 'attention' | 'disabled';
type Traffic = { down: number; up: number };

interface RuleDraft {
  nodeId: string;
  target: string;
  port: string;
  protocol: ForwardingProtocol;
  name: string;
}

type RuleEditor = ({ mode: 'new' } | { mode: 'edit'; ruleId: number }) & {
  session: number;
  draft: RuleDraft;
  initial: RuleDraft;
};

interface PolicyDraft {
  nodeId: string;
  tcp: boolean;
  udp: boolean;
  start: string;
  end: string;
  max: string;
  allowPrivate: boolean;
  quota: string;
}

type PolicyDialog = {
  mode: 'grant' | 'edit';
  session: number;
  draft: PolicyDraft;
  initialQuota: { text: string; bytes: number | null };
};

const PROTOCOL_LABEL: Record<ForwardingProtocol, string> = { tcp: 'TCP', udp: 'UDP', tcp_udp: 'TCP + UDP' };

// 新授权的默认策略：同时开放 TCP 与 UDP，使用完整的转发端口段，并允许内部目标。
const GRANT_DEFAULTS = { start: 30_000, end: 59_999, max: 100, allowPrivate: false } as const;

const writablePort = (port: ForwardingPort): number => {
  if (typeof port !== 'number') throw new Error('脱敏的转发信息不能用于修改规则或授权');
  return port;
};

const entitlementWrite = (entry: ForwardingEntitlement, enabled = entry.enabled): ForwardingEntitlementWrite => ({
  enabled,
  tcp_allowed: entry.tcp_allowed,
  udp_allowed: entry.udp_allowed,
  listen_port_start: writablePort(entry.listen_port_start),
  listen_port_end: writablePort(entry.listen_port_end),
  max_rules: entry.max_rules,
  allow_private_targets: entry.allow_private_targets,
});

const policyWrite = (draft: PolicyDraft, enabled: boolean): ForwardingEntitlementWrite => ({
  enabled,
  tcp_allowed: draft.tcp,
  udp_allowed: draft.udp,
  listen_port_start: Number(draft.start),
  listen_port_end: Number(draft.end),
  max_rules: Number(draft.max),
  allow_private_targets: draft.allowPrivate,
});
const policyLimit = (dialog: PolicyDialog) =>
  forwardingQuotaValue(dialog.draft.quota, dialog.initialQuota.text, dialog.initialQuota.bytes);
const policyChanged = (dialog: PolicyDialog, entry: ForwardingEntitlement | undefined) =>
  !entry ||
  JSON.stringify(policyWrite(dialog.draft, entry.enabled)) !== JSON.stringify(entitlementWrite(entry)) ||
  policyLimit(dialog) !== (entry.cycle_limit_bytes ?? null);
const effectiveStatus = (rule: ForwardingRule, entry: ForwardingEntitlement | undefined) =>
  !rule.enabled || !entry?.enabled ? 'disabled' : entry.quota_suspended ? 'quota-paused' : rule.runtime_status;
const ruleCategory = (rule: ForwardingRule, entry: ForwardingEntitlement | undefined): Exclude<RuleFilter, 'all'> => {
  const status = effectiveStatus(rule, entry);
  return status === 'active' || status === 'disabled' ? status : 'attention';
};

const defaultProtocol = (entry: ForwardingEntitlement): ForwardingProtocol =>
  entry.tcp_allowed && entry.udp_allowed ? 'tcp_udp' : entry.tcp_allowed ? 'tcp' : 'udp';

const usesTcp = (protocol: ForwardingProtocol) => protocol !== 'udp';
const usesUdp = (protocol: ForwardingProtocol) => protocol !== 'tcp';
const protocolsOverlap = (left: ForwardingProtocol, right: ForwardingProtocol) =>
  (usesTcp(left) && usesTcp(right)) || (usesUdp(left) && usesUdp(right));
const protocolAllowed = (entry: ForwardingEntitlement, protocol: ForwardingProtocol) =>
  (!usesTcp(protocol) || entry.tcp_allowed) && (!usesUdp(protocol) || entry.udp_allowed);
const allowedText = (entry: { tcp_allowed: boolean; udp_allowed: boolean }) =>
  [entry.tcp_allowed && 'TCP', entry.udp_allowed && 'UDP'].filter(Boolean).join(' + ');

/** 同一台机器上已启用、协议有交集的规则占用该端口；停用的规则不占端口。 */
const occupantOf = (
  rules: ForwardingRule[],
  nodeId: string,
  protocol: ForwardingProtocol,
  port: number,
  excludeRuleId?: number,
) =>
  rules.find(
    rule =>
      rule.id !== excludeRuleId &&
      rule.node_id === nodeId &&
      rule.enabled &&
      rule.listen_port === port &&
      protocolsOverlap(rule.protocol, protocol),
  );

const nextFreePort = (
  entry: ForwardingEntitlement,
  rules: ForwardingRule[],
  protocol: ForwardingProtocol,
  excludeRuleId?: number,
) => {
  if (typeof entry.listen_port_start !== 'number' || typeof entry.listen_port_end !== 'number') return null;
  for (let port = entry.listen_port_start; port <= entry.listen_port_end; port += 1) {
    if (!occupantOf(rules, entry.node_id, protocol, port, excludeRuleId)) return port;
  }
  return null;
};

const entryState = (entry: ForwardingEntitlement, rules: ForwardingRule[]) => {
  if (!entry.enabled) return { tone: 'off', text: '已停止' };
  if (entry.quota_suspended) return { tone: 'warn', text: '配额暂停' };
  const own = rules.filter(rule => rule.node_id === entry.node_id);
  const count = (status: ForwardingRule['runtime_status']) =>
    own.filter(rule => effectiveStatus(rule, entry) === status).length;
  const active = count('active');
  const pending = count('pending');
  const failed = count('error');
  const unknown = count('unknown');
  const servicePaused = count('service-paused');
  const disabled = count('disabled');
  if (own.length) {
    return {
      tone: failed ? 'bad' : pending || unknown || servicePaused ? 'warn' : active ? 'ok' : 'off',
      text: [
        active && `${active} 条正常`,
        pending && `${pending} 条同步中`,
        failed && `${failed} 条失败`,
        unknown && `${unknown} 条待确认`,
        servicePaused && `${servicePaused} 条有效期暂停`,
        disabled && `${disabled} 条已停用`,
      ]
        .filter(Boolean)
        .join(' · '),
    };
  }
  if (entry.reconcile_status === 'error') return { tone: 'warn', text: '入口状态待确认' };
  return entry.applied_generation !== entry.desired_generation
    ? { tone: 'warn', text: '同步中' }
    : { tone: 'ok', text: '已就绪' };
};

const RULE_STATUS: Record<ForwardingRule['runtime_status'], { tone: string; text: string } | null> = {
  active: null,
  pending: { tone: 'warn', text: '同步中' },
  error: { tone: 'bad', text: '收敛失败' },
  disabled: { tone: 'off', text: '已停用' },
  'quota-paused': { tone: 'warn', text: '配额暂停' },
  'service-paused': { tone: 'warn', text: '有效期暂停' },
  unknown: { tone: 'warn', text: '状态待确认' },
};

/** 规则名默认就是目标地址；只有自定义过的名称才单独显示。 */
const customName = (rule: ForwardingRule) =>
  rule.name.trim() !== endpointText(rule.target_host, rule.target_port) ? rule.name : null;

// 服务端时间形如 `2026-10-14 07:03:09.915561+00`，先整理成浏览器都能解析的 ISO 形式。
const parseTimestamp = (text: string) =>
  Date.parse(
    text
      .trim()
      .replace(' ', 'T')
      .replace(/(\.\d{3})\d+/, '$1')
      .replace(/([+-]\d{2})$/, '$1:00'),
  );

const clockText = (text: string) => {
  const at = parseTimestamp(text);
  if (Number.isNaN(at)) return text;
  const date = new Date(at);
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
};

const rateText = (bytesPerSec: number) => observeBpsReading(bytesPerSec * 8);

const entryAddress = (entry: ForwardingEntitlement, port: ForwardingPort, family: 'v4' | 'v6') => {
  const host = family === 'v4' ? entry.public_ipv4 : entry.public_ipv6;
  return host ? endpointText(host, port) : null;
};

interface RuleCheck {
  target: ParsedTarget | null;
  targetBad: boolean;
  portBad: boolean;
  problems: string[];
  ok: boolean;
}

function checkRule(
  draft: RuleDraft,
  entry: ForwardingEntitlement | undefined,
  rules: ForwardingRule[],
  editingId: number | undefined,
): RuleCheck {
  const parsed = parseTarget(draft.target);
  const target = 'error' in parsed ? null : parsed;
  const problems: string[] = [];
  let targetBad = false;
  let portBad = false;
  if (!entry) return { target, targetBad, portBad, problems: ['选择转发入口'], ok: false };
  if (typeof entry.listen_port_start !== 'number' || typeof entry.listen_port_end !== 'number')
    return { target, targetBad, portBad, problems: ['脱敏的转发信息仅供查看'], ok: false };
  if (!entry.enabled) return { target, targetBad, portBad, problems: [`${entry.node_name} 已停止转发`], ok: false };
  if ('error' in parsed) {
    if (draft.target.trim()) {
      problems.push(parsed.error);
      targetBad = true;
    }
  } else if (blockedAddress(parsed.host, entry.allow_private_targets)) {
    problems.push(
      entry.allow_private_targets
        ? '回环、链路本地、元数据及保留地址始终拒绝'
        : `${entry.node_name} 未允许转发到私网、环回或保留地址`,
    );
    targetBad = true;
  }
  const port = Number(draft.port);
  if (!/^\d+$/.test(draft.port.trim())) {
    problems.push('入口端口填数字');
    portBad = true;
  } else if (port < entry.listen_port_start || port > entry.listen_port_end) {
    problems.push(`入口端口超出授权范围 ${entry.listen_port_start}–${entry.listen_port_end}`);
    portBad = true;
  } else {
    const occupant = occupantOf(rules, entry.node_id, draft.protocol, port, editingId);
    if (occupant) {
      const free = nextFreePort(entry, rules, draft.protocol, editingId);
      problems.push(
        `入口端口 ${port} 已被「${occupant.name}」占用（${PROTOCOL_LABEL[occupant.protocol]}）${
          free === null ? '，授权范围内已无空闲端口' : `，下一个空闲端口 ${free}`
        }`,
      );
      portBad = true;
    }
  }
  if (!protocolAllowed(entry, draft.protocol)) problems.push(`${entry.node_name} 只授权了 ${allowedText(entry)}`);
  if (editingId === undefined && rules.filter(rule => rule.node_id === entry.node_id).length >= entry.max_rules) {
    problems.push(`${entry.node_name} 已达上限 ${entry.max_rules} 条`);
  }
  return { target, targetBad, portBad, problems, ok: !!target && problems.length === 0 };
}

interface PolicyCheck {
  rangeText: string;
  rangeBad: boolean;
  maxText: string;
  maxBad: boolean;
  problem: string | null;
  ok: boolean;
}

function checkPolicy(dialog: PolicyDialog, rules: ForwardingRule[]): PolicyCheck {
  const { draft } = dialog;
  const ownRules = rules.filter(rule => rule.node_id === draft.nodeId);
  const enabledRules = ownRules.filter(rule => rule.enabled);
  const start = Number(draft.start);
  const end = Number(draft.end);
  const max = Number(draft.max);
  let rangeText: string;
  let rangeBad = true;
  if (!/^\d+$/.test(draft.start.trim()) || !/^\d+$/.test(draft.end.trim())) rangeText = '填数字端口';
  else if (start < 1024 || end > 65_535 || end < start) rangeText = '范围在 1024–65535 内，结束不小于起始';
  else {
    const outside = enabledRules.filter(
      rule => typeof rule.listen_port !== 'number' || rule.listen_port < start || rule.listen_port > end,
    );
    rangeBad = outside.length > 0;
    rangeText = rangeBad
      ? `${outside.length} 条启用中的规则不在新范围内（${outside.map(rule => rule.listen_port).join('、')}）`
      : `共 ${end - start + 1} 个端口`;
  }
  let maxText = '1–256 条';
  let maxBad = true;
  if (/^\d+$/.test(draft.max.trim()) && max >= 1 && max <= 256) {
    maxBad = max < ownRules.length;
    maxText = maxBad
      ? `已有 ${ownRules.length} 条规则，上限不能低于 ${ownRules.length}`
      : dialog.mode === 'edit'
        ? `已建 ${ownRules.length} 条`
        : maxText;
  }
  let problem: string | null = null;
  if (!draft.tcp && !draft.udp) problem = '至少允许一种协议';
  else {
    const blocked = enabledRules.filter(
      rule => (usesTcp(rule.protocol) && !draft.tcp) || (usesUdp(rule.protocol) && !draft.udp),
    );
    if (blocked.length > 0) problem = `${blocked.length} 条启用中的规则使用了要关闭的协议，先停用或修改这些规则`;
  }
  if (dialog.mode === 'grant' && !draft.nodeId) problem = problem ?? '选择要授权的机器';
  return { rangeText, rangeBad, maxText, maxBad, problem, ok: !rangeBad && !maxBad && !problem };
}

export function UserForwardingCard({
  user,
  nodes,
  canAuthorize,
  canManageRules,
  canViewRealtime = false,
  accountingDisabled,
  onAccountingModeChange,
  servicePolicy,
}: {
  user: UserListItem;
  nodes: ForwardingNodeOption[];
  canAuthorize: boolean;
  canManageRules: boolean;
  /** 当前速率来自用户实时订阅，只有能看实时流量的身份才订阅。 */
  canViewRealtime?: boolean;
  accountingDisabled: boolean;
  onAccountingModeChange: (mode: UserTrafficAccountingMode) => void;
  /** 账期与有效期由用户页提供（与「接入」读数栏同一份），排在计量口径之后。 */
  servicePolicy?: ReactNode;
}) {
  const qc = useQueryClient();
  const key = ['forwarding', user.tenant_id, user.id] as const;
  const overview = useQuery({
    queryKey: key,
    queryFn: () => fetchForwardingOverview(user.tenant_id, user.id),
    refetchInterval: query =>
      query.state.data?.rules.some(rule => ['pending', 'error', 'unknown'].includes(rule.runtime_status)) ||
      query.state.data?.entitlements.some(
        entry =>
          entry.enabled &&
          (entry.desired_generation !== entry.applied_generation ||
            entry.quota_job_status === 'queued' ||
            entry.quota_job_status === 'running' ||
            (entry.cycle_limit_bytes != null &&
              entry.cycle_used_bytes !== undefined &&
              entry.cycle_used_bytes >= entry.cycle_limit_bytes) !== !!entry.quota_suspended),
      )
        ? 5_000
        : query.state.data?.entitlements.some(entry => entry.enabled)
          ? 30_000
          : false,
  });
  // 本期用量与用户页「用量与额度」共用同一份账期汇总
  const probes = useQuery({
    queryKey: forwardingProbeKey(user.tenant_id, user.id),
    queryFn: () => fetchForwardingProbes(user.tenant_id, user.id),
    enabled: canManageRules && !!overview.data?.rules.length,
    refetchInterval: query => (query.state.data?.some(forwardingProbePending) ? 2_000 : false),
    retry: false,
  });
  const cycle = useQuery({ queryKey: ['user-cycles'], queryFn: () => fetchUserCycleUsage() });
  // 地区旗取机器列表；取不到时不画旗，不影响其余内容
  const fleet = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), retry: false });
  const realtime = useUserTrafficSource(user.tenant_id, user.id, canViewRealtime);
  const accountingMultiplier = user.traffic_accounting_mode === 'double' ? 2 : 1;

  const [pickedNode, setFilterNode] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<RuleFilter>('all');
  const [search, setSearch] = useState('');
  const [sort, setSort] = useState<RuleSort>('port');
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(() => new Set());
  const [editor, setEditor] = useState<RuleEditor | null>(null);
  const [policy, setPolicy] = useState<PolicyDialog | null>(null);
  const [menu, setMenu] = useState<number | null>(null);
  const [deleting, setDeleting] = useState<ForwardingRule | null>(null);
  const serial = useRef(0);
  const opener = useRef<HTMLElement | null>(null);
  const activeSession = useRef<number | null>(null);
  useLayoutEffect(() => {
    activeSession.current = editor?.session ?? policy?.session ?? null;
  }, [editor?.session, policy?.session]);
  const newSession = useCallback((trigger: HTMLElement) => {
    opener.current = trigger;
    return ++serial.current;
  }, []);
  const restoreFocus = useCallback((session: number) => {
    if (activeSession.current === session) opener.current?.focus();
  }, []);
  const closeEditors = useCallback(() => {
    opener.current?.focus();
    setEditor(null);
    setPolicy(null);
  }, []);
  const storeResult = (value: ForwardingOverview) => qc.setQueryData(key, value);
  const savePolicy = useMutation({
    mutationFn: async (dialog: PolicyDialog) => {
      const nodeId = dialog.draft.nodeId;
      let value = qc.getQueryData<ForwardingOverview>(key);
      let current = value?.entitlements.find(entry => entry.node_id === nodeId);
      const body = policyWrite(dialog.draft, current?.enabled ?? true);
      const limit = policyLimit(dialog);
      if (limit === undefined || !checkPolicy(dialog, value?.rules ?? []).ok)
        throw new Error('授权参数无效，请检查后重试');
      let savedPolicy = false;
      if (!current || JSON.stringify(body) !== JSON.stringify(entitlementWrite(current))) {
        value = await setForwardingEntitlement(user.tenant_id, user.id, nodeId, body);
        storeResult(value);
        current = value.entitlements.find(entry => entry.node_id === nodeId);
        savedPolicy = true;
        // A newly granted node must stay editable if its separate quota request fails.
        setPolicy(active => (active?.session === dialog.session ? { ...active, mode: 'edit' } : active));
      }
      if (!current) throw new Error('保存响应缺少转发授权，请刷新后检查');
      if (limit !== (current.cycle_limit_bytes ?? null)) {
        try {
          value = await setForwardingQuota(user.tenant_id, user.id, nodeId, limit);
          storeResult(value);
          void qc.invalidateQueries({ queryKey: forwardingQuotaJobsKey(user.tenant_id, user.id, nodeId) });
        } catch (error) {
          throw new Error(
            `${current.node_name}：${savedPolicy ? '授权已保存，配额保存失败' : '配额保存失败'}：${error instanceof Error ? error.message : '请求失败，请重试'}`,
            { cause: error },
          );
        }
      }
      return value;
    },
    onSuccess: (_value, dialog) => {
      restoreFocus(dialog.session);
      setPolicy(active => (active?.session === dialog.session ? null : active));
      void qc.invalidateQueries({ queryKey: forwardingProbeKey(user.tenant_id, user.id) });
    },
  });
  const lifecycle = useMutation({
    mutationFn: async ({ nodeId, kind }: { nodeId: string; kind: 'stop' | 'remove' | 'resume'; session?: number }) => {
      const current = qc.getQueryData<ForwardingOverview>(key)?.entitlements.find(entry => entry.node_id === nodeId);
      if (!current) throw new Error('转发授权已不存在，请刷新');
      if (kind === 'remove') {
        if (current.enabled) throw new Error('先停止这台机器的转发，再移除入口');
        return deleteForwardingEntitlement(user.tenant_id, user.id, nodeId);
      }
      return setForwardingEntitlement(user.tenant_id, user.id, nodeId, entitlementWrite(current, kind === 'resume'));
    },
    onSuccess: (value, variables) => {
      storeResult(value);
      if (variables.kind === 'remove') setFilterNode(current => (current === variables.nodeId ? null : current));
      if (variables.session !== undefined) {
        restoreFocus(variables.session);
        setPolicy(active => (active?.session === variables.session ? null : active));
      }
      void qc.invalidateQueries({ queryKey: forwardingProbeKey(user.tenant_id, user.id) });
    },
  });
  const createRule = useMutation({
    mutationFn: ({ body }: { body: ForwardingRuleWrite; session: number }) =>
      createForwardingRule(user.tenant_id, user.id, body),
    onSuccess: (value, variables) => {
      storeResult(value);
      restoreFocus(variables.session);
      setEditor(active => (active?.session === variables.session ? null : active));
      void qc.invalidateQueries({ queryKey: forwardingProbeKey(user.tenant_id, user.id) });
    },
  });
  const updateRule = useMutation({
    mutationFn: (value: { ruleId: number; body: ForwardingRuleWrite; session?: number }) =>
      updateForwardingRule(user.tenant_id, user.id, value.ruleId, value.body),
    onSuccess: (value, variables) => {
      storeResult(value);
      if (variables.session !== undefined) {
        restoreFocus(variables.session);
        setEditor(active => (active?.session === variables.session ? null : active));
      }
      void qc.invalidateQueries({ queryKey: forwardingProbeKey(user.tenant_id, user.id) });
    },
  });
  const removeRule = useMutation({
    mutationFn: (ruleId: number) => deleteForwardingRule(user.tenant_id, user.id, ruleId),
    onSuccess: (value, ruleId) => {
      storeResult(value);
      setDeleting(null);
      setExpanded(current => {
        const next = new Set(current);
        next.delete(ruleId);
        return next;
      });
    },
  });

  const entries = useMemo(() => overview.data?.entitlements ?? [], [overview.data]);
  const rules = useMemo(() => overview.data?.rules ?? [], [overview.data]);
  const filterNode = entries.some(entry => entry.node_id === pickedNode) ? pickedNode : null;
  const entryOf = (nodeId: string) => entries.find(entry => entry.node_id === nodeId);
  const activeEntries = entries.filter(entry => entry.enabled);
  const granted = new Set(entries.map(entry => entry.node_id));
  const availableNodes = nodes.filter(node => !granted.has(node.id) && node.api_port != null);
  const countryOf = useMemo(() => {
    const map = new Map((fleet.data?.nodes ?? []).map(node => [node.node_id, node.public_ipv4_country ?? null]));
    return (nodeId: string) => map.get(nodeId) ?? null;
  }, [fleet.data]);
  const loadFailed = !!overview.error && !overview.data;

  // 本期：账期汇总里这位用户的转发行；已删除的规则只计入合计
  const cycleTraffic = useMemo(() => {
    if (!cycle.data) return null;
    const rows = (cycle.data.forwarding ?? []).filter(
      row => row.tenant_id === user.tenant_id && row.user_id === user.id,
    );
    const byRule = new Map<number, Traffic>();
    const byNode = new Map<string, Traffic>();
    for (const row of rows) {
      const down = row.downlink_bytes * accountingMultiplier;
      const up = row.uplink_bytes * accountingMultiplier;
      const node = byNode.get(row.node_id) ?? { down: 0, up: 0 };
      byNode.set(row.node_id, { down: node.down + down, up: node.up + up });
      const current = byRule.get(row.rule_id) ?? { down: 0, up: 0 };
      byRule.set(row.rule_id, { down: current.down + down, up: current.up + up });
    }
    const live = new Set(rules.map(rule => rule.id));
    const down = rows.reduce((sum, row) => sum + row.downlink_bytes, 0) * accountingMultiplier;
    const up = rows.reduce((sum, row) => sum + row.uplink_bytes, 0) * accountingMultiplier;
    const deleted =
      rows.filter(row => !live.has(row.rule_id)).reduce((sum, row) => sum + row.downlink_bytes + row.uplink_bytes, 0) *
      accountingMultiplier;
    const period = cycle.data.periods?.find(p => p.tenant_id === user.tenant_id && p.user_id === user.id);
    return { byRule, byNode, down, up, deleted, start: period?.starts_at, end: period?.ends_at };
  }, [accountingMultiplier, cycle.data, rules, user.tenant_id, user.id]);
  const cycleOf = (rule: ForwardingRule): Traffic | null =>
    cycleTraffic ? (cycleTraffic.byRule.get(rule.id) ?? { down: 0, up: 0 }) : null;

  // 当前：只认新鲜、就绪的报告；覆盖不到的机器上的规则读作「—」，不是 0
  const live = useMemo(
    () => (canViewRealtime ? forwardingRuleRates(realtime.payload, user.tenant_id, user.id, realtime.now) : null),
    [canViewRealtime, realtime.payload, realtime.now, user.tenant_id, user.id],
  );
  const liveOf = (rule: ForwardingRule): Traffic | null =>
    live && rule.runtime_status === 'active'
      ? (live.rates.get(rule.id) ?? (live.covered.has(rule.node_id) ? { down: 0, up: 0 } : null))
      : null;
  const liveSum = useMemo(() => {
    if (!live || (live.covered.size === 0 && live.rates.size === 0)) return null;
    let total = 0;
    let active = 0;
    for (const rate of live.rates.values()) {
      total += rate.down + rate.up;
      if (rate.down + rate.up > 0) active += 1;
    }
    return { total, active };
  }, [live]);

  const maxCycle = Math.max(
    1,
    ...rules.map(rule => (cycleTraffic?.byRule.get(rule.id)?.down ?? 0) + (cycleTraffic?.byRule.get(rule.id)?.up ?? 0)),
  );
  const scopedRules = rules.filter(rule => !filterNode || rule.node_id === filterNode);
  const statusCounts = {
    all: scopedRules.length,
    active: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.node_id)) === 'active').length,
    attention: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.node_id)) === 'attention').length,
    disabled: scopedRules.filter(rule => ruleCategory(rule, entryOf(rule.node_id)) === 'disabled').length,
  };
  const visible = useMemo(() => {
    const query = search.trim().toLowerCase();
    const filtered = rules.filter(rule => {
      const entry = entries.find(entry => entry.node_id === rule.node_id);
      return (
        (!filterNode || rule.node_id === filterNode) &&
        (statusFilter === 'all' || ruleCategory(rule, entry) === statusFilter) &&
        (!query ||
          `:${rule.listen_port} ${rule.name} ${endpointText(rule.target_host, rule.target_port)} ${entry?.node_name ?? ''}`
            .toLowerCase()
            .includes(query))
      );
    });
    const totalOf = (rule: ForwardingRule) => {
      const value = cycleTraffic?.byRule.get(rule.id);
      return value ? value.down + value.up : 0;
    };
    const by: Record<RuleSort, (a: ForwardingRule, b: ForwardingRule) => number> = {
      port: (a, b) =>
        typeof a.listen_port === 'number' && typeof b.listen_port === 'number' ? a.listen_port - b.listen_port : 0,
      cycle: (a, b) => totalOf(b) - totalOf(a),
      created: (a, b) => parseTimestamp(b.created_at) - parseTimestamp(a.created_at),
    };
    return [...filtered].sort((a, b) => by[sort](a, b) || a.id - b.id);
  }, [rules, entries, filterNode, statusFilter, search, sort, cycleTraffic]);
  const creatableEntries = activeEntries.filter(
    entry =>
      (!filterNode || entry.node_id === filterNode) &&
      rules.filter(rule => rule.node_id === entry.node_id).length < entry.max_rules,
  );
  const selectedEntry = filterNode ? entryOf(filterNode) : undefined;
  const createHint = !entries.length
    ? '先授权一台机器'
    : selectedEntry && !selectedEntry.enabled
      ? '这台机器已停止转发'
      : '规则数已达上限';
  const closeRuleView = () => {
    setEditor(null);
    setMenu(null);
  };
  const beginCreate = (trigger: HTMLElement) => {
    const entry = creatableEntries[0];
    if (!entry) return;
    if (editor?.mode === 'new') {
      closeEditors();
      return;
    }
    const protocol = defaultProtocol(entry);
    const draft: RuleDraft = {
      nodeId: entry.node_id,
      target: '',
      port: String(nextFreePort(entry, rules, protocol) ?? ''),
      protocol,
      name: '',
    };
    setPolicy(null);
    setMenu(null);
    setEditor({ mode: 'new', session: newSession(trigger), initial: draft, draft });
  };
  const beginEdit = (rule: ForwardingRule, trigger: HTMLElement) => {
    if (editor?.mode === 'edit' && editor.ruleId === rule.id) {
      closeEditors();
      return;
    }
    const draft = {
      nodeId: rule.node_id,
      target: endpointText(rule.target_host, rule.target_port),
      port: String(rule.listen_port),
      protocol: rule.protocol,
      name: customName(rule) ?? '',
    };
    setPolicy(null);
    setMenu(null);
    setEditor({ mode: 'edit', ruleId: rule.id, session: newSession(trigger), initial: draft, draft });
  };
  const toggleRule = (rule: ForwardingRule) =>
    updateRule.mutate({
      ruleId: rule.id,
      body: {
        node_id: rule.node_id,
        name: rule.name,
        protocol: rule.protocol,
        listen_port: writablePort(rule.listen_port),
        target_host: rule.target_host,
        target_port: writablePort(rule.target_port),
        enabled: !rule.enabled,
      },
    });
  const openGrant = (trigger: HTMLElement) => {
    if (policy?.mode === 'grant') {
      closeEditors();
      return;
    }
    setEditor(null);
    setMenu(null);
    setPolicy({
      mode: 'grant',
      session: newSession(trigger),
      initialQuota: { text: '', bytes: null },
      draft: {
        nodeId: '',
        tcp: true,
        udp: true,
        start: String(GRANT_DEFAULTS.start),
        end: String(GRANT_DEFAULTS.end),
        max: String(GRANT_DEFAULTS.max),
        allowPrivate: GRANT_DEFAULTS.allowPrivate,
        quota: '',
      },
    });
  };
  const openPolicy = (entry: ForwardingEntitlement, trigger: HTMLElement) => {
    if (policy?.mode === 'edit' && policy.draft.nodeId === entry.node_id) {
      closeEditors();
      return;
    }
    const quota = forwardingQuotaText(entry.cycle_limit_bytes);
    setEditor(null);
    setMenu(null);
    setPolicy({
      mode: 'edit',
      session: newSession(trigger),
      initialQuota: { text: quota, bytes: entry.cycle_limit_bytes ?? null },
      draft: {
        nodeId: entry.node_id,
        tcp: entry.tcp_allowed,
        udp: entry.udp_allowed,
        start: String(entry.listen_port_start),
        end: String(entry.listen_port_end),
        max: String(entry.max_rules),
        allowPrivate: entry.allow_private_targets,
        quota,
      },
    });
  };
  const toggleExpanded = (ruleId: number) => {
    if (editor?.mode === 'edit' && editor.ruleId === ruleId) {
      closeEditors();
      return;
    }
    setExpanded(current => {
      const next = new Set(current);
      if (next.has(ruleId)) next.delete(ruleId);
      else next.add(ruleId);
      return next;
    });
  };
  useUnsavedChanges(
    !!editor && JSON.stringify(editor.draft) !== JSON.stringify(editor.initial),
    `${user.id} 的转发规则`,
    `forwarding-rule:${user.tenant_id}/${user.id}`,
  );
  useUnsavedChanges(
    !!policy && policyChanged(policy, entryOf(policy.draft.nodeId)),
    `${user.id} 的转发授权`,
    `forwarding-node:${user.tenant_id}/${user.id}`,
  );
  const actionError =
    (updateRule.error &&
      (updateRule.variables?.session === undefined || updateRule.variables.session !== editor?.session)) ||
    (savePolicy.variables?.session !== policy?.session && savePolicy.error) ||
    null;
  const ruleForm = (draftEditor: RuleEditor) => {
    const editing = draftEditor.mode === 'edit' ? rules.find(rule => rule.id === draftEditor.ruleId) : undefined;
    const mutation = draftEditor.mode === 'new' ? createRule : updateRule;
    const ownMutation = mutation.variables?.session === draftEditor.session;
    return (
      <RuleForm
        key={draftEditor.session}
        editor={draftEditor}
        entry={entryOf(draftEditor.draft.nodeId)}
        entries={draftEditor.mode === 'new' && selectedEntry ? [selectedEntry] : activeEntries}
        rules={rules}
        pending={ownMutation && mutation.isPending}
        error={ownMutation ? mutation.error : null}
        onChange={draft =>
          setEditor(active => (active?.session === draftEditor.session ? { ...active, draft } : active))
        }
        onCancel={closeEditors}
        onSubmit={(target, draft) => {
          const body: ForwardingRuleWrite = {
            node_id: draft.nodeId,
            name: draft.name.trim() || endpointText(target.host, target.port),
            protocol: draft.protocol,
            listen_port: Number(draft.port),
            target_host: submittedHost(target),
            target_port: target.port,
            enabled: editing ? editing.enabled : true,
          };
          if (draftEditor.mode === 'new') createRule.mutate({ body, session: draftEditor.session });
          else updateRule.mutate({ ruleId: draftEditor.ruleId, body, session: draftEditor.session });
        }}
      />
    );
  };
  const policyForm = () => {
    if (!policy) return null;
    const ownSave = savePolicy.variables?.session === policy.session;
    const ownAction = lifecycle.variables?.session === policy.session;
    return (
      <PolicyEditorView
        key={policy.session}
        dialog={policy}
        rules={rules}
        nodes={availableNodes}
        countryOf={countryOf}
        entry={entryOf(policy.draft.nodeId)}
        tenant={user.tenant_id}
        user={user.id}
        pending={(ownSave && savePolicy.isPending) || (ownAction && lifecycle.isPending)}
        error={(ownSave && savePolicy.error) || (ownAction && lifecycle.error)}
        onChange={draft => setPolicy(active => (active?.session === policy.session ? { ...active, draft } : active))}
        onClose={closeEditors}
        onSubmit={() => savePolicy.mutate(policy)}
        onLifecycle={kind => lifecycle.mutate({ nodeId: policy.draft.nodeId, kind, session: policy.session })}
      />
    );
  };

  const sortedEntries = [...entries].sort(
    (a, b) => Number(b.enabled) - Number(a.enabled) || a.node_name.localeCompare(b.node_name, 'zh-CN'),
  );
  const [figure, unit] = cycleTraffic ? bytes(cycleTraffic.down + cycleTraffic.up).split(' ') : ['', ''];
  const deletionSummary = (rule: ForwardingRule) => {
    const entry = entryOf(rule.node_id);
    const usage = cycleOf(rule);
    const address = entry
      ? (entryAddress(entry, rule.listen_port, 'v4') ?? entryAddress(entry, rule.listen_port, 'v6'))
      : null;
    return {
      entry: address ?? `:${rule.listen_port}`,
      node: entry?.node_name ?? '转发入口',
      cycleTraffic: usage ? usage.down + usage.up : null,
    };
  };
  const deletingView = deleting ? deletionSummary(deleting) : null;
  const cycleDate = (text: string) => {
    const date = new Date(Date.parse(text) + 8 * 3_600_000);
    return `${date.getUTCMonth() + 1} 月 ${date.getUTCDate()} 日`;
  };

  return (
    <>
      <section className="panel config-panel user-dcard user-usage-card user-forwarding-usage">
        <header>
          <PanelTitle of="usage">用量与额度</PanelTitle>
          <span className="rt">
            <b>{entries.length}</b> 个入口
          </span>
        </header>
        <div className="user-dcard-body fwd-body">
          {overview.isPending ? (
            <FieldLoading label="读取转发授权" />
          ) : loadFailed ? (
            <ErrorBox error={overview.error} />
          ) : (
            <div className="user-usage panel-full-bleed-body">
              <aside className="fwd-ledger user-usage-ledger" aria-label="本期转发">
                <div className="user-usage-hero">
                  <span className="user-usage-hero-label">本期转发</span>
                  {cycleTraffic ? (
                    <strong className="user-usage-hero-value">
                      {figure} <small>{unit}</small>
                    </strong>
                  ) : (
                    <strong className="user-usage-hero-value none">{cycle.isPending ? '读取中' : '—'}</strong>
                  )}
                  {cycleTraffic?.start && cycleTraffic.end && (
                    <span className="user-usage-reset">
                      {cycleDate(cycleTraffic.start)}起<span className="usage-dot">·</span>
                      {cycleDate(cycleTraffic.end)}重置
                    </span>
                  )}
                  {cycleTraffic && cycleTraffic.down + cycleTraffic.up > 0 && (
                    <div className="usage-split" aria-hidden="true">
                      <i className="down" style={{ flexGrow: cycleTraffic.down }} />
                      <i className="up" style={{ flexGrow: cycleTraffic.up }} />
                    </div>
                  )}
                  {cycleTraffic && (
                    <dl className="usage-io">
                      <div className="down">
                        <dt>
                          <i aria-hidden="true" />
                          下行
                        </dt>
                        <dd>{bytes(cycleTraffic.down)}</dd>
                        <dd className="usage-share">
                          {shareOf(cycleTraffic.down, cycleTraffic.down + cycleTraffic.up)}
                        </dd>
                      </div>
                      <div className="up">
                        <dt>
                          <i aria-hidden="true" />
                          上行
                        </dt>
                        <dd>{bytes(cycleTraffic.up)}</dd>
                        <dd className="usage-share">{shareOf(cycleTraffic.up, cycleTraffic.down + cycleTraffic.up)}</dd>
                      </div>
                    </dl>
                  )}
                  {cycleTraffic && cycleTraffic.deleted > 0 && (
                    <span className="fwd-deleted">其中 {bytes(cycleTraffic.deleted)} 来自已删除的规则</span>
                  )}
                </div>
                <UserTrafficAccountingSetting
                  mode={user.traffic_accounting_mode ?? 'single'}
                  disabled={accountingDisabled}
                  onChange={onAccountingModeChange}
                />
                {servicePolicy}
                {canViewRealtime && (
                  <div className="fwd-live">
                    <span>当前</span>
                    <b>{liveSum ? rateText(liveSum.total) : '—'}</b>
                    {liveSum && <small>{liveSum.active} 条在转发</small>}
                  </div>
                )}
              </aside>
              {entries.length === 0 && policy?.mode !== 'grant' ? (
                <EmptyState
                  icon="forwarding"
                  title="尚未授权转发入口"
                  action={
                    canAuthorize && availableNodes.length > 0 ? (
                      <button
                        type="button"
                        className="btn primary fwd-empty-action"
                        onClick={event => openGrant(event.currentTarget)}
                      >
                        <Icon of="plus" size={13} />
                        授权机器
                      </button>
                    ) : undefined
                  }
                >
                  {canAuthorize
                    ? `授权一台机器后，${user.id} 可以在这台机器的端口范围内自建 TCP / UDP 端口转发。`
                    : '管理员授权机器后，用户才可以创建端口转发规则。'}
                </EmptyState>
              ) : (
                <div className="qta fwd-quotas fwe-aligned" role="list" aria-label="各转发入口的用量与规则额度">
                  {sortedEntries.map(entry => {
                    const state = entryState(entry, rules);
                    const count = rules.filter(rule => rule.node_id === entry.node_id).length;
                    const traffic = cycleTraffic?.byNode.get(entry.node_id);
                    const percent = entry.max_rules > 0 ? (count / entry.max_rules) * 100 : 0;
                    const editing = canAuthorize && policy?.mode === 'edit' && policy.draft.nodeId === entry.node_id;
                    const resuming =
                      lifecycle.isPending &&
                      lifecycle.variables?.kind === 'resume' &&
                      lifecycle.variables.nodeId === entry.node_id;
                    return (
                      <div
                        key={entry.node_id}
                        role="listitem"
                        className={`qta-r fwd-quota-row fwe-row ${percent >= 100 ? 'over' : percent >= 95 ? 'warn' : 'ok'}${editing ? ' fwe-open' : ''}`}
                      >
                        <span className="qta-name fwe-entry-name">
                          <span className="qta-app">
                            <RegionFlag code={countryOf(entry.node_id)} />
                            <b>{entry.node_name || entry.node_id}</b>
                          </span>
                          <span className="qta-sub">
                            {state.tone === 'ok' ? (
                              `${allowedText(entry)} · ${entry.listen_port_start}–${entry.listen_port_end}`
                            ) : (
                              <span className={`fwd-st ${state.tone}`}>{state.text}</span>
                            )}
                          </span>
                        </span>
                        <span className="qta-figs">
                          {entry.cycle_used_bytes !== undefined
                            ? bytes(entry.cycle_used_bytes)
                            : cycleTraffic
                              ? bytes(traffic ? traffic.down + traffic.up : 0)
                              : '—'}
                          {entry.cycle_limit_bytes != null && (
                            <small className="dim"> / {bytes(entry.cycle_limit_bytes)}</small>
                          )}
                        </span>
                        <span className="qta-left">
                          规则
                          <b>
                            {count} / {entry.max_rules}
                          </b>
                        </span>
                        <span className="qta-meter">
                          <span className="qta-t" aria-hidden="true">
                            <i style={{ width: `${Math.min(100, percent)}%` }} />
                          </span>
                          <span className="qta-pct">{Math.round(percent)}%</span>
                        </span>
                        <span className="qta-acts">
                          {canAuthorize && (
                            <>
                              {!entry.enabled && (
                                <button
                                  type="button"
                                  className="btn fwe-resume"
                                  aria-label={`恢复 ${entry.node_name} 的转发`}
                                  disabled={resuming}
                                  onClick={() => lifecycle.mutate({ nodeId: entry.node_id, kind: 'resume' })}
                                >
                                  {resuming ? '恢复中…' : '恢复'}
                                </button>
                              )}
                              <button
                                type="button"
                                className="fwd-ib"
                                aria-label={`编辑 ${entry.node_name} 的转发授权`}
                                title="编辑授权"
                                aria-expanded={editing}
                                onClick={event => openPolicy(entry, event.currentTarget)}
                              >
                                <Icon of="pencil" size={13} />
                              </button>
                            </>
                          )}
                        </span>
                        {editing && <div className="fwe-sheet">{policyForm()}</div>}
                        {lifecycle.error &&
                          lifecycle.variables?.nodeId === entry.node_id &&
                          lifecycle.variables.session === undefined && <ErrorBox error={lifecycle.error} />}
                      </div>
                    );
                  })}
                  {canAuthorize && policy?.mode === 'grant' ? (
                    <div className="qta-r fwd-quota-row fwe-row fwe-open fwe-grant" role="listitem">
                      <span className="qta-name">
                        <span className="qta-app">
                          <b>授权机器</b>
                        </span>
                        <span className="qta-sub">转发入口</span>
                      </span>
                      <div className="fwe-sheet">{policyForm()}</div>
                    </div>
                  ) : (
                    canAuthorize &&
                    availableNodes.length > 0 && (
                      <button type="button" className="fwd-add" onClick={event => openGrant(event.currentTarget)}>
                        <Icon of="plus" size={13} />
                        <b>授权机器</b>
                      </button>
                    )
                  )}
                </div>
              )}
            </div>
          )}
        </div>
        {!loadFailed && !overview.isPending && (
          <footer className="user-dcard-foot fwd-foot">
            <span>转发流量单独统计，不占用线路额度；规则保存后由转发入口自动生效，不需要变更单。</span>
          </footer>
        )}
      </section>
      <section className="panel config-panel user-dcard user-forwarding-card fwr-panel" aria-label="转发入口">
        <header>
          <PanelTitle of="forwarding">转发入口</PanelTitle>
          <span className="rt">
            <b>{rules.length}</b> 条规则
          </span>
          {canManageRules && overview.data && (
            <button
              type="button"
              className="btn fwr-create"
              disabled={creatableEntries.length === 0}
              title={creatableEntries.length === 0 ? createHint : undefined}
              aria-expanded={editor?.mode === 'new'}
              onClick={event => beginCreate(event.currentTarget)}
            >
              <Icon of="plus" size={13} />
              新建规则
            </button>
          )}
        </header>
        {/* 完整列表自行铺满卡片；加载与错误状态保留普通正文留白。 */}
        <div className="user-dcard-body fwd-body">
          {overview.isPending ? (
            <FieldLoading label="读取转发规则" />
          ) : loadFailed ? (
            <ErrorBox error={overview.error} />
          ) : (
            <div className="fwd-main fwr-main panel-full-bleed-body">
              <div className="fwr-tools">
                <label className="fwr-machine">
                  <span>机器</span>
                  <select
                    className="f"
                    aria-label="筛选转发机器"
                    value={filterNode ?? ''}
                    onChange={event => {
                      closeRuleView();
                      setFilterNode(event.target.value || null);
                    }}
                  >
                    <option value="">全部机器 · {entries.length}</option>
                    {sortedEntries.map(entry => (
                      <option key={entry.node_id} value={entry.node_id}>
                        {entry.node_name} · {rules.filter(rule => rule.node_id === entry.node_id).length} 条
                        {!entry.enabled ? ' · 已停止' : ''}
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
                    setSort(event.target.value as RuleSort);
                  }}
                >
                  <option value="port">端口升序</option>
                  <option value="cycle">本期流量 ↓</option>
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
                {canManageRules && selectedEntry && creatableEntries.length === 0 && (
                  <span className="fwr-constraint">{createHint}</span>
                )}
                <span className="fwr-result" role="status">
                  {visible.length} 条规则
                </span>
              </div>
              {actionError && <ErrorBox error={actionError} />}
              {overview.error && <ErrorBox error={overview.error} />}
              {editor?.mode === 'new' && (
                <div className="fwe-new">
                  <div className="fwe-new-head">新建规则{selectedEntry && ` · ${selectedEntry.node_name}`}</div>
                  {ruleForm(editor)}
                </div>
              )}
              <div className="fwr-groups">
                {sortedEntries.map(entry => {
                  const groupRules = visible.filter(rule => rule.node_id === entry.node_id);
                  if (!groupRules.length) return null;
                  const state = entryState(entry, rules);
                  return (
                    <section className="fwr-group" key={entry.node_id} aria-label={`${entry.node_name}的规则`}>
                      <header className="fwr-group-head">
                        <span className="fwr-group-name">
                          <RegionFlag code={countryOf(entry.node_id)} />
                          <b>{entry.node_name}</b>
                        </span>
                        {entry.public_ipv4 && <code>{entry.public_ipv4}</code>}
                        {state.tone !== 'ok' && <span className={`fwd-st ${state.tone}`}>{state.text}</span>}
                        <span className="fwr-group-count">{groupRules.length} 条</span>
                      </header>
                      <div className="fwr-columns" aria-hidden="true">
                        <span>入口端口</span>
                        <span>转发目标</span>
                        <span>协议</span>
                        <span>本期流量</span>
                        <span>{canViewRealtime ? '当前' : '状态'}</span>
                        <span />
                      </div>
                      <div role="list" aria-label={`${entry.node_name}转发规则`}>
                        {groupRules.map(rule => {
                          const editing = editor?.mode === 'edit' && editor.ruleId === rule.id;
                          return (
                            <RuleItem
                              key={rule.id}
                              rule={rule}
                              entry={entry}
                              probe={probes.data?.find(probe => probe.rule_id === rule.id)}
                              probeError={probes.error}
                              cycleTraffic={cycleOf(rule)}
                              live={liveOf(rule)}
                              showLive={canViewRealtime}
                              maxCycle={maxCycle}
                              open={expanded.has(rule.id)}
                              editing={editing}
                              sheet={editing ? ruleForm(editor) : null}
                              canManage={canManageRules && entry.enabled}
                              canProbe={canManageRules}
                              busy={updateRule.isPending && updateRule.variables?.ruleId === rule.id}
                              menuOpen={menu === rule.id}
                              onMenu={open => setMenu(open ? rule.id : null)}
                              onToggleOpen={() => toggleExpanded(rule.id)}
                              onEdit={trigger => beginEdit(rule, trigger)}
                              onToggleEnabled={() => toggleRule(rule)}
                              onDelete={() => {
                                removeRule.reset();
                                setDeleting(rule);
                              }}
                            />
                          );
                        })}
                      </div>
                    </section>
                  );
                })}
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
                        : canManageRules
                          ? '点击「新建规则」添加第一个转发目标。'
                          : '暂无转发规则。'}
                    </p>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </section>

      {deleting && deletingView && (
        <Confirm
          title="删除转发规则"
          body={
            <>
              <p className="fwd-cpath">
                <code>{deletingView.entry}</code>
                <i>→</i>
                <code>{endpointText(deleting.target_host, deleting.target_port)}</code>
              </p>
              <p>
                {deleting.enabled
                  ? `${deletingView.node} 立即停止监听 ${deleting.listen_port}，正在经过它的连接会断开。`
                  : `规则已停用，删除后入口端口 ${deleting.listen_port} 不再保留给它。`}
                {deletingView.cycleTraffic !== null && `本期已转发 ${bytes(deletingView.cycleTraffic)}，仍计入用量。`}
              </p>
              {removeRule.error && <ErrorBox error={removeRule.error} />}
            </>
          }
          confirmLabel={removeRule.isPending ? '删除中…' : '删除规则'}
          confirmDisabled={removeRule.isPending}
          onConfirm={() => removeRule.mutate(deleting.id)}
          onCancel={() => setDeleting(null)}
        />
      )}
    </>
  );
}

/* ── 规则行 ── */

function RuleItem({
  rule,
  entry,
  probe,
  probeError,
  cycleTraffic,
  live,
  showLive,
  maxCycle,
  open,
  editing,
  sheet,
  canManage,
  canProbe,
  busy,
  menuOpen,
  onMenu,
  onToggleOpen,
  onEdit,
  onToggleEnabled,
  onDelete,
}: {
  rule: ForwardingRule;
  entry: ForwardingEntitlement | undefined;
  probe: ForwardingProbe | undefined;
  probeError: Error | null;
  cycleTraffic: Traffic | null;
  live: Traffic | null;
  showLive: boolean;
  maxCycle: number;
  open: boolean;
  editing: boolean;
  sheet: ReactNode;
  canManage: boolean;
  canProbe: boolean;
  busy: boolean;
  menuOpen: boolean;
  onMenu: (open: boolean) => void;
  onToggleOpen: () => void;
  onEdit: (trigger: HTMLElement) => void;
  onToggleEnabled: () => void;
  onDelete: () => void;
}) {
  const name = customName(rule);
  const target = endpointText(rule.target_host, rule.target_port);
  const v4 = entry ? entryAddress(entry, rule.listen_port, 'v4') : null;
  const v6 = entry ? entryAddress(entry, rule.listen_port, 'v6') : null;
  const canCopy = typeof rule.listen_port === 'number';
  const state = effectiveStatus(rule, entry);
  const status = RULE_STATUS[state];
  const detailId = useId();
  const expandedNow = editing || open;
  const cycleTotal = cycleTraffic ? cycleTraffic.down + cycleTraffic.up : null;
  const nowCell = status ? (
    <span className={`fwd-st ${status.tone}`}>{status.text}</span>
  ) : !showLive ? (
    <span className="fwd-st ok">已监听</span>
  ) : live === null ? (
    <span className="fwd-idle">—</span>
  ) : live.down + live.up > 0 ? (
    <span className="fwd-rate" title={`下行 ${rateText(live.down)} · 上行 ${rateText(live.up)}`}>
      <i aria-hidden="true" />
      {rateText(live.down + live.up)}
    </span>
  ) : (
    <span className="fwd-idle">空闲</span>
  );
  const hasMenu = canManage || (canCopy && (!!v4 || !!v6));
  return (
    <div
      role="listitem"
      className={`fwd-item fwr-item st-${state}${expandedNow ? ' open' : ''}${editing ? ' fwe-editing' : ''}`}
    >
      <div className="fwd-row">
        <button
          type="button"
          className="fwr-rmain"
          aria-expanded={expandedNow}
          aria-controls={expandedNow ? detailId : undefined}
          aria-label={`${name ? `${name}，` : ''}入口 ${rule.listen_port} 转发到 ${target}`}
          onClick={onToggleOpen}
        >
          <span className="fwd-port">
            <Icon of="chevronDown" size={10} className="fwd-chev" />:{rule.listen_port}
          </span>
          <span className={`fwd-target${name ? '' : ' plain'}`} title={target}>
            {name && <b>{name}</b>}
            <code>
              <i>→</i>
              {target}
            </code>
          </span>
          <span className="fwd-proto">{PROTOCOL_LABEL[rule.protocol]}</span>
          <span
            className="fwd-cycle"
            title={
              cycleTraffic
                ? `本期 下行 ${bytes(cycleTraffic.down)} · 上行 ${bytes(cycleTraffic.up)}`
                : '本期用量暂不可用'
            }
          >
            <b>{cycleTotal === null ? '—' : bytes(cycleTotal)}</b>
            <span className="fwd-bar" aria-hidden="true">
              <i
                style={{
                  width: `${cycleTotal ? Math.max(2, (cycleTotal / maxCycle) * 100) : 0}%`,
                }}
              />
            </span>
          </span>
          <span className="fwd-now">{nowCell}</span>
        </button>
        <span className="fwr-acts">
          {canManage && (
            <button
              type="button"
              className="btn fwr-edit"
              aria-label={`编辑 ${entry?.node_name ?? ''} :${rule.listen_port}`}
              title="编辑"
              aria-expanded={editing}
              onClick={event => onEdit(event.currentTarget)}
            >
              编辑
            </button>
          )}
          {hasMenu && (
            <MoreMenu
              label={`${entry?.node_name ?? ''} :${rule.listen_port} 的更多操作`}
              open={menuOpen}
              onOpenChange={onMenu}
            >
              {canCopy && v4 && (
                <CopyButton text={v4} role="menuitem" label="复制 IPv4 入口">
                  {(state, label) => (
                    <>
                      <Icon of={copyStateIcon(state)} size={14} className="action-menu-icon" />
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
                  note={
                    rule.enabled ? `入口 ${rule.listen_port} 停止监听，规则保留` : `重新监听入口 ${rule.listen_port}`
                  }
                  disabled={busy}
                  onSelect={onToggleEnabled}
                />
              )}
              {canCopy && v6 && (
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
                  note={rule.enabled ? `入口 ${rule.listen_port} 立即停止，端口释放` : '规则与端口一起释放'}
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
        open && (
          <div className="fwd-detail fwr-detail" id={detailId}>
            <dl className="fwd-kv">
              <div className="wide">
                <dt>入口</dt>
                <dd>
                  {v4 && (
                    <span className="fwd-addr">
                      {v4}
                      {canCopy && <AddressCopy text={v4} label="复制 IPv4 入口" />}
                    </span>
                  )}
                  {v6 && (
                    <span className="fwd-addr">
                      {v6}
                      {canCopy && <AddressCopy text={v6} label="复制 IPv6 入口" />}
                    </span>
                  )}
                  {!v4 && !v6 && <span className="fwd-dim">暂无公网地址</span>}
                </dd>
              </div>
              <div className="wide">
                <dt>目标</dt>
                <dd>
                  <code>{target}</code>
                  <span className="fwd-dim"> · {PROTOCOL_LABEL[rule.protocol]}</span>
                </dd>
              </div>
              <div>
                <dt>本期</dt>
                <dd>{cycleTraffic ? <TrafficPair value={cycleTraffic} format={bytes} /> : '—'}</dd>
              </div>
              <div>
                <dt>累计</dt>
                <dd>
                  <TrafficPair value={{ down: rule.downlink_bytes, up: rule.uplink_bytes }} format={bytes} />
                </dd>
              </div>
              {showLive && (
                <div>
                  <dt>当前</dt>
                  <dd>{live ? <TrafficPair value={live} format={rateText} /> : <span className="fwd-dim">—</span>}</dd>
                </div>
              )}
              <div>
                <dt>创建</dt>
                <dd>
                  {clockText(rule.created_at)}
                  <span className="fwd-dim"> · 修改 {clockText(rule.updated_at)}</span>
                </dd>
              </div>
              <div className="wide">
                <dt>状态</dt>
                <dd>
                  <RuleStateLine rule={rule} entry={entry} />
                </dd>
              </div>
            </dl>
            {canProbe && (
              <ForwardingRuleProbe
                rule={rule}
                entry={entry}
                result={probe}
                queryError={probeError}
                canManage={canProbe}
              />
            )}
          </div>
        )
      )}
    </div>
  );
}

function RuleStateLine({ rule, entry }: { rule: ForwardingRule; entry: ForwardingEntitlement | undefined }) {
  const node = entry?.node_name ?? '转发入口';
  switch (effectiveStatus(rule, entry)) {
    case 'active':
      return <>已监听 · {node} 已应用这条规则</>;
    case 'pending':
      return (
        <>
          <span className="fwd-st warn">同步中</span> · 等待 {node} 应用这次改动
        </>
      );
    case 'error':
      return (
        <>
          <span className="fwd-st bad">收敛失败</span> · {rule.runtime_error ?? '机器未能应用这条转发规则'}
        </>
      );
    case 'unknown':
      return (
        <>
          <span className="fwd-st warn">状态待确认</span> · 等待 {node} 确认运行状态。
          {entry?.reported_at && `上次回报 ${clockText(entry.reported_at)}。`}
        </>
      );
    case 'disabled':
      return <>{entry?.enabled === false ? '入口转发已停止' : '已停用'} · 入口端口不监听，规则保留</>;
    case 'service-paused':
      return <>有效期暂停 · 服务生效或续期后恢复仍启用的规则</>;
    case 'quota-paused':
      return <>配额暂停 · 补足额度后恢复仍启用的规则</>;
  }
}

function TrafficPair({ value, format }: { value: Traffic; format: (value: number) => string }) {
  return (
    <span className="fwd-pair">
      <Icon of="rx" size={11} className="fwd-dir" />
      {format(value.down)}
      <Icon of="tx" size={11} className="fwd-dir up" />
      {format(value.up)}
    </span>
  );
}

function AddressCopy({ text, label }: { text: string; label: string }) {
  return (
    <CopyButton text={text} label={label} iconOnly className="fwd-copy">
      {state => <Icon of={copyStateIcon(state)} size={12} />}
    </CopyButton>
  );
}

/* ── 授权和规则共用的行内表单布局 ── */

function RuleForm({
  editor,
  entry,
  entries,
  rules,
  pending,
  error,
  onChange,
  onCancel,
  onSubmit,
}: {
  editor: RuleEditor;
  entry: ForwardingEntitlement | undefined;
  entries: ForwardingEntitlement[];
  rules: ForwardingRule[];
  pending: boolean;
  error: unknown;
  onChange: (draft: RuleDraft) => void;
  onCancel: () => void;
  onSubmit: (target: ParsedTarget, draft: RuleDraft) => void;
}) {
  const { draft } = editor;
  const creating = editor.mode === 'new';
  const editingId = editor.mode === 'edit' ? editor.ruleId : undefined;
  const id = useId();
  const check = checkRule(draft, entry, rules, editingId);
  const dirty = JSON.stringify(draft) !== JSON.stringify(editor.initial);
  const installed = rules.find(rule => rule.id === editingId);
  const changesListener =
    installed?.enabled &&
    (draft.nodeId !== editor.initial.nodeId ||
      draft.target !== editor.initial.target ||
      draft.port !== editor.initial.port ||
      draft.protocol !== editor.initial.protocol);
  const choosable = creating
    ? entries.filter(
        item =>
          item.node_id === draft.nodeId || rules.filter(rule => rule.node_id === item.node_id).length < item.max_rules,
      )
    : [];
  return (
    <form
      className="fwe-form rule inline"
      aria-label={
        creating
          ? '新建转发规则'
          : `编辑入口 ${rules.find(rule => rule.id === editingId)?.listen_port ?? draft.port} 的规则`
      }
      onKeyDown={event => {
        if (event.key === 'Escape' && !pending) {
          event.preventDefault();
          event.stopPropagation();
          onCancel();
        }
      }}
      onSubmit={event => {
        event.preventDefault();
        if (check.ok && check.target && !pending && dirty) onSubmit(check.target, draft);
      }}
    >
      <div className="fwe-fields">
        {creating && choosable.length > 1 && (
          <InlineField label="转发入口" htmlFor={`${id}-node`}>
            <select
              id={`${id}-node`}
              className="f fwe-select"
              value={draft.nodeId}
              disabled={pending}
              onChange={event => {
                const next = entries.find(item => item.node_id === event.target.value);
                if (!next) return;
                const protocol = defaultProtocol(next);
                onChange({
                  ...draft,
                  nodeId: next.node_id,
                  protocol,
                  port: String(nextFreePort(next, rules, protocol) ?? ''),
                });
              }}
            >
              {choosable.map(item => (
                <option key={item.node_id} value={item.node_id}>
                  {item.node_name} · {rules.filter(rule => rule.node_id === item.node_id).length}/{item.max_rules} 条
                </option>
              ))}
            </select>
          </InlineField>
        )}
        <InlineField label="目标地址" htmlFor={`${id}-target`}>
          <input
            id={`${id}-target`}
            className="f mono fwe-target"
            autoFocus
            autoComplete="off"
            spellCheck={false}
            placeholder="example.com:443"
            value={draft.target}
            disabled={pending}
            aria-invalid={check.targetBad || undefined}
            aria-describedby={`${id}-target-hint`}
            onChange={event => onChange({ ...draft, target: event.target.value })}
          />
          <small id={`${id}-target-hint`}>
            {check.target
              ? check.target.kind === '域名'
                ? '域名，在转发入口上解析'
                : `${check.target.kind} · 端口 ${check.target.port}`
              : '主机:端口'}
          </small>
        </InlineField>
        <InlineField label="入口端口" htmlFor={`${id}-port`}>
          <input
            id={`${id}-port`}
            className="f mono fwe-num port"
            inputMode="numeric"
            autoComplete="off"
            value={draft.port}
            disabled={pending}
            aria-invalid={check.portBad || undefined}
            onChange={event => onChange({ ...draft, port: event.target.value })}
          />
          <small>{entry && `授权范围 ${entry.listen_port_start}–${entry.listen_port_end}`}</small>
        </InlineField>
        <InlineField label="协议">
          <SegmentedControl
            value={draft.protocol}
            ariaLabel="协议"
            disabled={pending}
            options={(['tcp', 'udp', 'tcp_udp'] as const).map(protocol => ({
              value: protocol,
              label: PROTOCOL_LABEL[protocol],
              disabled: !entry || !protocolAllowed(entry, protocol),
            }))}
            onChange={protocol => onChange({ ...draft, protocol })}
          />
        </InlineField>
        <InlineField label="名称" htmlFor={`${id}-name`}>
          <input
            id={`${id}-name`}
            className="f fwe-name"
            autoComplete="off"
            maxLength={80}
            placeholder={check.target ? endpointText(check.target.host, check.target.port) : '可选'}
            value={draft.name}
            disabled={pending}
            onChange={event => onChange({ ...draft, name: event.target.value })}
          />
        </InlineField>
      </div>
      {check.problems.length > 0 && (
        <p className="fwe-problem" role="alert">
          <Icon of="warn" size={12} />
          {check.problems[0]}
        </p>
      )}
      {changesListener && (
        <p className="fwe-problem">
          <Icon of="warn" size={12} />
          保存后会重新监听本条规则，经过它的已有连接会断开；其他规则继续运行。
        </p>
      )}
      {error ? <ErrorBox error={error} /> : null}
      <footer className="fwe-foot">
        <span className="fwe-preview">
          {check.target && entry && !check.portBad && (
            <>
              <code>{endpointText(entry.public_ipv4 ?? entry.public_ipv6 ?? '入口', Number(draft.port))}</code>
              <i>→</i>
              <code>{endpointText(check.target.host, check.target.port)}</code>
            </>
          )}
        </span>
        <span className="sp" />
        <button type="button" className="btn" disabled={pending} onClick={onCancel}>
          取消
        </button>
        <button type="submit" className="btn primary" disabled={!check.ok || !dirty || pending}>
          {pending ? '保存中…' : creating ? '创建规则' : '保存'}
        </button>
      </footer>
    </form>
  );
}

function PolicyEditorView({
  dialog,
  rules,
  nodes,
  countryOf,
  entry,
  tenant,
  user,
  pending,
  error,
  onChange,
  onClose,
  onSubmit,
  onLifecycle,
}: {
  dialog: PolicyDialog;
  rules: ForwardingRule[];
  nodes: ForwardingNodeOption[];
  countryOf: (nodeId: string) => string | null;
  entry: ForwardingEntitlement | undefined;
  tenant: string;
  user: string;
  pending: boolean;
  error: unknown;
  onChange: (draft: PolicyDraft) => void;
  onClose: () => void;
  onSubmit: () => void;
  onLifecycle: (kind: 'stop' | 'remove') => void;
}) {
  const { draft } = dialog;
  const grant = dialog.mode === 'grant';
  const id = useId();
  const [confirming, setConfirming] = useState<'stop' | 'remove' | null>(null);
  const check = checkPolicy(dialog, rules);
  const limit = policyLimit(dialog);
  const dirty = policyChanged(dialog, entry);
  const set = (patch: Partial<PolicyDraft>) => {
    setConfirming(null);
    onChange({ ...draft, ...patch });
  };
  const own = rules.filter(rule => rule.node_id === draft.nodeId);
  return (
    <form
      className="fwe-form inline"
      aria-label={grant ? '授权转发入口' : `编辑 ${entry?.node_name ?? draft.nodeId} 的转发授权`}
      onKeyDown={event => {
        if (event.key === 'Escape' && !pending) {
          event.preventDefault();
          event.stopPropagation();
          if (confirming) setConfirming(null);
          else onClose();
        }
      }}
      onSubmit={event => {
        event.preventDefault();
        if (!confirming && check.ok && limit !== undefined && dirty && !pending) onSubmit();
      }}
    >
      <div className="fwe-fields">
        {grant && (
          <InlineField label="机器">
            <span className="fwe-pick" role="radiogroup" aria-label="选择机器">
              {nodes.map(node => (
                <button
                  key={node.id}
                  type="button"
                  role="radio"
                  aria-checked={draft.nodeId === node.id}
                  disabled={pending}
                  onClick={() => set({ nodeId: node.id })}
                >
                  <RegionFlag code={countryOf(node.id)} />
                  {node.name || node.id}
                </button>
              ))}
            </span>
          </InlineField>
        )}
        <InlineField label="流量配额" htmlFor={`${id}-quota`}>
          <span className="fwe-unit">
            <input
              id={`${id}-quota`}
              className="f mono fwe-num quota"
              aria-label="本期转发额度（GiB）"
              inputMode="decimal"
              autoComplete="off"
              autoFocus={!grant}
              placeholder="不限"
              maxLength={24}
              value={draft.quota}
              disabled={pending}
              aria-invalid={limit === undefined || undefined}
              onChange={event => set({ quota: event.target.value })}
            />
            <i>GiB</i>
          </span>
          <small className={limit === undefined ? 'bad' : undefined} role={limit === undefined ? 'alert' : undefined}>
            {limit === undefined ? '请输入正数，最多两位小数' : '留空表示不限'}
          </small>
          {entry && <ForwardingQuotaHistory tenant={tenant} user={user} entry={entry} />}
        </InlineField>
        <InlineField label="协议">
          <SegmentedControl
            value={draft.tcp && draft.udp ? 'both' : draft.tcp ? 'tcp' : 'udp'}
            ariaLabel="允许的协议"
            disabled={pending}
            options={[
              { value: 'tcp', label: '只 TCP' },
              { value: 'udp', label: '只 UDP' },
              { value: 'both', label: 'TCP + UDP' },
            ]}
            onChange={choice => set({ tcp: choice !== 'udp', udp: choice !== 'tcp' })}
          />
        </InlineField>
        <InlineField label="入口端口">
          <span className="fwe-range">
            <input
              className="f mono fwe-num port"
              inputMode="numeric"
              aria-label="端口起始"
              value={draft.start}
              disabled={pending}
              aria-invalid={check.rangeBad || undefined}
              onChange={event => set({ start: event.target.value })}
            />
            <i>–</i>
            <input
              className="f mono fwe-num port"
              inputMode="numeric"
              aria-label="端口结束"
              value={draft.end}
              disabled={pending}
              aria-invalid={check.rangeBad || undefined}
              onChange={event => set({ end: event.target.value })}
            />
          </span>
          <small className={check.rangeBad ? 'bad' : undefined}>{check.rangeText}</small>
        </InlineField>
        <InlineField label="规则上限" htmlFor={`${id}-max`}>
          <input
            id={`${id}-max`}
            className="f mono fwe-num max"
            inputMode="numeric"
            value={draft.max}
            disabled={pending}
            aria-invalid={check.maxBad || undefined}
            onChange={event => set({ max: event.target.value })}
          />
          <small className={check.maxBad ? 'bad' : undefined}>{check.maxText}</small>
        </InlineField>
        <InlineField label="私网目标">
          <SegmentedControl
            value={draft.allowPrivate}
            ariaLabel="允许转发到私网地址"
            disabled={pending}
            options={[
              { value: false, label: '拒绝' },
              { value: true, label: '允许' },
            ]}
            onChange={allowPrivate => set({ allowPrivate })}
          />
          <small>回环、链路本地和元数据地址始终拒绝。</small>
        </InlineField>
      </div>
      {check.problem && (
        <p className="fwe-problem" role="alert">
          <Icon of="warn" size={12} />
          {check.problem}
        </p>
      )}
      {error ? <ErrorBox error={error} /> : null}
      {confirming ? (
        <footer className="fwe-foot confirming" role="alert">
          <p>
            {confirming === 'stop'
              ? `${entry?.node_name} 的 ${own.filter(rule => rule.enabled).length} 条规则停止监听，经过的连接会断开；规则与用量保留，其他用户不受影响。`
              : `连同 ${own.length} 条规则一起移除，入口端口释放，本期用量保留。`}
          </p>
          <button type="button" className="btn" disabled={pending} onClick={() => setConfirming(null)}>
            返回
          </button>
          <button
            type="button"
            className="btn danger"
            disabled={pending}
            autoFocus
            onClick={() => onLifecycle(confirming)}
          >
            {pending ? '处理中…' : confirming === 'stop' ? '停止转发' : '移除入口'}
          </button>
        </footer>
      ) : (
        <footer className="fwe-foot">
          {!grant && entry && (
            <button
              type="button"
              className="btn fwe-side"
              disabled={dirty || pending}
              title={dirty ? '先保存或取消修改' : undefined}
              onClick={() => setConfirming(entry.enabled ? 'stop' : 'remove')}
            >
              <Icon of={entry.enabled ? 'pause' : 'trash'} size={12} />
              {entry.enabled ? '停止转发' : '移除入口'}
            </button>
          )}
          <span className="sp" />
          <button type="button" className="btn" disabled={pending} onClick={onClose}>
            取消
          </button>
          <button
            type="submit"
            className="btn primary"
            disabled={!check.ok || limit === undefined || !dirty || pending}
          >
            {pending ? '保存中…' : grant ? '授权' : '保存'}
          </button>
        </footer>
      )}
    </form>
  );
}

/* ── 「⋯」菜单：生产 .fg-menu.action-menu，点外面或按 Esc 关闭 ── */

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
          // 复制项留在菜单里显示「已复制」，其余项点完即关
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
  disabled = false,
  onSelect,
}: {
  icon: IconName;
  title: string;
  note: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  return (
    <button type="button" role="menuitem" className={danger ? 'dg' : undefined} disabled={disabled} onClick={onSelect}>
      <Icon of={icon} size={14} className="action-menu-icon" />
      <span>
        {title}
        <small>{note}</small>
      </span>
    </button>
  );
}
