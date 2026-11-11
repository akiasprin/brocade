// 实例详情「网络」页签的访问入口清单：状态与失败原因、撤销；新增与改派在组合台完成。
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  entryStateText,
  fetchCvmInstanceEntries,
  releaseCvmEntry,
  type CvmEntry,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { navigate } from '../../forge/route';
import { ErrorBox, Loading } from '../../ui/bits';
import { ENTRY_BOARD } from './board/inspector';
import { entryText } from './board/model';
import { useNicRequestId } from './network-common';
import { Section } from './presentation';

export function InstanceEntries({ instance, workspace }: { instance: CvmInstance; workspace: CvmWorkspace }) {
  const client = useQueryClient();
  const key = ['cvm-instance-entries', instance.desired.id];
  const entries = useQuery({
    queryKey: key,
    queryFn: () => fetchCvmInstanceEntries(instance.desired.id),
    refetchInterval: 15_000,
  });
  const operationId = useNicRequestId();
  const release = useMutation({
    mutationFn: (entry: CvmEntry) => {
      const body = { expected_revision: entry.revision };
      return releaseCvmEntry(entry.id, { ...body, request_id: operationId({ release: entry.id, ...body }) });
    },
    onSettled: () => {
      void client.invalidateQueries({ queryKey: key });
      void client.invalidateQueries({ queryKey: ENTRY_BOARD });
    },
  });
  return (
    <Section
      title="访问入口"
      aside={
        <button className="cvm-text-link" onClick={() => navigate('cvm', {})}>
          在组合台管理
        </button>
      }
    >
      {entries.isPending ? (
        <Loading />
      ) : !entries.data ? (
        <ErrorBox error={entries.error} />
      ) : entries.data.length ? (
        <ul className="cvm-entry-list" aria-label="访问入口">
          {entries.data.map(entry => {
            const text = entryText(entry);
            return (
              <li key={entry.id}>
                <span className={`cz-layer ${text.layer}`}>{text.badge}</span>
                <span className="cvm-entry-address">
                  <code>{workspace.redacted ? '已脱敏' : text.text}</code>
                  <span className="cvm-entry-target">{text.target}</span>
                </span>
                <span className={`cz-state ${entry.state}`}>{entryStateText[entry.state]}</span>
                {workspace.can_write && entry.state !== 'releasing' ? (
                  <button className="btn small" disabled={release.isPending} onClick={() => release.mutate(entry)}>
                    撤销
                  </button>
                ) : (
                  <span />
                )}
                {entry.error && <p className="cvm-notice error">{entry.error}</p>}
              </li>
            );
          })}
        </ul>
      ) : (
        <p className="note">没有访问入口。入口投递到主网卡，不占网卡槽位；在组合台从左侧地址连到这个容器即可接入。</p>
      )}
      {release.error && <ErrorBox error={release.error} />}
    </Section>
  );
}
