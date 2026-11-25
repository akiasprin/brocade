import { parsePortRanges, rangesText, reservedPortsText } from './entry-policy';
// 访问入口地址：管理员发布可投递到容器主网卡的地址，并维护协议端口的保留策略，
// IP 保留在入口机器上；跨宿主的入口经受管 IP 机器对通道送到容器所在宿主。
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import {
  deleteCvmEntryPublication,
  fetchCvmEntries,
  fetchCvmNicCatalog,
  setCvmEntryAdmission,
  writeCvmEntryPublication,
  type CvmEntryAddressOffer,
  type CvmEntryPublicationSpec,
  type CvmEntryPublicationView,
  type CvmNicCatalog,
  type CvmWorkspace,
} from '../../api';
import { Confirm, EmptyState, ErrorBox, Loading } from '../../ui/bits';
import { Icon, PanelTitle } from '../../ui/icons';
import { CapacityMeter } from './cards';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { ENTRY_BOARD } from './queries';
import { NIC_CATALOG, NicDialog, transportName, uncertainCvmResult, useNicRequestId } from './network-common';
import { ActionMenu } from '../../ui/choice-menu';
import { ResourceGrantNames } from './network-publications';
import { invalidateCvmChanges, ownerKey } from './queries';

const visible = (offer: CvmEntryAddressOffer) => offer.public_address ?? offer.address;

