/* 养护配置卡与弹层共享读取、编辑、控制和回执确认状态；保存成功与节点实际应用分别判定。 */
import { createContext, useContext, useState, type ReactNode } from 'react';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  commandIpMaintenance,
  fetchIpMaintenance,
  fetchIpMaintenanceRuns,
  fetchMaintenanceRegions,
  saveIpMaintenance,
  stopIpMaintenance,
  type IpMaintenancePolicy,
  type IpMaintenanceReport,
  type IpMaintenanceView,
  type MaintenancePhase,
  type MaintenanceRegion,
  type NodeAgentStateItem,
} from '../../api';
import { useAgentLiveness } from '../../ui/agent-alive';
import { useNow } from '../../ui/clock';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { placeName } from './regions-zh';
import { maintenanceDefaults } from './defaults';

export const ACTIVE: MaintenancePhase[] = ['jitter', 'preflight', 'requesting', 'dwelling', 'postcheck'];
export const ACTION_NAME: Record<string, string> = {
  google_search: 'Google 搜索',
  google_news: 'Google 新闻',
  google_maps: 'Google 地图',
  gemini_location: 'Gemini 定位更新',
  platform_page: 'Google 生态页',
  local_site: '地区站点',
};
/* 原因只写结论，不复述「已暂停本轮」这类状态行已经说明的部分。 */
export const REASON: Record<string, string> = {
  source_unavailable: '绑定的来源地址不可用',
  expected_exit_changed: '实测出口与预期不符',
  daily_budget: '已达每日上限',
  service_restricted: '限流',
  consecutive_network_failures: '连续网络失败',
  agent_restarted: 'Agent 重启',
  policy_changed: '策略已更新',
  policy_lease_expired: '策略连接超时',
  maintenance_receipt_buffer_full: '回执缓冲已满',
  non_public_target: '目标地址不是公网',
  no_ipv4_address: '无 IPv4 解析，已跳过',
  no_ipv6_address: '无 IPv6 解析，已跳过',
  network_timeout: '请求超时',
  network_request: '连接失败',
  response_size_limit: '页面超过大小上限',
  response_size_skipped: '页面过大，已跳过',
  consent_required: '停留在网站同意页',
  javascript_required: '页面需要 JavaScript',
  verification_required: '网站要求验证',
  article_redirect_unresolved: '新闻跳转页未解析',
  empty_response: '网站未返回页面内容',
  unexpected_response_status: '网站响应状态不完整',
  all_actions_failed: '本轮没有成功动作',
  gemini_location_unconfirmed: 'Gemini 未确认设备位置',
  gemini_location_response_invalid: 'Gemini 位置响应无法识别',
  gemini_location_redirect: 'Gemini 定位接口发生跳转，已跳过',
  gemini_location_invalid_coordinates: '养护目标坐标无效',
  stop_requested: '手动停止',
  agent_stopping: 'Agent 退出',
  worker_recovering: '执行器恢复中',
  round_deadline: '超过单轮时限',
  round_download_limit: '超过单轮下载上限',
};
export const EVIDENCE_SOURCES = [
  { source: 'google_jump', name: 'Google', mark: 'google' },
  { source: 'youtube_premium', name: 'YouTube Premium', mark: 'youtube' },
  { source: 'youtube_music', name: 'YouTube Music', mark: 'youtube' },
] as const;

