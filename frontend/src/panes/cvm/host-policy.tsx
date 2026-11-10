import { useMutation, useQueryClient } from '@tanstack/react-query';
import { setCvmHostPolicy, type CvmHost, type CvmWorkspace } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { QUERY } from './queries';

function useHostPolicy() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ host, enabled }: { host: CvmHost; enabled: boolean }) =>
      setCvmHostPolicy(host.node_id, enabled, host.host_revision),
    onSuccess: (next, { host }) => {
      const node = host.node_id;
      const active = next.hosts.find(item => item.node_id === node);
      const available = next.available_hosts?.find(item => item.node_id === node);
      const received = active ?? available;
      client.setQueryData<CvmWorkspace>(QUERY, current => {
        if (!current) return next;
        const previous = [...current.hosts, ...(current.available_hosts ?? [])].find(item => item.node_id === node);
        if (!received || (previous && previous.host_revision > received.host_revision)) return current;
        return {
          ...current,
          hosts: [...current.hosts.filter(item => item.node_id !== node), ...(active ? [active] : [])],
          available_hosts: [
            ...(current.available_hosts ?? []).filter(item => item.node_id !== node),
            ...(available ? [available] : []),
          ],
        };
      });
      void client.invalidateQueries({ queryKey: QUERY });
    },
    onError: () => {
      void client.invalidateQueries({ queryKey: QUERY });
    },
  });
}

export function RemoveHost({ host }: { host: CvmHost }) {
  const change = useHostPolicy();
  if (!host.can_manage_host) return null;
  return (
    <>
      <div className="cvm-card-footer">
        <button
          className="btn danger"
          title="取消容器宿主身份，保留机器的代理、组网及其他功能"
          disabled={change.isPending}
          onClick={() => change.mutate({ host, enabled: false })}
        >
          <Icon of="trash" size={13} />
          {change.isPending ? '移除中…' : '移除宿主'}
        </button>
      </div>
      {change.error && <ErrorBox error={change.error} />}
    </>
  );
}

export function AddHost({ workspace }: { workspace: CvmWorkspace }) {
  const change = useHostPolicy();
  const available = (workspace.available_hosts ?? []).filter(host => host.can_manage_host);
  if (!available.length) return null;
  return (
    <div>
      <select
        className="f"
        aria-label="添加宿主"
        value=""
        disabled={change.isPending}
        onChange={event => {
          const host = available.find(item => item.node_id === event.target.value);
          if (host) change.mutate({ host, enabled: true });
        }}
      >
        <option value="">{change.isPending ? '添加中…' : '＋ 添加宿主'}</option>
        {available.map(host => (
          <option value={host.node_id} key={host.node_id}>
            {host.name}
          </option>
        ))}
      </select>
      {change.error && <ErrorBox error={change.error} />}
    </div>
  );
}
