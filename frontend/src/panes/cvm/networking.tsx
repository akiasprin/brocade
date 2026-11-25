import { entrySshPort, entryTrafficText } from './entry-ports';
import { useQuery } from '@tanstack/react-query';
import { useId, useState } from 'react';
import {
  fetchCvmInstanceEntries,
  fetchCvmNicCatalog,
  fetchCvmNicState,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmNicState,
  type CvmNicTransport,
  type CvmWorkspace,
} from '../../api';
import { navigate } from '../../forge/route';
import { ErrorBox, Loading, SegmentedControl } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { ActionMenu } from '../../ui/choice-menu';
import { Icon, PanelTitle } from '../../ui/icons';
import { capacity, hasPendingCvmChange, requestId } from './queries';
import { CapacityMeter } from './cards';
import { NIC_CATALOG, NIC_STATE, addressModeName, transportName } from './network-common';
import { NicVersionDialog, TransportDialog } from './network-dialogs';
import { NetworkLogsButton } from './network-logs';
import { NetworkEntries } from './network-entries';
import type { NetworkDraftController, NetworkEdits } from './network-draft';
import { RouteEditor } from './route-editor';
import { targetKey } from './route-target';
import { networkProviderName } from './network-names';
import './network-edit.css';

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

const EMPTY_EDITS: NetworkEdits = {};

function MountNicEditor({
  instance,
  catalog,
  state,
  onAdd,
  onClose,
}: {
  instance: CvmInstance;
  catalog: CvmNicCatalog;
  state: CvmNicState;
  onAdd: (mount: NonNullable<NetworkEdits['mounts']>[number]) => void;
  onClose: () => void;
}) {
  const id = useId();
  const [publication, setPublication] = useState('');
  const [transport, setTransport] = useState<CvmNicTransport>('wireguard');
  const [useDns, setUseDns] = useState(false);
  const selected = catalog.publications.find(item => item.id === publication);
  const option = state.mount_options[publication];
  const valid = !!selected && !!option?.transports.includes(transport);
  return (
    <form
      className="cvm-composer"
      aria-label="挂载网卡"
      onSubmit={event => {
        event.preventDefault();
        if (valid)
          onAdd({
            draft_id: `draft-${requestId()}`,
            publication_id: publication,
            transport,
            use_dns_suggestions: useDns,
          });
      }}
    >
      <div className="cvm-composer-head">
        <b>挂载出口网卡</b>
        <span className="note">分配独立网卡与地址，承载取双方支持的交集</span>
        <button className="btn icon cvm-row-btn" type="button" aria-label="收起挂载网卡" title="收起" onClick={onClose}>
          <Icon of="close" size={13} />
        </button>
      </div>
      {catalog.publications.length ? (
        <div className="cvm-picks" role="radiogroup" aria-label="网络">
          {catalog.publications.map(item => {
            const transports = state.mount_options[item.id]?.transports ?? [];
            return (
              <label className="cvm-pick-row" data-on={publication === item.id || undefined} key={item.id}>
                <input
                  type="radio"
                  name={`${id}-nic-publication`}
                  value={item.id}
                  checked={publication === item.id}
                  disabled={!transports.length}
                  onChange={() => {
                    setPublication(item.id);
                    setTransport(transports[0] ?? 'wireguard');
                    setUseDns(false);
                  }}
                />
                <span className="cvm-pick-main">
                  <span>
                    <b>{item.spec.name}</b> <span className="cvm-faint">v{item.revision}</span>
                  </span>
                  <small>
                    {transports.length
                      ? `默认路由 ${item.spec.route_presets.join('、') || '无'}`
                      : (state.mount_options[item.id]?.error ?? '当前不可挂载')}
                  </small>
                </span>
              </label>
            );
          })}
        </div>
      ) : (
        <p className="note">当前使用者没有获授权的出口网络。</p>
      )}
      {option?.error && (
        <p className="cvm-notice" role="status">
          {option.error}
        </p>
      )}
      {selected && (
        <div className="cvm-composer-fields">
          <span className="cvm-composer-label">承载</span>
          <SegmentedControl
            ariaLabel="连接方式"
            value={transport}
            options={option?.transports.map(kind => ({ value: kind, label: transportName[kind] })) ?? []}
            onChange={setTransport}
          />
          {!!selected.spec.dns_suggestions.length && instance.desired.spec.execution.kind === 'cube' && (
            <label className="cvm-check">
              <input type="checkbox" checked={useDns} onChange={event => setUseDns(event.target.checked)} />
              使用网络建议的 DNS：{selected.spec.dns_suggestions.join('、')}
            </label>
          )}
        </div>
      )}
      <div className="cvm-composer-foot">
        <span className="note">加入未保存变更，与其他网络修改一起检查和保存。</span>
        <button className="btn sm" type="button" onClick={onClose}>
          取消
        </button>
        <button className={`btn sm${valid ? ' primary' : ''}`} type="submit" disabled={!valid}>
          加入变更
        </button>
      </div>
    </form>
  );
}

