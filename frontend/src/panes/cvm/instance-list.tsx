import './instance-list.css';
import { useQuery } from '@tanstack/react-query';
import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent } from 'react';
import {
  fetchCvmChainCatalog,
  fetchCvmEntries,
  fetchCvmNicCatalog,
  fetchCvmReadings,
  type CvmCardReading,
  type CvmEntry,
  type CvmHost,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { navigate } from '../../forge/route';
import { EmptyState, ErrorBox } from '../../ui/bits';
import { menuArrowKey, MenuLayer } from '../../ui/choice-menu';
import { useNow } from '../../ui/clock';
import { CopyButton } from '../../ui/copy-button';
import { Icon } from '../../ui/icons';
import { useMetricQuery } from '../../ui/metric-cadence';
import { AccessDialog, canEditInstanceAccess } from './access';
import { entryText } from './network-model';
import { compactSpec, entryChip, exitChips, NetChipView, type NetChip } from './network-chips';
import { CapacityMeter, percent, reservationUsage, shareOf } from './cards';
import { entrySshPort } from './entry-ports';
import { entryContainsPort } from './entry-policy';
import {
  actionBlocked,
  attentionActionText,
  ListCommandDialog,
  listActionText,
  needsAttention,
  powerActions,
  useListCommandRecovery,
  type ListCommand,
  type ListCommandRecovery,
} from './instance-list-actions';
import { NIC_CATALOG } from './network-common';
import { AppliedState, instanceLamp } from './presentation';
import {
  capacity,
  GIB,
  guestTerminalAccess,
  MIB,
  hasPendingCvmChange,
  ownerKey,
  stackHosts,
  statusText,
  supportsCreate,
} from './queries';
import { cvmTerminalUrl, openCvmTerminal } from './terminal-access';

const filters = [
  { key: 'all', label: '全部', matches: () => true },
  { key: 'attention', label: '需处理', matches: needsAttention },
  { key: 'pending', label: '待发布', matches: hasPendingCvmChange },
  { key: 'running', label: '运行中', matches: (item: CvmInstance) => item.report?.power === 'running' },
  { key: 'stopped', label: '已关机', matches: (item: CvmInstance) => item.report?.power === 'stopped' },
] as const;

function openDetail(instance: CvmInstance, tab?: 'network') {
  navigate('cvm', { p: 'instance', id: instance.desired.id, ...(tab ? { tab } : {}) });
}

function activateRow(event: MouseEvent<HTMLTableRowElement>, instance: CvmInstance) {
  if (event.target instanceof Element && !event.target.closest('button, a, input, label, [role="dialog"]'))
    openDetail(instance);
}

function SelectVisible({ total, selected, onChange }: { total: number; selected: number; onChange: () => void }) {
  const ref = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    if (ref.current) ref.current.indeterminate = selected > 0 && selected < total;
  }, [selected, total]);
  return (
    <input
      ref={ref}
      type="checkbox"
      aria-label="选择当前清单中的容器"
      checked={total > 0 && total === selected}
      onChange={onChange}
    />
  );
}

function sshCommand(instance: CvmInstance, host: CvmHost | undefined, entries: CvmEntry[]) {
  const user =
    instance.desired.spec.execution.kind === 'baremetal' ? host?.capabilities.host_system?.terminal_user : 'cube';
  if (!user || !/^[a-zA-Z0-9_-]{1,32}$/.test(user)) return null;
  for (const entry of entries) {
    if (entry.state !== 'active') continue;
    const port = entrySshPort(entry.mode, entry.reserved_ports);
    if (port !== null && /^[a-fA-F0-9:.]+$/.test(entry.visible_address))
      return `ssh -l ${user} -p ${port} -- ${entry.visible_address}`;
  }
  const address = instance.report?.access_address;
  if (
    instance.status === 'applied' &&
    instance.desired.power === 'running' &&
    address &&
    /^[a-fA-F0-9:.]+$/.test(address)
  )
    return `ssh -l ${user} -p 22 -- ${address}`;
  return null;
}

const powerIcon = (action: (typeof powerActions)[number]) =>
  action === 'start' ? 'play' : action === 'stop' ? 'pause' : 'refresh';

