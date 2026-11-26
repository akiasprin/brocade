import { skipToken, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRef, useState, type RefObject } from 'react';
import {
  commandCvm,
  type CvmChange,
  type CvmEntry,
  type CvmHost,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { entryText } from './network-model';
import { uncertainCvmResult } from './network-common';
import { capacity, hasPendingCvmChange, invalidateCvmChanges, QUERY, requestId } from './queries';

export const powerActions = ['start', 'stop', 'restart'] as const;
type PowerAction = (typeof powerActions)[number];
export type ListAction = PowerAction | 'retry' | 'undo_pending' | 'delete' | 'release_host';

export const listActionText: Record<ListAction, string> = {
  start: '开机',
  stop: '关机',
  restart: '重启',
  retry: '核查并重试原任务',
  undo_pending: '撤回待发布变更',
  delete: '删除容器',
  release_host: '解除纳管',
};

export const needsAttention = (instance: CvmInstance) => instance.status === 'failed' || instance.status === 'unknown';
export const attentionActionText = (instance: CvmInstance) => (instance.status === 'unknown' ? '核查' : '重试');

export function actionBlocked(
  action: ListAction,
  instance: CvmInstance,
  host: CvmHost | undefined,
  workspace: CvmWorkspace,
  batch = false,
): string | undefined {
  const powerAction = action === 'start' || action === 'stop' || action === 'restart';
  if (workspace.redacted || !(powerAction ? workspace.can_use : workspace.can_write)) return '当前账号无权操作';
  if (action === 'undo_pending') {
    if (!hasPendingCvmChange(instance)) return '没有待发布变更';
    if (instance.blocking_deployment_id != null) return '发布单已锁定';
    if (instance.operation_kind === 'assign') return '使用授权不可撤回';
    if (instance.operation_kind === 'undo_pending') return '撤回变更已保存';
    return;
  }
  if (!instance.reserved) return '实例已释放';
  // Recovery can resume failed deletion and works on an offline host with recorded capabilities.
  if (action === 'retry') return needsAttention(instance) ? undefined : '当前没有需要重试的任务';
  if (instance.desired.power === 'deleted') return '正在删除或解除纳管';
  const baremetal = instance.desired.spec.execution.kind === 'baremetal';
  if (action === 'release_host') return baremetal ? undefined : '仅裸机支持解除纳管';
  if (baremetal) return '裸机不支持此操作';
  if (action === 'delete') {
    if (!host) return '未取得宿主资料';
    if (!host.capabilities.delete) return '宿主不支持清盘删除';
    if (batch && instance.blocking_deployment_id != null) return '发布单已锁定';
    if (batch && hasPendingCvmChange(instance)) return '请先完成待发布变更';
    return;
  }
  if (hasPendingCvmChange(instance) || instance.status !== 'applied') return '请先完成当前变更';
  if (!host?.fresh || host.error) return '宿主执行能力不可用';
  if (!host.capabilities.stable_storage || !host.capabilities[action]) return '宿主不支持此操作';
  const power = instance.report?.power;
  if (power !== instance.desired.power) return '运行状态尚未确认';
  if (action === 'start') return power === 'stopped' ? undefined : '已开机';
  return power === 'running' ? undefined : '已关机';
}

export type ListCommand = {
  action: ListAction;
  instances: CvmInstance[];
  batch?: boolean;
  anchor?: RefObject<HTMLButtonElement | null>;
};

type CommandAttempt = { id: string; generation: number; change: CvmChange };
type Result =
  | { kind: 'saved' | 'skipped' | 'failed'; message: string }
  | { kind: 'uncertain'; message: string; attempt: CommandAttempt };

type ListCommandResults = Record<string, Result>;
export type ListCommandRecovery = {
  command: Omit<ListCommand, 'anchor'>;
  results: ListCommandResults;
  sending: boolean;
};
const COMMAND_RECOVERY = ['cvm-list-command-recovery'] as const;

/** Keep one command in memory across list navigation, scoped to the current session's query cache. */
export function useListCommandRecovery() {
  return useQuery<ListCommandRecovery | null>({
    queryKey: COMMAND_RECOVERY,
    queryFn: skipToken,
    initialData: null,
    gcTime: Infinity,
  }).data;
}

const actionDescription: Record<ListAction, string> = {
  start: '发布后使用现有系统盘和出站配置开机。',
  stop: '发布后关机会中断连接，系统盘与出站配置保留。',
  restart: '发布后重启会短暂中断连接，系统盘与出站配置保留。',
  retry: '提交后由宿主核查并重试，沿用原任务身份。',
  undo_pending: '恢复到上一次已发布的配置。',
  delete: '保存后等待发布。发布并执行清盘后，系统盘数据无法恢复。',
  release_host: '发布后撤销受管网络与终端访问，保留宿主操作系统和磁盘。',
};

function newAttempt(action: ListAction, instance: CvmInstance): CommandAttempt {
  const change: CvmChange =
    action === 'delete'
      ? { action, delete_data: true }
      : action === 'undo_pending'
        ? { action, expected_revision: instance.revision_id! }
        : { action };
  return { id: requestId(), generation: instance.desired.generation, change };
}

function DeleteImpact({ instance, host, entries }: { instance: CvmInstance; host?: CvmHost; entries?: CvmEntry[] }) {
  const { resources, interfaces } = instance.desired.spec;
  const attached = instance.report?.interfaces.filter(nic => nic.attached) ?? [];
  const nicCount = new Set([...interfaces, ...attached].map(nic => nic.id)).size;
  const entriesText = entries?.map(entry => {
    const text = entryText(entry);
    return `${entry.mode.kind === 'l3' ? '整 IP' : text.badge} ${text.text}`;
  });
  return (
    <dl className="cvm-list-delete-impact">
      <dt>系统盘</dt>
      <dd>销毁 {capacity(resources.root_disk_bytes)} 系统盘及其数据</dd>
      <dt>入站方式</dt>
      <dd>
        {entriesText
          ? entriesText.length
            ? `撤销 ${entriesText.join('、')}`
            : '无'
          : '撤销该容器的全部入站方式（明细暂不可用）'}
      </dd>
      <dt>出站方式</dt>
      <dd>{nicCount ? `移除 ${nicCount} 个出站方式` : '清理该容器的受管出站方式'}</dd>
      <dt>宿主分配</dt>
      <dd>
        {host?.name ?? instance.node_id} · 清理确认后释放 {resources.vcpus} vCPU · {capacity(resources.memory_bytes)}{' '}
        内存 · {capacity(resources.root_disk_bytes)} 系统盘
      </dd>
    </dl>
  );
}

export function ListCommandDialog({
  command,
  workspace,
  entriesById,
  onClose,
  onSaved,
  retainRecovery = false,
  previousResults,
}: {
  command: ListCommand;
  workspace: CvmWorkspace;
  entriesById?: ReadonlyMap<string, CvmEntry[]>;
  onClose: () => void;
  onSaved: (ids: string[], message: string) => void;
  retainRecovery?: boolean;
  previousResults?: ListCommandResults;
}) {
  const client = useQueryClient();
  const submitting = useRef(false);
  const [typed, setTyped] = useState<Record<string, string>>({});
  const { anchor, ...recoveryCommand } = command;
  const { action, instances, batch } = recoveryCommand;
  const destructive = action === 'delete' || action === 'release_host';
  const first = instances[0];
  const name = first.desired.spec.name;
  const actionText = action === 'retry' ? attentionActionText(first) : listActionText[action];
  const title = batch ? `批量${actionText}` : actionText;
  const reason = (snapshot: CvmInstance, current: CvmWorkspace) => {
    const instance = current.instances.find(item => item.desired.id === snapshot.desired.id);
    if (!instance) return '实例已不可见';
    if (snapshot.desired.generation !== instance.desired.generation || snapshot.revision_id !== instance.revision_id)
      return '配置已变化，请重新选择';
    return actionBlocked(
      action,
      instance,
      current.hosts.find(host => host.node_id === instance.node_id),
      current,
      batch,
    );
  };
  const mutation = useMutation({
    mutationFn: async (previous: ListCommandResults) => {
      const recoveryQuery = () => client.getQueryCache().find({ queryKey: COMMAND_RECOVERY, exact: true });
      const recovery = retainRecovery ? recoveryQuery() : undefined;
      // Logout/login clears the cache. An earlier request must not repopulate a new session's recovery entry.
      const ownsRecovery = () => recovery !== undefined && recovery === recoveryQuery();
      const record = { command: recoveryCommand, results: previous };
      if (recovery) client.setQueryData<ListCommandRecovery>(COMMAND_RECOVERY, { ...record, sending: true });
      const results = { ...previous };
      for (const instance of instances) {
        if (recovery && !ownsRecovery()) break;
        const id = instance.desired.id;
        const old = previous[id];
        if (old && old.kind !== 'uncertain') continue;
        const current = client.getQueryData<CvmWorkspace>(QUERY) ?? workspace;
        const blocked = old?.attempt ? undefined : reason(instance, current);
        if (blocked) {
          results[id] = { kind: 'skipped', message: `跳过 · ${blocked}` };
          continue;
        }
        const attempt = old?.attempt ?? newAttempt(action, instance);
        try {
          const saved = await commandCvm(id, attempt.generation, attempt.change, attempt.id);
          if (recovery && !ownsRecovery()) break;
          results[id] = { kind: 'saved', message: action === 'retry' ? '已提交核查与重试' : '已保存' };
          client.setQueryData<CvmWorkspace>(
            QUERY,
            oldWorkspace =>
              oldWorkspace && {
                ...oldWorkspace,
                instances: oldWorkspace.instances.map(item =>
                  item.desired.id === id &&
                  item.desired.generation <= saved.desired.generation &&
                  (item.revision_id ?? 0) <= (saved.revision_id ?? 0)
                    ? saved
                    : item,
                ),
              },
          );
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          results[id] = uncertainCvmResult(error)
            ? { kind: 'uncertain', message: `结果未确认 · ${message}`, attempt }
            : { kind: 'failed', message: `保存失败 · ${message}` };
        }
      }
      if (ownsRecovery()) {
        const recoveryState: ListCommandRecovery | null = Object.values(results).some(
          result => result.kind === 'uncertain',
        )
          ? { command: recoveryCommand, results, sending: false }
          : null;
        client.setQueryData<ListCommandRecovery | null>(COMMAND_RECOVERY, () => recoveryState);
      }
      return results;
    },
    onSuccess: results => {
      const saved = Object.keys(results).filter(id => results[id].kind === 'saved');
      if (saved.length) onSaved(saved, batch ? `已保存 ${saved.length} 项${title}变更` : results[saved[0]].message);
      if (!batch && saved.length) onClose();
    },
    onSettled: () => {
      invalidateCvmChanges(client);
      submitting.current = false;
    },
  });
  const completed = mutation.data !== undefined || previousResults !== undefined;
  const rows = instances.map(instance => ({
    instance,
    host: workspace.hosts.find(host => host.node_id === instance.node_id),
    blocked: reason(instance, workspace),
  }));
  // Rows shown as skipped at confirmation must stay skipped even if they become eligible during saving.
  const skips = Object.fromEntries(
    rows.flatMap(({ instance, blocked }): [string, Result][] =>
      blocked ? [[instance.desired.id, { kind: 'skipped', message: `跳过 · ${blocked}` }]] : [],
    ),
  );
  const results = mutation.data ?? mutation.variables ?? previousResults ?? skips;
  const uncertain = Object.values(results).some(result => result.kind === 'uncertain');
  const eligible = rows.filter(row => !row.blocked);
  const confirmed = eligible.every(({ instance }) => typed[instance.desired.id] === instance.desired.spec.name);
  const canSubmit = completed ? uncertain : eligible.length > 0 && (!destructive || confirmed);
  const placement = anchor ? ({ mode: 'popover', anchor, align: 'end' } as const) : ({ mode: 'modal' } as const);
  return (
    <DialogLayer {...placement} label={title} onClose={onClose} canClose={() => !mutation.isPending}>
      <div className="dialog-surface confirm-card cvm-list-confirm">
        <div className="confirm-title">{batch ? title : `${title} · ${name}`}</div>
        <div className="confirm-body">
          <p>{actionDescription[action]}</p>
          {batch && !completed && <p className="note">逐个检查并保存，跳过的容器不影响其他容器。</p>}
          <ul
            className="cvm-list-results"
            aria-live="polite"
            hidden={!batch && !destructive && !results[first.desired.id]}
          >
            {rows.map(({ instance, host }) => {
              const id = instance.desired.id;
              const instanceName = instance.desired.spec.name;
              const result = results[id];
              return (
                <li key={id}>
                  {(batch || result) && (
                    <div className="cvm-list-result-row">
                      <span>
                        <b>{instanceName}</b>
                        {batch && <small className="cvm-list-muted">{host?.name ?? instance.node_id}</small>}
                      </span>
                      <span data-result={result?.kind}>{result?.message ?? '将保存'}</span>
                    </div>
                  )}
                  {destructive && !completed && !result && (
                    <>
                      {action === 'delete' && (
                        <DeleteImpact
                          instance={instance}
                          host={host}
                          entries={entriesById && (entriesById.get(id) ?? [])}
                        />
                      )}
                      <label className="cvm-field">
                        {batch ? `输入 ${instanceName} 确认` : '输入实例名称确认'}
                        <input
                          className="f"
                          value={typed[id] ?? ''}
                          autoComplete="off"
                          autoCapitalize="none"
                          spellCheck={false}
                          onChange={event => setTyped({ ...typed, [id]: event.target.value })}
                          disabled={mutation.isPending}
                        />
                      </label>
                    </>
                  )}
                </li>
              );
            })}
          </ul>
          {uncertain && (
            <p className="note" role="status">
              可以重试结果未确认的原请求。
            </p>
          )}
        </div>
        <div className="confirm-actions">
          <DialogClose className="btn" disabled={mutation.isPending}>
            {completed ? '关闭' : '取消'}
          </DialogClose>
          {(!completed || uncertain) && (
            <button
              className={canSubmit ? `btn ${destructive ? 'danger' : 'primary'}` : 'btn'}
              type="button"
              disabled={!canSubmit || mutation.isPending}
              onClick={() => {
                if (submitting.current) return;
                submitting.current = true;
                mutation.mutate(results);
              }}
            >
              {mutation.isPending
                ? '正在保存…'
                : uncertain
                  ? '重试原请求'
                  : action === 'retry'
                    ? `确认${actionText}`
                    : action === 'undo_pending'
                      ? '确认撤回'
                      : destructive
                        ? `保存${title}`
                        : '保存变更'}
            </button>
          )}
        </div>
      </div>
    </DialogLayer>
  );
}
