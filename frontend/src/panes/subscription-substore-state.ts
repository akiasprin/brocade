import type { SubStoreRun, SubStoreView } from '../api';
export const SUBSTORE_TITLE = 'Sub-Store 后端同步';
export type Tone = 'ok' | 'warn' | 'bad' | 'live' | 'muted';
export const activeRun = (run?: SubStoreRun | null) =>
  !!run && ['queued', 'running', 'retry_wait'].includes(run.status);
export const resultText: Record<string, string> = {
  queued: '排队中',
  running: '执行中',
  retry_wait: '等待重试',
  succeeded: '成功',
  partial: '部分完成',
  failed: '失败',
  canceled: '已停止',
  superseded: '设置已变化',
  interrupted: '中断',
};
export const taskText: Record<SubStoreRun['kind'], string> = {
  sync: '登记同步',
  verify: '拉取验证',
  inspect: '周期核验',
  cleanup: '清理来源',
};
export function syncStatus(view: SubStoreView) {
  const { config: c, active } = view;
  const latest = view.latest?.config_version === c?.version ? view.latest : null;
  if (!c) return { kind: 'none', tone: 'muted' as Tone, label: '未连接', detail: '' };
  if (view.inactive) return { kind: 'paused', tone: 'muted' as Tone, label: '已暂停', detail: '用户服务已暂停' };
  if (active)
    return {
      kind: 'running',
      tone: active.status === 'retry_wait' ? ('warn' as Tone) : ('live' as Tone),
      label: resultText[active.status],
      detail: active.attempts.at(-1)?.events.at(-1)?.name ?? '等待工作线程',
    };
  if (latest?.status === 'failed')
    return { kind: 'failed', tone: 'bad' as Tone, label: '同步失败', detail: latest.error ?? '' };
  if (c.verified)
    return {
      kind: 'synced',
      tone: 'ok' as Tone,
      label: '已同步',
      detail: latest?.output_count != null ? `${latest.output_count} 个节点` : '',
    };
  if (c.registered)
    return { kind: 'pending', tone: 'warn' as Tone, label: '待验证', detail: latest?.error ?? '等待拉取确认' };
  return { kind: 'unsynced', tone: 'muted' as Tone, label: '未同步', detail: '' };
}
export function checkpoints(view: SubStoreView): string[] {
  const status = syncStatus(view);
  if (status.kind === 'none') return ['empty', 'empty', 'empty'];
  if (status.kind === 'paused') return ['paused', 'paused', 'paused'];
  if (status.kind === 'synced') return ['ok', 'ok', 'ok'];
  if (status.kind === 'unsynced') return ['track', 'track', 'track'];
  if (status.kind === 'pending' && view.latest?.status === 'succeeded') return ['ok', 'warn', 'track'];
  const run = view.active ?? (view.latest?.config_version === view.config?.version ? view.latest : null);
  const events = run?.attempts.at(-1)?.events ?? [];
  return [0, 1, 2].map(cp => {
    const group = events.filter(e => e.checkpoint === cp);
    if (group.some(e => e.state === 'failed'))
      return view.active?.status === 'retry_wait' || status.kind === 'pending' ? 'warn' : 'bad';
    if (group.some(e => e.state === 'running')) return 'live';
    if (group.length && group.every(e => e.state === 'done')) return 'ok';
    return cp === 0 && view.config?.registered ? 'ok' : 'track';
  });
}
export function timeText(at?: string | null, withSeconds = false) {
  if (!at) return '—';
  return new Date(at).toLocaleTimeString('zh-CN', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    ...(withSeconds ? { second: '2-digit' as const } : {}),
  });
}
export function dayText(at?: string | null) {
  return at ? new Date(at).toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' }) : '';
}
