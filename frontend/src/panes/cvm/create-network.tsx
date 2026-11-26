import { useQuery } from '@tanstack/react-query';
import {
  fetchCvmEntries,
  fetchCvmNicCatalog,
  type CvmEntryMode,
  type CvmEntryProtocol,
  type CvmHost,
  type CvmInstance,
  type CvmNicTransport,
} from '../../api';
import { ErrorBox } from '../../ui/bits';
import { ENTRY_BOARD } from './queries';
import { portIntervalText } from './entry-ports';
import { initialEntryTraffic, entryTrafficFromDraft, entryUsageConflict, type EntryTrafficDraft } from './entry-policy';
import { EntryTrafficEditor } from './entry-traffic-editor';
import { NIC_CATALOG, transportName } from './network-common';

export interface CreateNetworkTemplate {
  entry?: { publication_id: string; address: string; mode: CvmEntryMode };
  nics: { publication_id: string; transport: CvmNicTransport }[];
}

type NicChoice = CreateNetworkTemplate['nics'][number];
interface EntryDraft {
  traffic?: EntryTrafficDraft;
  selection: string;
  protocol: CvmEntryProtocol;
  listen: string;
  target: string;
}
export interface CreateNetworkDraft {
  entry: EntryDraft;
  nics: NicChoice[];
}

const emptyEntry: EntryDraft = { selection: '', protocol: 'tcp', listen: '', target: '22' };
export function initialCreateNetwork(source?: CvmInstance): CreateNetworkDraft {
  return {
    entry: emptyEntry,
    // Reuse only publication + transport. The server allocates fresh NIC IDs, MACs, and addresses.
    nics:
      source?.desired.spec.interfaces.flatMap(nic =>
        nic.purpose === 'business' && nic.allocated
          ? [{ publication_id: nic.allocated.publication_id, transport: nic.allocated.transport }]
          : [],
      ) ?? [],
  };
}