function InstanceActions({
  instance,
  host,
  workspace,
  entries,
  onCommand,
  commandLocked,
}: {
  instance: CvmInstance;
  host?: CvmHost;
  workspace: CvmWorkspace;
  entries: CvmEntry[];
  onCommand: (command: ListCommand) => void;
  commandLocked: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [accessOpen, setAccessOpen] = useState(false);
  const [terminalBlocked, setTerminalBlocked] = useState(false);
  const more = useRef<HTMLButtonElement>(null);
  if (workspace.redacted || (!workspace.can_use && !workspace.can_write)) return null;
  const terminal = guestTerminalAccess(instance, host, workspace.can_use);
  const deleted = instance.desired.power === 'deleted';
  const baremetal = instance.desired.spec.execution.kind === 'baremetal';
  const name = instance.desired.spec.name;
  const copy = workspace.can_use && !deleted && sshCommand(instance, host, entries);
  const primary = instance.report?.power === 'running' ? 'stop' : 'start';
  const power = powerActions.filter(
    action =>
      !commandLocked &&
      (action === primary || action === 'restart') &&
      !actionBlocked(action, instance, host, workspace),
  );
  const recovery = !commandLocked && !actionBlocked('retry', instance, host, workspace);
  const recoveryLabel = attentionActionText(instance);
  const remove = baremetal ? 'release_host' : 'delete';
  const canRemove = !commandLocked && !actionBlocked(remove, instance, host, workspace);
  const canAccess = !commandLocked && canEditInstanceAccess(instance, workspace);
  const canCopyNew = workspace.can_write && !commandLocked && !baremetal && !deleted && needsAttention(instance);
  const canUseMenu = !!power.length || recovery || canRemove || !!copy || terminal.enabled || canAccess || !baremetal;
  const action = (action: ListCommand['action'], button?: HTMLButtonElement) => {
    setOpen(false);
    // Items inside the menu unmount when selected; keep the confirmation attached to the row.
    const anchor = button
      ? {
          get current() {
            return button.isConnected && button.getClientRects().length ? button : more.current;
          },
        }
      : undefined;
    onCommand({ action, instances: [instance], anchor });
  };
  const openTerminal = () => setTerminalBlocked(!openCvmTerminal(instance.desired.id));
  const item = (label: string, note: string, onSelect: (button: HTMLButtonElement) => void, danger = false) => (
    <button
      type="button"
      role="menuitem"
      key={label}
      className={danger ? 'danger' : undefined}
      onClick={event => {
        setOpen(false);
        onSelect(event.currentTarget);
      }}
    >
      <b>{label}</b>
      <small>{note}</small>
    </button>
  );
  return (
    <div className="cvm-list-actions">
      {recovery ? (
        <button
          className="btn sm cvm-list-attention-action"
          type="button"
          aria-haspopup="dialog"
          onClick={event => action('retry', event.currentTarget)}
        >
          <Icon of={instance.status === 'failed' ? 'refresh' : 'diag'} size={13} />
          {recoveryLabel}
        </button>
      ) : (
        <div className="cvm-list-quick">
          {power.map(kind => (
            <button
              className="btn icon cvm-row-btn"
              key={kind}
              type="button"
              aria-label={listActionText[kind]}
              title={listActionText[kind]}
              aria-haspopup="dialog"
              onClick={event => action(kind, event.currentTarget)}
            >
              <Icon of={powerIcon(kind)} size={14} />
            </button>
          ))}
          {terminal.enabled && !deleted && (
            <button
              className="btn icon cvm-row-btn"
              type="button"
              aria-label="终端"
              title={`在新页面打开 ${name} 的终端`}
              onClick={openTerminal}
            >
              <Icon of="nodes" size={14} />
            </button>
          )}
          {copy && <CopyButton className="btn icon cvm-row-btn" iconOnly label="复制 SSH 命令" text={copy} />}
        </div>
      )}
      {canUseMenu && (
        <button
          ref={more}
          className="rule-machine-more cvm-list-more"
          type="button"
          aria-label={`${name} 更多操作`}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen(true)}
        >
          <Icon of="more" size={14} />
        </button>
      )}
      {terminalBlocked && (
        <a
          className="btn sm cvm-list-blocked"
          href={cvmTerminalUrl(instance.desired.id)}
          target="_blank"
          rel="noopener"
        >
          新页面未能打开，请手动打开
        </a>
      )}
      {open && (
        <MenuLayer anchor={more} label={`${name} 更多操作`} action onClose={() => setOpen(false)}>
          <div
            className="dialog-surface choice-menu machine-action-menu"
            role="menu"
            aria-label={`${name} 更多操作`}
            onKeyDown={menuArrowKey}
          >
            {power.map(kind =>
              item(
                listActionText[kind],
                kind === 'restart' ? '保留系统盘与地址，连接短暂中断' : '保存为待发布变更',
                button => action(kind, button),
              ),
            )}
            {recovery && item(recoveryLabel, '沿用原任务身份核查或重试', button => action('retry', button))}
            {terminal.enabled && !deleted && item('终端', '在新页面打开容器终端', openTerminal)}
            {copy && (
              <CopyButton role="menuitem" text={copy} label="复制 SSH 命令" successLabel="已复制 SSH 命令">
                {(_, label) => (
                  <>
                    <b>{label}</b>
                    <small className="mono">{copy}</small>
                  </>
                )}
              </CopyButton>
            )}
            {canAccess &&
              item('使用授权', instance.user_id ? '收回或查看授权' : '授权给同租户用户', () => setAccessOpen(true))}
            {!baremetal && item('查看网络', '入站方式、出站方式与路由', () => openDetail(instance, 'network'))}
            {canCopyNew &&
              item('复制为新容器', '沿用镜像与规格填写创建表单', () =>
                navigate('cvm', { p: 'new', copy: instance.desired.id }),
              )}
            {canRemove &&
              item(
                listActionText[remove],
                baremetal ? '撤销受管网络与终端，保留系统和磁盘' : '销毁系统盘并释放网络与分配',
                () => action(remove),
                true,
              )}
          </div>
        </MenuLayer>
      )}
      {accessOpen && <AccessDialog instance={instance} workspace={workspace} onClose={() => setAccessOpen(false)} />}
    </div>
  );
}

