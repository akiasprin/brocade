// 实例详情顶部的小组合台：访问入口 → 网卡 → 出口，与组合台使用同一套插座、连线和标记。
// 入口投递到主网卡；每张业务网卡连到它所属的出口网络，并列出经由它的路由；未匹配业务路由的
// 流量走主网卡（宿主 NAT 或裸机原有网络）。概览只读，「网络」页签可撤销入口、挂载网卡和编辑路由。
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { type ReactNode } from 'react';
import {
  entryStateText,
  fetchCvmInstanceEntries,
  fetchCvmNicCatalog,
  fetchCvmChainCatalog,
  fetchCvmNicState,
  releaseCvmEntry,
  type CvmEntry,
  type CvmInstance,
  type CvmInterface,
  type CvmWorkspace,
} from '../../api';
import { RegionFlag } from '../../ui/region-flag';
import { navigate } from '../../forge/route';
import { ErrorBox, Loading } from '../../ui/bits';
import { ENTRY_BOARD } from './board/inspector';
import { entryText, routeLabel } from './board/model';
import { curve, useAnchorGeometry } from './board/wires';
import { CapacityMeter } from './cards';
import { NIC_CATALOG, NIC_STATE, addressModeName, transportName, useNicRequestId } from './network-common';
import { networkProviderName } from './network-names';
import { NetworkLogsButton } from './network-logs';
import { capacity } from './queries';

const short = (bytes: number) => capacity(bytes).replace(' MiB', 'M').replace(' GiB', 'G');

