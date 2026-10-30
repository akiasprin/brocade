import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { createApp, moveChain, type SnapshotApp, type SnapshotChain } from '../api';
import { appId } from '../model-id';
import { ErrorBox } from '../ui/bits';
import { DialogClose, DialogLayer } from '../ui/dialog';

const NEW_GROUP = '__new_group__';

/** The click/keyboard equivalent of moving a card across groups. */
export function ChainGroupMove({
  apps,
  app,
  chain,
  onMoved,
}: {
  apps: SnapshotApp[];
  app: SnapshotApp;
  chain: SnapshotChain;
  onMoved?: (target: string) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button className="btn" onClick={() => setOpen(true)}>
        移动到分组
      </button>
      {open && <MoveDialog apps={apps} app={app} chain={chain} onClose={() => setOpen(false)} onMoved={onMoved} />}
    </>
  );
}

function MoveDialog({
  apps,
  app,
  chain,
  onClose,
  onMoved,
}: {
  apps: SnapshotApp[];
  app: SnapshotApp;
  chain: SnapshotChain;
  onClose: () => void;
  onMoved?: (target: string) => void;
}) {
  const qc = useQueryClient();
  const [target, setTarget] = useState('');
  const [label, setLabel] = useState('');
  const [newId] = useState(() => appId(new Set(apps.map(item => item.id))));
  const save = useMutation({
    mutationFn: async () => {
      const destination = target === NEW_GROUP ? newId : target;
      if (!destination || destination === app.id) throw new Error('请选择目标分组');
      if (target === NEW_GROUP) {
        if (!label.trim()) throw new Error('请填写新分组名称');
        await createApp({ id: newId, label: label.trim() });
      }
      await moveChain(app.id, chain.id, destination);
      onMoved?.(destination);
      // Preview through the real server transaction before dismissing; failure remains visible
      // in the draft and can be discarded without changing the committed model.
      await qc.invalidateQueries({ queryKey: ['snapshot'] }, { throwOnError: true });
    },
    onSuccess: onClose,
  });
  return (
    <DialogLayer label="移动到分组" onClose={onClose} canClose={() => !save.isPending}>
      <form
        className="dialog-surface sub-dialog chain-group-move"
        onSubmit={event => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <header className="sub-dialog-head">
          <b>移动到分组</b>
          <DialogClose className="sub-dialog-close" aria-label="关闭换组" disabled={save.isPending}>
            ×
          </DialogClose>
        </header>
        <div className="chain-group-move-body">
          <p>
            <b>{chain.name || chain.id}</b>
            <br />
            <span className="hint">当前分组：{app.label || app.id}</span>
          </p>
          <label className="chain-group-move-field">
            <span>目标分组</span>
            <select
              className="f"
              value={target}
              onChange={event => setTarget(event.target.value)}
              disabled={save.isPending}
            >
              <option value="">请选择分组</option>
              {apps
                .filter(item => item.id !== app.id)
                .map(item => (
                  <option key={item.id} value={item.id}>
                    {item.label || item.id}
                  </option>
                ))}
              <option value={NEW_GROUP}>＋ 新建分组…</option>
            </select>
          </label>
          {target === NEW_GROUP && (
            <label className="chain-group-move-field">
              <span>新分组名称</span>
              <input
                className="f"
                value={label}
                onChange={event => setLabel(event.target.value)}
                disabled={save.isPending}
                autoFocus
              />
            </label>
          )}
          <p className="hint">
            历史用量保留在原分组；节点应用新发布后，新增用量计入目标分组，并使用目标分组配额。原分组配额不会复制。
          </p>
          <p className="hint">链 ID、入口凭据、规则和授权保留。移动先保存到草稿，提交并发布后生效。</p>
          <p className="hint">
            默认发布会重启 Xray 并中断连接。进程交接 Beta 只在排空期限内保留 TCP 连接，UDP 会话需重连。
          </p>
          {save.error && <ErrorBox error={save.error} />}
        </div>
        <footer className="chain-group-move-actions">
          <DialogClose className="btn" disabled={save.isPending}>
            取消
          </DialogClose>
          <button
            className="btn primary"
            disabled={save.isPending || !target || (target === NEW_GROUP && !label.trim())}
          >
            {save.isPending ? '保存中…' : '保存到草稿'}
          </button>
        </footer>
      </form>
    </DialogLayer>
  );
}