function hostState(host?: CvmHost) {
  if (!host) return '未取得宿主资料';
  if (host.error) return '执行能力不可用';
  if (!host.fresh) return '能力已过期';
  if (host.capabilities.baremetal) return 'Agent 需升级';
  return supportsCreate(host) ? '可创建' : '未开放创建';
}

function HostHeader({
  host,
  id,
  count,
  reserved,
  onCreate,
}: {
  host: CvmHost | undefined;
  id: string;
  count: number;
  reserved?: CvmInstance[];
  onCreate?: () => void;
}) {
  const caps = host?.capabilities;
  const ready = !!host && supportsCreate(host);
  const state = hostState(host);
  const used = reserved && reservationUsage(reserved);
  const resources =
    used && caps
      ? [
          { label: 'vCPU', used: used.vcpus, total: caps.allocatable_vcpus, format: String },
          { label: '内存', used: used.memory, total: caps.allocatable_memory_bytes, format: capacity },
          { label: '系统盘', used: used.disk, total: caps.allocatable_disk_bytes, format: capacity },
        ]
      : [];
  const tone =
    !host || host.error || caps?.baremetal ? 'st-halted' : !host.fresh ? 'st-gold' : ready ? 'st-succeeded' : '';
  return (
    <div className="cvm-list-host">
      <Icon of="host" size={14} />
      <b>{host?.name ?? id}</b>
      <span className="cvm-list-muted">
        {caps?.backend ? `${caps.backend} · ` : ''}
        {count} 个工作负载
      </span>
      {reserved && (
        <span className={`st ${tone}`} title={host?.error ?? undefined}>
          {state}
        </span>
      )}
      {!!resources.length && (
        <div className="cvm-list-host-capacity" aria-label="已分配 / 宿主容量">
          {resources.map(item => (
            <span className="cvm-list-capacity" key={item.label} title={`${item.label} 已分配 / 宿主容量`}>
              <span>{item.label}</span>
              <CapacityMeter used={item.used} total={item.total} allowOversell />
              <span className="mono">
                {item.format(item.used)} / {item.format(item.total)}
              </span>
            </span>
          ))}
        </div>
      )}
      {ready && onCreate && (
        <button className="btn sm cvm-row-btn cvm-list-create" type="button" onClick={onCreate}>
          <Icon of="plus" size={13} />
          在此创建
        </button>
      )}
    </div>
  );
}

function Usage({ share, title }: { share: number | null; title: string }) {
  return (
    <span className="cvm-list-usage" title={title}>
      {share === null ? (
        <span className="cvm-list-muted">—</span>
      ) : (
        <>
          <span className="mono">{percent(share)}</span>
          <CapacityMeter used={share} total={1} />
        </>
      )}
    </span>
  );
}