export function InstanceFlow({
  instance,
  workspace,
  editable = false,
  mountSlot,
  routesAction,
  logsAction,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  /** 「网络」页签：显示入口状态与撤销、挂载空位和编辑路由。 */
  editable?: boolean;
  mountSlot?: ReactNode;
  routesAction?: ReactNode;
  logsAction?: ReactNode;
}) {
  const client = useQueryClient();
  const id = instance.desired.id;
  const entryKey = ['cvm-instance-entries', id];
  const entries = useQuery({
    queryKey: entryKey,
    queryFn: () => fetchCvmInstanceEntries(id),
    refetchInterval: 15_000,
  });
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });
  const network = useQuery({
    queryKey: [...NIC_STATE, id],
    queryFn: () => fetchCvmNicState(id),
    refetchInterval: 5000,
  });
  const chains = useQuery({
    queryKey: ['cvm-chain-egress', id],
    queryFn: () => fetchCvmChainCatalog(id),
    refetchInterval: 15_000,
  });
  const chainRoutes = new Map<string, typeof instance.desired.spec.routing.routes>();
  for (const r of instance.desired.spec.routing.routes)
    if (r.chain_id) chainRoutes.set(r.chain_id, [...(chainRoutes.get(r.chain_id) ?? []), r]);
  const operationId = useNicRequestId();
  const release = useMutation({
    mutationFn: (entry: CvmEntry) => {
      const body = { expected_revision: entry.revision };
      return releaseCvmEntry(entry.id, { ...body, request_id: operationId({ release: entry.id, ...body }) });
    },
    onSettled: () => {
      void client.invalidateQueries({ queryKey: entryKey });
      void client.invalidateQueries({ queryKey: ENTRY_BOARD });
    },
  });
  const { root, geo } = useAnchorGeometry<HTMLElement>();
  const spec = instance.desired.spec;
  const native = spec.execution?.kind === 'baremetal';
  const host = workspace.hosts.find(item => item.node_id === instance.node_id);
  const hidden = workspace.redacted;
  const nameOf = (node: string) => networkProviderName(catalog.data, node, workspace.hosts);
  // 业务网卡在上、主网卡在下：主网卡承接入口，也是未匹配路由的兜底出口。
  const nics = [...spec.interfaces].sort((a, b) => Number(!a.allocated) - Number(!b.allocated));
  const contact = nics.find(nic => !nic.allocated);
  const observed = (nic: CvmInterface) => instance.report?.interfaces.find(item => item.id === nic.id);
  const publicationOf = (nic: CvmInterface) =>
    catalog.data?.publications.find(item => item.id === nic.allocated?.publication_id);
  const routesVia = (nic: CvmInterface) =>
    spec.routing.routes.map((route, index) => ({ route, index })).filter(({ route }) => route.interface_id === nic.id);
  const priority = (nic: CvmInterface, destination: string) =>
    spec.routing.priorities.find(item => item.interface_id === nic.id)?.[destination.includes(':') ? 'ipv6' : 'ipv4'];
  const list = entries.data ?? [];
  const waiting = list.filter(entry => entry.state !== 'active').length;

  const nicAddress = (nic: CvmInterface) =>
    hidden
      ? '已脱敏'
      : nic.allocated
        ? [nic.allocated.ipv4, nic.allocated.ipv6].filter(Boolean).join(' · ') || '待分配地址'
        : observed(nic)?.addresses.join(' · ') || '待确认地址';

  return (
    <section className={`cvm-flow${editable ? ' editable' : ''}`} ref={root} aria-label="入口、网卡与出口">
      <svg className="cz-wires" width={geo.width} height={geo.height} aria-hidden="true">
        {contact &&
          list.map(entry => {
            const from = geo.anchors[`entry:${entry.id}`];
            const to = geo.anchors['nic-in'];
            return from && to ? (
              <g key={entry.id} className={`cz-wire in ${entryText(entry).layer} on ${entry.state}`}>
                <path d={curve(from, to)} />
              </g>
            ) : null;
          })}
        {nics.map(nic => {
          const from = geo.anchors[`nic-out:${nic.id}`];
          const to = geo.anchors[`exit:${nic.id}`];
          const routes = routesVia(nic);
          const layer = routes.length && routes.every(({ route }) => route.service) ? 'L4' : 'L3';
          const used = !nic.allocated || routes.length > 0;
          return from && to ? (
            <g key={nic.id} className={`cz-wire out ${layer} ${nic.allocated ? '' : 'private'} ${used ? 'on' : 'off'}`}>
              <path d={curve(from, to)} />
            </g>
          ) : null;
        })}
        {contact &&
          [...chainRoutes.keys()].map(chain => {
            const from = geo.anchors[`nic-out:${contact.id}`];
            const to = geo.anchors[`chain:${chain}`];
            return from && to ? (
              <g key={chain} className="cz-wire out L4 on">
                <path d={curve(from, to)} />
              </g>
            ) : null;
          })}
      </svg>

      {/* ── 访问入口 ── */}
      <div className="cvm-flow-col in">
        <header className="cvm-lane-head">
          <h2>访问入口</h2>
          <small>
            {list.length} 个 · 投递到主网卡
            {waiting > 0 && !editable ? ` · ${waiting} 个未生效` : ''}
          </small>
          {workspace.can_write && (
            <span className="cvm-lane-aside">
              <button className="cvm-text-link" onClick={() => navigate('cvm', {})}>
                {editable ? '在组合台新增 →' : '在组合台中查看 →'}
              </button>
            </span>
          )}
        </header>
        <div className="cvm-tile cvm-flow-list">
          {entries.isPending ? (
            <Loading />
          ) : !entries.data ? (
            <ErrorBox error={entries.error} />
          ) : list.length ? (
            <ul aria-label="访问入口">
              {list.map(entry => {
                const text = entryText(entry);
                return (
                  <li key={entry.id} className={`cvm-flow-entry ${entry.state}`}>
                    <span className={`cz-layer ${text.layer}`}>{text.badge}</span>
                    <code title={hidden ? undefined : text.text}>{hidden ? '已脱敏' : text.text}</code>
                    {editable ? (
                      <span className={`cz-state ${entry.state}`} title={entry.error ?? undefined}>
                        {entryStateText[entry.state]}
                      </span>
                    ) : (
                      <span className="cvm-flow-target">
                        {entry.state !== 'active' && (
                          <i
                            className={`cz-state-dot ${entry.state}`}
                            role="img"
                            aria-label={entryStateText[entry.state]}
                          />
                        )}
                        {text.target}
                      </span>
                    )}
                    {editable && (
                      <span className="cvm-flow-sub">
                        <span>
                          {text.target} · {nameOf(entry.provider_node)}
                        </span>
                        <span className="cvm-flow-acts">
                          <NetworkLogsButton
                            iconOnly
                            title="入口网络日志"
                            allowed={entry.can_read_logs ?? workspace.can_manage_network}
                            machines={[
                              { node_id: entry.provider_node, name: nameOf(entry.provider_node), role: '入口' },
                              { node_id: instance.node_id, name: host?.name, role: '计算' },
                            ]}
                            resources={[entry.id, ...(entry.tunnel_id ? [entry.tunnel_id] : [])]}
                          />
                          {workspace.can_write && entry.state !== 'releasing' && (
                            <button
                              className="btn small"
                              disabled={release.isPending}
                              onClick={() => release.mutate(entry)}
                            >
                              撤销
                            </button>
                          )}
                        </span>
                      </span>
                    )}
                    {editable && entry.error && <p className="cvm-notice error">{entry.error}</p>}
                    <span className="cz-socket out idle" data-anchor={`entry:${entry.id}`} />
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="note cvm-flow-empty">
              没有访问入口。入口投递到主网卡，不占网卡槽位；在组合台从左侧地址连到这个容器即可接入。
            </p>
          )}
          {release.error && <ErrorBox error={release.error} />}
        </div>
      </div>

      {/* ── 容器与网卡 ── */}
      <div className="cvm-flow-col mid">
        <header className="cvm-lane-head">
          <h2>{editable ? '网卡' : '容器'}</h2>
          <small>
            {nics.length} / {host?.capabilities.max_interfaces ?? nics.length} 张网卡
          </small>
          {logsAction && <span className="cvm-lane-aside">{logsAction}</span>}
        </header>
        <div className="cvm-tile cvm-flow-box">
          <div className="cvm-flow-box-head">
            <span className={`cz-dot ${instance.status} ${instance.desired.power}`} />
            <b title={spec.name}>{spec.name}</b>
            <small>
              {spec.resources.vcpus}C · {short(spec.resources.memory_bytes)} · {short(spec.resources.root_disk_bytes)}
            </small>
          </div>
          <p className="cvm-flow-box-sub">
            {host?.name ?? instance.node_id} ·{' '}
            {spec.image ? `${spec.image.name} ${spec.image.version} ${spec.image.architecture}` : '裸机'} ·{' '}
            {instance.user_id}
          </p>
          <ul className="cvm-flow-nics" aria-label="网卡">
            {nics.map(nic => {
              const publication = publicationOf(nic);
              return (
                <li key={nic.id} className="cvm-flow-nic">
                  {nic === contact && <span className="cz-port in idle" data-anchor="nic-in" />}
                  <span className="cvm-flow-dev">{observed(nic)?.device || '待应用'}</span>
                  <span className="cvm-flow-nic-name">
                    {nic.allocated ? (publication?.spec.name ?? '业务网卡') : '管理主网卡'}
                    <small>
                      {nic.allocated
                        ? `${transportName[nic.allocated.transport]} · v${nic.allocated.publication_revision}`
                        : native
                          ? '宿主原有网络 · 入口投递'
                          : '宿主 NAT · 入口投递'}
                    </small>
                  </span>
                  <code className="cvm-flow-ip">{nicAddress(nic)}</code>
                  <span className="cz-port out idle" data-anchor={`nic-out:${nic.id}`} />
                </li>
              );
            })}
            {mountSlot}
          </ul>
        </div>
      </div>

      {/* ── 出口：按网卡分组，主网卡兜底 ── */}
      <div className="cvm-flow-col out">
        <header className="cvm-lane-head">
          <h2>{editable ? '路由' : '出口'}</h2>
          <small>{spec.routing.routes.length} 条业务路由 · 按优先级</small>
          {routesAction && <span className="cvm-lane-aside">{routesAction}</span>}
        </header>
        {nics.map(nic => {
          const publication = publicationOf(nic);
          const routes = routesVia(nic);
          const usage = network.data?.usage[nic.id];
          const allocated = nic.allocated;
          return (
            <div className={`cvm-tile cvm-flow-exit${allocated ? '' : ' local'}`} key={nic.id}>
              <span className="cz-socket idle" data-anchor={`exit:${nic.id}`} />
              <div className="cvm-flow-exit-head">
                <b>{allocated ? (publication?.spec.name ?? '业务网卡') : `本机 · ${host?.name ?? instance.node_id}`}</b>
                <span className="cz-kind">
                  {allocated
                    ? addressModeName[allocated.address_mode].split(' · ')[0]
                    : native
                      ? '原有网络'
                      : '宿主 NAT'}
                </span>
              </div>
              {allocated && (
                <p className="cvm-flow-exit-sub">
                  {nameOf(allocated.provider_node)}
                  {!hidden && (allocated.public_ipv4 || allocated.public_ipv6)
                    ? ` · 对外 ${[allocated.public_ipv4, allocated.public_ipv6].filter(Boolean).join(' / ')}`
                    : ''}
                </p>
              )}
              <ul
                className="cvm-flow-routes"
                aria-label={allocated ? `经由 ${publication?.spec.name ?? '业务网卡'}` : '兜底出口'}
              >
                {routes.map(({ route, index }) => (
                  <li key={index}>
                    <span className={`cz-layer ${route.service ? 'L4' : 'L3'}`}>{route.service ? 'L4' : 'L3'}</span>
                    <code>{hidden ? '已脱敏' : routeLabel(route)}</code>
                    <span>{priority(nic, route.destination) ?? '—'}</span>
                  </li>
                ))}
                {!allocated && (
                  <li className="fallback">
                    <span className="cz-layer">其余</span>
                    <span>未匹配业务路由的流量</span>
                    <span>最低</span>
                  </li>
                )}
                {allocated && !routes.length && <li className="cvm-flow-unused">没有路由经由这张网卡</li>}
              </ul>
              {allocated && usage && allocated.limits.monthly_bytes ? (
                <div className="cvm-flow-usage">
                  <span>本月</span>
                  <CapacityMeter
                    used={usage.uploaded_bytes + usage.downloaded_bytes}
                    total={allocated.limits.monthly_bytes}
                  />
                  <span>
                    <b>{capacity(usage.uploaded_bytes + usage.downloaded_bytes)}</b> /{' '}
                    {capacity(allocated.limits.monthly_bytes)}
                  </span>
                </div>
              ) : null}
            </div>
          );
        })}
        {[...chainRoutes].map(([chainId, routes]) => {
          const chain = chains.data?.chains.find(c => c.id === chainId);
          const usage = chains.data?.traffic.find(t => t.kind === 'access' && t.app_id === chain?.app_id);
          return (
            <div className={`cvm-tile cvm-flow-exit${chain?.reachable === false ? ' failed' : ''}`} key={chainId}>
              <span className="cz-socket idle" data-anchor={`chain:${chainId}`} />
              <div className="cvm-flow-exit-head">
                <b>
                  <RegionFlag code={chain?.country} /> {chain?.name ?? chainId}
                </b>
                <span className="cz-kind">线路 · 主网卡</span>
              </div>
              <p className="cvm-flow-exit-sub">
                {chain?.machines.join(' → ') ?? '正在读取线路'} ·{' '}
                {chain?.reachable === false ? '不通' : chain?.rtt_ms != null ? `${chain.rtt_ms} ms` : '未拨测'}
              </p>
              <ul className="cvm-flow-routes" aria-label={`经由线路 ${chain?.name ?? chainId}`}>
                {routes.map((r, i) => (
                  <li key={i}>
                    <span className={`cz-layer ${r.service ? 'L4' : 'L3'}`}>{r.service ? 'L4' : 'L3'}</span>
                    <code>{hidden ? '已脱敏' : routeLabel(r)}</code>
                    <span>
                      {spec.routing.priorities.find(p => p.chain_id === chainId)?.[
                        r.destination.includes(':') ? 'ipv6' : 'ipv4'
                      ] ?? '—'}
                    </span>
                  </li>
                ))}
              </ul>
              <p className="note">
                使用 {chains.data?.owner?.name ?? instance.user_id} 的授权
                {usage ? ` · 本期 ${BigInt(usage.used_bytes).toLocaleString()} B` : ''}。TCP/UDP；故障不回落宿主 NAT。
              </p>
            </div>
          );
        })}
        {chains.error && <ErrorBox error={chains.error} />}
        {editable && (
          <p className="cvm-lane-note">
            {native ? '无匹配业务路由时使用宿主原有默认路由；' : '无匹配业务路由时通过主网卡 NAT 出网；'}
            选中的网卡或线路故障时保持不可达，不回落到本机。
          </p>
        )}
      </div>
    </section>
  );
}
