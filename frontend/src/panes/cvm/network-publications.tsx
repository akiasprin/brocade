import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
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
import { EmptyState, ErrorBox, Loading } from '../../ui/bits';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import {
  NIC_CATALOG,
  NIC_STATE,
  NicDialog,
  addressModeName,
  networkLines,
  transportName,
  useNicRequestId,
} from './network-common';
import { ownerKey, requestId } from './queries';

export function NetworkPublications({ workspace }: { workspace: CvmWorkspace }) {
  const client = useQueryClient();
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });
  const [editing, setEditing] = useState<CvmNicPublication | null | undefined>();
  const [updating, setUpdating] = useState<CvmNicPublication | null>(null);
  const [deleting, setDeleting] = useState<CvmNicPublication | null>(null);
  const operationId = useNicRequestId();
  const admission = useMutation({
    mutationFn: (p: CvmNicPublication) => {
      const body = { expected_revision: p.revision, admitting: !p.admitting };
      return setCvmNicAdmission(p.id, { ...body, request_id: operationId([p.id, body]) });
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: NIC_CATALOG });
      void client.invalidateQueries({ queryKey: NIC_STATE });
    },
  });
  if (catalog.isPending) return <Loading />;
  if (catalog.error && !catalog.data) return <ErrorBox error={catalog.error} />;
  const data = catalog.data;
  if (!data) return null;
  return (
    <div className="cvm-network-publications">
      <div className="cvm-network-heading">
        <div>
          <b>网络</b>
          <p className="note">由机器提供网络能力，容器挂载后获得独立网卡和地址。路由在容器侧配置。</p>
        </div>
        {data.can_manage && (
          <button className="btn primary" onClick={() => setEditing(null)}>
            ＋ 创建网络
          </button>
        )}
      </div>
      {catalog.error && <ErrorBox error={catalog.error} />}
      {admission.error && <ErrorBox error={admission.error} />}
      {!data.publications.length ? (
        <EmptyState icon="forwarding" title="暂无可用网络">
          {data.can_manage
            ? '选择具备网络提供能力的机器，创建网络规格并授权给使用者。'
            : '管理员创建网络并授权后，就可以在容器内挂载。'}
        </EmptyState>
      ) : (
        <div className="node-card-grid cvm-publication-grid">
          {data.publications.map(p => (
            <article className="node-card cvm-publication-card tone-ok" key={p.id}>
              <header>
                <div>
                  <h3>{p.spec.name}</h3>
                  <small>
                    {data.providers.find(provider => provider.node_id === p.spec.provider_node)?.name ??
                      p.spec.provider_node}{' '}
                    · {addressModeName[p.spec.address_mode]} · 规格版本 {p.revision}
                  </small>
                </div>
                <span className={`st ${p.admitting ? 'st-succeeded' : 'st-pending'}`}>
                  {p.admitting ? '接受新挂载' : '已停止新挂载'}
                </span>
              </header>
              <dl className="cvm-facts">
                <div>
                  <dt>地址模式</dt>
                  <dd>{addressModeName[p.spec.address_mode]}</dd>
                </div>
                <div>
                  <dt>可访问网络</dt>
                  <dd>{p.spec.destinations.join('、') || '仅主动入站及回包'}</dd>
                </div>
                <div>
                  <dt>承载</dt>
                  <dd>{p.spec.transports.map(t => transportName[t.kind]).join(' / ')}</dd>
                </div>
              </dl>
              <div className="cvm-network-figures">
                <span>
                  <b>{data.attachments.filter(n => n.publication_id === p.id && n.attached).length}</b> /{' '}
                  {p.spec.limits.max_attachments} 张网卡
                </span>
                <span className="cvm-mode">v{p.revision}</span>
              </div>
              <div className="cvm-network-chips">
                {data.attachments
                  .filter(n => n.publication_id === p.id && n.attached)
                  .map(n => (
                    <button
                      type="button"
                      key={n.interface_id}
                      onClick={() => navigate('cvm', { p: 'instance', id: n.instance_id })}
                    >
                      {n.instance_name}
                      {n.publication_revision < p.revision && <small>旧规格</small>}
                    </button>
                  ))}
              </div>
              {data.can_manage && (
                <footer>
                  <button className="btn small" disabled={admission.isPending} onClick={() => setEditing(p)}>
                    修改规格
                  </button>
                  <label className="switch cvm-admission-switch">
                    <span className="switch-label">接受新挂载</span>
                    <input
                      type="checkbox"
                      role="switch"
                      checked={p.admitting}
                      aria-checked={p.admitting}
                      aria-label={`${p.spec.name} 接受新挂载`}
                      disabled={admission.isPending}
                      onChange={() => admission.mutate(p)}
                    />
                    <span className="switch-ui" aria-hidden="true">
                      <span />
                    </span>
                  </label>
                  <button className="btn small" disabled={admission.isPending} onClick={() => setDeleting(p)}>
                    删除网络
                  </button>
                  {data.attachments.some(
                    n => n.publication_id === p.id && n.attached && n.publication_revision < p.revision,
                  ) && (
                    <button className="btn small" onClick={() => setUpdating(p)}>
                      批量应用新规格
                    </button>
                  )}
                </footer>
              )}
            </article>
          ))}
        </div>
      )}
      {editing !== undefined && (
        <PublicationDialog
          key={editing?.id ?? 'new'}
          workspace={workspace}
          catalog={data}
          publication={editing}
          onClose={() => setEditing(undefined)}
        />
      )}
      {updating && (
        <BulkNicVersionDialog
          publication={updating}
          attachments={data.attachments.filter(n => n.publication_id === updating.id)}
          onClose={() => setUpdating(null)}
        />
      )}
      {deleting && (
        <DeletePublication
          publication={deleting}
          dependencies={data.attachments.filter(n => n.publication_id === deleting.id)}
          onClose={() => setDeleting(null)}
        />
      )}
    </div>
  );
}

