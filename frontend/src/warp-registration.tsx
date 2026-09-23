import { useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchCommittedSnapshot, registerWarpBinding, type ExternalOutbound, type ExternalWarpBinding } from './api';
import { draft } from './draft';
import { ErrorBox } from './ui/bits';

export function warpRegistrationBlockReason(committed: boolean, pendingDeletion: boolean): string | null {
  if (pendingDeletion) return '当前变更集正在删除这条 WARP；先撤销删除后再申请身份。';
  if (!committed) return '这条 WARP 还只存在于当前变更集。先创建修订，再回来申请机器身份。';
  return null;
}

export function useWarpRegistrationAvailability(tunnel: ExternalOutbound) {
  const entries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const committed = useQuery({
    queryKey: ['snapshot', 'committed'],
    queryFn: fetchCommittedSnapshot,
    enabled: tunnel.protocol.t === 'warp',
  });
  const committedTunnel = (committed.data?.snapshot.external_outbounds ?? []).find(
    candidate => candidate.tenant === tunnel.tenant && candidate.id === tunnel.id && candidate.protocol.t === 'warp',
  );
  const pendingDeletion = entries.some(
    entry =>
      entry.op.op === 'delete_external_outbound' && entry.op.tenant_id === tunnel.tenant && entry.op.id === tunnel.id,
  );

  return {
    committed: !!committedTunnel,
    committedRevision: committed.data?.snapshot.revision ?? null,
    checking: committed.isPending,
    blockedReason: committed.data
      ? warpRegistrationBlockReason(!!committedTunnel, pendingDeletion)
      : committed.isPending
        ? '正在确认这条 WARP 是否已经进入修订…'
        : '无法确认这条 WARP 的已提交状态，暂不申请外部身份。',
    error: committed.error,
  };
}

export function WarpRegistrationAction({
  tunnel,
  nodeId,
  nodeName,
  editable,
  context = 'rule',
  onRegistered,
}: {
  tunnel: ExternalOutbound;
  nodeId: string;
  nodeName: string;
  editable: boolean;
  context?: 'rule' | 'detail';
  onRegistered?: (binding: ExternalWarpBinding) => void;
}) {
  const qc = useQueryClient();
  const [acceptTerms, setAcceptTerms] = useState(false);
  const availability = useWarpRegistrationAvailability(tunnel);
  const existing = tunnel.bindings.find(binding => binding.node === nodeId);
  const register = useMutation({
    mutationFn: async () => {
      if (!nodeId) throw new Error('没有可注册的机器。');
      return registerWarpBinding(tunnel.tenant, tunnel.id, nodeId);
    },
    onSuccess: async result => {
      setAcceptTerms(false);
      onRegistered?.(result.binding);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
    },
  });

  if (tunnel.protocol.t !== 'warp') return null;
  if (existing || register.isSuccess) {
    return (
      <div className={`warp-registration-action ${context} ready`} role="status">
        <span className="warp-registration-mark" aria-hidden="true">
          ✓
        </span>
        <span className="warp-registration-copy">
          <b>{nodeName} 的 WARP 身份已就绪</b>
          <small>{context === 'rule' ? '保存这张规则表，创建修订后即可进入发布。' : '规则引用后即可进入发布。'}</small>
        </span>
      </div>
    );
  }

  return (
    <div className={`warp-registration-action ${context} blocked`}>
      <div className="warp-registration-head">
        <span className="warp-registration-mark" aria-hidden="true">
          !
        </span>
        <span className="warp-registration-copy">
          <b>{nodeId ? `${nodeName} 尚无 WARP 身份` : '没有可注册的机器'}</b>
          <small>
            {nodeId
              ? context === 'rule'
                ? '当前规则发布前必须补齐；可以留在本页直接申请。'
                : '每台实际使用 WARP 的机器都需要独立身份。'
              : '先纳管一台属于该租户的机器。'}
          </small>
        </span>
      </div>

      {nodeId && availability.error ? (
        <ErrorBox error={availability.error} />
      ) : nodeId && availability.blockedReason ? (
        <p className="warp-registration-note">{availability.blockedReason}</p>
      ) : nodeId && !editable ? (
        <p className="warp-registration-note">当前角色只能查看；请由具有编辑权限的操作者申请身份。</p>
      ) : nodeId ? (
        <>
          <label className="warp-terms">
            <input
              type="checkbox"
              checked={acceptTerms}
              disabled={register.isPending}
              onChange={event => setAcceptTerms(event.target.checked)}
            />
            <span>我同意 Cloudflare Application Terms，并知悉 WireGuard 注册接口为非官方兼容能力。</span>
          </label>
          <div className="warp-registration-submit-row">
            <button
              type="button"
              className="btn primary warp-registration-submit"
              disabled={!acceptTerms || register.isPending}
              onClick={() => register.mutate()}
            >
              {register.isPending ? '正在申请身份…' : `为 ${nodeName} 申请 WARP 身份`}
            </button>
          </div>
        </>
      ) : null}

      {register.error && <ErrorBox error={register.error} />}
    </div>
  );
}
