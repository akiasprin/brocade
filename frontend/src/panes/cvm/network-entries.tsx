import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useId, useState } from 'react';
import {
  entryStateText,
  fetchCvmEntries,
  fetchCvmInstanceEntries,
  repointCvmEntry,
  type CvmEntry,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { ErrorBox, Loading } from '../../ui/bits';
import { Icon, PanelTitle } from '../../ui/icons';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import { entryMachines, entryText, nextFreePort } from './network-model';
import { entryPortText, entryTrafficText, entrySshPort } from './entry-ports';
import { initialEntryTraffic, entryTrafficFromDraft, entryUsageConflict, entryModesOverlap } from './entry-policy';
import { EntryTrafficEditor } from './entry-traffic-editor';
import { uncertainCvmResult } from './network-common';
import { editCount, type NetworkEdits } from './network-draft';
import { NetworkLogsButton } from './network-logs';
import { ENTRY_BOARD, invalidateCvmChanges, requestId } from './queries';

type Addition = NonNullable<NetworkEdits['entry_additions']>[number];
type RepointAttempt = { entryId: string; request: Parameters<typeof repointCvmEntry>[1] };

export function stagedEntryLabel(entry: Addition, address = entry.address) {
  return `${address} · ${entryTrafficText(entry.mode)}`;
}

export function NetworkEntries({
  instance,
  workspace,
  providerName,
  edits,
  onChange,
  writable,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  providerName: (node: string) => string;
  edits: NetworkEdits;
  onChange: (edits: NetworkEdits) => void;
  writable: boolean;
}) {
  const id = instance.desired.id;
  const entryKey = ['cvm-instance-entries', id] as const;
  const client = useQueryClient();
  const entries = useQuery({
    queryKey: entryKey,
    queryFn: () => fetchCvmInstanceEntries(id),
    enabled: !workspace.redacted,
    refetchInterval: 15_000,
  });
  const formId = useId();
  const [adding, setAdding] = useState(false);
  // 入口发布名称取自同一份入口总表（清单页也读它）；只有在添加时才需要高频刷新可选地址。
  const board = useQuery({
    queryKey: ENTRY_BOARD,
    queryFn: fetchCvmEntries,
    enabled: !workspace.redacted,
    refetchInterval: adding && writable ? 15_000 : 60_000,
  });
  const publications = board;
  const [selected, setSelected] = useState('');
  const [traffic, setTraffic] = useState(() => initialEntryTraffic());
  const additions = edits.entry_additions ?? [];
  const releases = edits.entry_releases ?? [];
  const [moving, setMoving] = useState<{ entry: CvmEntry; target: string } | null>(null);
  const move = useMutation({
    mutationFn: ({ entryId, request }: RepointAttempt) => repointCvmEntry(entryId, request),
    onSuccess: async entry => {
      await client.cancelQueries({ queryKey: entryKey, exact: true });
      client.setQueryData<CvmEntry[]>(entryKey, old => old?.filter(row => row.id !== entry.id));
      setMoving(null);
      invalidateCvmChanges(client);
    },
    onError: () => invalidateCvmChanges(client),
  });
  const uncertain = uncertainCvmResult(move.error);
  const retry = uncertain && move.variables?.entryId === moving?.entry.id ? move.variables : undefined;
  useUnsavedChanges(!!moving?.target || move.isPending || !!retry, '改派访问入口');
  const targets = !instance.user_id
    ? []
    : workspace.instances.filter(
        target =>
          target.desired.id !== id &&
          target.reserved &&
          target.desired.power !== 'deleted' &&
          target.tenant_id === instance.tenant_id &&
          target.user_id === instance.user_id,
      );
  const hasEdits = editCount(edits) > 0;
  const editingEntries = writable && !move.isPending && !retry;
  const current = moving && entries.data?.find(entry => entry.id === moving.entry.id);
  const stale = !!moving && (!current || current.revision !== moving.entry.revision || current.state === 'releasing');
  const selectedTarget = targets.find(target => target.desired.id === moving?.target);
  const canMove = editingEntries && !hasEdits && !stale && !!selectedTarget;
  const rows = board.data
    ? entryMachines(board.data, workspace.hosts).flatMap(machine =>
        machine.addresses
          .filter(
            row =>
              row.publication.admitting &&
              !row.publication.retired &&
              row.publication.spec.grants.some(
                grant => grant.tenant_id === instance.tenant_id && grant.user_id === instance.user_id,
              ),
          )
          .map(row => ({ ...row, machine: machine.name })),
      )
    : [];
  const row = rows.find(item => item.key === selected);
  const offer = row?.offer;
  let addition: Addition | undefined;
  let validation = '';
  if (row && offer) {
    try {
      const mode = entryTrafficFromDraft(traffic, offer);
      if (entryUsageConflict(mode, offer, row.usage)) throw new Error('所选地址或端口已被占用。');
      if (
        additions.some(
          entry =>
            entry.address === offer.address &&
            (entry.publication_id === row.publication.id ||
              rows.some(
                candidate =>
                  candidate.publication.id === entry.publication_id &&
                  candidate.publication.spec.provider_node === row.publication.spec.provider_node,
              )) &&
            entryModesOverlap(entry.mode, mode, offer),
        )
      )
        throw new Error('与草稿中已添加的入口冲突。');
      addition = { publication_id: row.publication.id, address: offer.address, mode };
    } catch (error) {
      validation = error instanceof Error ? error.message : String(error);
    }
  }
  const ssh = addition ? entrySshPort(addition.mode, offer?.reserved_ports, offer) : null;
  const visible = offer?.public_address ?? offer?.address;
  const publicationName = (id: string) => publications.data?.publications.find(item => item.id === id)?.spec.name;
  const closeAdding = () => {
    setAdding(false);
    setSelected('');
  };
  const count = (entries.data?.length ?? 0) + additions.length;
  return (
    <section className="panel titled cvm-net-panel" aria-label="访问入口">
      <header>
        <PanelTitle of="ingress">访问入口</PanelTitle>
        <span className="hint">{workspace.redacted ? '投递到主网卡' : `${count} 个 · 投递到主网卡`}</span>
        {writable && (
          <span className="cvm-panel-tools">
            <button
              className="btn sm"
              type="button"
              disabled={!!moving || adding}
              onClick={() => setAdding(true)}
              aria-expanded={adding}
            >
              <Icon of="plus" size={13} />
              添加入口
            </button>
          </span>
        )}
      </header>
      {workspace.redacted ? (
        <p className="note cvm-net-empty">访问地址已脱敏。</p>
      ) : (
        <>
          {entries.isPending && <Loading />}
          {entries.error && <ErrorBox error={entries.error} />}
          <ul className="cvm-nrows" aria-label="访问入口列表">
            {entries.data?.map(entry => {
              const removing = releases.some(change => change.id === entry.id);
              const provider = providerName(entry.provider_node);
              const row = entryRow(entry);
              const tone = removing
                ? 'st-pending'
                : entry.state === 'active'
                  ? 'st-succeeded'
                  : entry.state === 'failed' || entry.state === 'unauthorized'
                    ? 'st-halted'
                    : 'st-pending';
              return (
                <li className="cvm-nrow" key={entry.id} data-removing={removing || undefined}>
                  <span className={`cvm-nrow-lead${row.l3 ? ' l3' : ''}`}>{row.lead}</span>
                  <div className="cvm-nrow-main">
                    <span className="cvm-nrow-title mono">
                      {row.address}
                      {row.target && <span className="cvm-faint"> {row.target}</span>}
                    </span>
                    <small>{[publicationName(entry.publication_id), provider].filter(Boolean).join(' · ')}</small>
                    {entry.error && <p className="cvm-notice error">{entry.error}</p>}
                  </div>
                  <span className={`st ${tone}`}>{removing ? '待撤销' : entryStateText[entry.state]}</span>
                  <span className="cvm-nrow-acts">
                    <NetworkLogsButton
                      iconOnly
                      title="入口网络日志"
                      allowed={entry.can_read_logs ?? workspace.can_manage_network}
                      machines={[
                        { node_id: entry.provider_node, name: provider, role: '入口' },
                        {
                          node_id: instance.node_id,
                          name: workspace.hosts.find(host => host.node_id === instance.node_id)?.name,
                          role: '计算',
                        },
                      ]}
                      resources={[entry.id, ...(entry.tunnel_id ? [entry.tunnel_id] : [])]}
                    />
                    {writable && targets.length > 0 && !removing && entry.state !== 'releasing' && (
                      <button
                        className="btn sm cvm-row-btn"
                        type="button"
                        disabled={!editingEntries || hasEdits || !!moving}
                        title={hasEdits ? '请先保存或丢弃当前网络草稿' : '改投到同一使用者的其他容器'}
                        onClick={() => {
                          setAdding(false);
                          move.reset();
                          setMoving({ entry, target: '' });
                        }}
                      >
                        改派
                      </button>
                    )}
                    {editingEntries && entry.state !== 'releasing' && (
                      <button
                        className="btn sm cvm-row-btn"
                        type="button"
                        disabled={!!moving}
                        onClick={() =>
                          onChange({
                            ...edits,
                            entry_releases: removing
                              ? releases.filter(change => change.id !== entry.id)
                              : [...releases, { id: entry.id, expected_revision: entry.revision }],
                          })
                        }
                      >
                        {removing ? '取消撤销' : '撤销'}
                      </button>
                    )}
                  </span>
                </li>
              );
            })}
            {additions.map((entry, index) => (
              <li className="cvm-nrow" data-draft="" key={`${entry.publication_id}:${index}`}>
                <span className={`cvm-nrow-lead${entry.mode.kind === 'l3' ? ' l3' : ''}`}>{entryLead(entry.mode)}</span>
                <div className="cvm-nrow-main">
                  <span className="cvm-nrow-title mono">{stagedEntryLabel(entry)}</span>
                  <small>{publicationName(entry.publication_id) ?? '新访问入口'}</small>
                </div>
                <span className="st st-gold">未保存</span>
                {writable && (
                  <span className="cvm-nrow-acts">
                    <button
                      className="btn sm cvm-row-btn"
                      type="button"
                      onClick={() =>
                        onChange({
                          ...edits,
                          entry_additions: additions.filter((_, i) => i !== index),
                        })
                      }
                    >
                      移除草稿
                    </button>
                  </span>
                )}
              </li>
            ))}
            {entries.data?.length === 0 && !additions.length && !adding && (
              <li className="cvm-nrow cvm-nrow-empty">
                暂无访问入口{writable ? '。添加后可从外部经入口地址访问容器，SSH 入口会自动换算端口。' : ''}
              </li>
            )}
          </ul>
          {moving && (
            <form
              className="cvm-composer"
              aria-label="改派访问入口"
              onSubmit={event => {
                event.preventDefault();
                if (move.isPending || !writable) return;
                if (retry) {
                  move.mutate(retry);
                } else if (canMove) {
                  move.mutate({
                    entryId: moving.entry.id,
                    request: {
                      request_id: requestId(),
                      expected_revision: moving.entry.revision,
                      instance_id: moving.target,
                    },
                  });
                }
              }}
            >
              <div className="cvm-composer-head">
                <b>改派访问入口</b>
                <span className="mono">{entryText(moving.entry).text}</span>
              </div>
              <p className="note">只更改投递目标，地址与端口保持不变。改派单独保存，保存后进入顶部待发布列表。</p>
              <label className="cvm-field">
                目标容器
                <select
                  className="f"
                  value={moving.target}
                  disabled={!editingEntries}
                  onChange={event => setMoving({ ...moving, target: event.currentTarget.value })}
                >
                  <option value="">选择同一使用者的容器</option>
                  {moving.target && !selectedTarget && (
                    <option value={moving.target} disabled>
                      目标已不可用
                    </option>
                  )}
                  {targets.map(target => (
                    <option key={target.desired.id} value={target.desired.id}>
                      {target.desired.spec.name} · {providerName(target.node_id)}
                    </option>
                  ))}
                </select>
              </label>
              {move.error && <ErrorBox error={move.error} />}
              {retry && (
                <p className="cvm-notice" role="status">
                  尚未确认上次保存结果；请重试原改派。
                </p>
              )}
              {!retry && stale && (
                <p className="cvm-notice" role="status">
                  入口已变化，请取消后重新选择。
                </p>
              )}
              {!retry && hasEdits && <p className="cvm-notice">请先保存或丢弃当前网络草稿。</p>}
              <div className="cvm-composer-foot">
                <button
                  className="btn sm"
                  type="button"
                  disabled={move.isPending || !!retry}
                  onClick={() => {
                    move.reset();
                    setMoving(null);
                  }}
                >
                  取消改派
                </button>
                <button
                  className={`btn sm${move.isPending || !writable || (!retry && !canMove) ? '' : ' primary'}`}
                  type="submit"
                  disabled={move.isPending || !writable || (!retry && !canMove)}
                >
                  {move.isPending ? '正在保存…' : retry ? '重试原改派' : '保存改派'}
                </button>
              </div>
            </form>
          )}
          {adding && editingEntries && !moving && (
            <form
              className="cvm-composer"
              aria-label="添加访问入口"
              onSubmit={event => {
                event.preventDefault();
                if (!addition) return;
                onChange({ ...edits, entry_additions: [...additions, addition] });
                closeAdding();
              }}
            >
              <div className="cvm-composer-head">
                <b>新访问入口</b>
                <span className="note">→ {instance.desired.spec.name} 主网卡</span>
                <button
                  className="btn icon cvm-row-btn"
                  type="button"
                  aria-label="收起添加入口"
                  title="收起"
                  onClick={closeAdding}
                >
                  <Icon of="close" size={13} />
                </button>
              </div>
              {board.isPending && <Loading />}
              {board.error && <ErrorBox error={board.error} />}
              {board.data && !rows.length && <p className="note">当前使用者没有获授权且开放的入口地址。</p>}
              {!!rows.length && (
                <div className="cvm-picks" role="radiogroup" aria-label="入口地址">
                  {rows.map(row => {
                    const offer = row.offer;
                    const shared = offer.kind === 'shared';
                    return (
                      <label className="cvm-pick-row" data-on={selected === row.key || undefined} key={row.key}>
                        <input
                          type="radio"
                          name={`${formId}-entry-address`}
                          value={row.key}
                          checked={selected === row.key}
                          onChange={() => {
                            setSelected(row.key);
                            setTraffic(
                              initialEntryTraffic(
                                shared,
                                offer.kind === 'shared'
                                  ? String(nextFreePort(offer, row.usage, 'tcp', offer.port_start + 22) ?? '')
                                  : '',
                              ),
                            );
                          }}
                        />
                        <span className="cvm-pick-main">
                          <span>
                            <b>{row.publication.spec.name}</b>{' '}
                            <span className="mono cvm-faint">{offer.public_address ?? offer.address}</span>
                          </span>
                          <small>
                            {offer.kind === 'shared'
                              ? `共享端口 · ${offer.port_start}–${offer.port_end}`
                              : '整 IP · 含全部协议'}{' '}
                            · {row.machine}
                          </small>
                        </span>
                        <span className="cvm-pick-tag">{shared ? 'L4' : 'L3'}</span>
                      </label>
                    );
                  })}
                </div>
              )}
              {offer && <EntryTrafficEditor value={traffic} onChange={setTraffic} offer={offer} />}
              {addition && (
                <p className="cvm-composer-preview">
                  <Icon of="ingress" size={13} />
                  <span className="mono">{stagedEntryLabel(addition, visible)}</span>
                  {ssh !== null && <span className="cvm-faint">· SSH 端口 {ssh}</span>}
                </p>
              )}
              {validation && (
                <p className="cvm-notice" role="status">
                  {validation}
                </p>
              )}
              <div className="cvm-composer-foot">
                <span className="note">加入未保存变更，与其他网络修改一起检查和保存。</span>
                <button className="btn sm" type="button" onClick={closeAdding}>
                  取消
                </button>
                <button className={`btn sm${addition ? ' primary' : ''}`} type="submit" disabled={!addition}>
                  加入变更
                </button>
              </div>
            </form>
          )}
        </>
      )}
    </section>
  );
}

function entryLead(mode: CvmEntry['mode']) {
  if (mode.kind === 'l3') return 'L3';
  if (mode.kind === 'protocols') return 'IP';
  return mode.protocol === 'tcp_udp' ? 'TCP+UDP' : mode.protocol.toUpperCase();
}

function entryRow(entry: CvmEntry) {
  const lead = entryLead(entry.mode);
  if (entry.mode.kind === 'l3') return { lead, l3: true, address: entry.visible_address, target: '整 IP · 全部协议' };
  if (entry.mode.kind === 'protocols')
    return { lead, l3: true, address: entry.visible_address, target: entryTrafficText(entry.mode) };
  const listen = entryPortText(entry.mode);
  const target = entryPortText(entry.mode, true);
  return { lead, l3: false, address: `${entry.visible_address}:${listen}`, target: `→ ${target}` };
}
