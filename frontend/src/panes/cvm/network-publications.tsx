import './network-resource.css';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useId, useMemo, useState } from 'react';
import {
  fetchCvmNicCatalog,
  setCvmNicAdmission,
  writeCvmNicPublication,
  deleteCvmNicPublication,
  type CvmNicAttachment,
  type CvmNicCatalog,
  type CvmNicPublication,
  type CvmNicPublicationSpec,
  type CvmWorkspace,
} from '../../api';
import { navigate } from '../../forge/route';
import { BulkNicVersionDialog } from './network-bulk';
import { Confirm, EmptyState, ErrorBox, Loading } from '../../ui/bits';
import { ActionMenu } from '../../ui/choice-menu';
import { bytes } from '../../ui/format';
import { Icon, PanelTitle } from '../../ui/icons';
import { GeneratedUserAvatar } from '../../ui/user-avatar';
import { CapacityMeter } from './cards';
import { TitleActions } from '../../ui/title-actions';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { CvmPageHeader } from './presentation';
import {
  NIC_CATALOG,
  NicDialog,
  addressModeName,
  networkLines,
  transportName,
  uncertainCvmResult,
  useNicRequestId,
} from './network-common';
import { networkProviderName } from './network-names';
import { invalidateCvmChanges, ownerKey, requestId } from './queries';

type Grant = CvmNicPublicationSpec['grants'][number];

const grantKey = (grant: Grant) => ownerKey(grant.tenant_id, grant.user_id);
const ownerNames = (workspace: CvmWorkspace) => new Map(workspace.owners.map(owner => [grantKey(owner), owner.name]));
const grantName = (names: Map<string, string>, grant: Grant) => names.get(grantKey(grant)) ?? grant.user_id;
const useNetworkCatalog = () =>
  useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });

export function ResourceGrantNames({ grants, workspace }: { grants: Grant[]; workspace: CvmWorkspace }) {
  const names = ownerNames(workspace);
  if (!grants.length) return <span className="note">未授权</span>;
  return (
    <div className="cvm-resource-chips">
      {grants.map(grant => (
        <span
          className="cvm-resource-chip"
          title={`${grantName(names, grant)} · ${grant.tenant_id}`}
          key={grantKey(grant)}
        >
          <GeneratedUserAvatar id={grant.user_id} variant="mini" />
          {grantName(names, grant)}
        </span>
      ))}
    </div>
  );
}

