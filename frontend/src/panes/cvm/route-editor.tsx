import { useMutation, useQuery } from '@tanstack/react-query';
import { useId, useState, type FormEvent, type ReactNode } from 'react';
import {
  fetchCvmChainCatalog,
  matchCvmRoute,
  type CvmContainerRoute,
  type CvmContainerRoutes,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmRouteMatchRequest,
  type CvmRouteService,
} from '../../api';
import type { CvmNetworkEditRequest } from '../../api/cvm-network-edit';
import { ErrorBox } from '../../ui/bits';
import { Icon, PanelTitle } from '../../ui/icons';
import { networkLines } from './network-common';
import { routeTarget, targetKey } from './route-target';
import './route-editor.css';

export interface RouteEditorTarget {
  key: string;
  name: string;
  ipv4: boolean;
  ipv6: boolean;
  available: boolean;
}

type Family = 'ipv4' | 'ipv6';
const families = ['ipv4', 'ipv6'] as const;
const familyName = { ipv4: 'IPv4', ipv6: 'IPv6' } as const;
const maxPriority = 65535;
const maxRoutes = 256;
const isChain = (key: string) => key.startsWith('chain:');

function targetName(key: string, options: readonly RouteEditorTarget[]) {
  if (!key) return '未选择出口';
  const name = options.find(option => option.key === key)?.name ?? '出口详情不可用';
  return `${isChain(key) ? '线路' : '网卡'} · ${name}`;
}

function withPriority(routing: CvmContainerRoutes, option: RouteEditorTarget): CvmContainerRoutes {
  if (routing.priorities.some(p => targetKey(p) === option.key)) return routing;
  const next = (family: Family) =>
    option[family] ? Math.min(maxPriority, Math.max(0, ...routing.priorities.map(p => p[family] ?? 0)) + 10) : null;
  return {
    ...routing,
    priorities: [...routing.priorities, { ...routeTarget(option.key), ipv4: next('ipv4'), ipv6: next('ipv6') }],
  };
}

/** 表里写成 `tcp/443`、`udp/1000-2000`，与输入格式一致；不限协议时写「全部」。 */
function serviceText(route: CvmContainerRoute) {
  const service = route.service;
  if (!service) return route.chain_id ? 'TCP / UDP' : '全部';
  return `${service.protocol}/${service.port_start}${service.port_start === service.port_end ? '' : `-${service.port_end}`}`;
}

/** 空白表示不限协议；`tcp` 单写表示全部端口。端口段 1–65535。 */
function parseService(text: string): CvmRouteService | null | undefined {
  const value = text.trim().toLowerCase();
  if (!value || value === '全部') return null;
  const match = /^(tcp|udp)(?:\/(\d{1,5})(?:-(\d{1,5}))?)?$/.exec(value);
  if (!match) return undefined;
  if (!match[2]) return { protocol: match[1] as 'tcp' | 'udp', port_start: 1, port_end: 65535 };
  const port_start = Number(match[2]);
  const port_end = Number(match[3] ?? match[2]);
  if (port_start < 1 || port_end < port_start || port_end > 65535) return undefined;
  return { protocol: match[1] as 'tcp' | 'udp', port_start, port_end };
}

function validDestination(text: string) {
  const value = text.trim();
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:\/(\d{1,2}))?$/.exec(value);
  if (v4) return v4.slice(1, 5).every(octet => Number(octet) <= 255) && (v4[5] === undefined || Number(v4[5]) <= 32);
  const v6 = /^([0-9a-f:.]+)(?:\/(\d{1,3}))?$/i.exec(value);
  return !!v6 && v6[1].includes(':') && (v6[2] === undefined || Number(v6[2]) <= 128);
}

/** 命中测试的一行输入：`203.0.113.9`、`203.0.113.9 udp/53`、`2001:db8::1 other`。未写协议按 TCP 443。 */
function parseProbe(
  text: string,
): { destination: string; protocol: CvmRouteMatchRequest['protocol']; port?: number } | null {
  const match = /^\s*(\S+)(?:\s+(tcp|udp|other)(?:\/(\d{1,5}))?)?\s*$/i.exec(text);
  if (!match || !validDestination(match[1]) || match[1].includes('/')) return null;
  const protocol = (match[2]?.toLowerCase() ?? 'tcp') as CvmRouteMatchRequest['protocol'];
  if (protocol === 'other') return match[3] ? null : { destination: match[1], protocol };
  const port = Number(match[3] ?? 443);
  return port >= 1 && port <= 65535 ? { destination: match[1], protocol, port } : null;
}