export function EntryPublications({ workspace }: { workspace: CvmWorkspace }) {
  const client = useQueryClient();
  const board = useQuery({ queryKey: ENTRY_BOARD, queryFn: fetchCvmEntries, refetchInterval: 15_000 });
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });
  const [editing, setEditing] = useState<CvmEntryPublicationView | null | undefined>();
  const [deleting, setDeleting] = useState<CvmEntryPublicationView | null>(null);
  const operationId = useNicRequestId();
  const settled = () => invalidateCvmChanges(client);
  const admission = useMutation({
    mutationFn: (p: CvmEntryPublicationView) => {
      const body = { expected_revision: p.revision, admitting: !p.admitting };
      return setCvmEntryAdmission(p.id, { ...body, request_id: operationId([p.id, body]) });
    },
    onSuccess: settled,
  });
  const remove = useMutation({
    mutationFn: (p: CvmEntryPublicationView) => {
      const body = { expected_revision: p.revision, admitting: false };
      return deleteCvmEntryPublication(p.id, { ...body, request_id: operationId(['delete', p.id, body]) });
    },
    onSuccess: () => {
      settled();
      setDeleting(null);
    },
  });
  if (board.isPending) return <Loading />;
  if (!board.data) return <ErrorBox error={board.error} />;
  const data = board.data;
  const addresses = data.publications.reduce((sum, p) => sum + p.spec.addresses.length, 0);
  return (
    <section className="panel titled cvm-net-panel cvm-resource-panel cvm-entry-publications" aria-label="访问入口地址">
      <header>
        <PanelTitle of="ingress">访问入口地址</PanelTitle>
        <span className="hint" title="按整 IP 或 TCP／UDP 单端口、连续端口段投递到主网卡，不占网卡槽位。">
          <span className="header-stat">
            {data.publications.length} 组入口 · {addresses} 个地址 ·{' '}
          </span>
          按整 IP 或单端口／连续端口段投递到主网卡，不占网卡槽位
        </span>
        {data.can_manage && (
          <span className="cvm-panel-tools">
            <button type="button" className="btn sm" onClick={() => setEditing(null)}>
              <Icon of="plus" size={13} />
              添加入口地址
            </button>
          </span>
        )}
      </header>
      {board.error && <ErrorBox error={board.error} />}
      {admission.error && <ErrorBox error={admission.error} />}
      {!data.publications.length ? (
        <EmptyState icon="forwarding" title="暂无入口地址">
          {data.can_manage
            ? '选择已升级 Agent 的机器，添加共享地址的端口段或独立地址，并授权给使用者。'
            : '管理员添加入口地址并授权后，就可以为容器接入。'}
        </EmptyState>
      ) : (
        <div className="cvm-resource-scroll" role="region" aria-label="访问入口地址列表" tabIndex={0}>
          <table className="tbl cvm-resource-table" aria-label="访问入口地址">
            <thead>
              <tr>
                <th scope="col">发布</th>
                <th scope="col">类型</th>
                <th scope="col">地址</th>
                <th scope="col">入口占用</th>
                {data.can_manage ? (
                  <>
                    <th scope="col">授权使用者</th>
                    <th scope="col">接受新入口</th>
                    <th scope="col">操作</th>
                  </>
                ) : (
                  <th scope="col">状态</th>
                )}
              </tr>
            </thead>
            <tbody>
              {data.publications.map(p => {
                const kinds = new Set(p.spec.addresses.map(offer => offer.kind));
                return (
                  <tr key={p.id}>
                    <td>
                      {data.can_manage ? (
                        <button
                          type="button"
                          className="cvm-resource-link"
                          disabled={admission.isPending}
                          onClick={() => setEditing(p)}
                        >
                          {p.spec.name}
                        </button>
                      ) : (
                        <strong>{p.spec.name}</strong>
                      )}
                      <span className="cvm-pub-sub">
                        {p.provider_name} · 规格 v{p.revision}
                      </span>
                      <span className="cvm-pub-sub">
                        {p.spec.transports.length
                          ? [...new Set(p.spec.transports.map(t => transportName[t.kind]))].join(' / ') + ' 连接'
                          : '本机投递'}
                      </span>
                    </td>
                    <td>{kinds.size > 1 ? '共享 / 独占地址' : kinds.has('shared') ? '共享地址' : '独占地址'}</td>
                    <td>
                      <ul className="cvm-resource-addresses">
                        {p.spec.addresses.map(offer => {
                          const usage = p.usage.find(item => item.address === offer.address);
                          return (
                            <li key={offer.address}>
                              <span className={`cz-layer ${offer.kind === 'shared' ? '共享' : '独占'}`}>
                                {offer.kind === 'shared' ? '共享' : '独占'}
                              </span>
                              <code>{visible(offer)}</code>
                              {offer.kind === 'shared' && (
                                <span className="mono">
                                  {offer.port_start === offer.port_end
                                    ? `端口 ${offer.port_start}`
                                    : `${offer.port_start}–${offer.port_end}`}
                                </span>
                              )}
                              {reservedPortsText(offer.reserved_ports) && (
                                <small>保留 {reservedPortsText(offer.reserved_ports)}</small>
                              )}
                              <small>
                                {usage == null
                                  ? '占用待核对'
                                  : offer.kind === 'shared'
                                    ? `${usage.ports.length} 段占用`
                                    : usage.whole_taken
                                      ? '已分配'
                                      : '空闲'}
                              </small>
                            </li>
                          );
                        })}
                      </ul>
                    </td>
                    <td className="cvm-resource-nowrap">
                      <span className="cvm-resource-usage">
                        <CapacityMeter used={p.active_entries} total={p.spec.max_entries} />
                        <span className="mono">
                          {p.active_entries} / {p.spec.max_entries}
                        </span>
                      </span>
                    </td>
                    {data.can_manage ? (
                      <>
                        <td>
                          <ResourceGrantNames grants={p.spec.grants} workspace={workspace} />
                        </td>
                        <td>
                          <label className="switch cvm-admission-switch" title="停止新接入不撤销已有入口">
                            <input
                              type="checkbox"
                              role="switch"
                              checked={p.admitting}
                              aria-label={`${p.spec.name} 接受新入口`}
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
                              {
                                label: '修改',
                                note: '修改地址和授权',
                                disabled: admission.isPending,
                                onSelect: () => setEditing(p),
                              },
                              {
                                label: '删除',
                                note: p.active_entries ? '仍有入口在用，需先撤销' : '删除这份入口地址发布',
                                danger: true,
                                disabled: admission.isPending,
                                onSelect: () => setDeleting(p),
                              },
                            ]}
                          />
                        </td>
                      </>
                    ) : (
                      <td>{p.admitting ? '接受新入口' : '已停止新增'}</td>
                    )}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {editing !== undefined && (
        <EntryPublicationDialog
          workspace={workspace}
          catalog={catalog.data}
          publication={editing}
          onClose={() => setEditing(undefined)}
        />
      )}
      {deleting && (
        <Confirm
          title="删除入口地址发布"
          confirmLabel={remove.isPending ? '正在删除…' : '删除'}
          confirmDisabled={remove.isPending || deleting.active_entries > 0}
          onCancel={() => setDeleting(null)}
          onConfirm={() => remove.mutate(deleting)}
          body={
            <>
              {deleting.active_entries
                ? '仍有入口在用：请先从容器撤销入口，并等待入口机器确认清理。'
                : `删除后不能再从「${deleting.spec.name}」接入新入口。`}
              {remove.error && <ErrorBox error={remove.error} />}
            </>
          }
        />
      )}
    </section>
  );
}

function blankSpec(catalog: CvmNicCatalog | undefined): CvmEntryPublicationSpec {
  const provider = catalog?.providers.find(provider => provider.fresh && provider.capabilities.entries);
  const address = provider?.capabilities.addresses.find(ip => !ip.includes(':')) ?? '';
  return {
    name: '',
    provider_node: provider?.node_id ?? '',
    wireguard_transport: 'udp',
    addresses: address ? [{ address, kind: 'shared', port_start: 30000, port_end: 30999 }] : [],
    transports: [
      {
        kind: provider?.capabilities.transports.includes('wireguard') ? 'wireguard' : 'xray',
        listen_side: 'provider',
        endpoint: address,
        port_start: 44000,
        port_end: 44099,
        mtu: 1420,
      },
    ],
    grants: [],
    max_entries: 64,
  };
}

function EntryPublicationDialog({
  workspace,
  catalog,
  publication,
  onClose,
}: {
  workspace: CvmWorkspace;
  catalog: CvmNicCatalog | undefined;
  publication: CvmEntryPublicationView | null;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const [initial] = useState(() => structuredClone(publication?.spec ?? blankSpec(catalog)));
  const [spec, setSpec] = useState(initial);
  const [reservedDrafts, setReservedDrafts] = useState<Record<number, { tcp: string; udp: string }>>({});
  const reservedText = (index: number, protocol: 'tcp' | 'udp') =>
    reservedDrafts[index]?.[protocol] ?? rangesText(spec.addresses[index].reserved_ports?.[protocol] ?? []);
  const dirty = JSON.stringify(spec) !== JSON.stringify(initial) || Object.keys(reservedDrafts).length > 0;

  const [localError, setLocalError] = useState<string | null>(null);
  const operationId = useNicRequestId();
  useUnsavedChanges(dirty, '入口地址发布');
  const provider = catalog?.providers.find(provider => provider.node_id === spec.provider_node);
  const save = useMutation({
    mutationFn: (nextSpec: CvmEntryPublicationSpec) => {
      const body = { expected_revision: publication?.revision ?? 0, spec: nextSpec };
      return writeCvmEntryPublication(publication?.id ?? null, { ...body, request_id: operationId(body) });
    },
    onSuccess: () => {
      invalidateCvmChanges(client);
      onClose();
    },
  });
  const patch = (value: Partial<CvmEntryPublicationSpec>) => setSpec(old => ({ ...old, ...value }));
  const patchAddress = (index: number, value: Partial<CvmEntryAddressOffer>) =>
    patch({
      addresses: spec.addresses.map((offer, i) =>
        i === index ? ({ ...offer, ...value } as CvmEntryAddressOffer) : offer,
      ),
    });
  const reserveSsh = (index: number) => {
    try {
      const tcp = rangesText(
        parsePortRanges(
          [reservedText(index, 'tcp'), ...(provider?.capabilities.ssh_ports ?? []).map(String)]
            .filter(Boolean)
            .join(','),
        ),
      );
      setReservedDrafts(old => ({ ...old, [index]: { tcp, udp: reservedText(index, 'udp') } }));
      setLocalError(null);
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : '保留端口无效');
    }
  };
  const transport = spec.transports[0];
  const submit = () => {
    if (!spec.name.trim() || !spec.provider_node || !spec.addresses.length) {
      setLocalError('请填写名称、入口机器和至少一个地址。');
      return;
    }
    setLocalError(null);
    try {
      const normalized = {
        ...spec,
        addresses: spec.addresses.map((offer, index) => ({
          ...offer,
          reserved_ports: {
            tcp: parsePortRanges(reservedText(index, 'tcp')),
            udp: parsePortRanges(reservedText(index, 'udp')),
          },
        })),
      };
      save.mutate(uncertainCvmResult(save.error) && save.variables ? save.variables : normalized);
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : '保留端口无效');
    }
  };
  return (
    <NicDialog
      title={publication ? '修改入口地址' : '添加入口地址'}
      error={save.error ?? localError}
      onClose={onClose}
      pending={save.isPending}
      uncertain={uncertainCvmResult(save.error)}
      onSubmit={submit}
      dirty={dirty}
    >
      <p className="note">添加可分配给容器的访问地址，并授权使用者。保存后等待发布，用户再为容器选择地址和端口。</p>
      <div className="cvm-form-grid">
        <label>
          名称
          <input className="f" value={spec.name} maxLength={80} onChange={e => patch({ name: e.target.value })} />
        </label>
        <label>
          入口机器
          <select
            className="f"
            value={spec.provider_node}
            disabled={!!publication}
            onChange={e => {
              const next = catalog?.providers.find(item => item.node_id === e.target.value);
              const address = next?.capabilities.addresses.find(ip => !ip.includes(':')) ?? '';
              patch({
                provider_node: e.target.value,
                transports: spec.transports.map(offer => ({ ...offer, endpoint: address })),
              });
            }}
          >
            <option value="">选择机器</option>
            {catalog?.providers.map(item => (
              <option key={item.node_id} value={item.node_id} disabled={!item.fresh || !item.capabilities.entries}>
                {item.name}
                {!item.fresh ? ' · 能力已过期' : !item.capabilities.entries ? ' · Agent 尚不支持入口' : ''}
              </option>
            ))}
          </select>
        </label>
        <label>
          入口数上限
          <input
            className="f"
            type="number"
            min={1}
            max={4096}
            value={spec.max_entries}
            onChange={e => patch({ max_entries: Number(e.target.value) })}
          />
        </label>
      </div>
      <fieldset className="cvm-form-fieldset">
        <legend>地址</legend>
        {spec.addresses.map((offer, index) => (
          <div className="cvm-form-grid cvm-entry-address" key={index}>
            <label>
              机器上的地址
              <input
                className="f mono"
                list="entry-local-addresses"
                value={offer.address}
                onChange={e => patchAddress(index, { address: e.target.value.trim() })}
              />
            </label>
            <label>
              对外地址（可选）
              <input
                className="f mono"
                placeholder="一对一 NAT 时的公网地址"
                value={offer.public_address ?? ''}
                onChange={e => patchAddress(index, { public_address: e.target.value.trim() || null })}
              />
            </label>
            <label>
              方式
              <select
                className="f"
                value={offer.kind}
                onChange={e =>
                  patch({
                    addresses: spec.addresses.map((item, i) =>
                      i !== index
                        ? item
                        : e.target.value === 'shared'
                          ? {
                              address: item.address,
                              public_address: item.public_address,
                              reserved_ports: item.reserved_ports,
                              kind: 'shared',
                              port_start: 30000,
                              port_end: 30999,
                            }
                          : {
                              address: item.address,
                              public_address: item.public_address,
                              reserved_ports: item.reserved_ports,
                              kind: 'dedicated',
                            },
                    ),
                  })
                }
              >
                <option value="shared">共享地址 · 分配端口</option>
                <option value="dedicated">独占地址</option>
              </select>
            </label>
            {offer.kind === 'shared' && (
              <label>
                端口段
                <div className="cvm-port-pair">
                  <input
                    className="f"
                    aria-label="起始端口"
                    type="number"
                    min={1}
                    max={65535}
                    value={offer.port_start}
                    onChange={e => patchAddress(index, { port_start: Number(e.target.value) })}
                  />
                  <span>—</span>
                  <input
                    className="f"
                    aria-label="结束端口"
                    type="number"
                    min={1}
                    max={65535}
                    value={offer.port_end}
                    onChange={e => patchAddress(index, { port_end: Number(e.target.value) })}
                  />
                </div>
              </label>
            )}
            <div className="cvm-form-grid cvm-entry-reservations">
              {(['tcp', 'udp'] as const).map(protocol => (
                <label key={protocol}>
                  保留 {protocol.toUpperCase()} 端口
                  <input
                    className="f mono"
                    value={reservedText(index, protocol)}
                    maxLength={4096}
                    placeholder="22,443,8000-8100"
                    onChange={event =>
                      setReservedDrafts(old => ({
                        ...old,
                        [index]: {
                          tcp: reservedText(index, 'tcp'),
                          udp: reservedText(index, 'udp'),
                          [protocol]: event.target.value,
                        },
                      }))
                    }
                  />
                </label>
              ))}
              {!!provider?.capabilities.ssh_ports?.length && (
                <button type="button" className="btn small" onClick={() => reserveSsh(index)}>
                  保留检测到的 SSH 端口：{provider.capabilities.ssh_ports.join(', ')}
                </button>
              )}
              <p className="note">保留端口继续由入口机器使用，不转发到容器。填写端口列表或范围；不会自动保留端口。</p>
            </div>
            <button
              className="btn small"
              onClick={() => {
                const next: typeof reservedDrafts = {};
                for (const [key, value] of Object.entries(reservedDrafts)) {
                  const i = Number(key);
                  if (i !== index) next[i > index ? i - 1 : i] = value;
                }
                setReservedDrafts(next);
                patch({ addresses: spec.addresses.filter((_, i) => i !== index) });
              }}
            >
              移除地址
            </button>
          </div>
        ))}
        <datalist id="entry-local-addresses">
          {provider?.capabilities.addresses.map(address => (
            <option key={address} value={address} />
          ))}
        </datalist>
        <button
          className="btn"
          onClick={() => patch({ addresses: [...spec.addresses, { address: '', kind: 'dedicated' }] })}
        >
          ＋ 添加地址
        </button>
        <p className="note">
          共享地址按端口分配给多个容器；独占地址由一个容器持有，按所选协议与端口转发。需要继续在入口机器上使用的 SSH
          等服务端口应填入保留列表。地址仍保留在入口机器上，机器主动连接的回包和通道使用的端口不参与转发。
        </p>
      </fieldset>
      <label className="cvm-check">
        <input
          type="checkbox"
          checked={!!transport}
          onChange={e =>
            patch({
              wireguard_transport: 'udp',
              transports: e.target.checked
                ? [
                    {
                      kind: provider?.capabilities.transports.includes('wireguard') ? 'wireguard' : 'xray',
                      listen_side: 'provider',
                      endpoint: provider?.capabilities.addresses.find(ip => !ip.includes(':')) ?? '',
                      port_start: 44000,
                      port_end: 44099,
                      mtu: 1420,
                    },
                  ]
                : [],
            })
          }
        />
        允许转发到其他机器
      </label>
      {!transport && <p className="note">仅转发到入口机器上的实例，不建立跨机器通道。</p>}
      {transport && (
        <fieldset className="cvm-form-fieldset">
          <legend>跨机器传输</legend>
          <div className="cvm-form-grid">
            <label>
              传输方式
              <select
                className="f"
                value={transport.kind}
                onChange={e =>
                  patch({
                    wireguard_transport: 'udp',
                    transports: [{ ...transport, kind: e.target.value as 'xray' | 'wireguard', mtu: 1420 }],
                  })
                }
              >
                <option value="wireguard" disabled={!provider?.capabilities.transports.includes('wireguard')}>
                  WireGuard
                </option>
                <option value="xray" disabled={!provider?.capabilities.entry_xray}>
                  Xray
                </option>
              </select>
            </label>
            {transport.kind === 'wireguard' && (
              <label>
                WireGuard 传输
                <select
                  className="f"
                  value={spec.wireguard_transport}
                  onChange={e =>
                    patch({
                      wireguard_transport: e.target.value as 'udp' | 'phantun',
                      transports: [{ ...transport, mtu: e.target.value === 'phantun' ? 1380 : 1420 }],
                    })
                  }
                >
                  <option value="udp">UDP 直连</option>
                  <option value="phantun" disabled={!provider?.capabilities.entry_phantun}>
                    Phantun / FakeTCP
                  </option>
                </select>
              </label>
            )}
            <label>
              入口机器端点 IP
              <input
                className="f mono"
                value={transport.endpoint}
                onChange={e => patch({ transports: [{ ...transport, endpoint: e.target.value.trim() }] })}
              />
            </label>
            <label>
              通道端口范围
              <div className="cvm-port-pair">
                <input
                  className="f"
                  aria-label="通道起始端口"
                  type="number"
                  min={1}
                  max={65535}
                  value={transport.port_start}
                  onChange={e => patch({ transports: [{ ...transport, port_start: Number(e.target.value) }] })}
                />
                <span>—</span>
                <input
                  className="f"
                  aria-label="通道结束端口"
                  type="number"
                  min={1}
                  max={65535}
                  value={transport.port_end}
                  onChange={e => patch({ transports: [{ ...transport, port_end: Number(e.target.value) }] })}
                />
              </div>
            </label>
          </div>
          <p className="note">
            目标在另一台机器时，使用所选方式传输 所选协议的入口流量，每对机器共用一条通道；目标在本机时不经过通道。
            {transport.kind === 'xray'
              ? 'Xray 使用加密连接传输原始 IP 包。'
              : spec.wireguard_transport === 'phantun'
                ? 'Phantun 将 WireGuard 的 UDP 包封装为 FakeTCP。'
                : 'WireGuard 直接通过 UDP 连接。'}
            通道实际使用的端口不参与整 IP 转发，两端机器均须支持所选方式。
          </p>
          {!!publication?.active_entries && (
            <p className="note">更换传输方式前，请先撤销正在使用的跨机器入口并等待清理。</p>
          )}
        </fieldset>
      )}
      <fieldset className="cvm-network-grants">
        <legend>允许接入的用户</legend>
        {workspace.owners.map(owner => {
          const key = ownerKey(owner.tenant_id, owner.user_id);
          const checked = spec.grants.some(grant => ownerKey(grant.tenant_id, grant.user_id) === key);
          return (
            <label className="cvm-check" key={key}>
              <input
                type="checkbox"
                checked={checked}
                onChange={e =>
                  patch({
                    grants: e.target.checked
                      ? [...spec.grants, { tenant_id: owner.tenant_id, user_id: owner.user_id }]
                      : spec.grants.filter(grant => ownerKey(grant.tenant_id, grant.user_id) !== key),
                  })
                }
              />
              {owner.name}
            </label>
          );
        })}
      </fieldset>
      {localError && <p className="cvm-notice">{localError}</p>}
      {save.error && <ErrorBox error={save.error} />}
    </NicDialog>
  );
}