export function NetworkPublications({
  workspace,
  onEdit,
  onCreate,
}: {
  workspace: CvmWorkspace;
  onEdit: (id: string) => void;
  onCreate: () => void;
}) {
  const client = useQueryClient();
  const catalog = useNetworkCatalog();
  const [deleting, setDeleting] = useState<CvmNicPublication | null>(null);
  const operationId = useNicRequestId();
  const admission = useMutation({
    mutationFn: (p: CvmNicPublication) => {
      const body = { expected_revision: p.revision, admitting: !p.admitting };
      return setCvmNicAdmission(p.id, { ...body, request_id: operationId([p.id, body]) });
    },
    onSuccess: () => invalidateCvmChanges(client),
  });
  if (catalog.isPending) return <Loading />;
  if (!catalog.data) return <ErrorBox error={catalog.error} />;
  const data = catalog.data;
  const occupancy = new Map<string, { held: Set<string>; attached: number; outdated: number }>();
  const revisions = new Map(data.publications.map(p => [p.id, p.revision]));
  for (const nic of data.attachments) {
    const revision = revisions.get(nic.publication_id);
    if (revision === undefined) continue;
    const total = occupancy.get(nic.publication_id) ?? { held: new Set<string>(), attached: 0, outdated: 0 };
    // An allocation awaiting cleanup still occupies capacity; two allocations for one NIC count once.
    total.held.add(nic.interface_id);
    if (nic.attached) {
      total.attached++;
      if (nic.publication_revision < revision) total.outdated++;
    }
    occupancy.set(nic.publication_id, total);
  }
  return (
    <section className="panel titled cvm-net-panel cvm-resource-panel cvm-network-publications" aria-label="出口网络">
      <header>
        <PanelTitle of="outbound">出口网络</PanelTitle>
        <span className="hint" title="挂载后获得独立网卡与地址，只承担出网。流量额度按单张网卡的 UTC 自然月计算。">
          <span className="header-stat">{data.publications.length} 个网络 · </span>
          容器挂载后获得独立网卡与地址，只承担出网；按 UTC 自然月计量流量额度
        </span>
        {data.can_manage && (
          <span className="cvm-panel-tools">
            <button type="button" className="btn sm" onClick={onCreate}>
              <Icon of="plus" size={13} />
              添加出口网络
            </button>
          </span>
        )}
      </header>
      {catalog.error && <ErrorBox error={catalog.error} />}
      {admission.error && <ErrorBox error={admission.error} />}
      {!data.publications.length ? (
        <EmptyState icon="forwarding" title="暂无可用网络">
          {data.can_manage
            ? '选择具备网络提供能力的机器，创建网络规格并授权给使用者。'
            : '管理员创建网络并授权后，就可以在容器内挂载。'}
        </EmptyState>
      ) : (
        <div className="cvm-resource-scroll" role="region" aria-label="出口网络列表" tabIndex={0}>
          <table className="tbl cvm-resource-table" aria-label="出口网络">
            <thead>
              <tr>
                <th scope="col">网络</th>
                <th scope="col">地址模式</th>
                <th scope="col">连接方式</th>
                <th scope="col">网卡</th>
                <th scope="col">规格</th>
                <th scope="col">月流量额度（UTC）</th>
                {data.can_manage && (
                  <>
                    <th scope="col">授权使用者</th>
                    <th scope="col">接受新挂载</th>
                    <th scope="col">操作</th>
                  </>
                )}
              </tr>
            </thead>
            <tbody>
              {data.publications.map(p => {
                const total = occupancy.get(p.id);
                const held = total?.held.size ?? 0;
                const awaitingCleanup = held - (total?.attached ?? 0);
                const outdated = total?.outdated ?? 0;
                return (
                  <tr
                    key={p.id}
                    className={data.can_manage ? 'rowlink' : undefined}
                    onClick={event => {
                      // The name button supplies keyboard navigation; row controls and their portals keep their own actions.
                      const target = event.target;
                      if (
                        data.can_manage &&
                        target instanceof Element &&
                        event.currentTarget.contains(target) &&
                        !target.closest('button, label, input')
                      ) {
                        onEdit(p.id);
                      }
                    }}
                  >
                    <td>
                      {data.can_manage ? (
                        <button type="button" className="cvm-resource-link" onClick={() => onEdit(p.id)}>
                          {p.spec.name}
                        </button>
                      ) : (
                        <strong>{p.spec.name}</strong>
                      )}
                      <span className="cvm-pub-sub">
                        {networkProviderName(data, p.spec.provider_node, workspace.hosts)}
                      </span>
                    </td>
                    <td>{addressModeName[p.spec.address_mode]}</td>
                    <td>{p.spec.transports.map(t => transportName[t.kind]).join(' · ') || '—'}</td>
                    <td className="cvm-resource-nowrap">
                      <span className="cvm-resource-usage">
                        {data.can_manage && <CapacityMeter used={held} total={p.spec.limits.max_attachments} />}
                        <span className="mono">
                          {data.can_manage ? `${held} / ` : ''}
                          {p.spec.limits.max_attachments}
                        </span>
                      </span>
                      {awaitingCleanup > 0 && <span className="cvm-pub-sub">{awaitingCleanup} 张待清理</span>}
                    </td>
                    <td>
                      <span className="mono">v{p.revision}</span>
                      {outdated > 0 && <span className="st st-gold">{outdated} 张旧规格</span>}
                      {!data.can_manage && !p.admitting && <span className="st st-pending">已停止新挂载</span>}
                    </td>
                    <td className="cvm-resource-nowrap">
                      {p.spec.limits.monthly_bytes == null ? '不限额' : bytes(p.spec.limits.monthly_bytes)}
                    </td>
                    {data.can_manage && (
                      <>
                        <td>
                          <ResourceGrantNames grants={p.spec.grants} workspace={workspace} />
                        </td>
                        <td>
                          <label className="switch cvm-admission-switch" title="停止新挂载不影响已有网卡">
                            <input
                              type="checkbox"
                              role="switch"
                              checked={p.admitting}
                              aria-label={`${p.spec.name} 接受新挂载`}
                              disabled={admission.isPending}
                              onChange={() => admission.mutate(p)}
                            />
                            <span className="switch-ui" aria-hidden="true">
                              <span />
                            </span>
                          </label>
                        </td>
                        <td>
                          <ActionMenu
                            label={`${p.spec.name} 更多操作`}
                            items={[
                              { label: '修改规格', note: '在整页编辑规格及授权', onSelect: () => onEdit(p.id) },
                              {
                                label: '删除网络',
                                note: '删除前会检查网卡及清理依赖',
                                danger: true,
                                disabled: admission.isPending,
                                onSelect: () => setDeleting(p),
                              },
                            ]}
                          />
                        </td>
                      </>
                    )}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {deleting && (
        <DeletePublication
          publication={deleting}
          dependencies={data.attachments.filter(n => n.publication_id === deleting.id)}
          onClose={() => setDeleting(null)}
        />
      )}
    </section>
  );
}

export function NetworkPublicationEditor({
  workspace,
  id,
  initialNode,
  onBack,
}: {
  workspace: CvmWorkspace;
  id?: string;
  initialNode?: string;
  onBack: () => void;
}) {
  const catalog = useNetworkCatalog();
  const publication = id ? catalog.data?.publications.find(p => p.id === id) : null;
  if (catalog.data?.can_manage && workspace.can_manage_network && !workspace.redacted && (publication || !id)) {
    return (
      <PublicationForm
        key={id ?? initialNode ?? 'new'}
        workspace={workspace}
        catalog={catalog.data}
        publication={publication ?? null}
        initialNode={initialNode}
        onClose={onBack}
        catalogError={catalog.error}
      />
    );
  }
  return (
    <>
      <CvmPageHeader
        title={id ? '出口网络' : '添加出口网络'}
        tools={
          <TitleActions>
            <button type="button" className="btn" onClick={onBack}>
              返回网络资源
            </button>
          </TitleActions>
        }
      />
      <div className="nd-paper-body">
        {catalog.isPending ? (
          <Loading />
        ) : !catalog.data ? (
          <ErrorBox error={catalog.error} />
        ) : (
          <EmptyState icon="forwarding" title="没有可编辑的网络">
            出口网络不存在或不在当前账号的管理范围内。
          </EmptyState>
        )}
      </div>
    </>
  );
}

function defaultSpec(catalog: CvmNicCatalog, initialNode?: string): CvmNicPublicationSpec {
  const provider = catalog.providers.find(p =>
    initialNode ? p.node_id === initialNode : p.fresh && p.capabilities.provide,
  );
  const v4 = provider?.capabilities.addresses.find(ip => !ip.includes(':')) ?? null;
  return {
    name: '',
    provider_node: provider?.node_id ?? '',
    resource_domain: `pool-${requestId()}`,
    ipv4_pool: '10.120.0.0/24',
    ipv6_pool: null,
    public_ipv4_pool: null,
    public_ipv6_pool: null,
    shared_ipv4: v4,
    shared_ipv6: null,
    address_mode: 'shared_nat',
    destinations: ['0.0.0.0/0'],
    route_presets: ['0.0.0.0/0'],
    dns_suggestions: [],
    transports: (provider?.capabilities.transports ?? []).map((kind, i) => ({
      kind,
      endpoint: v4 ?? provider?.capabilities.addresses[0] ?? '',
      port_start: 42000 + i * 1000,
      port_end: 42999 + i * 1000,
      mtu: 1400,
    })),
    grants: [],
    limits: { max_attachments: 64, upload_bps: null, download_bps: null, monthly_bytes: null },
  };
}

function PublicationForm({
  workspace,
  catalog,
  publication,
  initialNode,
  onClose,
  catalogError,
}: {
  workspace: CvmWorkspace;
  catalog: CvmNicCatalog;
  publication: CvmNicPublication | null;
  initialNode?: string;
  onClose: () => void;
  catalogError: unknown;
}) {
  const client = useQueryClient();
  const formId = useId();
  const [initial] = useState(() => publication?.spec ?? defaultSpec(catalog, initialNode));
  const [baseRevision] = useState(publication?.revision ?? 0);
  const [discarding, setDiscarding] = useState(false);
  const [spec, setSpec] = useState(initial);
  const [destinations, setDestinations] = useState(initial.destinations.join('\n'));
  const [presets, setPresets] = useState(initial.route_presets.join('\n'));
  const [dns, setDns] = useState(initial.dns_suggestions.join('\n'));
  const [localError, setLocalError] = useState<string | null>(null);
  const operationId = useNicRequestId();
  const provider = catalog.providers.find(provider => provider.node_id === spec.provider_node);
  const draft = {
    ...spec,
    destinations: networkLines(destinations),
    route_presets: networkLines(presets),
    dns_suggestions: networkLines(dns),
  };
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);
  const markSaved = useUnsavedChanges(dirty, '网络规格');
  const publicationId = publication?.id;
  const rows = useMemo(
    () => derivedNicRows(catalog.attachments, workspace.instances, publicationId),
    [catalog.attachments, workspace.instances, publicationId],
  );
  const save = useMutation({
    mutationFn: () => {
      const body = { expected_revision: baseRevision, spec: draft };
      return writeCvmNicPublication(publication?.id ?? null, { ...body, request_id: operationId(body) });
    },
    onSuccess: () => {
      markSaved();
      invalidateCvmChanges(client);
      onClose();
    },
  });
  const patch = (value: Partial<CvmNicPublicationSpec>) => setSpec(old => ({ ...old, ...value }));
  const changeProvider = (node: string) => {
    const provider = catalog.providers.find(provider => provider.node_id === node);
    const address =
      provider?.capabilities.addresses.find(ip => !ip.includes(':')) ?? provider?.capabilities.addresses[0] ?? '';
    patch({
      provider_node: node,
      shared_ipv4: address && !address.includes(':') ? address : null,
      transports: (provider?.capabilities.transports ?? []).map((kind, i) => ({
        kind,
        endpoint: address,
        port_start: 42000 + i * 1000,
        port_end: 42999 + i * 1000,
        mtu: 1400,
      })),
    });
  };
  const authorizationOnly =
    !!publication && JSON.stringify({ ...draft, grants: [] }) === JSON.stringify({ ...initial, grants: [] });
  const stale = publication != null && publication.revision !== baseRevision;
  const uncertain = uncertainCvmResult(save.error);
  const submit = () => {
    if (save.isPending || !dirty || (stale && !uncertain)) return;
    if (
      !uncertain &&
      (!spec.name.trim() ||
        (!authorizationOnly && (!provider?.fresh || !provider.capabilities.provide)) ||
        !spec.transports.length ||
        (!spec.ipv4_pool && !spec.ipv6_pool))
    ) {
      setLocalError('请填写名称、可用提供机器、地址池和连接方式。');
      return;
    }
    setLocalError(null);
    save.mutate();
  };
  const title = publication ? '修改网络规格' : '添加出口网络';
  const specFields = (
    <>
      <div className="cvm-form-grid">
        <label>
          名称
          <input className="f" value={spec.name} maxLength={80} onChange={e => patch({ name: e.target.value })} />
        </label>
        <label>
          提供机器
          <select
            className="f"
            value={spec.provider_node}
            disabled={!!publication}
            onChange={e => changeProvider(e.target.value)}
          >
            <option value="">选择机器</option>
            {publication && !provider && (
              <option value={spec.provider_node}>
                {networkProviderName(catalog, spec.provider_node, workspace.hosts)}
              </option>
            )}
            {catalog.providers.map(provider => (
              <option
                key={provider.node_id}
                value={provider.node_id}
                disabled={!provider.fresh || !provider.capabilities.provide}
              >
                {provider.name}
                {!provider.fresh ? ' · 能力已过期' : !provider.capabilities.provide ? ' · 网络能力不可用' : ''}
              </option>
            ))}
          </select>
        </label>
        {!publication && (
          <label>
            地址池来源
            <select
              className="f"
              value={spec.resource_domain}
              onChange={e => {
                const pool = catalog.publications.find(p => p.spec.resource_domain === e.target.value)?.spec;
                patch({
                  resource_domain: e.target.value,
                  ...(pool ? { ipv4_pool: pool.ipv4_pool, ipv6_pool: pool.ipv6_pool } : {}),
                });
              }}
            >
              <option value={initial.resource_domain}>新建独立地址池</option>
              {[...new Map(catalog.publications.map(p => [p.spec.resource_domain, p])).values()].map(p => (
                <option key={p.spec.resource_domain} value={p.spec.resource_domain}>
                  复用「{p.spec.name}」的地址池
                </option>
              ))}
            </select>
          </label>
        )}
        <label>
          地址模式
          <select
            className="f"
            value={spec.address_mode}
            onChange={e => patch({ address_mode: e.target.value as CvmNicPublicationSpec['address_mode'] })}
          >
            {Object.entries(addressModeName).map(([value, name]) => (
              <option key={value} value={value}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <label>
          IPv4 地址池
          <input
            className="f"
            placeholder="10.120.0.0/24；留空不提供 IPv4"
            value={spec.ipv4_pool ?? ''}
            onChange={e => patch({ ipv4_pool: e.target.value || null })}
          />
        </label>
        <label>
          IPv6 地址池
          <input
            className="f"
            placeholder="fd12:3456::/120；留空不提供 IPv6"
            value={spec.ipv6_pool ?? ''}
            onChange={e => patch({ ipv6_pool: e.target.value || null })}
          />
        </label>
        {spec.address_mode === 'shared_nat' ? (
          <>
            <label>
              共享出口 IPv4
              <input
                className="f"
                placeholder="提供机器实际可用的 IPv4"
                value={spec.shared_ipv4 ?? ''}
                onChange={e => patch({ shared_ipv4: e.target.value || null })}
              />
            </label>
            <label>
              共享出口 IPv6
              <input
                className="f"
                placeholder="提供机器实际可用的 IPv6"
                value={spec.shared_ipv6 ?? ''}
                onChange={e => patch({ shared_ipv6: e.target.value || null })}
              />
            </label>
          </>
        ) : spec.address_mode === 'dedicated_nat' ? (
          <>
            <label>
              对外 IPv4 地址池
              <input
                className="f"
                value={spec.public_ipv4_pool ?? ''}
                onChange={e => patch({ public_ipv4_pool: e.target.value || null })}
              />
            </label>
            <label>
              对外 IPv6 地址池
              <input
                className="f"
                value={spec.public_ipv6_pool ?? ''}
                onChange={e => patch({ public_ipv6_pool: e.target.value || null })}
              />
            </label>
          </>
        ) : (
          <p className="note">路由直达模式将地址池中的地址分配给容器，请使用提供机器真实可路由的地址资源。</p>
        )}
      </div>
      <p className="note">
        地址池用于分配容器网卡的地址，例如 10.120.0.0/24。共享出口地址是外部服务看到的地址，应填写提供机器实际可用的
        IP。
      </p>
      <label className="cvm-field">
        可访问的目标 IP／网段
        <textarea
          className="f"
          rows={3}
          value={destinations}
          onChange={e => setDestinations(e.target.value)}
          placeholder="全路由填写 0.0.0.0/0 或 ::/0；指定网络填写对应网段"
        />
      </label>
      <label className="cvm-field">
        挂载时初始化的路由（可选）
        <textarea className="f" rows={2} value={presets} onChange={e => setPresets(e.target.value)} />
      </label>
      <p className="note">
        目标网段限制此网络允许访问的地址。每行填写一个 IP 或网段，0.0.0.0/0 表示全部 IPv4，::/0 表示全部
        IPv6。初始路由只在新挂载时添加，留空后需在容器路由表中手动添加。
      </p>
      <fieldset className="cvm-network-offers">
        <legend>支持的连接方式</legend>
        {(['xray', 'wireguard'] as const).map(kind => {
          const offer = spec.transports.find(t => t.kind === kind);
          const supported = provider?.capabilities.transports.includes(kind);
          return (
            <div key={kind}>
              <label className="cvm-check">
                <input
                  type="checkbox"
                  checked={!!offer}
                  disabled={!supported}
                  onChange={e =>
                    patch({
                      transports: e.target.checked
                        ? [
                            ...spec.transports,
                            {
                              kind,
                              endpoint: spec.shared_ipv4 ?? provider?.capabilities.addresses[0] ?? '',
                              port_start: kind === 'wireguard' ? 42000 : 43000,
                              port_end: kind === 'wireguard' ? 42999 : 43999,
                              mtu: 1400,
                            },
                          ]
                        : spec.transports.filter(t => t.kind !== kind),
                    })
                  }
                />
                {transportName[kind]}
                {!supported && <small> · 该机器尚未支持</small>}
              </label>
              {offer && (
                <details className="cvm-resource-connection">
                  <summary>{transportName[kind]} 连接参数</summary>
                  <div className="cvm-form-grid">
                    <label>
                      连接发起方向
                      <select
                        className="f"
                        value={offer.listen_side === 'consumer' ? 'consumer' : 'provider'}
                        onChange={e =>
                          patch({
                            transports: spec.transports.map(t =>
                              t.kind === kind ? { ...t, listen_side: e.target.value as 'consumer' | 'provider' } : t,
                            ),
                          })
                        }
                      >
                        <option value="provider">容器宿主连接提供机器</option>
                        <option value="consumer">提供机器连接容器宿主</option>
                      </select>
                    </label>
                    <label>
                      提供机器端点 IP
                      <input
                        className="f"
                        value={offer.endpoint}
                        onChange={e =>
                          patch({
                            transports: spec.transports.map(t =>
                              t.kind === kind ? { ...t, endpoint: e.target.value } : t,
                            ),
                          })
                        }
                      />
                    </label>
                    <label>
                      连接端口范围
                      <div className="cvm-port-pair">
                        <input
                          className="f"
                          aria-label={`${transportName[kind]} 起始端口`}
                          type="number"
                          min={1}
                          max={65535}
                          value={offer.port_start}
                          onChange={e =>
                            patch({
                              transports: spec.transports.map(t =>
                                t.kind === kind ? { ...t, port_start: Number(e.target.value) } : t,
                              ),
                            })
                          }
                        />
                        <span>—</span>
                        <input
                          className="f"
                          aria-label={`${transportName[kind]} 结束端口`}
                          type="number"
                          min={1}
                          max={65535}
                          value={offer.port_end}
                          onChange={e =>
                            patch({
                              transports: spec.transports.map(t =>
                                t.kind === kind ? { ...t, port_end: Number(e.target.value) } : t,
                              ),
                            })
                          }
                        />
                      </div>
                    </label>
                  </div>
                </details>
              )}
            </div>
          );
        })}
      </fieldset>
      <p className="note">
        选择提供机器发起连接时，系统使用容器宿主在机器设置中的同地址族公开地址；端点 IP
        决定外层地址族。监听地址与端口须实际可达。
      </p>
      <details className="cvm-resource-connection">
        <summary>
          容量、速率与 DNS 建议 · UTC 月流量{' '}
          {spec.limits.monthly_bytes == null ? '不限额' : bytes(spec.limits.monthly_bytes)}
        </summary>
        <div className="cvm-form-grid">
          <label>
            最多挂载网卡数
            <input
              className="f"
              type="number"
              min={1}
              max={1024}
              value={spec.limits.max_attachments}
              onChange={e => patch({ limits: { ...spec.limits, max_attachments: Number(e.target.value) } })}
            />
          </label>
          {(
            [
              ['upload_bps', '上传上限（bit/s）'],
              ['download_bps', '下载上限（bit/s）'],
              ['monthly_bytes', '每张网卡月流量额度（UTC，字节）'],
            ] as const
          ).map(([key, label]) => (
            <label key={key}>
              {label}
              <input
                className="f"
                type="number"
                min={1}
                placeholder="留空不额外限制"
                value={spec.limits[key] ?? ''}
                onChange={e =>
                  patch({ limits: { ...spec.limits, [key]: e.target.value ? Number(e.target.value) : null } })
                }
              />
            </label>
          ))}
          <label>
            有效 MTU
            <input
              className="f"
              type="number"
              min={1280}
              max={1500}
              value={spec.transports[0]?.mtu ?? 1400}
              onChange={e => patch({ transports: spec.transports.map(t => ({ ...t, mtu: Number(e.target.value) })) })}
            />
          </label>
        </div>
        <label className="cvm-field">
          DNS 建议地址
          <textarea className="f" rows={2} value={dns} onChange={e => setDns(e.target.value)} />
        </label>
      </details>
      {provider?.capabilities.error && <p className="cvm-notice">{provider.capabilities.error}</p>}
    </>
  );
  const feedback = (
    <>
      {localError && (
        <p className="cvm-notice error" role="alert">
          {localError}
        </p>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </>
  );
  const grants = (
    <GrantEditor
      workspace={workspace}
      initial={initial.grants}
      grants={draft.grants}
      rows={rows}
      disabled={save.isPending || uncertain || stale}
      onChange={grants => patch({ grants })}
    />
  );
  return (
    <>
      {dirty && (
        <div className="fg-draft cvm-resource-draft">
          <div className="fg-draft-head" role="status">
            <span>有未保存的网络变更</span>
            <span className="note">保存后在「发布」中统一应用</span>
          </div>
        </div>
      )}
      <CvmPageHeader
        detail
        title={publication?.spec.name ?? title}
        meta={
          publication &&
          `${networkProviderName(catalog, publication.spec.provider_node, workspace.hosts)} · ${addressModeName[publication.spec.address_mode]} · 规格 v${publication.revision}`
        }
        tools={
          <TitleActions
            primary={
              <button
                className="btn primary"
                type="submit"
                form={formId}
                disabled={save.isPending || !dirty || (stale && !uncertain)}
              >
                {save.isPending
                  ? '正在保存…'
                  : uncertain
                    ? '重试保存'
                    : publication
                      ? `保存为 v${baseRevision + 1}`
                      : '保存变更'}
              </button>
            }
          >
            <button
              className="btn"
              type="button"
              disabled={save.isPending}
              onClick={() => (dirty ? setDiscarding(true) : onClose())}
            >
              返回网络资源
            </button>
          </TitleActions>
        }
      />
      <div className="nd-paper-body cvm-resource-editor">
        {catalogError != null && <ErrorBox error={catalogError} />}
        {uncertain && (
          <p className="cvm-notice" role="status">
            上次保存结果尚未确认。请重试原操作，或返回列表核查。
          </p>
        )}
        {stale && !uncertain && (
          <p className="cvm-notice" role="alert">
            网络规格已更新。请返回列表并重新打开后再修改。
          </p>
        )}
        {feedback}
        <div className={`cvm-resource-editor-grid${publication ? '' : ' is-new'}`}>
          <form
            id={formId}
            onSubmit={event => {
              event.preventDefault();
              submit();
            }}
            noValidate
          >
            <div className="cvm-panel">
              <fieldset
                className="cvm-resource-form cvm-network-fields"
                disabled={save.isPending || uncertain || stale}
              >
                <legend className="cvm-lane-head cvm-panel-head">规格配置</legend>
                <div className="cvm-panel-body">
                  <p className="note">
                    {publication
                      ? `保存将创建 v${baseRevision + 1}，已有网卡保留原版本。`
                      : '设置并授权容器可使用的出口网络。'}{' '}
                    保存后在「发布」中统一应用。
                  </p>
                  {specFields}
                </div>
              </fieldset>
            </div>
            {grants}
          </form>
          {publication && (
            <DerivedNics publication={publication} rows={rows} workspace={workspace} dirty={dirty || save.isPending} />
          )}
        </div>
      </div>
      {discarding && (
        <Confirm
          title="放弃未保存的网络变更"
          body="离开后，本页尚未保存的规格与授权修改会丢失。"
          confirmLabel="放弃并返回"
          onCancel={() => setDiscarding(false)}
          onConfirm={onClose}
        />
      )}
    </>
  );
}

function derivedNicRows(attachments: CvmNicAttachment[], instances: CvmWorkspace['instances'], id?: string) {
  if (!id) return [];
  const instancesById = new Map(instances.map(instance => [instance.desired.id, instance]));
  return attachments
    .filter(nic => nic.publication_id === id)
    .map(nic => {
      const instance = instancesById.get(nic.instance_id);
      const allocated = instance?.desired.spec.interfaces.find(
        item =>
          item.id === nic.interface_id &&
          item.allocated?.allocation_id === nic.allocation_id &&
          item.allocated.publication_revision === nic.publication_revision,
      )?.allocated;
      return { nic, instance, allocated };
    });
}

type NicRow = ReturnType<typeof derivedNicRows>[number];

function GrantEditor({
  workspace,
  grants,
  initial,
  rows,
  disabled,
  onChange,
}: {
  workspace: CvmWorkspace;
  grants: Grant[];
  initial: Grant[];
  rows: NicRow[];
  disabled: boolean;
  onChange: (grants: Grant[]) => void;
}) {
  const [selected, select] = useState('');
  const names = ownerNames(workspace);
  const counts = new Map<string, number>();
  let unattributed = 0;
  for (const { nic, instance } of rows) {
    if (!nic.attached) continue;
    if (!instance) unattributed++;
    else if (instance.user_id) {
      const key = ownerKey(instance.tenant_id, instance.user_id);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
  }
  const keys = new Set(grants.map(grantKey));
  const removed = initial.filter(grant => !keys.has(grantKey(grant)));
  const available = workspace.owners.filter(owner => !keys.has(grantKey(owner)));
  const chosen = available.find(owner => grantKey(owner) === selected);
  return (
    <div className="cvm-panel">
      <fieldset className="cvm-resource-grants" disabled={disabled}>
        <legend className="cvm-lane-head cvm-panel-head">授权使用者</legend>
        <div className="cvm-panel-body">
          <p className="note">撤销授权后，使用者不能新挂载；已有网卡将被阻断，但地址和路由保留。</p>
          <div className="cvm-resource-chips">
            {grants.map(grant => {
              const key = grantKey(grant);
              const name = grantName(names, grant);
              return (
                <span className="cvm-resource-chip" title={grant.tenant_id} key={key}>
                  <span>{name}</span>
                  <small>
                    {unattributed > 0 && '已核对 '}
                    {counts.get(key) ?? 0} 张网卡
                  </small>
                  <button
                    type="button"
                    className="btn icon"
                    aria-label={`撤销 ${name} 的授权`}
                    onClick={() => onChange(grants.filter(item => grantKey(item) !== key))}
                  >
                    <Icon of="close" size={12} />
                  </button>
                </span>
              );
            })}
            {!grants.length && <span className="note">尚未授权使用者</span>}
          </div>
          {unattributed > 0 && <p className="note">另有 {unattributed} 张网卡的使用者暂不可核对，未计入以上数量。</p>}
          {!!removed.length && (
            <ul className="cvm-resource-effects" aria-label="撤销授权影响">
              {removed.map(grant => {
                const count = counts.get(grantKey(grant)) ?? 0;
                return (
                  <li key={grantKey(grant)}>
                    <span>
                      待撤销 {grantName(names, grant)} 的授权{count ? `，将阻断其 ${count} 张网卡` : '，将停止其新挂载'}
                      。
                    </span>
                    <button
                      type="button"
                      className="cvm-resource-link"
                      aria-label={`保留 ${grantName(names, grant)} 的授权`}
                      onClick={() => {
                        const restore = grantKey(grant);
                        const originallyGranted = new Set(initial.map(grantKey));
                        onChange([
                          ...initial.filter(item => grantKey(item) === restore || keys.has(grantKey(item))),
                          ...grants.filter(item => !originallyGranted.has(grantKey(item))),
                        ]);
                      }}
                    >
                      保留授权
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          {!!available.length && (
            <div className="cvm-resource-add-grant">
              <select
                className="f"
                aria-label="选择授权使用者"
                value={chosen ? selected : ''}
                onChange={event => select(event.target.value)}
              >
                <option value="">选择使用者</option>
                {available.map(owner => (
                  <option key={grantKey(owner)} value={grantKey(owner)}>
                    {owner.name} ·{' '}
                    {workspace.tenants?.find(tenant => tenant.tenant_id === owner.tenant_id)?.name ?? owner.tenant_id}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="btn"
                disabled={!chosen}
                onClick={() => {
                  if (!chosen) return;
                  onChange([...grants, { tenant_id: chosen.tenant_id, user_id: chosen.user_id }]);
                  select('');
                }}
              >
                ＋ 授权
              </button>
            </div>
          )}
        </div>
      </fieldset>
    </div>
  );
}

function DerivedNics({
  publication,
  rows,
  workspace,
  dirty,
}: {
  publication: CvmNicPublication;
  rows: NicRow[];
  workspace: CvmWorkspace;
  dirty: boolean;
}) {
  const [bulk, setBulk] = useState(false);
  const attached = rows.filter(({ nic }) => nic.attached);
  const awaitingCleanup = rows.length - attached.length;
  const count = attached.filter(({ nic }) => nic.publication_revision < publication.revision).length;
  const names = ownerNames(workspace);
  return (
    <section className="cvm-resource-derived" aria-label="派生网卡">
      <div className="cvm-resource-panel">
        <header className="cvm-resource-heading cvm-lane-head">
          <h2>派生网卡</h2>
          <small className="header-stat">
            {attached.length} 张{awaitingCleanup > 0 && ` · ${awaitingCleanup} 条待清理`}
          </small>
          {count > 0 && (
            <span className="cvm-lane-aside">
              <button
                type="button"
                className="btn small"
                disabled={dirty}
                title={dirty ? '请先保存或放弃本页修改' : undefined}
                onClick={() => setBulk(true)}
              >
                批量应用 v{publication.revision} · {count} 张
              </button>
            </span>
          )}
        </header>
        {!rows.length ? (
          <EmptyState icon="topology" title="暂无派生网卡">
            此网络尚未分配网卡。
          </EmptyState>
        ) : (
          <div className="cvm-resource-scroll" role="region" aria-label="派生网卡列表" tabIndex={0}>
            <table className="tbl cvm-resource-table cvm-derived-table" aria-label="派生网卡">
              <thead>
                <tr>
                  <th scope="col">容器 / 网卡</th>
                  <th scope="col">使用者</th>
                  <th scope="col">地址</th>
                  <th scope="col">连接方式</th>
                  <th scope="col">规格版本</th>
                </tr>
              </thead>
              <tbody>
                {rows.map(({ nic, instance, allocated }) => {
                  const device = instance?.report?.interfaces.find(device => device.id === nic.interface_id)?.device;
                  const addresses = [allocated?.ipv4, allocated?.ipv6].filter(Boolean);
                  return (
                    <tr key={nic.allocation_id}>
                      <td>
                        <button
                          type="button"
                          className="cvm-resource-link"
                          onClick={() => navigate('cvm', { p: 'instance', id: nic.instance_id, tab: 'network' })}
                        >
                          {nic.instance_name}
                        </button>
                        <code className="cvm-pub-sub" title={nic.interface_id}>
                          {device || nic.interface_id.slice(-8)}
                        </code>
                      </td>
                      <td>
                        {instance
                          ? instance.user_id
                            ? grantName(names, { tenant_id: instance.tenant_id, user_id: instance.user_id })
                            : '未授权'
                          : '—'}
                      </td>
                      <td>
                        {addresses.length
                          ? addresses.map(address => (
                              <code key={address} className="cvm-resource-address">
                                {address}
                              </code>
                            ))
                          : '—'}
                      </td>
                      <td>{allocated ? transportName[allocated.transport] : '—'}</td>
                      <td>
                        <span className="mono">v{nic.publication_revision}</span>
                        {!nic.attached ? (
                          <span className="st st-pending">等待清理</span>
                        ) : nic.publication_revision < publication.revision ? (
                          <span className="st st-pending">旧规格</span>
                        ) : nic.publication_revision === publication.revision ? (
                          <span className="st st-succeeded">最新规格</span>
                        ) : (
                          <span className="st st-pending">待核对</span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
      <p className="cvm-notice">删除出口网络前须卸载派生网卡并等待清理完成。关闭「接受新挂载」不影响已有网卡。</p>
      {bulk && (
        <BulkNicVersionDialog
          publication={publication}
          attachments={rows.map(row => row.nic)}
          selectOutdated
          onClose={() => setBulk(false)}
        />
      )}
    </section>
  );
}

function DeletePublication({
  publication,
  dependencies,
  onClose,
}: {
  publication: CvmNicPublication;
  dependencies: CvmNicAttachment[];
  onClose: () => void;
}) {
  const client = useQueryClient();
  const operationId = useNicRequestId();
  const remove = useMutation({
    mutationFn: () => {
      const body = { expected_revision: publication.revision, admitting: false };
      return deleteCvmNicPublication(publication.id, { ...body, request_id: operationId([publication.id, body]) });
    },
    onSuccess: () => {
      invalidateCvmChanges(client);
      onClose();
    },
  });
  return (
    <NicDialog
      title="删除网络"
      error={remove.error}
      disabled={dependencies.length > 0}
      pending={remove.isPending}
      onClose={onClose}
      onSubmit={() => remove.mutate()}
      submit="删除网络"
    >
      <p>删除「{publication.spec.name}」。请先处理以下网卡引用并等待资源清理完成。</p>
      {dependencies.map(n => (
        <article className="cvm-attached" key={n.allocation_id}>
          <b>{n.instance_name}</b>
          <p className="note">
            {n.cleanup_pending ? '等待资源清理确认' : `仍挂载 · ${n.route_references} 个路由或优先级引用`}
          </p>
          <button
            className="btn small"
            onClick={() => {
              onClose();
              navigate('cvm', { p: 'instance', id: n.instance_id });
            }}
          >
            打开容器
          </button>
        </article>
      ))}
      {remove.error && <ErrorBox error={remove.error} />}
    </NicDialog>
  );
}
