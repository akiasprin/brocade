import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { resetUserCycleUsage, type UserCyclePeriod, type UserListItem } from '../api';
import { ErrorBox } from '../ui/bits';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { randomKey } from '../ui/platform';
import './user-cycle-reset.css';

/**
 * 读数栏「本期接入 / 本期转发」一行右端的「重置」。接入与转发两个读数栏各放一份，
 * 点哪一份都同时归零该用户的接入和转发。
 */
export function UserCycleReset({
  user,
  period,
  editable,
}: {
  user: UserListItem;
  period?: UserCyclePeriod;
  editable: boolean;
}) {
  const [open, setOpen] = useState(false);
  if (!editable) return null;
  return (
    <>
      <button
        type="button"
        className="user-cycle-reset-action"
        aria-haspopup="dialog"
        disabled={!period}
        onClick={() => setOpen(true)}
      >
        重置
      </button>
      {open && period && <ResetDialog user={user} period={period} onClose={() => setOpen(false)} />}
    </>
  );
}

function ResetDialog({ user, period, onClose }: { user: UserListItem; period: UserCyclePeriod; onClose: () => void }) {
  const qc = useQueryClient();
  // One identifier survives lost responses and retries. Reopening creates a new confirmed action.
  const [request] = useState(() => ({
    request_id: randomKey().replaceAll('-', ''),
    expected_reset_id: period.reset_id ?? null,
    expected_period_start: period.starts_at,
    expected_period_end: period.ends_at,
  }));
  const save = useMutation({
    mutationFn: () => resetUserCycleUsage(user.tenant_id, user.id, request),
    onSuccess: async () => {
      await Promise.all(
        ['user-cycles', 'quotas', 'forwarding', 'user-traffic-policy', 'clash-subscription', 'me-user'].map(key =>
          qc.invalidateQueries({ queryKey: [key] }),
        ),
      );
      onClose();
    },
  });
  return (
    <DialogLayer label={`${user.id} 的本期用量归零`} onClose={onClose} canClose={() => !save.isPending}>
      <div className="dialog-surface confirm-card">
        <div className="confirm-title">本期用量归零</div>
        <div className="confirm-body">
          <p>将用户「{user.id}」接入和转发的本期用量归零，从现在重新累计。</p>
          <p>历史用量与原始计量记录保留，每月 {user.traffic_reset_day ?? 1} 日的自动重置和服务有效期保持不变。</p>
          <p>因额度用尽而暂停或限速的服务，将在用量重新评估、节点同步后恢复；手动停用和有效期限制仍然生效。</p>
          {period.reset_at && (
            <p className="hint">
              本期上次归零：
              {new Date(period.reset_at).toLocaleString('zh-CN', { timeZone: 'Asia/Taipei', hour12: false })}（UTC+8）
            </p>
          )}
          {save.error && <ErrorBox error={save.error} />}
        </div>
        <div className="confirm-actions">
          <DialogClose className="btn" disabled={save.isPending}>
            取消
          </DialogClose>
          <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? '归零中…' : '确认本期归零'}
          </button>
        </div>
      </div>
    </DialogLayer>
  );
}
