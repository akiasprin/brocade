import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import {
  entryProtocolText,
  fetchCvmInstanceEntries,
  fetchCvmNicCatalog,
  fetchCvmNicState,
  type CvmInstance,
  type CvmNicState,
  type CvmWorkspace,
} from '../../api';
import { ErrorBox, Loading } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { capacity } from './queries';
import { Section } from './presentation';
import { CapacityMeter } from './cards';
import { NIC_CATALOG, NIC_STATE, addressModeName, transportName } from './network-common';
import { MountNicDialog, NicVersionDialog, RoutesDialog, TransportDialog } from './network-dialogs';
import { PublicationDialog } from './network-publications';
import { routeLabel } from './board/model';

function configured(binding: CvmNicState['bindings'][number] | undefined) {
  return (
    !!binding &&
    [binding.consumer, binding.provider].every(
      report =>
        report?.generation === binding.generation &&
        report.phase === binding.phase &&
        report.configured &&
        !report.error,
    )
  );
}
function connected(binding: CvmNicState['bindings'][number] | undefined) {
  return (
    configured(binding) &&
    !!binding?.consumer_fresh &&
    !!binding.provider_fresh &&
    binding.consumer?.reachable === true &&
    binding.provider?.reachable === true
  );
}
const useNetworkState = (id: string) =>
  useQuery({ queryKey: [...NIC_STATE, id], queryFn: () => fetchCvmNicState(id), refetchInterval: 5000 });

