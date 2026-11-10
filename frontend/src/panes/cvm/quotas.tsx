import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useCallback, useState } from 'react';
import { setCvmQuota, type CvmQuota, type CvmWorkspace, type CvmHostNetworkQuotaChange } from '../../api';
import { EmptyState, ErrorBox } from '../../ui/bits';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';

import { GIB, MIB, hasStack, ownerKey, QUERY } from './queries';
import { CapacityRows, quotaRows } from './cards';
import { NetworkQuotaEditor, networkQuotaStatus } from './network-quota';

export function QuotaList({ workspace }: { workspace: CvmWorkspace }) {
  const [editing, setEditing] = useState<string | null>(null);
  if (!workspace.can_manage_quotas) return null;
  const owners = workspace.owners;
  return (
    <>
      {!owners.length && (
        <EmptyState icon="users" title="尚无可授权用户">
          创建用户后可在此分配额度。
        </EmptyState>
      )}
      <div className="node-card-grid cvm-quota-grid" role="list" aria-label="用户额度">
        {owners.map(owner => {
          const key = ownerKey(owner.tenant_id, owner.user_id);
          const quota = workspace.quotas.find(q => ownerKey(q.tenant_id, q.user_id) === key);
          return (
            <article
              className={`node-card cvm-quota-card tone-${quota?.enabled ? 'ok' : 'idle'}`}
              role="listitem"
              key={key}
            >
              <div className="node-card-head">
                <Icon of="users" size={15} />
                <b className="node-card-name" title={owner.name}>
                  {owner.name}
                </b>
                <span className="cvm-mode">{quota?.enabled ? '已授权' : '未授权'}</span>
              </div>
              <p
                className="cvm-card-sub"
                title={quota?.allowed_nodes
                  .map(id => workspace.hosts.find(h => h.node_id === id)?.name ?? id)
                  .join('、')}
              >
                {owner.tenant_id} · {quota?.enabled ? `可用 ${quota.allowed_nodes.length} 台宿主` : '尚未开放创建'}
              </p>
              {quota && <CapacityRows rows={quotaRows(workspace, quota)} />}
              {(workspace.network_quotas ?? [])
                .filter(q => q.tenant_id === owner.tenant_id && q.user_id === owner.user_id)
                .map(q => (
                  <p className="note" key={q.node_id}>
                    {workspace.hosts.find(host => host.node_id === q.node_id)?.name ?? q.node_id} · 网络{' '}
                    {q.limits.max_states.toLocaleString('zh-CN')} 条 · {networkQuotaStatus(q)}
                  </p>
                ))}
              <footer className="cvm-card-footer">
                <button className="btn small" onClick={() => setEditing(key)}>
                  {quota ? '编辑额度' : '授权使用容器'}
                </button>
              </footer>
            </article>
          );
        })}
      </div>
      {editing !== null && (
        <QuotaDialog key={editing} workspace={workspace} initialOwner={editing} onClose={() => setEditing(null)} />
      )}
    </>
  );
}

