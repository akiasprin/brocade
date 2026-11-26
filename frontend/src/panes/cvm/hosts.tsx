import './hosts.css';
import { useQuery } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { fetchCvmNicCatalog, type CvmHost, type CvmInstance, type CvmWorkspace } from '../../api';
import { navigate } from '../../forge/route';
import { Ago, EmptyState } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { AdoptHost } from './adopt';
import { CapacityMeter, reservationUsage } from './cards';
import { HostNetworkLogs, RemoveHost } from './host-policy';
import { NIC_CATALOG } from './network-common';
import { InstanceStateDot } from './presentation';
import { capacity, stackHosts, supportsCreate, supportsGuestTerminal } from './queries';
import { HostTerminalPolicy } from './terminal-settings';

export function HostList({
  workspace,
  onCreateOnHost = nodeId => navigate('cvm', { p: 'new', host: nodeId }),
}: {
  workspace: CvmWorkspace;
  onCreateOnHost?: (nodeId: string) => void;
}) {
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState<string | null>(null);
  const hosts = stackHosts(workspace);
  if (!hosts.length)
    return (
      <EmptyState icon="host" title="尚无宿主">
        机器启用执行能力并上报后，会在这里显示。机器原有功能可继续使用。
      </EmptyState>
    );

  const visible = hosts.filter(host => host.name.toLowerCase().includes(search.trim().toLowerCase()));
  return (
    <section className="cvm-hosts" aria-label="宿主清单">
      <div className="cvm-hosts-toolbar">
        <span>{visible.length} 台宿主 · 分配量 / 宿主容量</span>
        <label className="cvm-hosts-search">
          <Icon of="search" size={14} />
          <input
            className="f"
            type="search"
            aria-label="搜索宿主"
            placeholder="宿主名称"
            value={search}
            onChange={event => setSearch(event.target.value)}
          />
        </label>
      </div>
      {visible.length ? (
        <div className="cvm-hosts-frame">
          <table className="cvm-hosts-table" aria-label="宿主" role="table">
            <thead role="rowgroup">
              <tr role="row">
                {['宿主', '状态', 'vCPU', '内存', '系统盘', '容器', '运行模式', 'WebShell'].map(label => (
                  <th scope="col" key={label} role="columnheader">
                    {label}
                  </th>
                ))}
                <th scope="col" role="columnheader" aria-label="操作" />
              </tr>
            </thead>
            <tbody role="rowgroup">
              {visible.map(host => (
                <HostRow
                  key={host.node_id}
                  host={host}
                  workspace={workspace}
                  expanded={expanded === host.node_id}
                  onToggle={() => setExpanded(open => (open === host.node_id ? null : host.node_id))}
                  onCreateOnHost={onCreateOnHost}
                />
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <EmptyState icon="search" title="没有匹配的宿主">
          请修改宿主名称后重试。
        </EmptyState>
      )}
    </section>
  );
}

function HostRow({
  host,
  workspace,
  expanded,
  onToggle,
  onCreateOnHost,
}: {
  host: CvmHost;
  workspace: CvmWorkspace;
  expanded: boolean;
  onToggle: () => void;
  onCreateOnHost: (nodeId: string) => void;
}) {
  const detailsId = useId();
  const caps = host.capabilities;
  const instances = workspace.instances.filter(instance => instance.node_id === host.node_id);
  const here = instances.filter(item => item.desired.power !== 'deleted' || item.status !== 'applied');
  const reserved = instances.filter(item => item.reserved);
  const used = reservationUsage(reserved);
  const canCreate = supportsCreate(host);
  const [status, tone] = host.error
    ? ['不可用', 'failed']
    : !host.fresh
      ? ['能力已过期', 'pending']
      : caps.baremetal
        ? ['需要升级', 'failed']
        : canCreate
          ? ['可创建', 'succeeded']
          : ['未开放创建', 'pending'];
  const mode = caps.baremetal
    ? 'Agent 需升级'
    : caps.host_system_error
      ? '宿主系统管理不可用'
      : caps.host_system
        ? 'Cube + 宿主系统'
        : caps.backend && '共享自动连接预算';
  const capacityRows = [
    { label: 'vCPU', used: used.vcpus, total: caps.allocatable_vcpus, format: String },
    { label: '内存', used: used.memory, total: caps.allocatable_memory_bytes, format: capacity },
    { label: '系统盘', used: used.disk, total: caps.allocatable_disk_bytes, format: capacity },
  ];
  return (
    <>
      <tr className="cvm-hosts-row" data-expanded={expanded} role="row">
        <th className="cvm-hosts-identity" scope="row" role="rowheader">
          <button
            type="button"
            className="cvm-hosts-disclosure"
            aria-expanded={expanded}
            aria-controls={detailsId}
            onClick={onToggle}
          >
            <Icon of="host" size={15} />
            <strong>{host.name}</strong>
            <Icon of="chevronDown" size={13} />
          </button>
          <small>
            {caps.backend && <>{caps.backend} · </>}
            <Ago at={host.observed_at} />
            上报
            {caps.host_system && <> · {caps.host_system.os}</>}
          </small>
        </th>
        <td className="cvm-hosts-state" role="cell">
          <span className={`st st-${tone}`} role="status" title={host.error ?? undefined}>
            {status}
          </span>
        </td>
        {capacityRows.map(row => (
          <td className="cvm-hosts-capacity" role="cell" key={row.label}>
            <span className="cvm-hosts-mobile-label">{row.label}</span>
            <span className="mono" aria-label={`${row.label}分配量 / 宿主容量`}>
              {row.format(row.used)} / {row.total > 0 ? row.format(row.total) : '—'}
            </span>
            {row.total > 0 && <CapacityMeter used={row.used} total={row.total} allowOversell />}
          </td>
        ))}
        <td className="cvm-hosts-count" role="cell">
          <span className="cvm-hosts-mobile-label">工作负载</span>
          <span className="mono">{here.length}</span>
        </td>
        <td className="cvm-hosts-mode" role="cell">
          {mode || '—'}
        </td>
        <td className="cvm-hosts-terminal" role="cell">
          <HostTerminalPolicy host={host} />
        </td>
        <td className="cvm-hosts-logs" role="cell">
          {workspace.can_manage_network && <HostNetworkLogs host={host} />}
        </td>
      </tr>
      <tr id={detailsId} hidden={!expanded} className="cvm-hosts-detail-row" role="row">
        {expanded && (
          <td colSpan={9} role="cell">
            <HostDetails host={host} workspace={workspace} instances={here}>
              {workspace.can_write && canCreate && (
                <button className="btn sm" type="button" onClick={() => onCreateOnHost(host.node_id)}>
                  <Icon of="plus" size={13} /> 在此创建
                </button>
              )}
              <AdoptHost host={host} workspace={workspace} />
              <RemoveHost host={host} occupied={here.length > 0 || reserved.length > 0} />
            </HostDetails>
          </td>
        )}
      </tr>
    </>
  );
}

function HostDetails({
  host,
  workspace,
  instances,
  children,
}: {
  host: CvmHost;
  workspace: CvmWorkspace;
  instances: CvmInstance[];
  children: React.ReactNode;
}) {
  const caps = host.capabilities;
  const catalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: workspace.can_manage_network,
  });
  const provider = catalog.data?.providers.find(item => item.node_id === host.node_id);
  const network = provider?.fresh && !provider.capabilities.error && !catalog.isError ? provider.capabilities : null;
  const capabilities: [string, boolean | undefined][] = [
    ['入站方式', network?.entries],
    ['默认网络投递', caps.primary_ip_delivery],
    ['L4 路由', caps.route_services === false ? false : network?.route_service],
    ['线路出口', caps.chain_egress],
    ['持久终端', supportsGuestTerminal(caps)],
    ['安全改密', caps.stable_storage],
  ];
  return (
    <div className="cvm-hosts-detail">
      {(host.error || caps.host_system_error) && (
        <div className="cvm-hosts-errors">
          {host.error && <p className="cvm-notice error">{host.error}</p>}
          {caps.host_system_error && <p className="cvm-notice error">宿主系统管理不可用 · {caps.host_system_error}</p>}
        </div>
      )}
      {caps.baremetal && <p className="cvm-notice error">旧裸机独占模式已停用，请运行新版安装命令转换执行栈。</p>}
      <section>
        <h3>工作负载</h3>
        {instances.length ? (
          <div className="cvm-host-pills">
            {instances.map(instance => {
              const { id, spec } = instance.desired;
              return (
                <button
                  type="button"
                  className="cvm-host-pill"
                  key={id}
                  title={`打开 ${spec.name}`}
                  onClick={() => navigate('cvm', { p: 'instance', id })}
                >
                  <InstanceStateDot instance={instance} />
                  <b>{spec.name}</b>
                  <small>
                    {spec.resources.vcpus}C · {capacity(spec.resources.memory_bytes)}
                  </small>
                </button>
              );
            })}
          </div>
        ) : (
          <p className="note">暂无工作负载</p>
        )}
      </section>
      <section>
        <h3>执行能力{!host.fresh && ' · 待重新上报'}</h3>
        <ul className="cvm-hosts-capabilities">
          {capabilities.map(([label, supported]) => {
            const state = !host.fresh || supported == null ? '未知' : supported ? '支持' : '不支持';
            return (
              <li key={label} className={`st ${state === '支持' ? 'st-succeeded' : ''}`} title={`${label} · ${state}`}>
                <span className="cvm-hosts-capability-mark" aria-hidden="true">
                  {state === '支持' ? '✓' : state === '未知' ? '?' : '–'}
                </span>
                {label}
                <span className="cvm-hosts-sr-only"> · {state}</span>
              </li>
            );
          })}
        </ul>
        {host.fresh && <p className="note">出站方式上限 {caps.max_interfaces}</p>}
        {caps.host_system && (
          <p className="note">
            {caps.host_system.os} · 管理网卡 {caps.host_system.primary_device}{' '}
            <button className="btn sm" type="button" onClick={() => navigate('nodes', { p: 'node', id: host.node_id })}>
              查看宿主系统
            </button>
          </p>
        )}
      </section>
      <div className="cvm-hosts-detail-actions">{children}</div>
    </div>
  );
}