export function ManagedNetworking({
  instance,
  workspace,
  writable,
  onDetach,
  compact = false,
  onOpen,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  writable: boolean;
  onDetach: (id: string) => void;
  compact?: boolean;
  onOpen?: () => void;
}) {
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });
  const network = useNetworkState(instance.desired.id);
  const [dialog, setDialog] = useState<{
    kind: 'mount' | 'routes' | 'publication' | 'transport' | 'version';
    nic?: string;
  } | null>(null);
  const host = workspace.hosts.find(host => host.node_id === instance.node_id);
  const nics = instance.desired.spec.interfaces;
  const full = nics.length >= (host?.capabilities.max_interfaces ?? 0);
  const selected = nics.find(nic => nic.id === dialog?.nic);
  const canMount =
    writable &&
    !!host?.fresh &&
    host.capabilities.managed_networks &&
    !full &&
    (instance.desired.power !== 'running' || !!host?.capabilities.hot_attach);
  const mountReason = !host?.fresh
    ? '宿主能力已过期，请等待机器恢复后挂载。'
    : full
      ? `当前宿主开放 ${host?.capabilities.max_interfaces ?? 0} 张网卡，设备槽位已用满。`
      : instance.desired.power === 'running' && !host?.capabilities.hot_attach
        ? '宿主需要先关机再挂载网卡。'
        : null;
  if (compact)
    return (
      <Section
        className="cvm-widget-7"
        title="网卡"
        aside={
          <button className="cvm-text-link" onClick={onOpen}>
            网卡与路由 →
          </button>
        }
      >
        <ul className="cvm-nic-summary">
          {nics.map(nic => {
            const allocated = nic.allocated;
            const observed = instance.report?.interfaces.find(item => item.id === nic.id);
            const publication = catalog.data?.publications.find(p => p.id === allocated?.publication_id);
            const usage = network.data?.usage[nic.id];
            return (
              <li key={nic.id}>
                <span>
                  <b>{allocated ? (publication?.spec.name ?? '业务网卡') : '管理主网卡'}</b>
                  <small>
                    {observed?.device ?? '待应用'} ·{' '}
                    {workspace.redacted
                      ? '地址已脱敏'
                      : allocated
                        ? [allocated.ipv4, allocated.ipv6].filter(Boolean).join(' / ') || '待分配地址'
                        : observed?.addresses.join(' / ') || '待确认地址'}
                  </small>
                </span>
                <span>
                  <span>
                    {usage
                      ? `${capacity(usage.uploaded_bytes + usage.downloaded_bytes)}${allocated?.limits.monthly_bytes ? ` / ${capacity(allocated.limits.monthly_bytes)}` : ' · 本月'}${usage.coverage_gap ? ' · 有缺口' : ''}`
                      : allocated
                        ? '等待计量'
                        : '宿主 NAT'}
                  </span>
                  {usage && allocated?.limits.monthly_bytes ? (
                    <CapacityMeter
                      used={usage.uploaded_bytes + usage.downloaded_bytes}
                      total={allocated.limits.monthly_bytes}
                    />
                  ) : null}
                </span>
              </li>
            );
          })}
        </ul>
        {network.error && <ErrorBox error={network.error} />}
        {catalog.error && <ErrorBox error={catalog.error} />}
      </Section>
    );
  return (
    <Section title="网卡与路由" className="cvm-network-workspace">
      {mountReason && <p className="note">{mountReason}</p>}
      {network.error && <ErrorBox error={network.error} />}
      {catalog.error && <ErrorBox error={catalog.error} />}
      {(network.isPending || catalog.isPending) && <Loading />}
      {!nics.some(nic => nic.allocated) && (
        <div className="cvm-network-empty">
          <p>尚未挂载业务网卡。可从其他机器提供的网络中选择，地址由系统统一分配。</p>
          {catalog.data && !catalog.data.publications.length && <p className="note">当前没有已授权的网络。</p>}
          {workspace.can_manage_network && catalog.data && (
            <button className="btn small" onClick={() => setDialog({ kind: 'publication' })}>
              创建网络
            </button>
          )}
        </div>
      )}
      <div className="cvm-attached-list">
        {nics.map(nic => {
          const allocated = nic.allocated;
          const usage = network.data?.usage[nic.id];
          const observed = instance.report?.interfaces.find(actual => actual.id === nic.id);
          const publication = catalog.data?.publications.find(p => p.id === allocated?.publication_id);
          const binding = network.data?.bindings.find(
            binding =>
              binding.interface_id === nic.id &&
              binding.phase === 'active' &&
              binding.transport === allocated?.transport,
          );
          const linked = configured(binding);
          const deviceReady =
            instance.desired.power === 'running' && instance.status === 'applied' && !!observed?.attached;
          const referenced =
            instance.desired.spec.routing.routes.some(route => route.interface_id === nic.id) ||
            instance.desired.spec.routing.priorities.some(priority => priority.interface_id === nic.id);
          const dependencyReason = referenced ? '请先修改路由表中对此网卡的引用' : undefined;
          const localBlock = binding?.consumer?.local_block ?? binding?.provider?.local_block;
          const blocked = allocated?.blocked || allocated?.quota_exceeded || !!localBlock;
          const status = !allocated
            ? '宿主机 NAT'
            : allocated.blocked
              ? linked && instance.status === 'applied'
                ? '已撤权阻断'
                : '正在阻断'
              : localBlock
                ? '业务已阻断'
                : allocated.quota_exceeded
                  ? '流量额度已用尽'
                  : instance.desired.power === 'stopped'
                    ? '关机保留分配'
                    : deviceReady && linked
                      ? '已应用'
                      : '配置中';
          return (
            <article className="cvm-attached" key={nic.id}>
              <header>
                <div>
                  <h3>{allocated ? (publication?.spec.name ?? '业务网卡') : '管理主网卡'}</h3>
                  <small>
                    {allocated
                      ? `${transportName[allocated.transport]} · ${addressModeName[allocated.address_mode]} · v${allocated.publication_revision}`
                      : '宿主机 NAT 出网 · 优先级最低 · 保留管理联络'}
                  </small>
                </div>
                <span
                  className={`st ${deviceReady && (!allocated || linked) && !blocked ? 'st-succeeded' : 'st-pending'}`}
                >
                  {status}
                </span>
              </header>
              <p className="note cvm-nic-device">
                {observed?.device || (instance.desired.power === 'stopped' ? '待开机应用' : '等待实际设备')}
              </p>
              <dl className="cvm-facts cvm-nic-facts">
                <div>
                  <dt>网卡地址</dt>
                  <dd>
                    {workspace.redacted
                      ? '已脱敏'
                      : allocated
                        ? [allocated.ipv4, allocated.ipv6]
                            .filter(Boolean)
                            .map(address => <code key={address}>{address}</code>)
                        : observed?.addresses.map(address => <code key={address}>{address}</code>) || '尚未确认'}
                  </dd>
                </div>
                {!allocated && (
                  <div>
                    <dt>对外地址</dt>
                    <dd>宿主 NAT</dd>
                  </div>
                )}
                {allocated && (
                  <>
                    <div>
                      <dt>对外地址</dt>
                      <dd>
                        {workspace.redacted
                          ? '已脱敏'
                          : [allocated.public_ipv4, allocated.public_ipv6].filter(Boolean).join(' / ') ||
                            '由提供机器自动选择出口地址'}
                      </dd>
                    </div>
                    <div>
                      <dt>本月（UTC）</dt>
                      <dd>
                        {usage
                          ? `${capacity(usage.uploaded_bytes + usage.downloaded_bytes)}${allocated.limits.monthly_bytes ? ` / ${capacity(allocated.limits.monthly_bytes)}` : ''}${usage.coverage_gap ? ' · 有计量缺口' : ''}`
                          : '尚未收到计量'}
                      </dd>
                    </div>
                    <div>
                      <dt>优先级</dt>
                      <dd>
                        {instance.desired.spec.routing.priorities.find(p => p.interface_id === nic.id)?.ipv4 ?? '—'} /{' '}
                        {instance.desired.spec.routing.priorities.find(p => p.interface_id === nic.id)?.ipv6 ?? '—'}{' '}
                        <small>IPv4 / IPv6</small>
                      </dd>
                    </div>
                  </>
                )}
              </dl>
              {allocated && (
                <>
                  <p className="note cvm-nic-link-state">
                    <span>
                      {connected(binding)
                        ? '两端已连接'
                        : binding && (!binding.consumer_fresh || !binding.provider_fresh)
                          ? '连接观测已过期'
                          : '尚未确认连通'}
                    </span>
                    {instance.desired.power === 'stopped' && ' · 关机期间保留地址分配'}
                  </p>
                  {network.data?.recent_errors?.some(error => error.interface_id === nic.id) && (
                    <details>
                      <summary>最近网络执行错误</summary>
                      {network.data.recent_errors
                        .filter(error => error.interface_id === nic.id)
                        .map(error => (
                          <p className="note" key={error.id}>
                            {transportName[error.transport]}：{error.error}
                          </p>
                        ))}
                    </details>
                  )}
                  {localBlock && (
                    <p className="cvm-notice" role="status">
                      {localBlock}
                    </p>
                  )}
                  {network.data?.bindings
                    .filter(binding => binding.interface_id === nic.id && binding.error)
                    .map(binding => (
                      <p className="cvm-notice error" key={binding.id} role="alert">
                        {binding.error}
                      </p>
                    ))}
                  {workspace.can_write && (
                    <footer>
                      <div className="cvm-connection-actions">
                        <button
                          className="btn small"
                          disabled={!writable || !network.data || allocated.blocked}
                          onClick={() => setDialog({ kind: 'transport', nic: nic.id })}
                        >
                          更换承载
                        </button>
                        {publication && publication.revision > allocated.publication_revision && (
                          <button
                            className="btn small"
                            disabled={!writable || allocated.blocked}
                            onClick={() => setDialog({ kind: 'version', nic: nic.id })}
                          >
                            应用新规格
                          </button>
                        )}
                      </div>
                      <button
                        className="btn small"
                        disabled={
                          !writable ||
                          !!dependencyReason ||
                          (instance.desired.power === 'running' && !host?.capabilities.hot_detach)
                        }
                        title={dependencyReason}
                        onClick={() => onDetach(nic.id)}
                      >
                        卸载
                      </button>
                    </footer>
                  )}
                  {dependencyReason && <p className="note">{dependencyReason}。</p>}
                </>
              )}
            </article>
          );
        })}
        {workspace.can_write && !full && (
          <button
            className="cvm-mount-slot"
            disabled={!canMount || !network.data || !catalog.data}
            title={mountReason ?? undefined}
            aria-label="＋ 挂载网卡"
            onClick={() => setDialog({ kind: 'mount' })}
          >
            <span aria-hidden="true">＋</span>
            <b>挂载网卡</b>
            <small>
              还可挂载 {Math.max(0, (host?.capabilities.max_interfaces ?? 0) - nics.length)} 张 · 宿主开放{' '}
              {host?.capabilities.max_interfaces ?? 0} 张
            </small>
          </button>
        )}
      </div>
      <Section
        title="路由表"
        className="cvm-route-summary"
        aside={
          workspace.can_write && (
            <button
              className="btn small"
              aria-label="路由表"
              disabled={!writable || !catalog.data}
              onClick={() => setDialog({ kind: 'routes' })}
            >
              编辑路由
            </button>
          )
        }
      >
        <table className="cvm-route-table">
          <thead>
            <tr>
              <th>目标</th>
              <th>经由</th>
              <th>优先级</th>
            </tr>
          </thead>
          <tbody>
            {instance.desired.spec.routing.routes.map((route, index) => (
              <tr key={index}>
                <td>
                  <code>{workspace.redacted ? '已脱敏' : routeLabel(route)}</code>
                </td>
                <td>
                  {nics.find(n => n.id === route.interface_id)?.allocated ? '业务网卡' : '管理主网卡'} ·{' '}
                  {instance.report?.interfaces.find(n => n.id === route.interface_id)?.device ?? '待应用'}
                </td>
                <td>
                  {instance.desired.spec.routing.priorities.find(p => p.interface_id === route.interface_id)?.[
                    route.destination.includes(':') ? 'ipv6' : 'ipv4'
                  ] ?? '—'}
                </td>
              </tr>
            ))}
            <tr>
              <td>未匹配业务路由</td>
              <td>管理主网卡 · 宿主 NAT</td>
              <td>最低</td>
            </tr>
          </tbody>
        </table>
        <p className="note">路由表独立配置。无匹配业务路由时通过主网卡 NAT 出网；选中的业务网卡故障时保持不可达。</p>
      </Section>
      {dialog?.kind === 'publication' && catalog.data && (
        <PublicationDialog
          workspace={workspace}
          catalog={catalog.data}
          publication={null}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === 'mount' && catalog.data && network.data && (
        <MountNicDialog
          instance={instance}
          catalog={catalog.data}
          state={network.data}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === 'routes' && catalog.data && (
        <RoutesDialog instance={instance} catalog={catalog.data} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === 'transport' && selected?.allocated && network.data && (
        <TransportDialog instance={instance} nic={selected} state={network.data} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === 'version' && selected?.allocated && catalog.data && (
        <NicVersionDialog instance={instance} nic={selected} catalog={catalog.data} onClose={() => setDialog(null)} />
      )}
    </Section>
  );
}

/** SSH entry points: a TCP entry to port 22 or a whole-IP entry, then the host-internal address. */
export function ContainerAccessPanel({ instance, redacted }: { instance: CvmInstance; redacted: boolean }) {
  const entries = useQuery({
    queryKey: ['cvm-instance-entries', instance.desired.id],
    queryFn: () => fetchCvmInstanceEntries(instance.desired.id),
    enabled: !redacted,
    refetchInterval: 30_000,
  });
  const [selection, setSelection] = useState('');
  if (redacted) return <p className="note">连接地址已脱敏。</p>;
  const addresses: { key: string; address: string; port: number; label: string }[] = [];
  for (const entry of entries.data ?? []) {
    if (entry.state !== 'active') continue;
    if (entry.mode.kind === 'l3')
      addresses.push({ key: entry.id, address: entry.visible_address, port: 22, label: '整 IP 入口' });
    else if (entry.mode.target_port === 22 && entry.mode.protocol !== 'udp')
      addresses.push({
        key: entry.id,
        address: entry.visible_address,
        port: entry.mode.listen_port,
        label: `${entryProtocolText(entry.mode.protocol)} 入口`,
      });
  }
  const privateAddress = instance.report?.access_address;
  if (privateAddress && instance.desired.power === 'running' && instance.status === 'applied')
    addresses.push({ key: 'contact', address: privateAddress, port: 22, label: '宿主内管理地址' });
  const selected = addresses.find(address => address.key === selection) ?? addresses[0];
  if (!selected)
    return (
      <>
        <p>可通过实例终端连接。需要外部 SSH 时，请在组合台为容器添加指向 22 端口的访问入口。</p>
        {entries.error && <ErrorBox error={entries.error} />}
      </>
    );
  const command = `ssh -l cube -p ${selected.port} -- ${selected.address}`;
  return (
    <section className="cvm-ssh-login" aria-label="SSH 登录">
      <div className="cvm-network-heading">
        <b>SSH 登录</b>
        <CopyButton className="btn small" text={command} label="复制 SSH 命令" />
      </div>
      <code className="cvm-ssh-command">{command}</code>
      <p className="note">
        {selected.label}
        {selected.key === 'contact' ? '，在宿主上执行。' : ''}
      </p>
      {addresses.length > 1 && (
        <label className="cvm-field">
          切换地址
          <select className="f" value={selected.key} onChange={e => setSelection(e.target.value)}>
            {addresses.map(address => (
              <option key={address.key} value={address.key}>
                {address.label} · {address.address}:{address.port}
              </option>
            ))}
          </select>
        </label>
      )}
    </section>
  );
}
