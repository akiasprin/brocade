import './detail.css';
import { useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  commandCvm,
  fetchCvmCredential,
  fetchCvmInstanceEntries,
  fetchCvmNicState,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { CopyButton } from '../../ui/copy-button';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { ErrorBox } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { hasPendingCvmChange, invalidateCvmChanges, QUERY, requestId } from './queries';
import { NIC_STATE, uncertainCvmResult } from './network-common';

export const canEditInstanceAccess = (instance: CvmInstance, workspace: CvmWorkspace) =>
  workspace.can_write &&
  !workspace.redacted &&
  instance.desired.spec.execution.kind !== 'baremetal' &&
  instance.desired.power !== 'deleted';

export function InstanceAccess({ instance, workspace }: { instance: CvmInstance; workspace: CvmWorkspace }) {
  const [open, setOpen] = useState(false);
  if (!canEditInstanceAccess(instance, workspace)) return null;
  return (
    <>
      <button className="btn small" onClick={() => setOpen(true)}>
        使用授权
      </button>
      {open && <AccessDialog instance={instance} workspace={workspace} onClose={() => setOpen(false)} />}
    </>
  );
}

export function AccessDialog({
  instance,
  workspace,
  onClose,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { id, spec, generation, power } = instance.desired;
  const [opened] = useState(() => ({ owner: instance.user_id, generation }));
  const [user, setUser] = useState(opened.owner ?? '');
  const attempt = useRef<{ requestId: string; generation: number; user: string | null } | null>(null);
  const authorized = canEditInstanceAccess(instance, workspace);
  const checkingNetwork = authorized && opened.owner == null;
  const entries = useQuery({
    queryKey: ['cvm-instance-entries', id],
    queryFn: () => fetchCvmInstanceEntries(id),
    enabled: checkingNetwork,
    staleTime: 15_000,
    refetchInterval: 15_000,
  });
  const network = useQuery({
    queryKey: [...NIC_STATE, id],
    queryFn: () => fetchCvmNicState(id),
    enabled: checkingNetwork,
    staleTime: 5_000,
    refetchInterval: 5_000,
  });
  const mutation = useMutation({
    mutationFn: () => {
      const request = (attempt.current ??= {
        requestId: requestId(),
        generation: opened.generation,
        user: user || null,
      });
      return commandCvm(id, request.generation, { action: 'assign', user_id: request.user }, request.requestId);
    },
    onSuccess: result => {
      client.setQueryData<CvmWorkspace>(
        QUERY,
        current =>
          current && {
            ...current,
            instances: current.instances.map(item =>
              item.desired.id === id && item.desired.generation <= result.desired.generation ? result : item,
            ),
          },
      );
      invalidateCvmChanges(client);
      onClose();
    },
    onError: error => {
      if (!uncertainCvmResult(error)) attempt.current = null;
      invalidateCvmChanges(client);
    },
  });
  const replay = uncertainCvmResult(mutation.error);
  const ownerOptions = workspace.owners.filter(owner => owner.tenant_id === instance.tenant_id);
  const owner = ownerOptions.find(owner => owner.user_id === opened.owner);
  const freshHost = workspace.hosts.some(host => host.node_id === instance.node_id && host.fresh);
  const revoking = opened.owner != null && user === '';
  const waitingForStop =
    instance.operation_kind === 'assign' &&
    instance.user_id == null &&
    (hasPendingCvmChange(instance) ||
      power !== 'stopped' ||
      instance.status !== 'applied' ||
      instance.report?.power !== 'stopped' ||
      instance.report.generation !== generation);
  const configuredNetwork = spec.interfaces.some(nic => nic.allocated) || spec.routing.routes.length > 0;
  const networkInUse = configuredNetwork || !!entries.data?.length || !!network.data?.bindings.length;
  const cleaning =
    !configuredNetwork &&
    (entries.data ?? []).every(entry => entry.state === 'releasing') &&
    (network.data?.bindings ?? []).every(binding => binding.phase === 'withdrawing');
  const blocked =
    generation !== opened.generation || instance.user_id !== opened.owner
      ? '容器使用授权或配置已变化。请关闭窗口，核查后重新打开。'
      : instance.blocking_deployment_id
        ? '正在发布此容器。发布结束后才能修改使用授权。'
        : !freshHost
          ? '尚未获取到宿主的最新状态，请稍后再修改使用授权。'
          : checkingNetwork && waitingForStop
            ? '请先发布收回授权，并等待关机和网络清理确认。'
            : checkingNetwork && networkInUse
              ? cleaning
                ? '正在等待访问入口和业务网卡清理；确认完成后才能授权。'
                : '请先移除旧网络配置，并发布等待清理确认。'
              : checkingNetwork && (entries.error || network.error)
                ? '无法确认访问入口和业务网卡已清理，暂不能授权。'
                : checkingNetwork && (!entries.data || !network.data)
                  ? '正在检查访问入口和业务网卡…'
                  : null;
  const submitDisabled = !authorized || mutation.isPending || (!replay && (!!blocked || user === (opened.owner ?? '')));
  if (!authorized) return null;
  return (
    <DialogLayer label="容器使用授权" onClose={onClose} canClose={() => !mutation.isPending}>
      <section className="dialog-surface tunnel-dialog cvm-dialog cvm-access-dialog">
        <header>
          <b>使用授权 · {spec.name}</b>
          <DialogClose className="btn" disabled={mutation.isPending}>
            关闭
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body">
          <p className="note">每个容器至多授权给一名同租户用户。该用户可以查看、启停和连接此容器。</p>
          <label className="cvm-field">
            使用用户
            <select
              className="f"
              value={user}
              onChange={event => setUser(event.target.value)}
              disabled={mutation.isPending || replay || !!blocked}
            >
              <option value="">暂不授权</option>
              {opened.owner && !owner && <option value={opened.owner}>{opened.owner}</option>}
              {ownerOptions.map(option => (
                <option
                  key={option.user_id}
                  value={option.user_id}
                  disabled={!!opened.owner && option.user_id !== opened.owner}
                >
                  {option.name}
                </option>
              ))}
            </select>
          </label>
          {opened.owner && (
            <div className="cvm-access-consequences">
              <b>更换使用者前，须先收回授权并完成清理。</b>
              <ul className="cvm-impact-list">
                <li>保存后，{owner?.name ?? opened.owner} 立即失去控制台访问；收回授权不可撤回。</li>
                <li>发布后，容器关机，撤下访问入口和业务网卡，清空路由，并轮换登录密码。</li>
                <li>保留系统盘和数据；确认关机和网络已清理后才能重新授权。</li>
              </ul>
            </div>
          )}
          {replay ? (
            <p className="cvm-notice" role="status">
              上次保存的结果尚未确认。可重试同一请求，或关闭后核查。
            </p>
          ) : (
            blocked && (
              <p className="cvm-notice" role="status">
                {blocked}
              </p>
            )
          )}
          {mutation.error && <ErrorBox error={mutation.error} />}
        </div>
        <footer>
          <button
            className={`btn${submitDisabled ? '' : revoking ? ' danger' : ' primary'}`}
            disabled={submitDisabled}
            onClick={() => mutation.mutate()}
          >
            {mutation.isPending ? '正在保存…' : replay ? '重试原请求' : revoking ? '收回授权' : '保存使用授权'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

/** Explicit retrieval keeps plaintext out of workspace and query caches. */
export function GuestPassword({
  instance,
  onReset,
  resetDisabled = false,
}: {
  instance: CvmInstance;
  /** 重置会保存一项待发布变更；不传时不显示入口。 */
  onReset?: () => void;
  resetDisabled?: boolean;
}) {
  const [password, setPassword] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const revision = instance.desired.spec.login_revision;
  if (!revision) return null;
  const reveal = async () => {
    setPending(true);
    setError(null);
    try {
      const credential = await fetchCvmCredential(instance.desired.id);
      setPassword(credential.password);
    } catch (error) {
      setError(error instanceof Error ? error : new Error('登录密码读取失败'));
    } finally {
      setPending(false);
    }
  };
  return (
    <>
      <div className="cvm-password-row">
        <Icon of="access" size={13} />
        <span className="cvm-password-who">
          登录用户 <b className="mono">cube</b> · 自动生成密码（v{revision}）
        </span>
        {password ? (
          <span className="cvm-password-value">
            <input className="f mono" aria-label="容器登录密码" value={password} readOnly autoComplete="off" />
            <CopyButton className="btn small" text={password} label="复制密码" />
            <button className="btn small" type="button" aria-label="隐藏密码" onClick={() => setPassword(null)}>
              隐藏
            </button>
          </span>
        ) : (
          <button className="btn small" type="button" disabled={pending} onClick={() => void reveal()}>
            <Icon of="eye" size={13} />
            {pending ? '正在读取…' : '查看登录密码'}
          </button>
        )}
        {onReset && (
          <button
            className="btn small cvm-quiet-btn"
            type="button"
            disabled={resetDisabled}
            title="自动生成新密码，保留系统盘数据；保存后进入待发布"
            onClick={onReset}
          >
            <Icon of="refresh" size={13} />
            重置登录密码
          </button>
        )}
      </div>
      {error && <ErrorBox error={error} />}
    </>
  );
}
