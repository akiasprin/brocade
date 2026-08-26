import { useState, useSyncExternalStore } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { applyDraft, ApiError } from '../api';
import { draft } from '../draft';
import { can, useSession } from '../session';

// 尚未提交的改动。
// 草稿为空时整条不渲染——没有待处理项时界面上不应有常驻的横条占位。
//
// 位于面包屑下方而非顶栏内，是因为它需要展开列出每一条：改动数量本身不提供信息，
// 需要了解的是具体是哪几处，以及如何丢弃其中某一条。
export function DraftBar({ current }: { current: number | undefined }) {
  const { who } = useSession();
  const qc = useQueryClient();
  useSyncExternalStore(draft.subscribe, draft.version);
  const entries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (entries.length === 0) return null;

  const editable = can(who.role, 'edit');

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['compile'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['nodes'] });
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['deployment-verify'] });
  };

  const commit = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await applyDraft(draft.ops());
      // 先清除草稿再刷新：顺序相反时，刷新读取到的仍是草稿生效后的预览，
      // 与刚写库的内容相同，无法确认提交是否成功。
      draft.clear();
      setOpen(false);
      refresh();
      if (result.changed === 0) {
        setError('这些改动跟库里现有的值一模一样，没有产生新修订。');
      }
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={`fg-draft${open ? ' open' : ''}`}>
      <div className="fg-draft-head">
        <button className="fg-draft-count" onClick={() => setOpen(!open)} title="展开查看具体条目">
          {open ? '▾' : '▸'} {entries.length} 处未提交
        </button>
        <span className="note">
          尚未写入库。提交会将它们<b>合并为一个修订</b>（修订 {current ?? '…'} 的下一个）。
        </span>
        <span className="sp" />
        <button
          className="btn"
          disabled={busy}
          onClick={() => {
            draft.clear();
            setError(null);
            refresh();
          }}
        >
          全部丢弃
        </button>
        <button className="btn primary" disabled={busy || !editable} onClick={() => void commit()}>
          {busy ? '提交中…' : '提交'}
        </button>
      </div>

      {error && <div className="callout warn fg-draft-err">{error}</div>}

      {open && (
        <ul className="fg-draft-list">
          {entries.map(e => (
            <li key={e.key}>
              <span className="mono">{e.label}</span>
              <button
                className="btn"
                title="只丢这一条"
                onClick={() => {
                  draft.drop(e.key);
                  refresh();
                }}
              >
                丢弃
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