const nicKey = ({ publication_id, transport }: NicChoice) => JSON.stringify([publication_id, transport]);
export function useCreateNetwork({
  enabled,
  host,
  tenant,
  user,
  draft,
}: {
  enabled: boolean;
  host?: CvmHost;
  tenant: string;
  user: string | null;
  draft: CreateNetworkDraft;
}) {
  const board = useQuery({ queryKey: ENTRY_BOARD, queryFn: fetchCvmEntries, enabled: enabled && !!user });
  const catalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: enabled && (!!user || !!draft.nics.length),
  });
  const inventory = catalog.data?.providers;
  const provider = (node: string) => inventory?.find(item => item.node_id === node);
  const validProvider = (
    node: string,
    canUse: (caps: NonNullable<ReturnType<typeof provider>>['capabilities']) => boolean,
  ) => {
    // Scoped editors do not receive administrator-only capability inventories.
    if (!catalog.data?.can_manage) return true;
    const item = provider(node);
    return !!item?.fresh && !item.capabilities.error && canUse(item.capabilities);
  };
  const granted = (grants: { tenant_id: string; user_id: string }[]) =>
    !!user && grants.some(item => item.tenant_id === tenant && item.user_id === user);
  const entryOptions =
    host?.capabilities.primary_ip_delivery && !board.isError
      ? (board.data?.publications ?? [])
          .filter(
            item =>
              item.admitting &&
              !item.retired &&
              item.active_entries < item.spec.max_entries &&
              granted(item.spec.grants) &&
              validProvider(item.spec.provider_node, caps => !!caps.entries && !!caps.entry_protocol_rules) &&
              (item.spec.provider_node === host.node_id ||
                validProvider(host.node_id, caps => !!caps.entries && !!caps.entry_protocol_rules)),
          )
          .flatMap(publication =>
            publication.spec.addresses.flatMap(offer => {
              const usage = publication.usage.find(item => item.address === (offer.public_address ?? offer.address));
              if (usage?.whole_taken || (offer.kind === 'dedicated' && usage?.ports.length)) return [];
              if (
                offer.kind === 'dedicated' &&
                !validProvider(
                  publication.spec.provider_node,
                  caps => !caps.addresses.includes(offer.address) || !!caps.entry_host_l3,
                )
              )
                return [];
              return [{ key: JSON.stringify([publication.id, offer.address]), publication, offer, usage }];
            }),
          )
      : [];
  const nicSlots = Math.max(0, (host?.capabilities.max_interfaces ?? 1) - 1);
  const attachmentsByPublication = new Map<string, Set<string>>();
  for (const attachment of catalog.data?.attachments ?? []) {
    const ids = attachmentsByPublication.get(attachment.publication_id) ?? new Set<string>();
    ids.add(attachment.interface_id);
    attachmentsByPublication.set(attachment.publication_id, ids);
  }
  const remainingNics = new Map(
    (catalog.data?.publications ?? []).map(p => [
      p.id,
      p.spec.limits.max_attachments - (attachmentsByPublication.get(p.id)?.size ?? 0),
    ]),
  );
  const nicOptions =
    host && nicSlots && !catalog.isError
      ? (catalog.data?.publications ?? [])
          .filter(p => p.admitting && !p.retired && granted(p.spec.grants) && (remainingNics.get(p.id) ?? 0) > 0)
          .flatMap(p =>
            [...new Set(p.spec.transports.map(offer => offer.kind))]
              .filter(
                kind =>
                  validProvider(host.node_id, caps => caps.consume && caps.transports.includes(kind)) &&
                  validProvider(p.spec.provider_node, caps => caps.provide && caps.transports.includes(kind)),
              )
              .map(transport => ({ publication_id: p.id, transport })),
          )
      : [];
  const nicLabel = (choice: NicChoice) => {
    const name = catalog.data?.publications.find(item => item.id === choice.publication_id)?.spec.name;
    return `${name ?? choice.publication_id} · ${transportName[choice.transport]}`;
  };
  const entryOption = entryOptions.find(item => item.key === draft.entry.selection);
  const availableNics = new Set(nicOptions.map(nicKey));
  const { entry } = draft;
  let network: CreateNetworkTemplate | null = null;
  let problem: string | null = null;
  if (entry.selection || draft.nics.length) {
    try {
      if (!user) throw new Error('请为新容器选择获授权的使用者，或移除待配置的网络项。');
      if (draft.nics.length > nicSlots) throw new Error(`所选宿主最多可添加 ${nicSlots} 个出站方式。`);
      if (draft.nics.some(choice => !availableNics.has(nicKey(choice))))
        throw new Error('有出站方式对当前宿主或使用者不可用，请重新选择或移除。');
      for (const id of new Set(draft.nics.map(choice => choice.publication_id))) {
        if (draft.nics.filter(choice => choice.publication_id === id).length > (remainingNics.get(id) ?? 0))
          throw new Error('所选出站方式数量超过对应网络的可用数量。');
      }
      network = { nics: draft.nics };
      if (entry.selection) {
        if (!entryOption) throw new Error('所选入口地址已不可用，请重新选择。');
        const { offer, publication, usage } = entryOption;
        const mode = entryTrafficFromDraft(
          entry.traffic ?? initialEntryTraffic(offer.kind === 'shared', entry.listen),
          offer,
        );
        if (entryUsageConflict(mode, offer, usage)) throw new Error('入口地址或端口已占用或等待清理。');
        network.entry = { publication_id: publication.id, address: offer.address, mode };
      }
    } catch (error) {
      network = null;
      problem = error instanceof Error ? error.message : '网络配置无法校验。';
    }
  }
  return {
    board,
    catalog,
    entryOptions,
    entryOption,
    nicOptions,
    availableNics,
    nicSlots,
    nicLabel,
    network,
    problem,
  };
}