function InstanceUsage({
  reading,
  clock,
}: {
  reading?: CvmCardReading;
  clock?: { serverAt: number; receivedAt: number };
}) {
  const now = useNow();
  const latest = reading?.latest;
  const current =
    clock &&
    reading?.state === 'live' &&
    latest &&
    clock.serverAt + Math.max(0, now - clock.receivedAt) - latest.observed_at_unix_millis <= 120_000
      ? latest
      : null;
  const title = current
    ? `采样 ${new Date(current.observed_at_unix_millis).toLocaleString()}${current.has_gap ? ' · 采样有缺口' : ''}`
    : '暂无实时读数';
  return (
    <>
      <td className="cvm-list-metric" role="cell">
        <Usage
          share={current?.cpu_usage_basis_points != null ? current.cpu_usage_basis_points / 10_000 : null}
          title={title}
        />
      </td>
      <td className="cvm-list-metric" role="cell">
        <Usage share={shareOf(current?.memory_used_bytes, current?.memory_total_bytes)} title={title} />
      </td>
    </>
  );
}

/** 本月流量写成同一单位下的「已用 / 额度」，单位只写一次。 */
function monthPair(used: number | null, limit: number | null) {
  if (limit == null) return { used: used == null ? '—' : capacity(used), rest: '不限额' };
  const unit = limit >= GIB ? GIB : MIB;
  const name = unit === GIB ? 'GiB' : 'MiB';
  const scaled = (value: number) => {
    const number = value / unit;
    return String(Number(number.toFixed(number >= 100 || Number.isInteger(number) ? 0 : 1)));
  };
  return { used: used == null ? '—' : scaled(used), rest: `/ ${scaled(limit)} ${name}` };
}

function MonthTraffic({ reading }: { reading?: CvmCardReading }) {
  if (
    !reading ||
    (reading.month_used_bytes == null && reading.month_limit_bytes == null && !reading.month_coverage_gap)
  )
    return <span className="cvm-list-muted">—</span>;
  const observed = reading.month_observed_at_unix_millis;
  const pair = monthPair(reading.month_used_bytes ?? null, reading.month_limit_bytes ?? null);
  return (
    <span
      className="cvm-list-month"
      title={observed == null ? 'UTC 自然月' : `UTC 自然月 · 采样 ${new Date(observed).toLocaleString()}`}
    >
      <span className="mono">{pair.used}</span>
      <span className="cvm-list-muted mono"> {pair.rest}</span>
      {reading.month_coverage_gap && <small>统计有缺口</small>}
    </span>
  );
}

function ChipList({ chips, limit, onOpen }: { chips: NetChip[]; limit: number; onOpen: () => void }) {
  return (
    <div className="cvm-list-chips">
      {chips.slice(0, limit).map(chip => (
        <NetChipView key={chip.key} chip={chip} onClick={onOpen} />
      ))}
      {chips.length > limit && (
        <button
          className="cvm-chip local"
          type="button"
          aria-label={`查看其余 ${chips.length - limit} 项`}
          onClick={onOpen}
        >
          +{chips.length - limit}
        </button>
      )}
    </div>
  );
}

function entryMatches(entry: CvmEntry, term: string) {
  const text = entryText(entry);
  if (
    `${entry.address} ${entry.visible_address} ${text.badge} ${text.text} ${text.target}`.toLowerCase().includes(term)
  )
    return true;
  return entryContainsPort(entry.mode, /^\d{1,5}$/.test(term) ? Number(term) : NaN, entry.reserved_ports);
}

export type InstanceListView = {
  filter: (typeof filters)[number]['key'];
  host: string;
  owner: string;
  search: string;
};