export function ManagedNetworking({
  instance,
  workspace,
  draft,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  draft: NetworkDraftController;
}) {
  const host = workspace.hosts.find(host => host.node_id === instance.node_id);
  const canEdit =
    workspace.can_write &&
    !workspace.redacted &&
    instance.reserved &&
    instance.desired.power !== 'deleted' &&
    !instance.blocking_deployment_id;
  const initial =
    instance.operation_kind === 'create' &&
    hasPendingCvmChange(instance) &&
    !instance.report &&
    !instance.published_revision_id;
  const applied = instance.status === 'applied' && !hasPendingCvmChange(instance);
  const writable = canEdit && !draft.busy && !draft.stale && !draft.draft?.uncertain;
  const ready = writable && !!host?.fresh && (applied || initial);
  const routesWritable =
    ready ||
    (writable &&
      !!host?.fresh &&
      !hasPendingCvmChange(instance) &&
      (instance.status === 'failed' || instance.status === 'unknown'));
  const catalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: !workspace.redacted,
    refetchInterval: 15_000,
  });
  const network = useNetworkState(instance.desired.id);
  const [dialog, setDialog] = useState<{ kind: 'transport' | 'version'; nic: string } | null>(null);
  const [mountOpen, setMountOpen] = useState(false);
  const nics = instance.desired.spec.interfaces;
  const selected = nics.find(nic => nic.id === dialog?.nic);
  const edits = draft.draft?.request ?? EMPTY_EDITS;
  const mounts = edits.mounts ?? [];
  const detach = edits.detach_interface_ids ?? [];
  const routing = edits.routes?.routes ?? instance.desired.spec.routing;
  const baseRouting = draft.draft?.instance.desired.spec.routing ?? instance.desired.spec.routing;
  const changeRouting = (next: typeof routing, changes: NetworkEdits = edits) =>
    draft.change({
      ...changes,
      routes:
        JSON.stringify(baseRouting) === JSON.stringify(next)
          ? undefined
          : { expected_revision: baseRouting.revision, routes: next },
    });
  const withoutTarget = (target: string) => ({
    ...routing,
    routes: routing.routes.filter(route => targetKey(route) !== target),
    priorities: routing.priorities.filter(priority => targetKey(priority) !== target),
  });
  const slots = host?.capabilities.max_interfaces ?? 0;
  const used = nics.length + mounts.length - detach.length;
  const needsStop = !initial && instance.desired.power === 'running' && !host?.capabilities.hot_attach;
  const canMount = ready && !!host?.capabilities.managed_networks && used < slots && !needsStop;
  const mountReason = !canEdit
    ? null
    : !host?.fresh
      ? '宿主能力已过期，等待机器恢复后可挂载。'
      : used >= slots
        ? `当前宿主开放 ${slots} 张网卡，设备槽位已用满。`
        : needsStop
          ? '宿主需要先关机再挂载网卡。'
          : null;
  const publicationFor = (id: string) => catalog.data?.publications.find(item => item.id === id);
  const logAction = (
    <NetworkLogsButton
      label="配置日志"
      title="实例网络配置日志"
      allowed={workspace.can_manage_network}
      machines={[
        { node_id: instance.node_id, role: '计算' },
        ...nics.flatMap(nic => (nic.allocated ? [{ node_id: nic.allocated.provider_node, role: '出口' }] : [])),
      ].map(machine => ({ ...machine, name: workspace.hosts.find(h => h.node_id === machine.node_id)?.name }))}
      resources={[instance.desired.id, ...(network.data?.bindings.map(binding => binding.id) ?? [])]}
    />
  );
  const nicName = (nic: (typeof nics)[number]) =>
    nic.allocated
      ? (catalog.data?.publications.find(p => p.id === nic.allocated!.publication_id)?.spec.name ?? '业务网卡')
      : instance.desired.spec.execution.kind === 'baremetal'
        ? '宿主网络'
        : '管理主网卡';
  const free = Math.max(0, slots - used);
  return (
    <div className="cvm-network-edit">
      <div className="cvm-net-top">
        <NetworkEntries
          instance={instance}
          workspace={workspace}
          providerName={node => networkProviderName(catalog.data, node, workspace.hosts)}
          edits={edits}
          onChange={draft.change}
          writable={writable}
        />
        <section className="panel titled cvm-net-panel" aria-label="网卡">
          <header>
            <PanelTitle of="client">网卡</PanelTitle>
            <span className="hint">
              {used} / {slots || '—'} 张
            </span>
            <span className="cvm-panel-tools">
              {logAction}
              {canEdit && (
                <button
                  className="btn sm"
                  disabled={!canMount || !catalog.data || !network.data || mountOpen}
                  title={mountReason ?? undefined}
                  type="button"
                  aria-expanded={mountOpen}
                  onClick={() => setMountOpen(true)}
                >
                  <Icon of="plus" size={13} />
                  挂载网卡
                </button>
              )}
            </span>
          </header>
          {network.error && <ErrorBox error={network.error} />}
          {catalog.error && <ErrorBox error={catalog.error} />}
          {(network.isPending || (!workspace.redacted && catalog.isPending)) && <Loading />}
          <ul className="cvm-nrows" aria-label="网卡列表">
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
              const removing = detach.includes(nic.id);
              const referenced =
                routing.routes.some(route => targetKey(route) === nic.id) ||
                routing.priorities.some(priority => targetKey(priority) === nic.id);
              const localBlock = binding?.consumer?.local_block ?? binding?.provider?.local_block;
              const blocked = allocated?.blocked || allocated?.quota_exceeded || !!localBlock;
              const status = !allocated
                ? null
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
              const link = !allocated
                ? null
                : connected(binding)
                  ? '两端已连接'
                  : binding && (!binding.consumer_fresh || !binding.provider_fresh)
                    ? '连接观测已过期'
                    : '尚未确认连通';
              const addresses = workspace.redacted
                ? '已脱敏'
                : allocated
                  ? [allocated.ipv4, allocated.ipv6].filter(Boolean).join(' · ')
                  : observed?.addresses.join(' · ') || '尚未确认地址';
              const monthly =
                allocated && usage
                  ? `本月 ${capacity(usage.uploaded_bytes + usage.downloaded_bytes)}${allocated.limits.monthly_bytes ? ` / ${capacity(allocated.limits.monthly_bytes)}` : ''}（UTC 月）${usage.coverage_gap ? ' · 有计量缺口' : ''}`
                  : allocated
                    ? '尚未收到计量'
                    : null;
              const detachBlocked =
                !ready ||
                !applied ||
                (!removing && referenced) ||
                (instance.desired.power === 'running' && !host?.capabilities.hot_detach);
              const newer = publication && allocated && publication.revision > allocated.publication_revision;
              return (
                <li className="cvm-nrow" key={nic.id} data-removing={removing || undefined}>
                  <span className="cvm-nrow-dev">
                    {observed?.device || (instance.desired.power === 'stopped' ? '待开机' : '待确认')}
                  </span>
                  <div className="cvm-nrow-main">
                    <span className="cvm-nrow-title">
                      {nicName(nic)}
                      <span className="cvm-faint cvm-nrow-sub">
                        {allocated
                          ? `${transportName[allocated.transport]} · ${addressModeName[allocated.address_mode]} · v${allocated.publication_revision}`
                          : instance.desired.spec.execution.kind === 'baremetal'
                            ? '宿主原有网络'
                            : '宿主 NAT'}
                      </span>
                      {newer && <span className="st st-gold">可应用 v{publication!.revision}</span>}
                    </span>
                    <small className="mono" title={allocated ? '网卡地址与本月计量' : undefined}>
                      {[
                        addresses,
                        allocated && !workspace.redacted
                          ? `对外 ${[allocated.public_ipv4, allocated.public_ipv6].filter(Boolean).join(' / ') || '自动选择'}`
                          : null,
                        monthly,
                        allocated && !workspace.redacted
                          ? networkProviderName(catalog.data, allocated.provider_node, workspace.hosts)
                          : null,
                        !allocated && instance.desired.spec.execution.kind !== 'baremetal'
                          ? '优先级最低 · 保留管理联络'
                          : null,
                      ]
                        .filter(Boolean)
                        .join(' · ')}
                    </small>
                    {allocated && usage && allocated.limits.monthly_bytes ? (
                      <CapacityMeter
                        used={usage.uploaded_bytes + usage.downloaded_bytes}
                        total={allocated.limits.monthly_bytes}
                      />
                    ) : null}
                    {network.data?.recent_errors?.some(error => error.interface_id === nic.id) && (
                      <details className="cvm-nrow-details">
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
                  </div>
                  {allocated ? (
                    <span
                      className={`st ${removing ? 'st-pending' : deviceReady && linked && !blocked ? 'st-succeeded' : blocked ? 'st-halted' : 'st-pending'}`}
                      title={link ?? undefined}
                    >
                      {removing ? '待卸载' : status}
                    </span>
                  ) : (
                    <span className="st">自动</span>
                  )}
                  {allocated && workspace.can_write && (
                    <span className="cvm-nrow-acts">
                      <ActionMenu
                        label={`${nicName(nic)} 网卡操作`}
                        items={[
                          {
                            label: '更换连接方式',
                            note: `当前 ${transportName[allocated.transport]}；切换时短暂中断`,
                            disabled:
                              !ready || !applied || !!draft.draft || !network.data || allocated.blocked
                                ? '保存或丢弃当前草稿、等待配置收敛后可更换'
                                : false,
                            onSelect: () => setDialog({ kind: 'transport', nic: nic.id }),
                          },
                          ...(newer
                            ? [
                                {
                                  label: `应用新规格 v${publication!.revision}`,
                                  note: `当前 v${allocated.publication_revision}`,
                                  disabled:
                                    !ready || !applied || !!draft.draft || allocated.blocked
                                      ? '保存或丢弃当前草稿、等待配置收敛后可应用'
                                      : false,
                                  onSelect: () => setDialog({ kind: 'version', nic: nic.id }),
                                },
                              ]
                            : []),
                          {
                            label: removing ? '取消卸载' : '卸载网卡',
                            note: removing ? '保留这张网卡' : '释放地址与分配，保存后待发布',
                            danger: !removing,
                            disabled: detachBlocked
                              ? !removing && referenced
                                ? '请先在路由中移除此网卡的引用'
                                : instance.desired.power === 'running' && !host?.capabilities.hot_detach
                                  ? '宿主需要先关机再卸载网卡'
                                  : '等待配置收敛后可卸载'
                              : false,
                            onSelect: () =>
                              draft.change({
                                ...edits,
                                detach_interface_ids: removing
                                  ? detach.filter(id => id !== nic.id)
                                  : [...detach, nic.id],
                              }),
                          },
                        ]}
                      />
                    </span>
                  )}
                </li>
              );
            })}
            {mounts.map(mount => (
              <li className="cvm-nrow" data-draft="" key={mount.draft_id}>
                <span className="cvm-nrow-dev">新网卡</span>
                <div className="cvm-nrow-main">
                  <span className="cvm-nrow-title">
                    {publicationFor(mount.publication_id)?.spec.name ?? mount.publication_id}
                    <span className="cvm-faint cvm-nrow-sub">{transportName[mount.transport]}</span>
                  </span>
                  <small>保存并发布后分配地址</small>
                </div>
                <span className="st st-gold">未保存</span>
                {ready && (
                  <span className="cvm-nrow-acts">
                    <button
                      className="btn sm cvm-row-btn"
                      type="button"
                      onClick={() =>
                        changeRouting(withoutTarget(mount.draft_id), {
                          ...edits,
                          mounts: mounts.filter(other => other.draft_id !== mount.draft_id),
                        })
                      }
                    >
                      移除草稿
                    </button>
                  </span>
                )}
              </li>
            ))}
            {canEdit && !mountOpen && (
              <li className="cvm-nrow cvm-nrow-hint">
                {mountReason ?? `还可挂载 ${free} 张业务网卡 · 宿主开放 ${slots} 张`}
                {workspace.can_manage_network && catalog.data?.publications.length === 0 && (
                  <button
                    className="btn sm cvm-row-btn"
                    type="button"
                    onClick={() => navigate('cvm', { p: 'network-new' })}
                  >
                    创建出口网络
                  </button>
                )}
              </li>
            )}
          </ul>
          {mountOpen && canMount && catalog.data && network.data && (
            <MountNicEditor
              instance={instance}
              catalog={catalog.data}
              state={network.data}
              onClose={() => setMountOpen(false)}
              onAdd={mount => {
                draft.change({ ...edits, mounts: [...mounts, mount] });
                setMountOpen(false);
              }}
            />
          )}
        </section>
      </div>
      <RouteEditor
        instance={
          detach.length
            ? {
                ...instance,
                desired: {
                  ...instance.desired,
                  spec: { ...instance.desired.spec, interfaces: nics.filter(nic => !detach.includes(nic.id)) },
                },
              }
            : instance
        }
        catalog={catalog.data}
        routing={routing}
        onChange={changeRouting}
        writable={routesWritable}
        canTest={canEdit}
        networkEdit={draft.draft?.request}
        redacted={workspace.redacted}
        extraTargets={mounts.map(mount => {
          const publication = publicationFor(mount.publication_id);
          return {
            key: mount.draft_id,
            name: publication?.spec.name ?? mount.publication_id,
            ipv4: !!publication?.spec.ipv4_pool,
            ipv6: !!publication?.spec.ipv6_pool,
            available: true,
          };
        })}
      />
      {dialog?.kind === 'transport' && selected?.allocated && network.data && (
        <TransportDialog instance={instance} nic={selected} state={network.data} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === 'version' && selected?.allocated && catalog.data && (
        <NicVersionDialog instance={instance} nic={selected} catalog={catalog.data} onClose={() => setDialog(null)} />
      )}
    </div>
  );
}