const probeLabel = (probe: NonNullable<ReturnType<typeof parseProbe>>) =>
  probe.protocol === 'other' ? '其他协议' : `${probe.protocol.toUpperCase()} ${probe.port}`;

/* ── 一行路由的编辑格：新增行与就地编辑共用。 ── */
function RouteForm({
  route,
  options,
  canSelect,
  submitLabel,
  onSubmit,
  onCancel,
}: {
  route?: CvmContainerRoute;
  options: readonly RouteEditorTarget[];
  canSelect: (option: RouteEditorTarget) => boolean;
  submitLabel: string;
  onSubmit: (route: CvmContainerRoute, option: RouteEditorTarget) => void;
  onCancel?: () => void;
}) {
  const [destination, setDestination] = useState(route?.destination ?? '');
  const [service, setService] = useState(route?.service ? serviceText(route) : '');
  const [target, setTarget] = useState(route ? targetKey(route) : '');
  const option = options.find(item => item.key === target);
  const parsed = parseService(service);
  const destinationValid = validDestination(destination);
  const problem =
    destination.trim() && !destinationValid
      ? '目标须为 IP 地址或网段，例如 10.0.0.0/8'
      : parsed === undefined
        ? '协议端口写成 tcp/443、udp/1000-2000，留空表示不限'
        : null;
  const ready = destinationValid && parsed !== undefined && !!option && canSelect(option);
  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    if (!ready || !option) return;
    // 不限协议时不写 service 字段，和服务端返回的路由同形；原样「应用修改」不会凭空多出一处差异。
    onSubmit(
      { destination: destination.trim(), ...(parsed ? { service: parsed } : {}), ...routeTarget(option.key) },
      option,
    );
    if (!route) {
      setDestination('');
      setService('');
    }
  };
  return (
    <tr className="cvm-route-form" data-editing={route ? '' : undefined}>
      <td>
        <input
          className="f mono"
          aria-label="目标 IP／网段"
          aria-invalid={!!destination.trim() && !destinationValid}
          placeholder="例如 10.0.0.0/8"
          value={destination}
          onChange={event => setDestination(event.currentTarget.value)}
          onKeyDown={event => event.key === 'Enter' && submit(event)}
        />
      </td>
      <td>
        <input
          className="f mono"
          aria-label="协议端口"
          aria-invalid={parsed === undefined}
          placeholder={option && isChain(option.key) ? 'TCP / UDP' : route ? '全部' : 'tcp/443'}
          title="写成 tcp/443 或 udp/1000-2000；留空表示不限协议"
          value={service}
          onChange={event => setService(event.currentTarget.value)}
          onKeyDown={event => event.key === 'Enter' && submit(event)}
        />
      </td>
      <td>
        <select className="f" aria-label="经由" value={target} onChange={event => setTarget(event.currentTarget.value)}>
          <option value="">选择出口</option>
          {route && !option && (
            <option value={target} disabled>
              {targetName(target, options)}
            </option>
          )}
          {[false, true].map(chain => {
            const group = options.filter(item => isChain(item.key) === chain);
            return group.length ? (
              <optgroup key={String(chain)} label={chain ? '线路' : '网卡'}>
                {group.map(item => (
                  <option key={item.key} value={item.key} disabled={!canSelect(item)}>
                    {item.name}
                    {!item.available
                      ? ' · 不可选'
                      : item.ipv4 && !item.ipv6
                        ? ' · IPv4'
                        : !item.ipv4 && item.ipv6
                          ? ' · IPv6'
                          : ''}
                  </option>
                ))}
              </optgroup>
            ) : null;
          })}
        </select>
        {problem && (
          <span className="cvm-route-problem" role="status">
            {problem}
          </span>
        )}
      </td>
      <td className="cvm-route-acts">
        {onCancel && (
          <button className="btn icon cvm-row-btn" type="button" aria-label="取消编辑" title="取消" onClick={onCancel}>
            <Icon of="close" size={13} />
          </button>
        )}
        <button
          className={`btn icon cvm-route-submit${ready ? ' primary' : ''}`}
          type="button"
          aria-label={submitLabel}
          title={submitLabel}
          disabled={!ready}
          onClick={() => submit()}
        >
          <Icon of={route ? 'check' : 'plus'} size={13} />
        </button>
      </td>
    </tr>
  );
}

