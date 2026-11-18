// 「宿主」页签：沿用组合台的宿主卡——名称与状态、对齐的资源读数、宿主上的容器条，
// WebShell 开关在卡底，移除等低频操作收进 ⋯ 菜单。
import type { CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { Ago, EmptyState } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { capacity, stackHosts, supportsCreate } from './queries';
import { CapacityRows, reservedInstances, reservationUsage } from './cards';
import { HostTerminalPolicy } from './terminal-settings';
import { HostMenu } from './host-policy';
import { AdoptHost } from './adopt';

export function HostList({ workspace }: { workspace: CvmWorkspace }) {
  const hosts = stackHosts(workspace);
  if (!hosts.length)
    return (
      <EmptyState icon="nodes" title="尚无宿主">
        机器启用容器执行能力并上报后，会在这里显示。机器原有功能可继续使用。
      </EmptyState>
    );
  return (
    <section className="cvm-host-list" aria-label="宿主清单">
      <header className="cvm-lane-head">
        <h2>宿主</h2>
        <small>{hosts.length} 台 · 逻辑已分配 / 宿主基准 · 允许超售</small>
      </header>
      <div className="cvm-host-grid" role="list" aria-label="宿主">
        {hosts.map(host => {
          const caps = host.capabilities;
          const here = workspace.instances.filter(
            instance =>
              instance.node_id === host.node_id &&
              !(instance.desired.power === 'deleted' && instance.status === 'applied'),
          );
          const used = reservationUsage(reservedInstances(workspace).filter(item => item.node_id === host.node_id));
          const ready = !host.error && (supportsCreate(host) || (!!caps.baremetal && host.fresh));
          const label = host.error
            ? '执行能力不可用'
            : !host.fresh
              ? '能力已过期'
              : ready
                ? caps.baremetal
                  ? '裸机'
                  : '可创建'
                : '未开放创建';
          return (
            <article
              className={`cvm-tile cvm-host-card tone-${host.error ? 'bad' : ready ? 'ok' : 'warn'}`}
              role="listitem"
              key={host.node_id}
              aria-label={host.name}
            >
              <div className="cvm-host-card-head">
                <span className="cvm-host-mark" aria-hidden="true">
                  <Icon of="servers" size={16} />
                </span>
                <div className="cvm-host-identity">
                  <b title={host.name}>{host.name}</b>
                  <small className={host.fresh ? undefined : 'stale'}>
                    {caps.baremetal ? '裸机' : 'Cube'} · <Ago at={host.observed_at} />
                    上报
                  </small>
                </div>
                <span
                  className={`st ${host.error ? 'st-failed' : ready ? 'st-succeeded' : 'st-pending'}`}
                  role="status"
                >
                  {label}
                </span>
              </div>
              {host.error && <p className="cvm-notice error cvm-host-error">{host.error}</p>}
              <div className={ready ? undefined : 'cvm-host-muted'}>
                <CapacityRows
                  allowOversell={!caps.baremetal}
                  rows={[
                    {
                      label: caps.baremetal ? 'CPU 核数' : 'vCPU',
                      used: used.vcpus,
                      total: caps.allocatable_vcpus,
                      format: String,
                    },
                    { label: '内存', used: used.memory, total: caps.allocatable_memory_bytes, format: capacity },
                    { label: '系统盘', used: used.disk, total: caps.allocatable_disk_bytes, format: capacity },
                  ]}
                />
              </div>
              <p className="cvm-host-sub">
                容器 {here.length} · 网卡上限 {caps.max_interfaces}
              </p>
              {here.length ? (
                <div className="cvm-host-pills">
                  {here.map(instance => (
                    <button
                      type="button"
                      className="cvm-host-pill"
                      key={instance.desired.id}
                      title={`打开 ${instance.desired.spec.name}`}
                      onClick={() => navigate('cvm', { p: 'instance', id: instance.desired.id })}
                    >
                      <span className={`cz-dot ${instance.status} ${instance.desired.power}`} />
                      <b>{instance.desired.spec.name}</b>
                      <small>
                        {instance.desired.spec.resources.vcpus}C·
                        {capacity(instance.desired.spec.resources.memory_bytes)
                          .replace(' MiB', 'M')
                          .replace(' GiB', 'G')}
                      </small>
                    </button>
                  ))}
                </div>
              ) : (
                <p className="cvm-host-empty">{host.fresh ? '暂无容器' : '暂无容器 · 恢复上报后才能创建'}</p>
              )}
              {caps.baremetal && <p className="note">{caps.baremetal.os} · 整机独占 · 无容器隔离</p>}
              <AdoptHost host={host} workspace={workspace} />
              <footer className="cvm-host-foot">
                <HostTerminalPolicy host={host} />
                <HostMenu host={host} workspace={workspace} />
              </footer>
            </article>
          );
        })}
      </div>
    </section>
  );
}