function defaultSpec(catalog: CvmNicCatalog): CvmNicPublicationSpec {
  const provider = catalog.providers.find(provider => provider.fresh && provider.capabilities.provide);
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

export function PublicationDialog({
  workspace,
  catalog,
  publication,
  initialNode,
  onClose,
}: {
  workspace: CvmWorkspace;
  catalog: CvmNicCatalog;
  publication: CvmNicPublication | null;
  initialNode?: string;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const [initial] = useState(() => {
    const spec = publication?.spec ?? defaultSpec(catalog);
    return initialNode && !publication ? { ...spec, provider_node: initialNode } : spec;
  });
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
  useUnsavedChanges(JSON.stringify(draft) !== JSON.stringify(initial), '网络规格');
  const save = useMutation({
    mutationFn: () => {
      const body = { expected_revision: publication?.revision ?? 0, spec: draft };
      return writeCvmNicPublication(publication?.id ?? null, { ...body, request_id: operationId(body) });
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: NIC_CATALOG });
      void client.invalidateQueries({ queryKey: NIC_STATE });
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
  const submit = () => {
    if (
      !spec.name.trim() ||
      (!authorizationOnly && (!provider?.fresh || !provider.capabilities.provide)) ||
      !spec.transports.length ||
      (!spec.ipv4_pool && !spec.ipv6_pool)
    ) {
      setLocalError('请填写名称、可用提供机器、地址池和承载。');
      return;
    }
    setLocalError(null);
    save.mutate();
  };
  return (
    <NicDialog
      title={publication ? '修改网络规格' : '创建网络'}
      onClose={onClose}
      pending={save.isPending}
      onSubmit={submit}
      submit={publication ? '保存新版本' : '创建网络'}
    >
      {publication && (
        <p className="note">
          已有网卡保留当前版本，使用者可以单独应用新规格。授权撤销会阻断已有网卡，停止新挂载不影响已有网卡。
        </p>
      )}
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
      <p className="note">初始路由生成后由容器独立管理，保存网络新版本不会覆盖已有路由表。</p>
      <fieldset className="cvm-network-offers">
        <legend>支持的 IP 承载</legend>
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
                    承载端口范围
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
              )}
            </div>
          );
        })}
      </fieldset>
      <p className="note">
        选择提供机器发起连接时，系统使用容器宿主在机器设置中的同地址族公开地址；端点 IP
        决定外层地址族。监听地址与端口须实际可达。
      </p>
      <fieldset className="cvm-network-grants">
        <legend>允许挂载的用户</legend>
        {workspace.owners.map(owner => {
          const key = ownerKey(owner.tenant_id, owner.user_id);
          return (
            <label className="cvm-check" key={key}>
              <input
                type="checkbox"
                checked={spec.grants.some(g => ownerKey(g.tenant_id, g.user_id) === key)}
                onChange={e =>
                  patch({
                    grants: e.target.checked
                      ? [...spec.grants, { tenant_id: owner.tenant_id, user_id: owner.user_id }]
                      : spec.grants.filter(g => ownerKey(g.tenant_id, g.user_id) !== key),
                  })
                }
              />
              {owner.name}
              <small>{owner.tenant_id}</small>
            </label>
          );
        })}
      </fieldset>
      <details>
        <summary>容量、速率与 DNS 建议</summary>
        <div className="cvm-form-grid">
          <label>
            最多挂载实例数
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
              ['monthly_bytes', '每月流量额度（字节）'],
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
      {localError && (
        <p className="cvm-notice error" role="alert">
          {localError}
        </p>
      )}
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
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
      void client.invalidateQueries({ queryKey: NIC_CATALOG });
      onClose();
    },
  });
  return (
    <NicDialog
      title="删除网络"
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
