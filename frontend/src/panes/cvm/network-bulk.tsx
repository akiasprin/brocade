import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useRef, useState } from 'react';
import { updateCvmNicVersions, type CvmNicAttachment, type CvmNicPublication } from '../../api';
import { NicDialog, NIC_CATALOG, NIC_STATE } from './network-common';
import { QUERY, requestId } from './queries';

export function BulkNicVersionDialog({
  publication,
  attachments,
  onClose,
}: {
  publication: CvmNicPublication;
  attachments: CvmNicAttachment[];
  onClose: () => void;
}) {
  const [revision] = useState(publication.revision);
  const [choices] = useState(() => attachments.filter(n => n.attached && n.publication_revision < revision));
  const [selected, setSelected] = useState<string[]>([]);
  const [results, setResults] = useState<Record<string, string>>({});
  const attempts = useRef(new Map<string, { digest: string; id: string }>());
  const client = useQueryClient();
  const update = useMutation({
    mutationFn: async () => {
      const grouped = new Map<string, CvmNicAttachment[]>();
      for (const nic of choices.filter(n => selected.includes(n.interface_id) && results[n.instance_id] !== '已受理')) {
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
              if (!previous && attempts.current.size >= 64)
                throw new Error('本窗口已处理 64 个容器，请核查结果后重新打开。');
              const attempt = previous ?? { digest, id: requestId() };
              attempts.current.set(id, attempt);
              await updateCvmNicVersions(id, { ...body, request_id: attempt.id });
              setResults(old => ({ ...old, [id]: '已受理' }));
            } catch (error) {
              setResults(old => ({ ...old, [id]: error instanceof Error ? error.message : '请求结果未知，请核查' }));
            }
          }
        }),
      );
      void client.invalidateQueries({ queryKey: NIC_CATALOG });
      void client.invalidateQueries({ queryKey: NIC_STATE });
      void client.invalidateQueries({ queryKey: QUERY });
    },
  });
  const pending = choices.filter(n => selected.includes(n.interface_id) && results[n.instance_id] !== '已受理');
  return (
    <NicDialog
      title="批量应用网卡规格"
      onClose={onClose}
      pending={update.isPending}
      disabled={!pending.length}
      onSubmit={() => update.mutate()}
      submit="应用所选兼容规格"
    >
      <p>
        将「{publication.spec.name}」更新至版本 {revision}
        ，保留现有地址、承载和容器路由表。同一容器内的所选网卡在一次操作中保存。
      </p>
      <p className="note">
        最多选择 64
        张。需要重新分配地址、调整承载或处理路由依赖的网卡会分别报告原因，请到容器中处理后再更新。受理后还需等待两端应用。
      </p>
      <div className="cvm-attached-list">
        {choices.map(nic => (
          <label className="cvm-check cvm-network-bulk-choice" key={nic.allocation_id}>
            <input
              type="checkbox"
              checked={selected.includes(nic.interface_id)}
              disabled={
                results[nic.instance_id] === '已受理' || (!selected.includes(nic.interface_id) && selected.length >= 64)
              }
              onChange={e =>
                setSelected(old =>
                  e.target.checked ? [...old, nic.interface_id] : old.filter(id => id !== nic.interface_id),
                )
              }
            />
            <span>
              {nic.instance_name} · {nic.interface_id.slice(-8)}
              <small>
                当前版本 {nic.publication_revision}
                {results[nic.instance_id] ? ` · ${results[nic.instance_id]}` : ''}
              </small>
            </span>
          </label>
        ))}
      </div>
    </NicDialog>
  );
}