/* ── 出口优先级：同网段的多条路由按出口优先级比较。一条可拖动的出口序列，两个地址族同序；
   点出口可单独填写某一族的数值或让它不参与该族。 ── */
function PriorityStrip({
  routing,
  options,
  writable,
  onChange,
}: {
  routing: CvmContainerRoutes;
  options: readonly RouteEditorTarget[];
  writable: boolean;
  onChange: (next: CvmContainerRoutes) => void;
}) {
  const [drag, setDrag] = useState<{ from: string; over?: string }>();
  const [open, setOpen] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState('');
  const rows = routing.priorities.map(priority => ({
    key: targetKey(priority),
    ipv4: priority.ipv4,
    ipv6: priority.ipv6,
  }));
  const rank = (row: (typeof rows)[number]) => Math.min(row.ipv4 ?? Infinity, row.ipv6 ?? Infinity);
  const ordered = [...rows].sort((a, b) => rank(a) - rank(b) || (a.ipv4 ?? Infinity) - (b.ipv4 ?? Infinity));
  const familyOrder = (family: Family) =>
    rows
      .filter(row => row[family] != null)
      .sort((a, b) => a[family]! - b[family]!)
      .map(row => row.key)
      .join('|');
  const combined = (family: Family) =>
    ordered
      .filter(row => row[family] != null)
      .map(row => row.key)
      .join('|');
  const diverged = families.some(family => familyOrder(family) !== combined(family));
  const reorder = (key: string, to: number) => {
    const from = ordered.findIndex(row => row.key === key);
    if (!writable || from < 0 || to < 0 || to >= ordered.length || from === to) return;
    const order = ordered.map(row => row.key);
    order.splice(to, 0, ...order.splice(from, 1));
    const values = new Map(rows.map(row => [row.key, { ipv4: row.ipv4, ipv6: row.ipv6 }]));
    for (const family of families) {
      const participants = order.filter(target => values.get(target)![family] != null);
      const slots = participants.map(target => values.get(target)![family]!).sort((a, b) => a - b);
      let previous = -1;
      participants.forEach((target, index) => {
        // 保留原有的数值档位，只交换归属；重复值在显式排序时拉开，且不超过 u16。
        previous = Math.min(maxPriority - (participants.length - 1 - index), Math.max(previous + 1, slots[index]));
        values.get(target)![family] = previous;
      });
    }
    onChange({
      ...routing,
      priorities: routing.priorities.map(p => ({ ...p, ...values.get(targetKey(p))! })),
    });
    setAnnouncement(`${targetName(key, options)} 移到第 ${to + 1} 位`);
  };
  const setValue = (key: string, family: Family, text: string) => {
    const next = text === '' ? null : Number(text);
    if (next != null && !Number.isFinite(next)) return;
    onChange({
      ...routing,
      priorities: routing.priorities.map(p => (targetKey(p) === key ? { ...p, [family]: next } : p)),
    });
  };
  const editing = ordered.find(row => row.key === open);
  return (
    <div className="cvm-route-priorities">
      <div className="cvm-route-priority-strip" role="list" aria-label="出口优先级">
        <span className="cvm-route-caption">出口优先级</span>
        {ordered.map((row, index) => {
          const name = targetName(row.key, options);
          const movable = writable && ordered.length > 1;
          const value = (family: Family) => (row[family] == null ? '—' : row[family]);
          return (
            <span
              role="listitem"
              className="cvm-route-priority"
              key={row.key}
              data-open={open === row.key || undefined}
              data-drop={drag?.over === row.key || undefined}
              onDragOver={event => {
                if (!drag || drag.from === row.key || !movable) return;
                event.preventDefault();
                event.dataTransfer.dropEffect = 'move';
                if (drag.over !== row.key) setDrag({ from: drag.from, over: row.key });
              }}
              onDragLeave={event => {
                if (drag?.over === row.key && !event.currentTarget.contains(event.relatedTarget as Node | null))
                  setDrag({ from: drag.from });
              }}
              onDrop={event => {
                if (!drag || !movable) return;
                event.preventDefault();
                reorder(drag.from, index);
                setDrag(undefined);
              }}
            >
              {movable && (
                <button
                  type="button"
                  className="cvm-route-grip"
                  aria-label={`排序 ${name}`}
                  aria-keyshortcuts="ArrowLeft ArrowRight ArrowUp ArrowDown"
                  title="拖动或使用方向键排序"
                  draggable
                  onDragStart={event => {
                    event.dataTransfer.effectAllowed = 'move';
                    event.dataTransfer.setData('text/plain', name);
                    setDrag({ from: row.key });
                  }}
                  onDragEnd={() => setDrag(undefined)}
                  onKeyDown={event => {
                    const step = ['ArrowUp', 'ArrowLeft'].includes(event.key)
                      ? -1
                      : ['ArrowDown', 'ArrowRight'].includes(event.key)
                        ? 1
                        : 0;
                    if (!step) return;
                    event.preventDefault();
                    event.stopPropagation();
                    reorder(row.key, index + step);
                  }}
                >
                  <span aria-hidden="true">⠿</span>
                </button>
              )}
              <button
                type="button"
                className="cvm-route-priority-body"
                aria-expanded={writable ? open === row.key : undefined}
                aria-label={`${name} · IPv4 ${value('ipv4')} · IPv6 ${value('ipv6')}`}
                disabled={!writable}
                onClick={() => setOpen(open === row.key ? null : row.key)}
              >
                <b>{index + 1}</b>
                <span className="cvm-route-priority-name">{name.replace(/^网卡 · /, '')}</span>
                <span className="cvm-route-priority-values">
                  v4 {value('ipv4')} · v6 {value('ipv6')}
                </span>
              </button>
            </span>
          );
        })}
        {!ordered.length && <span className="note">添加路由时自动补充出口优先级。</span>}
      </div>
      {diverged && (
        <p className="note cvm-route-note">IPv4 与 IPv6 的出口顺序不同；这里按较小值排列，拖动后两族统一为同一顺序。</p>
      )}
      {editing && writable && (
        <div className="cvm-route-priority-edit" aria-label={`${targetName(editing.key, options)} 优先级`} role="group">
          <b>{targetName(editing.key, options)}</b>
          {families.map(family => {
            const capable = options.find(option => option.key === editing.key)?.[family];
            return (
              <label key={family}>
                {familyName[family]}
                <input
                  className="f mono"
                  type="number"
                  min={0}
                  max={maxPriority}
                  placeholder={capable ? '不参与' : '不支持'}
                  aria-label={`${familyName[family]} 优先级：${targetName(editing.key, options)}`}
                  aria-invalid={
                    editing[family] != null &&
                    (!Number.isInteger(editing[family]) || editing[family]! < 0 || editing[family]! > maxPriority)
                  }
                  disabled={!capable && editing[family] == null}
                  value={editing[family] ?? ''}
                  onChange={event => setValue(editing.key, family, event.currentTarget.value)}
                />
              </label>
            );
          })}
          <span className="note">数值越小越优先；留空不参与该地址族。出口不可用时不会自动改走其他出口。</span>
          <button
            type="button"
            className="btn sm cvm-row-btn danger"
            onClick={() => {
              onChange({
                ...routing,
                priorities: routing.priorities.filter(p => targetKey(p) !== editing.key),
                routes: routing.routes.filter(route => targetKey(route) !== editing.key),
              });
              setOpen(null);
            }}
          >
            移除此出口的路由与优先级
          </button>
          <button type="button" className="btn sm" onClick={() => setOpen(null)}>
            完成
          </button>
        </div>
      )}
      <span className="sr-only" role="status">
        {announcement}
      </span>
    </div>
  );
}