type Choices = ReturnType<typeof useCreateNetwork>;
export function CreateNetworkFields({
  choices,
  draft,
  onChange,
  user,
}: {
  choices: Choices;
  draft: CreateNetworkDraft;
  onChange: (draft: CreateNetworkDraft) => void;
  user: string | null;
}) {
  const { board, catalog, entryOptions, entryOption, nicOptions, availableNics, nicSlots, nicLabel, problem } = choices;
  const { entry, nics } = draft;
  const updateEntry = (next: Partial<EntryDraft>) => onChange({ ...draft, entry: { ...entry, ...next } });
  return (
    <div className="cvm-create-network-fields">
      {!user && (
        <p className="note">选择获授权的使用者后，可以选配入站方式和出站方式。也可以只创建容器，稍后配置网络。</p>
      )}
      {(user || nics.length > 0) && (board.isLoading || catalog.isLoading) && (
        <p className="note" role="status">
          正在读取网络资源…
        </p>
      )}
      {user && board.error && <ErrorBox error={board.error} />}
      {(user || nics.length > 0) && catalog.error && <ErrorBox error={catalog.error} />}
      {(user || entry.selection) && (
        <div className="cvm-create-network-choice">
          <label className="cvm-field">
            入站方式
            <select
              className="f"
              value={entry.selection}
              onChange={event => {
                const next = entryOptions.find(item => item.key === event.target.value);
                onChange({
                  ...draft,
                  entry: {
                    ...emptyEntry,
                    selection: event.target.value,
                    traffic: initialEntryTraffic(next?.offer.kind === 'shared'),
                  },
                });
              }}
            >
              <option value="">不添加入站方式</option>
              {entry.selection && !entryOption && (
                <option value={entry.selection} disabled>
                  上次选择的地址已不可用
                </option>
              )}
              {entryOptions.map(({ key, offer, publication }) => (
                <option key={key} value={key}>
                  {publication.spec.name} · {offer.public_address ?? offer.address} ·{' '}
                  {offer.kind === 'dedicated' ? '整 IP' : `端口 ${portIntervalText(offer.port_start, offer.port_end)}`}
                </option>
              ))}
            </select>
          </label>
          {entryOption && (
            <EntryTrafficEditor
              offer={entryOption.offer}
              value={entry.traffic ?? initialEntryTraffic(entryOption.offer.kind === 'shared', entry.listen)}
              onChange={traffic => updateEntry({ traffic })}
            />
          )}
          {user && !board.isLoading && !board.error && !entryOptions.length && (
            <p className="note">当前宿主和使用者没有可选的入口地址。</p>
          )}
        </div>
      )}
      {(user || nics.length > 0) && (
        <div className="cvm-create-network-choice">
          <label className="cvm-field">
            出站方式
            <select
              className="f"
              value=""
              disabled={nics.length >= nicSlots || !nicOptions.length}
              onChange={event => {
                const next = nicOptions.find(choice => nicKey(choice) === event.target.value);
                if (next) onChange({ ...draft, nics: [...nics, next] });
              }}
            >
              <option value="">添加出站方式</option>
              {nicOptions.map(choice => (
                <option key={nicKey(choice)} value={nicKey(choice)}>
                  {nicLabel(choice)}
                </option>
              ))}
            </select>
          </label>
          {nics.length > 0 && (
            <ul className="cvm-create-nic-choices">
              {nics.map((choice, index) => (
                <li key={`${nicKey(choice)}:${index}`}>
                  <span>
                    {nicLabel(choice)}
                    {!availableNics.has(nicKey(choice)) && <small> · 待确认可用性</small>}
                  </span>
                  <button
                    type="button"
                    className="btn small"
                    aria-label={`移除出站方式 ${index + 1}：${nicLabel(choice)}`}
                    onClick={() => onChange({ ...draft, nics: nics.filter((_, itemIndex) => index !== itemIndex) })}
                  >
                    移除
                  </button>
                </li>
              ))}
            </ul>
          )}
          {user && !catalog.isLoading && !catalog.error && !nicOptions.length && (
            <p className="note">
              {nicSlots === 0 ? '当前宿主不能添加更多出站方式。' : '当前宿主和使用者没有可用的出站方式。'}
            </p>
          )}
        </div>
      )}
      {problem && <p className="note">{problem}</p>}
    </div>
  );
}