/* ── 时间 ── */
const pad = (n: number) => String(n).padStart(2, '0');
export const clock = (secs: number) => {
  const d = new Date(secs * 1000);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
export const mmss = (secs: number) => {
  const s = Math.max(0, Math.round(secs));
  return `${Math.floor(s / 60)}:${pad(s % 60)}`;
};
export function span(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.round(s / 60)} 分钟`;
  if (s < 86400) {
    const h = Math.floor(s / 3600);
    const m = Math.round((s % 3600) / 60);
    return m ? `${h} 小时 ${m} 分` : `${h} 小时`;
  }
  return `${Math.round(s / 86400)} 天`;
}
export function ago(secs: number, now: number): string {
  const d = now / 1000 - secs;
  return d < 45 ? '刚刚' : `${span(d)}前`;
}
export function dayClock(secs: number, now: number): string {
  const day = (ms: number) => new Date(ms).toDateString();
  if (day(secs * 1000) === day(now)) return clock(secs);
  if (day(secs * 1000) === day(now - 86400000)) return `昨天 ${clock(secs)}`;
  if (day(secs * 1000) === day(now + 86400000)) return `明天 ${clock(secs)}`;
  const d = new Date(secs * 1000);
  return `${d.getMonth() + 1}月${d.getDate()}日 ${clock(secs)}`;
}
export const mib = (bytes: number) => bytes / 1048576;
export const fmtMiB = (bytes: number) => {
  const v = mib(bytes);
  return v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(1);
};

export type Tone = 'run' | 'idle' | 'off' | 'wait' | 'warn' | 'err' | 'unknown';

export interface Live {
  tone: Tone;
  title: string;
  detail: string | null;
  /** 环：进行中为本轮进度，等待下一轮为倒计时。null 不画弧。 */
  ring: number | null;
  /** 停留 / 等待的剩余秒数。 */
  countdown: number | null;
}

export function successfulActions(run: IpMaintenanceReport) {
  return run.actions.filter(
    action => action.error == null && action.status !== null && action.status >= 200 && action.status < 400,
  ).length;
}
export const actionWasSkipped = (error: string | null | undefined) =>
  error === 'response_size_skipped' || error === 'no_ipv4_address' || error === 'no_ipv6_address';

export function roundResult(run: IpMaintenanceReport): { tone: Tone; text: string } {
  const reason = REASON[run.reason ?? ''] ?? null;
  if (run.phase === 'complete' || run.phase === 'failed') return { tone: 'idle', text: '' };
  if (run.phase === 'cooldown')
    return { tone: 'warn', text: run.reason === 'service_restricted' ? '限流暂停' : `冷却：${reason ?? '服务异常'}` };
  if (run.phase === 'budget_wait') return { tone: 'wait', text: '今日额度已用完' };
  if (run.phase === 'interrupted') return { tone: 'off', text: `中断 · ${reason ?? '原因未知'}` };
  if (run.phase === 'blocked_egress') return { tone: 'err', text: reason ?? '出口不可用' };
  return { tone: 'run', text: '进行中' };
}

function useDwellFraction(report: IpMaintenanceReport | null, now: number): number {
  /* 回执只带截止时间。第一次看到某段停留时记下剩余秒数作为总长，之后按比例推进。 */
  const [seen, setSeen] = useState<{ key: string; total: number } | null>(null);
  if (!report || report.phase !== 'dwelling' || !report.phase_deadline_unix_secs) return 0;
  const left = Math.max(0, report.phase_deadline_unix_secs - now / 1000);
  const key = `${report.round_id}:${report.action_index}:${report.phase_deadline_unix_secs}`;
  const total = seen?.key === key ? seen.total : Math.max(left, 1);
  if (seen?.key !== key) setSeen({ key, total });
  return Math.min(1, 1 - left / total);
}

const reportFamily = (report: IpMaintenanceReport) =>
  report.family ?? (report.actual_exit_ip?.includes(':') ? 'v6' : 'v4');

async function stopReportedRounds(
  nodeId: string,
  view: IpMaintenanceView,
  reports: { family: 'v4' | 'v6'; report: IpMaintenanceReport | null }[],
) {
  const baseline = view.desired;
  const targets = reports.filter(item => item.report?.round_id && ACTIVE.includes(item.report.phase));
  if (!targets.length) throw new Error('没有可停止的轮次');
  let latest = view;
  const deadline = performance.now() + 90_000;
  const requestSignal = () => {
    const left = deadline - performance.now();
    if (left <= 0) throw new Error('停止尚未全部确认，请稍后重试');
    return AbortSignal.timeout(Math.min(15_000, Math.ceil(left)));
  };
  for (const [index, target] of targets.entries()) {
    const current = (latest.family_reports ?? (latest.report ? [latest.report] : [])).find(
      r => reportFamily(r) === target.family,
    );
    // Never stop a later automatic round or cross a policy/session change while waiting.
    if (
      latest.desired.generation !== baseline.generation ||
      latest.desired.session_generation !== baseline.session_generation
    )
      throw new Error('养护策略或会话已变化，请重新确认状态');
    if (!current || current.round_id !== target.report!.round_id || !ACTIVE.includes(current.phase)) continue;
    await stopIpMaintenance(nodeId, baseline.generation, current.round_id!, crypto.randomUUID(), requestSignal());
    if (index === targets.length - 1) break;
    // The server serializes stop intents. Wait for the first receipt before targeting the other family.
    while (true) {
      latest = await fetchIpMaintenance(nodeId, requestSignal());
      if (
        latest.desired.generation !== baseline.generation ||
        latest.desired.session_generation !== baseline.session_generation
      )
        throw new Error('养护策略或会话已变化，请重新确认状态');
      const receipt = (latest.family_reports ?? (latest.report ? [latest.report] : [])).find(
        r => reportFamily(r) === target.family,
      );
      if (receipt && receipt.applied_stop_sequence >= latest.desired.stop_sequence) break;
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
  }
}

function useCareState(node: NodeAgentStateItem, readOnly: boolean) {
  const editable = !readOnly;
  const qc = useQueryClient();
  const now = useNow();
  const liveness = useAgentLiveness(node);
  const query = useQuery({
    queryKey: ['node-ip-maintenance', node.node_id],
    queryFn: ({ signal }) => fetchIpMaintenance(node.node_id, signal),
    refetchInterval: 15_000,
  });
  const regions = useQuery({
    queryKey: ['ip-maintenance-regions'],
    queryFn: fetchMaintenanceRegions,
    staleTime: Infinity,
  });
  const [edit, setEdit] = useState<{ generation: number; policy: IpMaintenancePolicy } | null>(null);
  /* 自动开关在点击的同一帧翻转；请求结束并重新读取后交还给服务端的值。 */
  const [optimistic, setOptimistic] = useState<boolean | null>(null);
  const rawView = query.data;
  const defaults = rawView
    ? maintenanceDefaults(node, regions.data ?? [], rawView.default_policy ?? rawView.desired.policy)
    : null;
  const view: IpMaintenanceView | undefined =
    rawView?.desired.generation === 0 && defaults
      ? { ...rawView, default_policy: defaults.policy, desired: { ...rawView.desired, policy: defaults.policy } }
      : rawView;
  const baseline = view?.desired;
  const dual = baseline?.policy.family === 'dual';
  const familyReports = view?.family_reports ?? (view?.report ? [view.report] : []);
  const families = dual ? (['v4', 'v6'] as const) : ([baseline?.policy.family === 'v6' ? 'v6' : 'v4'] as const);
  const reports = families.map(family => ({
    family,
    report: dual ? (familyReports.find(r => reportFamily(r) === family) ?? null) : (view?.report ?? null),
  }));
  const primary = reports.find(item => item.report && ACTIVE.includes(item.report.phase)) ?? reports[0];
  const { report, family: displayFamily } = primary;
  const policy = edit?.policy ?? baseline?.policy;
  const dirty = !!edit && !!baseline && JSON.stringify(edit.policy) !== JSON.stringify(baseline.policy);
  useUnsavedChanges(dirty, `${node.name || node.node_id} 的养护策略`, `node-tab:${node.node_id}:config`);
  const dwell4 = useDwellFraction(reports.find(item => item.family === 'v4')?.report ?? null, now);
  const dwell6 = useDwellFraction(reports.find(item => item.family === 'v6')?.report ?? null, now);

  const invalidate = () =>
    Promise.all([
      qc.invalidateQueries({ queryKey: ['node-ip-maintenance', node.node_id] }),
      qc.invalidateQueries({ queryKey: ['nodes'] }),
    ]);
  const save = useMutation({
    mutationFn: async (next?: IpMaintenancePolicy) => {
      if (!editable) throw new Error('当前身份仅可查看养护信息');
      if (!baseline) throw new Error('请先读取养护策略');
      const generation = next ? baseline.generation : (edit?.generation ?? baseline.generation);
      const submitted = structuredClone(next ?? edit?.policy ?? baseline.policy);
      if (!submitted.region) throw new Error('请选择养护地区');
      await saveIpMaintenance(node.node_id, generation, submitted);
      return { generation, policy: submitted };
    },
    onSuccess: (submitted, next) => {
      // Retain edits made while saving, and never silently rebase a stale draft after a refresh.
      setEdit(current => {
        if (!current || current.generation !== submitted.generation) return current;
        const updated = next ? { ...current.policy, enabled: next.enabled } : current.policy;
        return JSON.stringify(updated) === JSON.stringify(submitted.policy)
          ? null
          : { generation: submitted.generation + 1, policy: updated };
      });
    },
    onSettled: async () => {
      try {
        await invalidate();
      } finally {
        setOptimistic(null);
      }
    },
  });
  const command = useMutation({
    mutationFn: async (kind: 'run' | 'session-reset') => {
      if (!editable) throw new Error('当前身份仅可查看养护信息');
      if (!baseline) throw new Error('请先读取养护策略');
      let generation = baseline.generation;
      /* 从未保存过时，先保存默认策略（自动保持关闭），再发执行。 */
      if (generation === 0) {
        await saveIpMaintenance(node.node_id, 0, { ...baseline.policy, enabled: false });
        generation = 1;
      }
      return commandIpMaintenance(node.node_id, generation, kind, crypto.randomUUID());
    },
    onSettled: async (_data, error, kind) => {
      const obsoleteHistory = ['node-ip-maintenance-runs', 'node-ip-maintenance-day', 'node-ip-maintenance-run'].map(
        key => [key, node.node_id, baseline?.session_generation ?? 0],
      );
      if (kind === 'session-reset' && !error) {
        // A successful reset deletes these rounds. Cancel old reads before refreshing the
        // authoritative session, then let the new session fetch its own empty history.
        await Promise.all(obsoleteHistory.map(queryKey => qc.cancelQueries({ queryKey })));
        setEdit(null);
        save.reset();
        stop.reset();
      }
      await invalidate();
      if (kind === 'session-reset' && !error)
        for (const queryKey of obsoleteHistory) qc.removeQueries({ queryKey, type: 'inactive' });
    },
  });
  const stop = useMutation({
    mutationFn: async () => {
      if (!editable) throw new Error('当前身份仅可查看养护信息');
      if (!baseline) throw new Error('请先读取养护策略');
      await stopReportedRounds(node.node_id, view!, reports);
    },
    onSettled: invalidate,
  });

  const region: MaintenanceRegion | undefined = regions.data?.find(r => r.id === (policy?.region ?? ''));
  const savedRegion = regions.data?.find(r => r.id === baseline?.policy.region);
  const online = liveness?.state === 'online';
  const anyActive = reports.some(item => item.report && ACTIVE.includes(item.report.phase));
  const dualIdleConfirmed =
    !dual ||
    (['v4', 'v6'] as const).every(family =>
      familyReports.some(
        r =>
          r.family === family &&
          r.generation === baseline?.generation &&
          r.applied_control_sequence >= (baseline?.control_sequence ?? 0),
      ),
    );

  const manualPending =
    !!view?.desired.manual_expires_at &&
    view.desired.manual_expires_at * 1000 > now &&
    (dual ? !dualIdleConfirmed : view.desired.control_sequence > (report?.applied_control_sequence ?? 0)) &&
    !anyActive;
  const enabled = !!baseline?.policy.enabled;
  const enabledShown = optimistic ?? enabled;
  const nowSecs = now / 1000;
  const describeReport = (family: 'v4' | 'v6', report: IpMaintenanceReport | null) => {
    const dwell = family === 'v6' ? dwell6 : dwell4;
    const fresh =
      !!report &&
      report.worker_observed_at_unix_secs * 1000 <= now + 60_000 &&
      now - report.worker_observed_at_unix_secs * 1000 < 90_000 &&
      !!view?.received_at_unix_secs &&
      now - view.received_at_unix_secs * 1000 < 90_000;
    const confirmed = !!view?.lifecycle_active && online && fresh;
    const active = !!report && ACTIVE.includes(report.phase);
    const policyPending =
      !!view && !!baseline && baseline.generation !== 0 && baseline.generation !== report?.generation;
    const stopPending =
      !!view &&
      view.desired.stop_sequence > (report?.applied_stop_sequence ?? 0) &&
      view.desired.target_round_id === report?.round_id;
    const sessionPending =
      (command.isPending && command.variables === 'session-reset') ||
      (!!view && view.desired.session_generation !== (report?.applied_session_generation ?? 0));
    const cooling = !!report?.cooldown_until_unix_secs && report.cooldown_until_unix_secs * 1000 > now;
    const first =
      !enabledShown &&
      optimistic === null &&
      !manualPending &&
      (baseline?.generation === 0 ||
        (!!baseline &&
          baseline.session_generation > 0 &&
          report?.applied_session_generation === baseline.session_generation &&
          !report.last_started_at_unix_secs &&
          !report.round_id));

    const live = ((): Live => {
      const off = (title: string, detail: string | null = null, tone: Tone = 'unknown'): Live => ({
        tone,
        title,
        detail,
        ring: null,
        countdown: null,
      });
      if (!view) return off('正在读取');
      if (!view.supported) return off('需要升级 Agent', '协议 68 起支持 IP 养护', 'off');
      if (!view.lifecycle_active) return off('机器已停用', '执行状态待确认');
      if (save.isPending && optimistic !== null)
        return off(optimistic ? '正在开启自动养护' : '正在关闭自动养护', '正在保存策略', 'wait');
      if (sessionPending) return off('正在重置会话', '等待节点确认', 'wait');
      if (first && !baseline?.policy.region) return off('请选择养护地区', defaults?.notice ?? null, 'off');
      if (first && !manualPending) return off('尚未开始', null, 'off');
      if (!online)
        return off('Agent 离线', report ? `最近回执 ${ago(report.observed_at_unix_secs, now)}` : '状态待确认');
      if (!fresh) return off('执行器无回执', '状态待确认');
      if (stopPending) return { ...off('正在停止', '等待节点确认', 'wait'), ring: null };
      if (policyPending) return off('正在应用新策略', '等待节点确认', 'wait');
      if (manualPending) return off('已请求执行', '等待节点开始', 'wait');
      if (!report) return off('尚无回执');
      const total = Math.max(report.actions_total, 1);
      const current = report.actions.find(a => a.index === report.action_index);
      const actionName = current ? (ACTION_NAME[current.kind] ?? current.kind) : '';
      const step = `${report.action_index} / ${report.actions_total}`;
      switch (report.phase) {
        case 'preflight':
          return { tone: 'run', title: '确认出口', detail: '本轮准备中', ring: 0.02, countdown: null };
        case 'jitter':
          return {
            tone: 'wait',
            title: '错峰等待',
            detail: report.phase_deadline_unix_secs ? mmss(report.phase_deadline_unix_secs - nowSecs) : null,
            ring: 0,
            countdown: report.phase_deadline_unix_secs ? report.phase_deadline_unix_secs - nowSecs : null,
          };
        case 'requesting':
          return {
            tone: 'run',
            title: `正在访问${actionName ? ` ${actionName}` : ''}`,
            detail: step,
            ring: (report.action_index - 0.7) / total,
            countdown: null,
          };
        case 'dwelling': {
          const left = report.phase_deadline_unix_secs ? report.phase_deadline_unix_secs - nowSecs : null;
          return {
            tone: 'run',
            title: `${actionName || '页面'} · 停留`,
            detail: step,
            ring: (report.action_index - 1 + 0.3 + 0.7 * dwell) / total,
            countdown: left,
          };
        }
        case 'postcheck':
          return { tone: 'run', title: '地区取证', detail: '读取服务识别的地区', ring: 0.99, countdown: null };
        case 'cooldown':
          return {
            tone: 'warn',
            title: report.reason === 'service_restricted' ? '限流暂停' : '异常冷却',
            detail: cooling
              ? `${span(report.cooldown_until_unix_secs! - nowSecs)}后恢复`
              : (REASON[report.reason ?? ''] ?? '服务异常'),
            ring: null,
            countdown: null,
          };
        case 'budget_wait':
          return {
            tone: 'warn',
            title: '今日额度已用完',
            detail: report.next_run_unix_secs ? `${dayClock(report.next_run_unix_secs, now)} 恢复` : null,
            ring: null,
            countdown: null,
          };
        case 'blocked_egress':
          return {
            tone: 'err',
            title: '出口不可用',
            detail: `${REASON[report.reason ?? ''] ?? '出口检查失败'}${report.actual_exit_ip ? ` · 实测 ${report.actual_exit_ip}` : ''}`,
            ring: null,
            countdown: null,
          };
        case 'interrupted':
          if (enabled && report.next_run_unix_secs && report.next_run_unix_secs > nowSecs)
            return {
              tone: 'idle',
              title: `下一轮 ${clock(report.next_run_unix_secs)}`,
              detail: `本轮已中断 · ${REASON[report.reason ?? ''] ?? ''}`,
              ring: null,
              countdown: report.next_run_unix_secs - nowSecs,
            };
          return off('本轮已中断', REASON[report.reason ?? ''] ?? null, 'off');
        case 'disabled':
          return off('自动养护已关闭', '可以手动执行一轮', 'off');
        default: {
          if (enabled && report.next_run_unix_secs) {
            const left = report.next_run_unix_secs - nowSecs;
            const interval = (baseline?.policy.interval_minutes ?? 45) * 60;
            return {
              tone: 'idle',
              title: left > 30 ? `下一轮 ${clock(report.next_run_unix_secs)}` : '即将开始下一轮',
              detail: left > 30 ? `${span(left)}后` : null,
              ring: Math.min(1, Math.max(0, 1 - left / interval)),
              countdown: left,
            };
          }
          return off('空闲', '可以手动执行一轮', 'off');
        }
      }
    })();

    return {
      family,
      report,
      fresh,
      confirmed,
      active,
      first,
      cooling,
      live,
      policyPending,
      stopPending,
      sessionPending,
    };
  };
  const familyStates = reports.map(item => describeReport(item.family, item.report));
  const state = familyStates.find(item => item.family === displayFamily)!;
  const { fresh, active, first, live } = state;
  const confirmed = familyStates.every(item => item.confirmed);
  const cooling = familyStates.some(item => item.cooling);
  const policyPending = familyStates.some(item => item.policyPending);
  const stopPending = familyStates.some(item => item.stopPending);
  const sessionPending = familyStates.some(item => item.sessionPending);
  const awaitingStop = stop.isPending || stopPending;
  const canStop = (item: typeof state) => editable && !!item.report?.round_id && item.active && !awaitingStop;

  const busy = save.isPending || command.isPending || stop.isPending;
  const runBlocked = !view
    ? '正在读取'
    : !view.supported
      ? '需要升级 Agent'
      : !view.lifecycle_active
        ? '机器已停用'
        : !baseline!.policy.region
          ? '请选择养护地区'
          : !baseline!.policy.google && !baseline!.policy.trust
            ? '至少开启一个模块'
            : familyStates.some(item => !item.first && !item.confirmed)
              ? '执行状态待确认'
              : anyActive
                ? '本轮进行中'
                : cooling
                  ? '冷却中'
                  : familyStates.some(item => item.report?.phase === 'budget_wait')
                    ? '今日额度已用完'
                    : policyPending || manualPending || stopPending
                      ? '等待节点确认'
                      : null;
  const evidence = EVIDENCE_SOURCES.map(item => {
    const hit = report?.evidence.find(e => e.source === item.source);
    return { ...item, country: hit?.country ?? null, known: !!hit };
  });
  const targetCountry = (savedRegion?.country ?? baseline?.policy.region.slice(0, 2) ?? '').replace('UK', 'GB');

  return {
    node,
    editable,
    resetSupported: !!view?.session_reset_supported,
    now,
    view,
    error: query.error ?? regions.error,
    regions: regions.data ?? [],
    report,
    baseline,
    policy,
    edit: {
      dirty,
      set(next: Partial<IpMaintenancePolicy>) {
        if (!editable || !policy || !baseline) return;
        setEdit({
          generation: edit?.generation ?? baseline.generation,
          policy: { ...structuredClone(policy), ...next },
        });
      },
      reset: () => {
        setEdit(null);
        save.reset();
      },
      save: () => save.mutate(undefined),
      saving: save.isPending,
      error: save.error,
    },
    region,
    savedRegion,
    defaultCountry: defaults?.country,
    defaultRegionNotice: first ? defaults?.notice : null,
    place: savedRegion ? placeName(savedRegion) : null,
    online,
    fresh,
    confirmed,
    active,
    anyActive,
    dualIdleConfirmed,
    dual,
    displayFamily,
    familyStates,
    first,
    enabled,
    cooling,
    pending: { policy: policyPending, stop: awaitingStop, session: sessionPending, manual: manualPending },
    live,
    exitIp:
      report?.actual_exit_ip ??
      (dual && displayFamily === 'v6' ? baseline?.policy.expected_public_ipv6 : baseline?.policy.expected_public_ip) ??
      (displayFamily === 'v6'
        ? (node.public_ipv6 ?? node.observed_public_ipv6?.current_ip)
        : (node.public_ipv4 ?? node.observed_public_ipv4?.current_ip)) ??
      '—',
    evidence,
    targetCountry,
    usage: {
      known: !!report,
      requests: view?.report?.daily_requests ?? 0,
      requestLimit: baseline?.policy.daily_requests ?? 0,
      bytes: view?.report?.daily_response_bytes ?? 0,
      byteLimit: (baseline?.policy.daily_download_mib ?? 0) * 1048576,
    },
    actions: {
      busy,
      runBlocked,
      run: () => command.mutate('run'),
      resetSession: () => command.mutate('session-reset'),
      stop: () => stop.mutate(),
      canStop: familyStates.some(canStop),
      /** 开关先按点击翻转，保存失败时回到已保存的值。 */
      enabledShown,
      /** 自动开关即时保存，不进草稿：与 iOS 设置里的开关一致。 */
      setEnabled(value: boolean) {
        if (!editable || !baseline) return;
        setOptimistic(value);
        save.mutate({ ...baseline.policy, enabled: value });
      },
      error: command.error ?? stop.error,
    },
  };
}

export type CareState = ReturnType<typeof useCareState>;
const Context = createContext<CareState | null>(null);
export function CareProvider({
  node,
  readOnly = false,
  children,
}: {
  node: NodeAgentStateItem;
  readOnly?: boolean;
  children: ReactNode;
}) {
  return <Context.Provider value={useCareState(node, readOnly)}>{children}</Context.Provider>;
}
export function useCare(): CareState {
  const value = useContext(Context);
  if (!value) throw new Error('CareProvider required');
  return value;
}

export function useRuns(node: string, session: number) {
  return useInfiniteQuery({
    queryKey: ['node-ip-maintenance-runs', node, session],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam, signal }) => fetchIpMaintenanceRuns(node, pageParam, signal),
    getNextPageParam: last => last.next_cursor ?? undefined,
  });
}