function QuotaDialog({
  workspace,
  onClose,
  initialOwner,
}: {
  workspace: CvmWorkspace;
  onClose: () => void;
  initialOwner: string;
}) {
  const qc = useQueryClient();
  const [owner, setOwner] = useState(initialOwner);
  const initial = (key: string): CvmQuota => {
    const [tenant, user] = key ? (JSON.parse(key) as [string, string]) : ['', ''];
    return (
      workspace.quotas.find(item => ownerKey(item.tenant_id, item.user_id) === key) ?? {
        tenant_id: tenant,
        user_id: user,
        enabled: true,
        allowed_nodes: [],
        max_instances: 1,
        max_vcpus: 1,
        max_memory_bytes: 512 * MIB,
        max_disk_bytes: 8 * GIB,
        max_interfaces: 4,
      }
    );
  };
  const [value, setValue] = useState(() => initial(owner));
  // Only container hosts can be granted; an existing grant elsewhere stays visible so it can be removed.
  const grantable = workspace.hosts.filter(host => hasStack(host) || value.allowed_nodes.includes(host.node_id));
  const [networkEdits, setNetworkEdits] = useState<
    Record<string, { change: CvmHostNetworkQuotaChange | null; blocked: boolean }>
  >({});
  const onNetworkChange = useCallback((node: string, change: CvmHostNetworkQuotaChange | null, blocked: boolean) => {
    setNetworkEdits(previous => ({ ...previous, [node]: { change, blocked } }));
  }, []);
  const networkBlocked = value.allowed_nodes.some(
    node =>
      workspace.hosts.find(host => host.node_id === node)?.capabilities.network_quota &&
      (!networkEdits[node] || networkEdits[node].blocked),
  );
  const save = useMutation({
    mutationFn: setCvmQuota,
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: QUERY });
      onClose();
    },
  });
  return (
    <DialogLayer label="容器 用户额度" onClose={onClose} canClose={() => !save.isPending}>
      <div className="dialog-surface tunnel-dialog cvm-dialog">
        <header>
          <b>用户额度</b>
          <DialogClose className="btn icon" aria-label="关闭" title="关闭" disabled={save.isPending}>
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body">
          <fieldset disabled={save.isPending} className="cvm-form-fieldset">
            <div className="cvm-form-grid">
              <label>
                使用用户
                <select
                  className="f"
                  value={owner}
                  onChange={event => {
                    setOwner(event.target.value);
                    setValue(initial(event.target.value));
                    setNetworkEdits({});
                  }}
                >
                  {workspace.owners.map(item => (
                    <option key={ownerKey(item.tenant_id, item.user_id)} value={ownerKey(item.tenant_id, item.user_id)}>
                      {item.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="cvm-check">
                <input
                  type="checkbox"
                  checked={value.enabled}
                  onChange={event => setValue({ ...value, enabled: event.target.checked })}
                />
                允许创建 容器
              </label>
              {(
                [
                  ['max_instances', '实例上限', 1],
                  ['max_vcpus', 'vCPU 总额', 1],
                  ['max_memory_bytes', '内存总额（MiB）', MIB],
                  ['max_disk_bytes', '磁盘总额（GiB）', GIB],
                  ['max_interfaces', '网卡总数', 1],
                ] as const
              ).map(([key, label, unit]) => (
                <label key={key}>
                  {label}
                  <input
                    className="f"
                    type="number"
                    min={0}
                    max={key === 'max_instances' ? 256 : key === 'max_interfaces' ? 4096 : 1048576}
                    step={1}
                    value={value[key] / unit}
                    onChange={event => setValue({ ...value, [key]: Number(event.target.value) * unit })}
                  />
                </label>
              ))}
            </div>
            <fieldset className="cvm-machine-grants">
              <legend>允许使用的机器</legend>
              {grantable.map(host => (
                <div className="cvm-quota-host" key={host.node_id}>
                  <label className="cvm-check">
                    <input
                      type="checkbox"
                      checked={value.allowed_nodes.includes(host.node_id)}
                      onChange={event =>
                        setValue({
                          ...value,
                          allowed_nodes: event.target.checked
                            ? [...value.allowed_nodes, host.node_id]
                            : value.allowed_nodes.filter(node => node !== host.node_id),
                        })
                      }
                    />
                    {host.name}
                    <small>{host.fresh && host.capabilities.create ? '可创建' : '能力未就绪'}</small>
                  </label>
                  {value.allowed_nodes.includes(host.node_id) && (
                    <NetworkQuotaEditor
                      key={`${owner}-${host.node_id}`}
                      host={host}
                      tenant={value.tenant_id}
                      user={value.user_id}
                      existing={workspace.network_quotas?.find(
                        q =>
                          q.node_id === host.node_id && q.tenant_id === value.tenant_id && q.user_id === value.user_id,
                      )}
                      onChange={onNetworkChange}
                    />
                  )}
                </div>
              ))}
              {!grantable.length && <p className="note">尚无机器上报 容器 能力。</p>}
            </fieldset>
            <p className="note">
              这里只分配使用权限和容量。实例、系统盘、网卡或网络额度仍有占用时，先释放相应资源再缩小授权范围。
            </p>
          </fieldset>
          {save.error && <ErrorBox error={save.error} />}
        </div>
        <footer>
          <button className="btn" disabled={save.isPending} onClick={onClose}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={save.isPending || !owner || networkBlocked}
            onClick={() =>
              save.mutate({
                ...value,
                host_network_quotas: value.allowed_nodes.flatMap(node =>
                  networkEdits[node]?.change ? [networkEdits[node].change!] : [],
                ),
              })
            }
          >
            {save.isPending ? '保存中…' : '保存额度'}
          </button>
        </footer>
      </div>
    </DialogLayer>
  );
}
