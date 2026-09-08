import { useState, useSyncExternalStore } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { applyDraft, ApiError, type ModelSettings } from '../api';
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
  const conflicts = useSyncExternalStore(draft.subscribe, draft.conflicts);
  const storageError = useSyncExternalStore(draft.subscribe, draft.storageError);
  const submitting = useSyncExternalStore(draft.subscribe, draft.isSubmitting);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (entries.length === 0 && conflicts.length === 0 && !storageError && !submitting) return null;

  const editable = can(who.role, 'edit');

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['compile'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['nodes'] });
    qc.invalidateQueries({ queryKey: ['certs'] });
    qc.invalidateQueries({ queryKey: ['users'] });
    qc.invalidateQueries({ queryKey: ['settings'] });
    qc.invalidateQueries({ queryKey: ['deployment-verify'] });
  };

  const commit = async () => {
    setBusy(true);
    setError(null);
    let batch: ReturnType<typeof draft.beginSubmission> | undefined;
    let succeeded = false;
    try {
      batch = draft.beginSubmission();
      await applyDraft(batch.entries.map(entry => entry.op));
      succeeded = true;
      // Seed the committed settings before removing their draft baseline. Cancel an older GET
      // first, so clearing the batch cannot briefly rebase the form onto the old cached value.
      const settings = batch.entries.find(entry => entry.op.op === 'update_settings')?.op;
      if (draft.ownsBatch(batch) && settings?.op === 'update_settings') {
        await qc.cancelQueries({ queryKey: ['settings'] });
        if (draft.ownsBatch(batch)) qc.setQueryData<ModelSettings>(['settings'], settings.settings);
      }
      draft.finishSubmission(batch, true);
      batch = undefined;
      setOpen(false);
      refresh();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    } finally {
      if (batch) draft.finishSubmission(batch, succeeded);
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
          disabled={busy || submitting || conflicts.length > 0}
          onClick={() => {
            draft.clear();
            setError(null);
            refresh();
          }}
        >
          全部丢弃
        </button>
        <button
          className="btn primary"
          disabled={busy || submitting || !editable || conflicts.length > 0 || entries.length === 0}
          onClick={() => void commit()}
        >
          {busy ? '提交中…' : '提交'}
        </button>
      </div>

      {error && <div className="callout warn fg-draft-err">{error}</div>}
      {storageError && (
        <div className="callout warn">
          {storageError}{' '}
          <button
            className="btn"
            disabled={busy || submitting}
            onClick={() => {
              try {
                draft.retryPersistence();
                setError(null);
              } catch (error) {
                setError(error instanceof Error ? error.message : String(error));
              }
            }}
          >
            重试保存记录
          </button>
        </div>
      )}
      {conflicts.length > 0 && (
        <div className="callout warn">
          <p>多个标签页同时修改了草稿，两份修改都已保留。请查看具体内容并选择要采用的版本；选择后其他版本将被丢弃。</p>
          {conflicts.map((branch, index) => (
            <details key={branch.id}>
              <summary>
                草稿 {index + 1} · {branch.entries.length} 处修改
              </summary>
              <pre style={{ maxHeight: 240, overflow: 'auto' }}>
                {JSON.stringify(
                  branch.entries.map(entry => entry.op),
                  null,
                  2,
                )}
              </pre>
              <button
                className="btn"
                disabled={busy || submitting || !editable}
                onClick={() => {
                  try {
                    draft.resolveConflict(branch.id);
                    setError(null);
                    refresh();
                  } catch (error) {
                    setError(error instanceof Error ? error.message : String(error));
                  }
                }}
              >
                采用这份草稿
              </button>
            </details>
          ))}
        </div>
      )}

      {open && (
        <ul className="fg-draft-list">
          {entries.map(e => (
            <li key={e.key}>
              <span className="mono">{e.label}</span>
              <button
                className="btn"
                title="只丢这一条"
                disabled={busy || submitting || conflicts.length > 0}
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
