import type { CvmWorkspace } from '../../api';
import { Ago, EmptyState } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { capacity, stackHosts, supportsCreate } from './queries';
import { CapacityRows, reservedInstances, reservationUsage } from './cards';
import { HostTerminalPolicy } from './terminal-settings';
import { RemoveHost } from './host-policy';

export function HostList({ workspace }: { workspace: CvmWorkspace }) {
  const hosts = stackHosts(workspace);
  if (!hosts.length)
    return (
      <EmptyState icon="nodes" title="尚无宿主">
        机器启用容器执行能力并上报后，会在这里显示。机器原有功能可继续使用。
      </EmptyState>
    );
  return (
    <div className="cvm-host-list">
      <div className="node-card-grid cvm-host-grid" role="list" aria-label="宿主">
        {hosts.map(host => {
          const caps = host.capabilities;
          const count = workspace.instances.filter(
            instance =>
              instance.node_id === host.node_id &&
              !(instance.desired.power === 'deleted' && instance.status === 'applied'),
          ).length;
          const used = reservationUsage(reservedInstances(workspace).filter(item => item.node_id === host.node_id));
          const ready = !host.error && supportsCreate(host);
          const label = host.error ? '执行能力不可用' : !host.fresh ? '能力已过期' : ready ? '可创建' : '未开放创建';
          return (
            <article
              className={`node-card cvm-host-card tone-${host.error ? 'bad' : ready ? 'ok' : 'warn'}`}
              role="listitem"
              key={host.node_id}
              aria-label={host.name}
            >
              <div className="cvm-host-card-head">
                <Icon of="nodes" size={15} />
                <div className="cvm-host-identity">
                  <b title={host.name}>{host.name}</b>
                  <small>
                    <Ago at={host.observed_at} />
                    上报
                  </small>
                </div>
                <span className={`st ${ready ? 'st-succeeded' : 'st-pending'}`} role="status">
                  {label}
                </span>
              </div>
              <p className="cvm-card-sub">
                {count} 个可见实例 · 最多 {caps.max_interfaces} 张网卡
              </p>
              <CapacityRows
                rows={[
                  { label: 'vCPU', used: used.vcpus, total: caps.allocatable_vcpus, format: String },
                  { label: '内存', used: used.memory, total: caps.allocatable_memory_bytes, format: capacity },
                  { label: '系统盘', used: used.disk, total: caps.allocatable_disk_bytes, format: capacity },
                ]}
              />
              <p className="note">可见预留 / 节点可分配</p>
              {host.error && <p className="note cvm-host-error">{host.error}</p>}
              <HostTerminalPolicy host={host} />
              <RemoveHost host={host} />
            </article>
          );
        })}
      </div>
      <p className="note cvm-host-summary">{hosts.length} 台宿主 · 资源与能力由节点上报</p>
    </div>
  );
}
