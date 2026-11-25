import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { setCvmHostPolicy, type CvmHost, type CvmWorkspace } from '../../api';
import { Confirm, ErrorBox } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { NetworkLogsDialog } from './network-logs';
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

export function HostNetworkLogs({ host }: { host: CvmHost }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        className="btn icon cvm-row-btn"
        type="button"
        aria-label={`${host.name} 网络配置日志`}
        title="宿主网络配置日志"
        onClick={() => setOpen(true)}
      >
        <Icon of="diag" size={15} />
      </button>
      {open && (
        <NetworkLogsDialog
          title="宿主网络配置日志"
          machines={[{ node_id: host.node_id, name: host.name, role: '计算' }]}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

export function RemoveHost({ host, occupied }: { host: CvmHost; occupied: boolean }) {
  const change = useHostPolicy();
  const [confirm, setConfirm] = useState(false);
  const reasonId = useId();
  if (!host.can_manage_host) return null;
  const reason = occupied ? '请先清理该宿主上的工作负载并等待资源清理完成。' : null;
  return (
    <>
      <button
        className="btn sm danger"
        type="button"
        aria-describedby={reason ? reasonId : undefined}
        disabled={occupied || change.isPending}
        onClick={() => setConfirm(true)}
      >
        {change.isPending ? '移除中…' : '移除宿主…'}
      </button>
      {reason && (
        <p className="note" id={reasonId}>
          {reason}
        </p>
      )}
      {change.error && (
        <div role="alert">
          <ErrorBox error={change.error} />
        </div>
      )}
      {confirm && (
        <Confirm
          title="移除宿主"
          body={<>{host.name} 将从宿主清单移除。机器的代理、组网及其他功能会保留。</>}
          confirmLabel="确认移除"
          confirmDisabled={occupied || change.isPending}
          onCancel={() => setConfirm(false)}
          onConfirm={() => {
            if (occupied || change.isPending) return;
            setConfirm(false);
            change.mutate({ host, enabled: false });
          }}
        />
      )}
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