/** SSH entry points: a TCP entry to port 22 or a whole-IP entry, then the host-internal address. */
export function ContainerAccessPanel({
  instance,
  redacted,
  loginUser,
}: {
  instance: CvmInstance;
  redacted: boolean;
  loginUser?: string;
}) {
  const entries = useQuery({
    queryKey: ['cvm-instance-entries', instance.desired.id],
    queryFn: () => fetchCvmInstanceEntries(instance.desired.id),
    enabled: !redacted,
    refetchInterval: 30_000,
  });
  const [selection, setSelection] = useState('');
  if (redacted) return <p className="note cvm-ssh-empty">连接地址已脱敏。</p>;
  const addresses: { key: string; address: string; port: number; label: string }[] = [];
  for (const entry of entries.data ?? []) {
    if (entry.state !== 'active') continue;
    const port = entrySshPort(entry.mode, entry.reserved_ports);
    if (port !== null)
      addresses.push({
        key: entry.id,
        address: entry.visible_address,
        port,
        label:
          entry.mode.kind === 'l3'
            ? '整 IP 入口'
            : `${entry.mode.kind === 'protocols' ? entryTrafficText(entry.mode) : entry.mode.protocol.toUpperCase().replace('_', '+')} 入口`,
      });
  }
  const privateAddress = instance.report?.access_address;
  if (privateAddress && instance.desired.power === 'running' && instance.status === 'applied')
    addresses.push({ key: 'contact', address: privateAddress, port: 22, label: '宿主内管理地址' });
  const selected = addresses.find(address => address.key === selection) ?? addresses[0];
  if (!selected)
    return (
      <>
        <p className="note cvm-ssh-empty">
          暂无可用的 SSH 地址，可通过实例终端连接；需要外部 SSH 时，在「网络」为容器添加指向 22 端口的访问入口。
        </p>
        {entries.error && <ErrorBox error={entries.error} />}
      </>
    );
  const user = instance.desired.spec.execution.kind === 'baremetal' ? loginUser : 'cube';
  if (!user || !/^[a-zA-Z0-9_-]{1,32}$/.test(user))
    return <p className="note cvm-ssh-empty">宿主登录账号尚未确认，可通过实例终端连接。</p>;
  const command = `ssh -l ${user} -p ${selected.port} -- ${selected.address}`;
  return (
    <section className="cvm-ssh" aria-label="SSH 登录">
      <code className="cvm-ssh-command">{command}</code>
      <span className="cvm-ssh-route">
        {addresses.length > 1 ? (
          // 原生下拉叠在文字标签上：可见的只是「经 TCP 入口」，选项里才写完整地址。
          <label className="cvm-ssh-switch" title="切换 SSH 地址">
            <span aria-hidden="true">
              经{/^[A-Za-z]/.test(selected.label) ? ' ' : ''}
              {selected.label}
              <Icon of="chevronDown" size={11} />
            </span>
            <select
              aria-label="切换 SSH 地址"
              value={selected.key}
              onChange={event => setSelection(event.target.value)}
            >
              {addresses.map(address => (
                <option key={address.key} value={address.key}>
                  {address.label} · {address.address}:{address.port}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <span className="note">
            经{/^[A-Za-z]/.test(selected.label) ? ' ' : ''}
            {selected.label}
            {selected.key === 'contact' ? '，在宿主上执行' : ''}
          </span>
        )}
        <CopyButton className="btn small" text={command} label="复制" />
      </span>
    </section>
  );
}
