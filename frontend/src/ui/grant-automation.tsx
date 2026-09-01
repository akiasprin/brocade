import type { GrantAutomationStatus } from '../api';

export type RuntimeTone = 'normal' | 'hot' | 'bad';

export interface RuntimeCrumbState {
  text: string;
  tone: RuntimeTone;
  title?: string;
}

/** Compress the durable permission outbox into the one short readout the breadcrumb can hold. */
export function compactGrantAutomation(
  status: GrantAutomationStatus | undefined,
  loading: boolean,
  failed: boolean,
): RuntimeCrumbState {
  if (failed) return { text: '权限状态未知', tone: 'bad', title: '权限队列状态读取失败' };
  if (!status) return { text: loading ? '检查中' : '权限状态未知', tone: loading ? 'normal' : 'bad' };

  const details = [
    `${status.pending_jobs} 项待处理`,
    status.retrying_jobs > 0 ? `${status.retrying_jobs} 项重试中` : null,
    status.failed_jobs > 0 ? `${status.failed_jobs} 项已终止` : null,
    status.max_attempts > 0 ? `最多已试 ${status.max_attempts} 次` : null,
    status.last_error,
  ]
    .filter(Boolean)
    .join(' · ');

  if (status.failed_jobs > 0) {
    return { text: `权限失败 ${status.failed_jobs}`, tone: 'bad', title: details };
  }
  if (status.retrying_jobs > 0) {
    const count =
      status.retrying_jobs === status.pending_jobs
        ? `${status.retrying_jobs}`
        : `${status.retrying_jobs}/${status.pending_jobs}`;
    return { text: `权限重试 ${count}`, tone: 'bad', title: details };
  }
  if (status.pending_jobs > 0) {
    return { text: `权限待同步 ${status.pending_jobs}`, tone: 'hot', title: details };
  }
  return { text: '同步正常', tone: 'normal', title: '配置已收敛，权限队列为空' };
}

export function RuntimeCrumbStatus({ state, revision }: { state: RuntimeCrumbState; revision?: number }) {
  return (
    <>
      <span className={`fg-blast${state.tone === 'normal' ? '' : ` ${state.tone}`}`} title={state.title}>
        {state.text}
      </span>
      <span className="fg-meta">R{revision ?? '…'}</span>
    </>
  );
}