function RouteDns({
  dns,
  writable,
  onChange,
}: {
  dns: string[];
  writable: boolean;
  onChange: (dns: string[]) => void;
}) {
  const [draft, setDraft] = useState({ text: '', normalized: '' });
  const normalized = dns.join(', ');
  return (
    <div className="cvm-route-line">
      <Icon of="dns" size={13} />
      <span className="cvm-route-caption">容器 DNS</span>
      {writable ? (
        <input
          className="f mono"
          aria-label="容器 DNS"
          value={draft.normalized === normalized ? draft.text : normalized}
          placeholder="系统默认 · 多个解析服务器 IP 用逗号或空格分隔"
          onChange={event => {
            const text = event.currentTarget.value;
            const next = networkLines(text.replace(/[,，\s]+/g, '\n'));
            setDraft({ text, normalized: next.join(', ') });
            onChange(next);
          }}
        />
      ) : (
        <span className="mono">{dns.join('、') || '系统默认'}</span>
      )}
    </div>
  );
}

export function RouteEditor({
  instance,
  catalog,
  routing,
  onChange,
  writable,
  extraTargets = [],
  redacted = false,
  canTest = writable,
  networkEdit,
}: {
  instance: CvmInstance;
  catalog?: CvmNicCatalog;
  routing: CvmContainerRoutes;
  onChange: (next: CvmContainerRoutes) => void;
  writable: boolean;
  extraTargets?: readonly RouteEditorTarget[];
  redacted?: boolean;
  canTest?: boolean;
  networkEdit?: CvmNetworkEditRequest;
}) {
  const id = useId();
  const { desired } = instance;
  const canEdit = writable && !redacted;
  const baremetal = desired.spec.execution.kind === 'baremetal';
  const fallback = baremetal ? '本机 · 宿主路由' : '主网卡 · 宿主 NAT';
  const [editing, setEditing] = useState<number | null>(null);
  const chains = useQuery({
    enabled: !redacted,
    queryKey: ['cvm-chain-egress', desired.id],
    queryFn: () => fetchCvmChainCatalog(desired.id),
    refetchInterval: 15_000,
  });
  const options: RouteEditorTarget[] = [
    ...desired.spec.interfaces.flatMap(nic => {
      const allocation = nic.allocated;
      if (!allocation) return [];
      const name = catalog?.publications.find(p => p.id === allocation.publication_id)?.spec.name;
      return [
        {
          key: nic.id,
          name: name ?? `业务网卡 ${allocation.slot}`,
          ipv4: !!allocation.ipv4,
          ipv6: !!allocation.ipv6,
          available: true,
        },
      ];
    }),
    ...extraTargets,
    ...(chains.data?.chains ?? []).map(chain => ({
      key: `chain:${chain.id}`,
      name:
        chain.name +
        (chain.unavailable_reason
          ? ` · ${chain.unavailable_reason}`
          : !chains.data?.supported
            ? ' · 宿主未就绪'
            : !chain.authorized
              ? ' · 未获授权'
              : ''),
      ipv4: true,
      ipv6: chain.ipv6,
      available: !!chains.data?.supported && chain.authorized && !chain.unavailable_reason,
    })),
  ];
  const canSelect = (option: RouteEditorTarget) =>
    option.available &&
    (routing.priorities.length < maxRoutes || routing.priorities.some(priority => targetKey(priority) === option.key));
  const selectable = options.some(canSelect);

  const [probeText, setProbeText] = useState('');
  const probe = parseProbe(probeText);
  const references = [...routing.priorities, ...routing.routes];
  const temporaryReferenced =
    !networkEdit && extraTargets.some(target => references.some(reference => targetKey(reference) === target.key));
  const matchBlocked = temporaryReferenced
    ? '当前草稿引用了尚未分配地址、且没有挂载草稿的网卡。请检查挂载草稿，再进行服务端命中测试。'
    : null;
  const body: CvmRouteMatchRequest | null = probe && {
    destination: probe.destination,
    protocol: probe.protocol,
    ...(probe.protocol === 'other' ? {} : { port: probe.port }),
    ...(networkEdit ? { network_edit: networkEdit } : { routes: routing }),
  };
  const scope = JSON.stringify([desired.id, desired.generation, body]);
  const match = useMutation({
    mutationFn: ({ instanceId, request }: { instanceId: string; request: CvmRouteMatchRequest; scope: string }) =>
      matchCvmRoute(instanceId, request, AbortSignal.timeout(10_000)),
  });
  const active = canTest && !redacted && !matchBlocked && !!body && scope === match.variables?.scope;
  const result = active ? match.data : undefined;
  // A mount can contribute implicit routes not present in the editor. In that case the index refers to the preview table.
  const hit =
    result?.kind === 'route'
      ? !networkEdit || networkEdit.routes
        ? result.route_index
        : routing.routes.findIndex(
            route =>
              route.destination === result.route.destination &&
              targetKey(route) === targetKey(result.route) &&
              serviceText(route) === serviceText(result.route),
          )
      : undefined;
  const hasChains = !!chains.data?.chains.length || references.some(reference => reference.chain_id);
  const replace = (index: number, route: CvmContainerRoute, option: RouteEditorTarget) => {
    const routes = routing.routes.map((old, i) => (i === index ? route : old));
    onChange(withPriority({ ...routing, routes }, option));
    setEditing(null);
  };
  const notes: ReactNode[] = [];
  if (chains.data?.reason && (canEdit || hasChains)) notes.push(chains.data.reason);
  if (hasChains)
    notes.push(
      `线路使用${chains.data?.owner?.name ? ` ${chains.data.owner.name} ` : '容器使用者'}的授权、额度与限速，仅支持 TCP／UDP。`,
    );
  if (canEdit && !selectable) notes.push('没有可选的出口：请先挂载业务网卡，或检查线路授权与出口优先级。');
  if (canEdit && routing.routes.length >= maxRoutes) notes.push('已达到 256 条路由上限。');

  return (
    <section className="panel titled cvm-net-panel cvm-route-panel" aria-labelledby={`${id}-title`}>
      <header>
        <span id={`${id}-title`}>
          <PanelTitle of="forwarding">路由与命中测试</PanelTitle>
        </span>
        {!redacted && <span className="hint">最长前缀 → 同前缀端口规则 → 出口优先级</span>}
      </header>
      {redacted ? (
        <p className="note cvm-net-empty">当前权限不显示目标网段、出口和 DNS 等路由详情。</p>
      ) : (
        <>
          <table className="cvm-route-table" aria-label="目标路由">
            <thead>
              <tr>
                <th scope="col">目标 IP／网段</th>
                <th scope="col">协议端口</th>
                <th scope="col">经由</th>
                <th scope="col">
                  <span className="sr-only">操作</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {routing.routes.map((route, index) =>
                editing === index && canEdit ? (
                  <RouteForm
                    key={`edit-${index}`}
                    route={route}
                    options={options}
                    canSelect={canSelect}
                    submitLabel="应用修改"
                    onSubmit={(next, option) => replace(index, next, option)}
                    onCancel={() => setEditing(null)}
                  />
                ) : (
                  <tr key={index} data-hit={hit === index || undefined}>
                    <td className="mono">{route.destination || <span className="cvm-faint">未填写</span>}</td>
                    <td className={`mono${route.service ? '' : ' cvm-faint'}`}>{serviceText(route)}</td>
                    <td>
                      <span className="cvm-route-via">
                        <Icon of={route.chain_id ? 'chains' : 'client'} size={13} />
                        <span title={targetName(targetKey(route), options)}>
                          {targetName(targetKey(route), options).replace(/^(网卡|线路) · /, '')}
                        </span>
                        {hit === index && <span className="st cvm-route-hit">命中</span>}
                      </span>
                    </td>
                    <td className="cvm-route-acts">
                      {canEdit && (
                        <>
                          <button
                            type="button"
                            className="btn icon cvm-row-btn"
                            title="编辑路由"
                            aria-label={`编辑路由 ${route.destination || index + 1}`}
                            onClick={() => setEditing(index)}
                          >
                            <Icon of="pencil" size={13} />
                          </button>
                          <button
                            type="button"
                            className="btn icon cvm-row-btn"
                            title="移除路由"
                            aria-label={`移除路由 ${route.destination || index + 1}`}
                            onClick={() => {
                              onChange({ ...routing, routes: routing.routes.filter((_, i) => i !== index) });
                              setEditing(null);
                            }}
                          >
                            <Icon of="close" size={13} />
                          </button>
                        </>
                      )}
                    </td>
                  </tr>
                ),
              )}
              {canEdit && editing === null && selectable && routing.routes.length < maxRoutes && (
                <RouteForm
                  key={`new-${routing.routes.length}`}
                  options={options}
                  canSelect={canSelect}
                  submitLabel="添加路由"
                  onSubmit={(route, option) =>
                    onChange(withPriority({ ...routing, routes: [...routing.routes, route] }, option))
                  }
                />
              )}
              <tr className="cvm-route-rest" data-hit={result?.kind === 'default' || undefined}>
                <td colSpan={2}>其余流量</td>
                <td>
                  <span className="cvm-route-via">
                    <Icon of="client" size={13} />
                    <span>{fallback}</span>
                    {result?.kind === 'default' && <span className="st cvm-route-hit">默认出口</span>}
                  </span>
                </td>
                <td />
              </tr>
            </tbody>
          </table>
          {(routing.priorities.length > 0 || canEdit) && (
            <PriorityStrip routing={routing} options={options} writable={canEdit} onChange={onChange} />
          )}
          {chains.error && canEdit && <ErrorBox error={chains.error} />}
          {notes.map((note, index) => (
            <p className="cvm-route-line cvm-route-notice" key={index}>
              <Icon of="info" size={13} />
              <span>{note}</span>
            </p>
          ))}
          {!baremetal && (
            <RouteDns dns={routing.dns} writable={canEdit} onChange={dns => onChange({ ...routing, dns })} />
          )}
          {canTest ? (
            <form
              className="cvm-route-line cvm-route-probe"
              aria-label="路由命中测试"
              onSubmit={event => {
                event.preventDefault();
                if (body && !matchBlocked && !(active && match.isPending))
                  match.mutate({ instanceId: desired.id, request: body, scope });
              }}
            >
              <Icon of="search" size={13} />
              <input
                className="f mono"
                aria-label="测试目标"
                aria-invalid={!!probeText.trim() && !probe}
                placeholder="203.0.113.9 tcp/443"
                value={probeText}
                onChange={event => setProbeText(event.currentTarget.value)}
              />
              <button className="btn sm" disabled={!body || !!matchBlocked || (active && match.isPending)}>
                {active && match.isPending ? '正在测试…' : '测试命中'}
              </button>
              <span className="cvm-route-probe-result" role="status">
                {matchBlocked ? (
                  <span className="hot">{matchBlocked}</span>
                ) : result ? (
                  result.kind === 'route' ? (
                    <>
                      命中 <b className="mono">{result.route.destination}</b> →{' '}
                      {targetName(targetKey(result.route), options)}
                      <span className="cvm-faint"> · 优先级 {result.priority}</span>
                      {result.blocked === 'chain_requires_tcp_udp' && (
                        <span className="hot">；该协议不会经线路转发。</span>
                      )}
                    </>
                  ) : result.kind === 'default' ? (
                    <>
                      未命中目标路由 → {fallback}
                      <span className="cvm-faint"> · 默认出口</span>
                    </>
                  ) : null
                ) : probeText.trim() && !probe ? (
                  <span className="cvm-faint">写成「IP 协议/端口」，例如 2001:db8::1 udp/53；未写协议按 TCP 443。</span>
                ) : probe ? (
                  <span className="cvm-faint">按 {probeLabel(probe)} 测试，只计算当前路由配置，不探测实际连通性。</span>
                ) : (
                  <span className="cvm-faint">输入目标地址，按服务端规则计算命中的路由与出口。</span>
                )}
              </span>
              {active && match.error && <ErrorBox error={match.error} />}
            </form>
          ) : (
            <p className="cvm-route-line note">命中测试需要路由配置权限。</p>
          )}
        </>
      )}
    </section>
  );
}