export function InstanceList({
  workspace,
  onCreateOnHost,
  view,
  onViewChange,
}: {
  workspace: CvmWorkspace;
  onCreateOnHost?: (nodeId: string) => void;
  view: InstanceListView;
  onViewChange: (view: InstanceListView) => void;
}) {
  const { filter, host: hostFilter, owner: ownerFilter, search } = view;
  const [checked, setChecked] = useState<string[]>([]);
  const [dialog, setDialog] = useState<{ key: number; command: ListCommand; resume?: ListCommandRecovery } | null>(
    null,
  );
  const [notice, setNotice] = useState('');
  const searchRef = useRef<HTMLInputElement>(null);
  // 「/」聚焦搜索：只在焦点不在输入控件里时响应，避免吞掉表单里的斜杠。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== '/' || event.ctrlKey || event.metaKey || event.altKey || event.isComposing) return;
      const target = event.target instanceof Element ? event.target : null;
      if (target?.closest('input, textarea, select, [contenteditable], [role="dialog"]')) return;
      if (document.querySelector('.dialog-layer:not([inert])')) return;
      event.preventDefault();
      searchRef.current?.focus();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);
  const unresolved = useListCommandRecovery();
  const commandLocked = !dialog && unresolved !== null;
  const selectable = workspace.can_use && !workspace.redacted;
  const manage = workspace.can_write && !workspace.redacted;
  const hosts = new Map(stackHosts(workspace).map(host => [host.node_id, host]));
  const instances = workspace.instances.filter(item => item.reserved);
  const networkEnabled = !workspace.redacted && instances.length > 0;
  const entries = useQuery({
    queryKey: ['cvm-entries'],
    queryFn: fetchCvmEntries,
    enabled: networkEnabled,
    refetchInterval: 15_000,
  });
  const catalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: networkEnabled && instances.some(item => item.desired.spec.interfaces.some(nic => nic.allocated)),
    refetchInterval: 60_000,
  });
  const chains = useQuery({
    queryKey: ['cvm-chain-egress', null],
    queryFn: () => fetchCvmChainCatalog(),
    enabled: networkEnabled && instances.some(item => item.desired.spec.routing.routes.some(route => route.chain_id)),
    refetchInterval: 60_000,
  });
  const readings = useMetricQuery({
    queryKey: ['cvm-readings'],
    queryFn: fetchCvmReadings,
    enabled: instances.length > 0,
    refetchInterval: 15_000,
    metricFacts: data => data.instances.map(item => item.state),
  });
  const readingById = new Map(readings.data?.instances.map(item => [item.id, item]));
  const readingClock =
    readings.data && !readings.error
      ? { serverAt: readings.data.observed_at_unix_millis, receivedAt: readings.dataUpdatedAt }
      : undefined;
  const entriesById = new Map<string, CvmEntry[]>();
  if (networkEnabled)
    for (const entry of entries.data?.entries ?? []) {
      const list = entriesById.get(entry.instance_id);
      if (list) list.push(entry);
      else entriesById.set(entry.instance_id, [entry]);
    }
  const ownerNames = new Map(workspace.owners.map(owner => [ownerKey(owner.tenant_id, owner.user_id), owner.name]));
  const ownerOf = (item: CvmInstance) => (item.user_id == null ? 'unassigned' : ownerKey(item.tenant_id, item.user_id));
  const ownerName = (item: CvmInstance) => ownerNames.get(ownerOf(item)) ?? item.user_id ?? '未授权';
  const owners = [
    ...new Map(instances.filter(item => item.user_id != null).map(item => [ownerOf(item), item])).values(),
  ];
  const duplicateOwnerName = new Set(owners.map(ownerName).filter((name, index, all) => all.indexOf(name) !== index));
  const terms = search.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const activeFilter = filters.find(item => item.key === filter)!;
  const visible = instances.filter(item => {
    if (
      !activeFilter.matches(item) ||
      (hostFilter && item.node_id !== hostFilter) ||
      (manage && ownerFilter && ownerOf(item) !== ownerFilter)
    )
      return false;
    const host = hosts.get(item.node_id)?.name ?? item.node_id;
    const text = `${item.desired.spec.name} ${ownerName(item)} ${host}`.toLowerCase();
    const address = workspace.redacted ? '' : item.report?.access_address?.toLowerCase();
    return terms.every(
      term =>
        text.includes(term) ||
        address?.includes(term) ||
        entriesById.get(item.desired.id)?.some(entry => entryMatches(entry, term)),
    );
  });
  const groups = new Map<string, CvmInstance[]>();
  for (const item of visible) {
    const group = groups.get(item.node_id);
    if (group) group.push(item);
    else groups.set(item.node_id, [item]);
  }
  const hostOptions = [...new Set([...(manage ? hosts.keys() : []), ...instances.map(item => item.node_id)])];
  const orderedHosts = hostOptions.sort(
    (a, b) => (hosts.get(a)?.name ?? a).localeCompare(hosts.get(b)?.name ?? b) || a.localeCompare(b),
  );
  const unfiltered = filter === 'all' && !terms.length && !(manage && ownerFilter);
  const groupIds = orderedHosts.filter(
    id => groups.has(id) || (manage && unfiltered && (!hostFilter || hostFilter === id)),
  );
  const selected = selectable ? visible.filter(item => checked.includes(item.desired.id)) : [];
  const selectedByHost = new Map<string, number>();
  for (const item of selected) selectedByHost.set(item.node_id, (selectedByHost.get(item.node_id) ?? 0) + 1);
  const batchActions = manage ? [...powerActions, 'delete' as const] : powerActions;
  const clear = () => setChecked([]);
  const onCommand = (next: ListCommand, resume?: ListCommandRecovery) => {
    if (unresolved && (!resume || unresolved.sending)) return;
    setNotice('');
    setDialog(previous => ({ key: (previous?.key ?? 0) + 1, command: next, resume }));
  };
  const queryErrors = [
    networkEnabled && entries.error,
    networkEnabled && catalog.error,
    networkEnabled && chains.error,
    readings.error,
  ].filter(error => error instanceof Error);
  const publications = new Map(catalog.data?.publications.map(item => [item.id, item.spec.name]));
  const chainNames = new Map(chains.data?.chains.map(item => [item.id, item.name]));
  const canAct = selectable || manage;
  const headers = ['容器', '规格', 'CPU', '内存', '入站方式', '出站方式', '本月流量', ...(canAct ? ['操作'] : [])];
  return (
    <section
      className="cvm-instance-list"
      aria-label="容器清单"
      onKeyDown={event => {
        if (event.key === 'Escape' && selected.length && event.currentTarget.contains(event.target as Node)) clear();
      }}
    >
      {commandLocked && unresolved && (selectable || manage) && (
        <div className="cvm-notice cvm-list-recovery" role="status">
          <span>{unresolved.sending ? '正在保存上一项操作…' : '上一项操作的结果尚未确认。'}</span>
          {!unresolved.sending && (
            <button className="btn sm" type="button" onClick={() => onCommand(unresolved.command, unresolved)}>
              重试原请求
            </button>
          )}
        </div>
      )}
      {selected.length > 0 ? (
        <div className="cvm-list-toolbar cvm-list-batch" role="group" aria-label="批量操作">
          <b>已选 {selected.length} 个容器</b>
          <ul className="cvm-list-host-counts cvm-list-muted" aria-label="所选容器的宿主分布">
            {orderedHosts
              .filter(id => selectedByHost.has(id))
              .map(id => (
                <li key={id}>
                  {hosts.get(id)?.name ?? id} {selectedByHost.get(id)} 个
                </li>
              ))}
          </ul>
          <div className="cvm-list-batch-actions">
            {batchActions.map(action => (
              <button
                className={action === 'delete' ? 'btn sm danger' : 'btn sm'}
                type="button"
                key={action}
                aria-haspopup="dialog"
                disabled={
                  commandLocked ||
                  (action !== 'delete' &&
                    selected.every(item => !!actionBlocked(action, item, hosts.get(item.node_id), workspace, true)))
                }
                onClick={() => onCommand({ action, instances: selected, batch: true })}
              >
                {action !== 'delete' && <Icon of={powerIcon(action)} size={13} />}
                {action === 'delete' ? '批量删除…' : listActionText[action]}
              </button>
            ))}
          </div>
          <button className="btn sm cvm-row-btn cvm-list-clear" type="button" onClick={clear}>
            取消选择 <kbd aria-hidden="true">Esc</kbd>
          </button>
        </div>
      ) : (
        <div className="cvm-list-toolbar">
          <div className="cvm-list-filters" role="group" aria-label="容器状态">
            {filters.map(item => {
              const count = instances.filter(item.matches).length;
              const tone =
                count > 0 && item.key === 'attention'
                  ? 'err'
                  : count > 0 && item.key === 'pending'
                    ? 'gold'
                    : undefined;
              return (
                <button
                  className="cvm-list-filter"
                  type="button"
                  key={item.key}
                  data-filter={item.key}
                  data-tone={tone}
                  aria-pressed={filter === item.key}
                  onClick={() => {
                    clear();
                    onViewChange({ ...view, filter: item.key });
                  }}
                >
                  {tone && <i className="cvm-list-filter-dot" aria-hidden="true" />}
                  {item.label} <small className="mono">{count}</small>
                </button>
              );
            })}
          </div>
          <span className="cvm-list-sep" aria-hidden="true" />
          <label className="cvm-list-select">
            <span>宿主</span>
            <select
              value={hostFilter}
              onChange={event => {
                clear();
                onViewChange({ ...view, host: event.target.value });
              }}
            >
              <option value="">全部</option>
              {orderedHosts.map(id => (
                <option key={id} value={id}>
                  {hosts.get(id)?.name ?? id}
                </option>
              ))}
            </select>
          </label>
          {manage && (
            <label className="cvm-list-select">
              <span>使用者</span>
              <select
                value={ownerFilter}
                onChange={event => {
                  clear();
                  onViewChange({ ...view, owner: event.target.value });
                }}
              >
                <option value="">全部</option>
                <option value="unassigned">未授权</option>
                {owners.map(item => (
                  <option key={ownerOf(item)} value={ownerOf(item)}>
                    {ownerName(item)}
                    {duplicateOwnerName.has(ownerName(item)) ? ` · ${item.tenant_id}` : ''}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="cvm-list-search">
            <Icon of="search" size={13} />
            <input
              ref={searchRef}
              className="f"
              type="search"
              aria-label="搜索容器"
              aria-keyshortcuts="/"
              placeholder={workspace.redacted ? '名称、使用者、宿主' : '名称、地址、端口'}
              value={search}
              onChange={event => {
                clear();
                onViewChange({ ...view, search: event.target.value });
              }}
            />
            {!search && <kbd aria-hidden="true">/</kbd>}
          </label>
        </div>
      )}
      {notice && (
        <p className="cvm-notice" role="status">
          {notice}
        </p>
      )}
      {queryErrors.map((error, index) => (
        <ErrorBox key={index} error={error} />
      ))}
      {groupIds.length ? (
        <div className="cvm-list-table-frame">
          <table className="cvm-list-table" aria-label="按宿主分组的容器" role="table">
            <thead role="rowgroup">
              <tr role="row">
                {selectable && (
                  <th scope="col" className="cvm-list-check" role="columnheader">
                    <SelectVisible
                      total={visible.length}
                      selected={selected.length}
                      onChange={() =>
                        setChecked(selected.length === visible.length ? [] : visible.map(item => item.desired.id))
                      }
                    />
                  </th>
                )}
                {headers.map((label, index) => (
                  <th key={label} scope="col" className={`cvm-list-col-${index}`} role="columnheader">
                    {label === '操作' ? <span className="sr-only">{label}</span> : label}
                  </th>
                ))}
              </tr>
            </thead>
            {groupIds.map(id => {
              const rows = groups.get(id) ?? [];
              const host = hosts.get(id);
              const columns = headers.length + Number(selectable);
              return (
                <tbody key={id} role="rowgroup">
                  <tr className="cvm-list-host-row" role="row">
                    <th colSpan={columns} scope="rowgroup" role="rowheader">
                      <HostHeader
                        host={host}
                        id={id}
                        count={rows.length}
                        reserved={manage ? instances.filter(item => item.node_id === id) : undefined}
                        onCreate={manage && onCreateOnHost ? () => onCreateOnHost(id) : undefined}
                      />
                    </th>
                  </tr>
                  {!rows.length && (
                    <tr role="row" className="cvm-list-empty-host">
                      <td role="cell" colSpan={columns}>
                        暂无容器
                      </td>
                    </tr>
                  )}
                  {rows.map(instance => {
                    const itemId = instance.desired.id;
                    const spec = instance.desired.spec;
                    const baremetal = spec.execution.kind === 'baremetal';
                    const mine = entriesById.get(itemId) ?? [];
                    const reading = readingById.get(itemId);
                    const entryChips = mine.map(entryChip);
                    const exits = exitChips(
                      instance,
                      id => publications.get(id),
                      id => chainNames.get(id),
                    );
                    const lamp = instanceLamp(instance);
                    const system = spec.image
                      ? `${spec.image.name} ${spec.image.version}`
                      : (host?.capabilities.host_system?.os ?? '裸机');
                    const attention = needsAttention(instance);
                    const tagged = hasPendingCvmChange(instance) || instance.status !== 'applied';
                    const revoking =
                      !instance.user_id &&
                      instance.operation_kind === 'assign' &&
                      (hasPendingCvmChange(instance) || instance.status !== 'applied');
                    const power =
                      tagged || attention
                        ? null
                        : instance.report?.power === 'stopped'
                          ? '已关机'
                          : instance.report?.power === 'running'
                            ? null
                            : lamp.why;
                    const meta = [revoking ? '收回授权中' : ownerName(instance), system, power]
                      .filter(Boolean)
                      .join(' · ');
                    const hostIssue = host && supportsCreate(host) ? null : `宿主${hostState(host)}`;
                    const issue =
                      attention && !workspace.redacted
                        ? [instance.report?.error, hostIssue].filter(Boolean).join(' · ') || statusText[instance.status]
                        : null;
                    return (
                      <tr
                        className="cvm-list-row"
                        data-selected={checked.includes(itemId)}
                        data-deleting={instance.desired.power === 'deleted'}
                        key={itemId}
                        role="row"
                        onClick={event => activateRow(event, instance)}
                      >
                        {selectable && (
                          <td className="cvm-list-check" role="cell">
                            <input
                              type="checkbox"
                              aria-label={`选择 ${spec.name}`}
                              checked={checked.includes(itemId)}
                              onChange={() =>
                                setChecked(old =>
                                  old.includes(itemId) ? old.filter(id => id !== itemId) : [...old, itemId],
                                )
                              }
                            />
                          </td>
                        )}
                        <td className="cvm-list-identity" role="cell">
                          <div className="cvm-list-name">
                            <span className="cvm-list-dot" data-tone={lamp.tone} title={lamp.why} aria-hidden="true" />
                            <button className="cvm-list-link" type="button" onClick={() => openDetail(instance)}>
                              {spec.name}
                            </button>
                            {baremetal && <span className="st">裸机</span>}
                            {tagged && <AppliedState instance={instance} />}
                            {!instance.user_id && !revoking && !tagged && <span className="st">未授权</span>}
                          </div>
                          <small className="cvm-list-meta" title={meta}>
                            {meta}
                          </small>
                          {issue && (
                            <small
                              className="cvm-list-error"
                              data-tone={instance.status === 'failed' ? 'bad' : 'warn'}
                              title={issue}
                            >
                              {issue}
                            </small>
                          )}
                        </td>
                        <td className="cvm-list-spec mono" role="cell">
                          <span
                            title={`${spec.resources.vcpus} ${baremetal ? '核' : 'vCPU'} · ${capacity(spec.resources.memory_bytes)} 内存 · ${capacity(spec.resources.root_disk_bytes)} 系统盘`}
                          >
                            {compactSpec(spec.resources)}
                          </span>
                        </td>
                        <InstanceUsage reading={reading} clock={readingClock} />
                        <td className="cvm-list-network" role="cell">
                          {workspace.redacted ? (
                            <span className="cvm-list-muted">已脱敏</span>
                          ) : mine.length ? (
                            <ChipList chips={entryChips} limit={2} onOpen={() => openDetail(instance, 'network')} />
                          ) : (
                            <span className={`cvm-list-muted${entries.data ? ' cvm-list-no-entry' : ''}`}>
                              {entries.data ? '—' : entries.error ? '暂不可用' : '读取中'}
                            </span>
                          )}
                        </td>
                        <td className="cvm-list-network" role="cell">
                          {!workspace.redacted && (
                            <ChipList chips={exits} limit={2} onOpen={() => openDetail(instance, 'network')} />
                          )}
                        </td>
                        <td className="cvm-list-traffic" role="cell">
                          <MonthTraffic reading={reading} />
                        </td>
                        {canAct && (
                          <td className="cvm-list-action-cell" role="cell">
                            <InstanceActions
                              instance={instance}
                              host={host}
                              workspace={workspace}
                              entries={mine}
                              onCommand={onCommand}
                              commandLocked={commandLocked}
                            />
                          </td>
                        )}
                      </tr>
                    );
                  })}
                </tbody>
              );
            })}
          </table>
        </div>
      ) : (
        <EmptyState icon="container" title={instances.length ? '没有匹配的容器' : '暂无可查看的容器'}>
          {instances.length
            ? `可搜索名称、使用者、宿主${workspace.redacted ? '' : '、地址或端口'}，也可调整筛选条件。`
            : manage
              ? '宿主接入后即可创建容器。'
              : '获授权的容器会显示在这里。'}
        </EmptyState>
      )}
      {dialog && (
        <ListCommandDialog
          key={dialog.key}
          command={dialog.command}
          workspace={workspace}
          entriesById={entries.isSuccess ? entriesById : undefined}
          retainRecovery
          previousResults={dialog.resume?.results}
          onClose={() => setDialog(current => (current === dialog ? null : current))}
          onSaved={(ids, message) => {
            setChecked(old => old.filter(id => !ids.includes(id)));
            setNotice(message);
          }}
        />
      )}
    </section>
  );
}
