// 访问入口地址：管理员按机器发布可以投递到容器主网卡的地址。共享地址按协议端口分给多个容器，
// 独立地址整 IP 交给一个容器；跨宿主的入口经 WireGuard 机器对通道送到容器所在宿主。
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
import { NIC_CATALOG, NicDialog, useNicRequestId } from './network-common';
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
  return (
    <div className="cvm-entry-publications">
      <div className="cvm-network-heading">
        <div>
          <b>访问入口地址</b>
          <p className="note">发布机器上的地址，容器在组合台按整 IP 或协议端口接入，不占网卡槽位。</p>
        </div>
        {data.can_manage && (
          <button className="btn primary" onClick={() => setEditing(null)}>
            ＋ 发布入口地址
          </button>
        )}
      </div>
      {board.error && <ErrorBox error={board.error} />}
      {admission.error && <ErrorBox error={admission.error} />}
      {!data.publications.length ? (
        <EmptyState icon="forwarding" title="暂无入口地址">
          {data.can_manage
            ? '选择已升级 Agent 的机器，发布共享地址的端口段或独立地址，并授权给使用者。'
            : '管理员发布入口地址并授权后，就可以在组合台为容器接入。'}
        </EmptyState>
      ) : (
        <div className="node-card-grid cvm-entry-grid">
          {data.publications.map(p => (
            <article className="node-card cvm-entry-card tone-ok" key={p.id}>
              <header>
                <div>
                  <h3>{p.spec.name}</h3>
                  <small>
                    {p.provider_name} · 规格版本 {p.revision}
                  </small>
                </div>
                <span className={`st ${p.admitting ? 'st-succeeded' : 'st-pending'}`}>
                  {p.admitting ? '接受新入口' : '已停止新增'}
                </span>
              </header>
              <dl className="cvm-facts">
                {p.spec.addresses.map((offer, index) => (
                  <div key={offer.address}>
                    <dt className="mono">{visible(offer)}</dt>
                    <dd>
                      {offer.kind === 'shared'
                        ? `共享 · ${offer.port_start}–${offer.port_end} · ${p.usage[index]?.ports.length ?? 0} 段占用`
                        : p.usage[index]?.whole_taken
                          ? '独立 · 已分配'
                          : '独立 · 空闲'}
                    </dd>
                  </div>
                ))}
              </dl>
              <div className="cvm-network-figures">
                <span>
                  <b>{p.active_entries}</b> / {p.spec.max_entries} 个入口
                </span>
              </div>
              {data.can_manage && (
                <footer>
                  <button className="btn small" disabled={admission.isPending} onClick={() => setEditing(p)}>
                    修改
                  </button>
                  <label className="switch cvm-admission-switch">
                    <span className="switch-label">接受新入口</span>
                    <input
                      type="checkbox"
                      role="switch"
                      checked={p.admitting}
                      disabled={admission.isPending}
                      onChange={() => admission.mutate(p)}
                    />
                  </label>
                  <button className="btn small danger" onClick={() => setDeleting(p)}>
                    删除
                  </button>
                </footer>
              )}
            </article>
          ))}
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
    </div>
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
      { kind: 'wireguard', listen_side: 'provider', endpoint: address, port_start: 44000, port_end: 44099, mtu: 1420 },
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
    if (!spec.name.trim() || !spec.provider_node || !spec.addresses.length || !transport) {
      setLocalError('请填写名称、入口机器、至少一个地址和通道。');
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
                <option value="shared">共享 · 按协议端口</option>
                <option value="dedicated">独立 · 整 IP</option>
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
          共享地址须是机器网卡上的实际地址；宿主正在监听、代理与组网保留的端口不会分配。独立地址整 IP
          交给一个容器，不能是机器的管理地址。
        </p>
      </fieldset>
      {transport && (
        <fieldset className="cvm-form-fieldset">
          <legend>跨宿主通道 · WireGuard</legend>
          <div className="cvm-form-grid">
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
          <p className="note">容器在别的宿主时，宿主主动连接入口机器的这个端点；每对机器共用一条通道。</p>
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
