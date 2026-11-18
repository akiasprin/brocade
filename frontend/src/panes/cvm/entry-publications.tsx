// 访问入口地址：管理员按机器发布可以投递到容器主网卡的地址。地址按 L4 协议端口或 L3 整 IP 转发给容器，
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
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { ENTRY_BOARD } from './board/inspector';
import { NIC_CATALOG, NicDialog, transportName, useNicRequestId } from './network-common';
import { ActionMenu } from '../../ui/choice-menu';
import { CapacityMeter } from './cards';
import { ownerKey } from './queries';

const visible = (offer: CvmEntryAddressOffer) => offer.public_address ?? offer.address;

export function EntryPublications({ workspace }: { workspace: CvmWorkspace }) {
  const client = useQueryClient();
  const board = useQuery({ queryKey: ENTRY_BOARD, queryFn: fetchCvmEntries, refetchInterval: 15_000 });
  const catalog = useQuery({ queryKey: NIC_CATALOG, queryFn: fetchCvmNicCatalog, refetchInterval: 15_000 });
  const [editing, setEditing] = useState<CvmEntryPublicationView | null | undefined>();
  const [deleting, setDeleting] = useState<CvmEntryPublicationView | null>(null);
  const operationId = useNicRequestId();
  const settled = () => void client.invalidateQueries({ queryKey: ENTRY_BOARD });
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
    <section className="cvm-network-lane cvm-entry-publications" aria-label="访问入口地址">
      <header className="cvm-lane-head">
        <h2>访问入口地址</h2>
        <small>
          {data.publications.length} 份发布 · {addresses} 个地址
        </small>
        {data.can_manage && (
          <span className="cvm-lane-aside">
            <button className="btn small" onClick={() => setEditing(null)}>
              ＋ 发布入口地址
            </button>
          </span>
        )}
      </header>
      <p className="cvm-lane-note">组合台左栏的来源：容器按整 IP 或协议端口接入，入口投递到主网卡，不占网卡槽位。</p>
      {board.error && <ErrorBox error={board.error} />}
      {admission.error && <ErrorBox error={admission.error} />}
      {!data.publications.length ? (
        <EmptyState icon="forwarding" title="暂无入口地址">
          {data.can_manage
            ? '选择已升级 Agent 的机器，发布共享地址的端口段或独立地址，并授权给使用者。'
            : '管理员发布入口地址并授权后，就可以在组合台为容器接入。'}
        </EmptyState>
      ) : (
        <div className="cvm-network-cards">
          {data.publications.map(p => {
            const kinds = new Set(p.spec.addresses.map(offer => offer.kind));
            return (
              <article className="cvm-tile cvm-entry-card" key={p.id} aria-label={p.spec.name}>
                <div className="cvm-pub-head">
                  <h3>{p.spec.name}</h3>
                  <span className="cz-kind">
                    {kinds.size > 1 ? '端口段与整 IP' : kinds.has('shared') ? '共享端口段' : '整 IP'}
                  </span>
                  {!p.admitting && <span className="st st-pending">已停止新增</span>}
                </div>
                <p className="cvm-pub-sub">
                  {p.provider_name} · 规格 v{p.revision}
                  {p.spec.transports.length
                    ? ` · ${[...new Set(p.spec.transports.map(t => transportName[t.kind]))].join(' / ')} 承载`
                    : ' · 本机投递'}
                </p>
                <div className="cvm-pub-rows">
                  {p.spec.addresses.map((offer, index) => (
                    <div className="cvm-pub-row" key={offer.address}>
                      <span className={`cz-layer ${offer.kind === 'shared' ? 'L4' : 'L3'}`}>
                        {offer.kind === 'shared' ? 'L4' : 'L3'}
                      </span>
                      <code>{visible(offer)}</code>
                      {offer.kind === 'shared' && (
                        <span className="cvm-pub-range">
                          {offer.port_start}–{offer.port_end}
                        </span>
                      )}
                      <span className="cvm-pub-use">
                        {offer.kind === 'shared'
                          ? `${p.usage[index]?.ports.length ?? 0} 段占用`
                          : p.usage[index]?.whole_taken
                            ? '已分配'
                            : '空闲'}
                      </span>
                    </div>
                  ))}
                </div>
                <div className="cvm-pub-figure">
                  <span>入口</span>
                  <CapacityMeter used={p.active_entries} total={p.spec.max_entries} />
                  <span>
                    <b>{p.active_entries}</b> / {p.spec.max_entries}
                  </span>
                </div>
                {data.can_manage && (
                  <footer className="cvm-pub-foot">
                    <button className="btn small" disabled={admission.isPending} onClick={() => setEditing(p)}>
                      修改
                    </button>
                    <label className="switch cvm-admission-switch">
                      <span className="switch-label">接受新入口</span>
                      <input
                        type="checkbox"
                        role="switch"
                        checked={p.admitting}
                        aria-checked={p.admitting}
                        aria-label={`${p.spec.name} 接受新入口`}
                        disabled={admission.isPending}
                        onChange={() => admission.mutate(p)}
                      />
                      <span className="switch-ui" aria-hidden="true">
                        <span />
                      </span>
                    </label>
                    <ActionMenu
                      label={`${p.spec.name} 更多操作`}
                      items={[
                        {
                          label: '删除',
                          note: p.active_entries ? '仍有入口在用，需先在组合台撤销' : '删除这份入口地址发布',
                          danger: true,
                          onSelect: () => setDeleting(p),
                        },
                      ]}
                    />
                  </footer>
                )}
              </article>
            );
          })}
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
                ? '仍有入口在用：请先在组合台撤销，并等待入口机器确认清理。'
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
  const [localError, setLocalError] = useState<string | null>(null);
  const operationId = useNicRequestId();
  useUnsavedChanges(JSON.stringify(spec) !== JSON.stringify(initial), '入口地址发布');
  const provider = catalog?.providers.find(provider => provider.node_id === spec.provider_node);
  const save = useMutation({
    mutationFn: () => {
      const body = { expected_revision: publication?.revision ?? 0, spec };
      return writeCvmEntryPublication(publication?.id ?? null, { ...body, request_id: operationId(body) });
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ENTRY_BOARD });
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
  const transport = spec.transports[0];
  const submit = () => {
    if (!spec.name.trim() || !spec.provider_node || !spec.addresses.length) {
      setLocalError('请填写名称、入口机器和至少一个地址。');
      return;
    }
    setLocalError(null);
    save.mutate();
  };
  return (
    <NicDialog
      title={publication ? '修改入口地址' : '发布入口地址'}
      onClose={onClose}
      pending={save.isPending}
      onSubmit={submit}
      submit={publication ? '保存新版本' : '发布'}
    >
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
          <div className="cvm-form-grid" key={index}>
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
                              kind: 'shared',
                              port_start: 30000,
                              port_end: 30999,
                            }
                          : { address: item.address, public_address: item.public_address, kind: 'dedicated' },
                    ),
                  })
                }
              >
                <option value="shared">按端口 · L4 转发</option>
                <option value="dedicated">整 IP · L3 转发</option>
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
            <button
              className="btn small"
              onClick={() => patch({ addresses: spec.addresses.filter((_, i) => i !== index) })}
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
          L4 按协议端口转发；L3 将该 IP 的新入站流量转发给一个实例，可以使用机器自身 IP，包含 SSH 等服务端口。 IP
          保留在入口机器上；机器主动连接的回包和通道自身使用的端口不参与转发。
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
                  value={spec.wireguard_transport ?? 'udp'}
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
            目标在另一台机器时，使用所选方式传输 L3／L4 转发流量，每对机器共用一条通道；目标在本机时不经过通道。
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
