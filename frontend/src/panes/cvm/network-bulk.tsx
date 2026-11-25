import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useRef, useState } from 'react';
import { updateCvmNicVersions, type CvmNicAttachment, type CvmNicPublication } from '../../api';
import { NicDialog, uncertainCvmResult } from './network-common';
import { invalidateCvmChanges, requestId } from './queries';

const MAX_BATCH = 64;

export function BulkNicVersionDialog({
  publication,
  attachments,
  onClose,
  selectOutdated = false,
}: {
  publication: CvmNicPublication;
  attachments: CvmNicAttachment[];
  onClose: () => void;
  selectOutdated?: boolean;
}) {
  const [revision] = useState(publication.revision);
  const [choices] = useState(() =>
    attachments.filter(n => n.publication_id === publication.id && n.attached && n.publication_revision < revision),
  );
  const [selected, setSelected] = useState(
    () => new Set(selectOutdated ? choices.slice(0, MAX_BATCH).map(n => n.allocation_id) : []),
  );
  const [results, setResults] = useState<Record<string, string>>({});
  const [saved, setSaved] = useState(() => new Set<string>());
  const attempts = useRef(new Map<string, { digest: string; id: string }>());
  const client = useQueryClient();
  const pending = choices.filter(n => selected.has(n.allocation_id) && !saved.has(n.instance_id));
  const update = useMutation({
    mutationFn: async () => {
      const grouped = new Map<string, CvmNicAttachment[]>();
      for (const nic of pending) {
        grouped.set(nic.instance_id, [...(grouped.get(nic.instance_id) ?? []), nic]);
      }
      const jobs = [...grouped.entries()];
      let cursor = 0;
      await Promise.all(
        Array.from({ length: Math.min(4, jobs.length) }, async () => {
          while (cursor < jobs.length) {
            const [id, nics] = jobs[cursor++];
            const body = {
              expected_generation: nics[0].generation,
              updates: nics.map(nic => ({
                interface_id: nic.interface_id,
                publication_revision: revision,
                reallocate: false,
              })),
            };
            try {
              const digest = JSON.stringify(body);
              const previous = attempts.current.get(id);
              if (previous && previous.digest !== digest)
                throw new Error('本容器上一批操作的结果尚未确认，请先核查或重试原选择。');
              if (!previous && attempts.current.size >= MAX_BATCH)
                throw new Error(`本窗口已处理 ${MAX_BATCH} 个容器，请核查结果后重新打开。`);
              const attempt = previous ?? { digest, id: requestId() };
              attempts.current.set(id, attempt);
              await updateCvmNicVersions(id, { ...body, request_id: attempt.id });
              setSaved(old => new Set(old).add(id));
              record(nics, '已保存，等待发布');
            } catch (error) {
              if (!uncertainCvmResult(error)) attempts.current.delete(id);
              record(nics, error instanceof Error ? error.message : '请求结果未知，请核查');
            }
          }
        }),
      );
      invalidateCvmChanges(client);
    },
  });
  function record(nics: CvmNicAttachment[], message: string) {
    setResults(old => ({ ...old, ...Object.fromEntries(nics.map(nic => [nic.allocation_id, message])) }));
  }
  return (
    <NicDialog
      title="批量应用网卡规格"
      onClose={onClose}
      pending={update.isPending}
      disabled={!pending.length}
      onSubmit={() => update.mutate()}
    >
      <p>
        将「{publication.spec.name}」更新至版本 {revision}
        ，保留现有地址、连接方式和容器路由表。同一容器内的所选网卡在一次操作中保存。
      </p>
      <p className="note">
        每批最多 {MAX_BATCH}{' '}
        张。需要重新分配地址、更换连接方式或修改路由时，请在容器中处理后再更新。保存后在「发布」中统一应用。
      </p>
      <p className="note" role="status">
        本次选中 {pending.length} / {choices.length} 张待更新网卡
        {saved.size > 0 && ` · ${saved.size} 个容器已保存`}
      </p>
      <div className="cvm-attached-list">
        {choices.map(nic => {
          const checked = selected.has(nic.allocation_id);
          const savedInstance = saved.has(nic.instance_id);
          const result = savedInstance && !checked ? '请重新打开后更新此网卡' : results[nic.allocation_id];
          return (
            <label className="cvm-check cvm-network-bulk-choice" key={nic.allocation_id}>
              <input
                type="checkbox"
                checked={checked}
                disabled={savedInstance || (!checked && pending.length >= MAX_BATCH)}
                onChange={event => {
                  const next = new Set(selected);
                  if (event.target.checked) next.add(nic.allocation_id);
                  else next.delete(nic.allocation_id);
                  setSelected(next);
                }}
              />
              <span>
                {nic.instance_name} · {nic.interface_id.slice(-8)}
                <small>
                  当前版本 {nic.publication_revision}
                  {result ? ` · ${result}` : ''}
                </small>
              </span>
            </label>
          );
        })}
      </div>
    </NicDialog>
  );
}
