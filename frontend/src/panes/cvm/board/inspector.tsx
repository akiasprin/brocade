// 组合台右侧检查器：选中容器的入口、出口与计算，以及待确认的组合变更。
// 所有写操作都带稳定的请求标识，重试同一操作不会重复创建资源；结果以服务端为准。
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type ReactNode } from 'react';
import {
  createCvmEntry,
  entryStateText,
  fetchCvmNicState,
  fetchCvmReadings,
  mountCvmNic,
  releaseCvmEntry,
  repointCvmEntry,
  setCvmRoutes,
  type CvmContainerRoutes,
  type CvmEntryBoard,
  type CvmEntryMode,
  type CvmEntryProtocol,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmWorkspace,
} from '../../../api';
import { navigate } from '../../../forge/route';
import { ErrorBox, SegmentedControl } from '../../../ui/bits';
import { CardRing } from '../../../ui/card-metrics';
import { useNow } from '../../../ui/clock';
import { Icon } from '../../../ui/icons';
import { useMetricQuery } from '../../../ui/metric-cadence';
import { percent, resourceTone, shareOf } from '../cards';
import { NIC_CATALOG, NIC_STATE, transportName, useNicRequestId } from '../network-common';
import { AppliedState } from '../presentation';
import { QUERY } from '../queries';
import {
  entryText,
  isDefault,
  nextFreePort,
  nicFor,
  parseService,
  repointRoute,
  routeLabel,
  withRoute,
  withoutRoute,
  type EntryMachine,
  type ExitTile,
  type Layer,
  type RuleLink,
} from './model';

export const ENTRY_BOARD = ['cvm-entries'] as const;

export type Proposal =
  | { kind: 'entry-new'; instance: string; row: string; whole: boolean }
  | { kind: 'entry-move'; instance: string; entry: string }
  | { kind: 'rule-new'; instance: string; exit: string }
  | { kind: 'rule-move'; instance: string; rule: string; exit: string };
export type Pick = { kind: 'entry' | 'rule'; instance: string };

function useSettled() {
  const client = useQueryClient();
  return () => {
    for (const key of [QUERY, ENTRY_BOARD, NIC_CATALOG, NIC_STATE]) void client.invalidateQueries({ queryKey: key });
  };
}

export function Inspector({
  workspace,
  instance,
  instances,
  entries,
  catalog,
  machines,
  exits,
  rules,
  pick,
  setPick,
  proposal,
  onSettle,
}: {
  workspace: CvmWorkspace;
  instance: CvmInstance | null;
  instances: CvmInstance[];
  entries: CvmEntryBoard;
  catalog: CvmNicCatalog | undefined;
  machines: EntryMachine[];
  exits: ExitTile[];
  rules: RuleLink[];
  pick: Pick | null;
  setPick: (next: Pick | null) => void;
  proposal: Proposal | null;
  onSettle: () => void;
}) {
  const settled = useSettled();
  const operationId = useNicRequestId();
  const readings = useMetricQuery({
    queryKey: ['cvm-readings'],
    queryFn: fetchCvmReadings,
    refetchInterval: 15_000,
    metricFacts: data => data.instances.map(item => item.state),
  });
  const now = useNow();
  const release = useMutation({
    mutationFn: (entry: CvmEntryBoard['entries'][number]) => {
      const body = { expected_revision: entry.revision };
      return releaseCvmEntry(entry.id, { ...body, request_id: operationId({ release: entry.id, ...body }) });
    },
    onSettled: settled,
  });
  const routes = useMutation({
    mutationFn: ({ target, next }: { target: CvmInstance; next: CvmContainerRoutes }) => {
      const body = {
        expected_generation: target.desired.generation,
        expected_revision: target.desired.spec.routing.revision,
        routes: next,
      };
      return setCvmRoutes(target.desired.id, {
        ...body,
        request_id: operationId({ instance: target.desired.id, ...body }),
      });
    },
    onSettled: settled,
  });
  if (!instance) {
    const l3 = entries.entries.filter(item => item.mode.kind === 'l3').length;
    const l4Rules = rules.filter(item => item.layer === 'L4').length;
    return (
      <div className="cz-inspector idle">
        <aside className="panel" aria-label="组合">
          <header className="cz-insp-head">
            <b>组合</b>
          </header>
          <div className="cz-insp-body">
            <div className="cvm-spec-figures cz-figures">
              <span>
                <b>{entries.entries.length}</b>
                <small>入口</small>
              </span>
              <span>
                <b>{instances.length}</b>
                <small>容器</small>
              </span>
              <span>
                <b>{rules.length}</b>
                <small>出口规则</small>
              </span>
            </div>
            <dl className="cvm-facts cz-split">
              <div>
                <dt>入口</dt>
                <dd>
                  L3 {l3} · L4 {entries.entries.length - l3}
                </dd>
              </div>
              <div>
                <dt>出口</dt>
                <dd>
                  L3 {rules.length - l4Rules} · L4 {l4Rules}
                </dd>
              </div>
            </dl>
            <ul className="cz-legend">
              <li>
                <i className="L3" />
                L3 · 整 IP / 网段
              </li>
              <li>
                <i className="L4" />
                L4 · 协议端口
              </li>
              <li>
                <i className="private" />
                内网
              </li>
            </ul>
          </div>
        </aside>
      </div>
    );
  }
  const id = instance.desired.id;
  const host = workspace.hosts.find(item => item.node_id === instance.node_id);
  const mine = entries.entries.filter(item => item.instance_id === id);
  const links = rules.filter(item => item.instance === id);
  const writable = workspace.can_write;
  const hasDefault = links.some(link => isDefault(link.route.destination) && !link.route.service);
  // Same freshness rule as the instance cards: a reading older than two minutes is unknown.
  const serverNow = readings.data
    ? readings.data.observed_at_unix_millis + Math.max(0, now - readings.dataUpdatedAt)
    : now;
  const reading = readings.data?.instances.find(item => item.id === id);
  const sample = reading?.latest;
  const current =
    !readings.error && reading?.state === 'live' && !!sample && serverNow - sample.observed_at_unix_millis <= 120_000;
  const rings = [
    {
      label: 'CPU',
      share: current && sample.cpu_usage_basis_points !== null ? sample.cpu_usage_basis_points / 10_000 : null,
      warn: 0.8,
      bad: 0.95,
    },
    {
      label: '内存',
      share: current ? shareOf(sample.memory_used_bytes, sample.memory_total_bytes) : null,
      warn: 0.85,
      bad: 0.95,
    },
    {
      label: '磁盘',
      share: current ? shareOf(sample.root_used_bytes, sample.root_total_bytes) : null,
      warn: 0.9,
      bad: 0.97,
    },
  ];
  return (
    <div className="cz-inspector">
      <aside className="panel" aria-label={instance.desired.spec.name}>
        <header className="cz-insp-head">
          <b>{instance.desired.spec.name}</b>
          <AppliedState instance={instance} />
        </header>
        {proposal && (
          <ProposalBlock
            key={JSON.stringify(proposal)}
            proposal={proposal}
            instance={instance}
            instances={instances}
            entries={entries}
            catalog={catalog}
            machines={machines}
            exits={exits}
            rules={rules}
            onClose={onSettle}
          />
        )}
        <div className="cz-insp-body">
          <div className="cz-insp-section">
            <span className="cz-insp-label">访问入口</span>
            {writable && (
              <button
                className={`cvm-text-link cz-row-action ${pick?.kind === 'entry' ? 'on' : ''}`}
                onClick={() => setPick(pick?.kind === 'entry' ? null : { kind: 'entry', instance: id })}
              >
                {pick?.kind === 'entry' ? '点选左侧地址…' : '＋ 入口'}
              </button>
            )}
          </div>
          {mine.length ? (
            mine.map(entry => {
              const text = entryText(entry);
              return (
                <div className="cz-row" key={entry.id}>
                  <span className={`cz-layer ${text.layer}`}>{text.badge}</span>
                  <span className="mono cz-row-text" title={text.text}>
                    {text.text}
                  </span>
                  <span className="cz-rtt mono">{text.target}</span>
                  <span className={`cz-state ${entry.state}`} title={entry.error ?? undefined}>
                    {entryStateText[entry.state]}
                  </span>
                  {writable && entry.state !== 'releasing' && (
                    <button
                      className="btn icon"
                      aria-label="撤销入口"
                      title="撤销这个入口"
                      disabled={release.isPending}
                      onClick={() => release.mutate(entry)}
                    >
                      <Icon of="close" size={12} />
                    </button>
                  )}
                </div>
              );
            })
          ) : (
            <p className="note cz-none">没有入口</p>
          )}
          {release.error && <ErrorBox error={release.error} />}
          <div className="cz-insp-section">
            <span className="cz-insp-label">出口</span>
            {writable && (
              <button
                className={`cvm-text-link cz-row-action ${pick?.kind === 'rule' ? 'on' : ''}`}
                onClick={() => setPick(pick?.kind === 'rule' ? null : { kind: 'rule', instance: id })}
              >
                {pick?.kind === 'rule' ? '点选右侧出口…' : '＋ 出口'}
              </button>
            )}
          </div>
          {links.map(link => {
            const exit = exits.find(item => item.id === link.exit);
            return (
              <div className="cz-row" key={link.key}>
                <span className={`cz-layer ${link.layer}`}>{link.layer}</span>
                <span className="mono cz-row-text" title={routeLabel(link.route)}>
                  {routeLabel(link.route)}
                </span>
                <span className="cz-rtt">{exit?.provider ?? '已卸载的网卡'}</span>
                {writable && (
                  <button
                    className="btn icon"
                    aria-label="撤销出口规则"
                    title="撤销这条出口规则"
                    disabled={routes.isPending}
                    onClick={() =>
                      routes.mutate({ target: instance, next: withoutRoute(instance.desired.spec.routing, link.index) })
                    }
                  >
                    <Icon of="close" size={12} />
                  </button>
                )}
              </div>
            );
          })}
          <div className="cz-row local">
            <span className="cz-layer">{hasDefault || links.length ? '其余' : '全部'}</span>
            <span className="cz-row-text">本机</span>
            <span className="cz-rtt">{host?.name ?? instance.node_id}</span>
          </div>
          {routes.error && <ErrorBox error={routes.error} />}
          <div className="cz-insp-section">
            <span className="cz-insp-label">计算</span>
          </div>
          <div className="cz-row">
            <b>{host?.name ?? instance.node_id}</b>
            <span className="cz-rtt">
              {instance.desired.spec.resources.vcpus} 核 ·{' '}
              {Math.round(instance.desired.spec.resources.memory_bytes / 1024 ** 2)} MiB
            </span>
          </div>
          <span className="node-card-resources cz-rings">
            {rings.map(ring => (
              <span className="node-card-resource" key={ring.label}>
                <CardRing share={ring.share} tone={resourceTone(ring.share, ring.warn, ring.bad)} />
                {ring.label}
                <b>{percent(ring.share)}</b>
              </span>
            ))}
          </span>
        </div>
        <footer className="cz-insp-foot">
          <button className="btn" onClick={() => navigate('cvm', { p: 'instance', id })}>
            详情
          </button>
        </footer>
      </aside>
    </div>
  );
}

/* ══ 拟定变更：在检查器顶部确认；新增入口与出口可在这里改协议、端口和匹配范围 ══ */

function ProposalBlock({
  proposal,
  instance,
  instances,
  entries,
  catalog,
  machines,
  exits,
  rules,
  onClose,
}: {
  proposal: Proposal;
  instance: CvmInstance;
  instances: CvmInstance[];
  entries: CvmEntryBoard;
  catalog: CvmNicCatalog | undefined;
  machines: EntryMachine[];
  exits: ExitTile[];
  rules: RuleLink[];
  onClose: () => void;
}) {
  const settled = useSettled();
  const operationId = useNicRequestId();
  const name = instance.desired.spec.name;
  const row =
    proposal.kind === 'entry-new'
      ? machines.flatMap(machine => machine.addresses).find(item => item.key === proposal.row)
      : null;
  const machine = row ? machines.find(item => item.addresses.includes(row)) : null;
  const shared = row?.offer.kind === 'shared' ? row.offer : null;
  const [protocol, setProtocol] = useState<CvmEntryProtocol>('tcp');
  const [target, setTarget] = useState(22);
  const [port, setPort] = useState<number | null>(() =>
    shared ? nextFreePort(shared, row?.usage, 'tcp', shared.port_start + 22) : null,
  );
  const exit =
    proposal.kind === 'rule-new' || proposal.kind === 'rule-move'
      ? exits.find(item => item.id === proposal.exit)
      : null;
  const nic = exit ? nicFor(instance, exit.id) : undefined;
  const network = useQuery({
    queryKey: [...NIC_STATE, instance.desired.id],
    queryFn: () => fetchCvmNicState(instance.desired.id),
    enabled: proposal.kind === 'rule-new' && !nic,
  });
  const hasDefault = rules.some(
    rule => rule.instance === instance.desired.id && isDefault(rule.route.destination) && !rule.route.service,
  );
  const [layer, setLayer] = useState<Layer>(exit?.kind !== 'private' && hasDefault ? 'L4' : 'L3');
  const defaultMatch = (next: Layer) =>
    next === 'L4' ? 'tcp/443' : exit?.kind === 'private' ? (exit.publication.spec.destinations[0] ?? '') : '0.0.0.0/0';
  const [match, setMatch] = useState(() => defaultMatch(layer));
  const [prefix, setPrefix] = useState(
    exit?.kind === 'private' ? (exit.publication.spec.destinations[0] ?? '0.0.0.0/0') : '0.0.0.0/0',
  );
  const [localError, setLocalError] = useState<string | null>(null);

  const apply = useMutation({
    mutationFn: async () => {
      if (proposal.kind === 'entry-new') {
        if (!row) throw new Error('入口地址已变化，请刷新');
        const mode: CvmEntryMode = shared
          ? { kind: 'l4', protocol, listen_port: port ?? 0, target_port: target }
          : { kind: 'l3' };
        const body = {
          instance_id: instance.desired.id,
          publication_id: row.publication.id,
          address: row.offer.address,
          mode,
        };
        return createCvmEntry({ ...body, request_id: operationId(body) });
      }
      if (proposal.kind === 'entry-move') {
        const entry = entries.entries.find(item => item.id === proposal.entry);
        if (!entry) throw new Error('入口已变化，请刷新');
        const body = { expected_revision: entry.revision, instance_id: instance.desired.id };
        return repointCvmEntry(entry.id, { ...body, request_id: operationId({ entry: entry.id, ...body }) });
      }
      const routing = instance.desired.spec.routing;
      const write = (next: CvmContainerRoutes) => {
        const body = {
          expected_generation: instance.desired.generation,
          expected_revision: routing.revision,
          routes: next,
        };
        return setCvmRoutes(instance.desired.id, { ...body, request_id: operationId(body) });
      };
      if (proposal.kind === 'rule-move') {
        const link = rules.find(item => item.key === proposal.rule);
        if (!link || !nic) throw new Error('出口已变化，请刷新');
        return write(repointRoute(routing, link.index, nic));
      }
      if (!exit) throw new Error('出口已变化，请刷新');
      if (!nic) {
        const transport = network.data?.mount_options[exit.id]?.transports[0];
        if (!transport) throw new Error(network.data?.mount_options[exit.id]?.error ?? '双方没有共同支持的承载');
        const body = { expected_generation: instance.desired.generation, publication_id: exit.id, transport };
        return mountCvmNic(instance.desired.id, { ...body, request_id: operationId(body) });
      }
      const route =
        layer === 'L3'
          ? { destination: match.trim(), interface_id: nic.id }
          : { destination: prefix.trim(), interface_id: nic.id, service: parseService(match) };
      return write(withRoute(routing, route, nic));
    },
    onSuccess: onClose,
    onSettled: settled,
  });

  let title: string;
  let lines: string[];
  let action: string;
  let fields: ReactNode = null;
  let ready = true;
  if (proposal.kind === 'entry-new') {
    if (!row || !machine) return null;
    const visible = row.offer.public_address ?? row.offer.address;
    title = `新增入口 · ${machine.name}`;
    lines = shared
      ? [
          `${visible}:${port ?? '—'} ${protocol === 'tcp_udp' ? 'TCP+UDP' : protocol.toUpperCase()} → ${name}:${target}`,
          '只转发所选协议端口',
        ]
      : [`${visible} 整 IP → ${name}`, '全部协议与端口'];
    lines.push(machine.node === instance.node_id ? '容器就在这台机器上' : '经机器对通道送到容器所在宿主');
    if (shared) {
      ready = !!port && port >= shared.port_start && port <= shared.port_end && target >= 1 && target <= 65535;
      fields = (
        <div className="cz-prop-fields">
          <SegmentedControl
            value={protocol}
            options={[
              { value: 'tcp' as CvmEntryProtocol, label: 'TCP' },
              { value: 'udp' as CvmEntryProtocol, label: 'UDP' },
              { value: 'tcp_udp' as CvmEntryProtocol, label: 'TCP+UDP' },
            ]}
            ariaLabel="协议"
            onChange={next => {
              setProtocol(next);
              setPort(old => nextFreePort(shared, row.usage, next, old ?? undefined));
            }}
          />
          <label>
            入口端口
            <input
              className="f mono"
              inputMode="numeric"
              value={port ?? ''}
              onChange={event => setPort(Number(event.target.value) || null)}
            />
          </label>
          <label>
            容器端口
            <input
              className="f mono"
              inputMode="numeric"
              value={target}
              onChange={event => setTarget(Number(event.target.value) || 0)}
            />
          </label>
        </div>
      );
    }
    action = '接入';
  } else if (proposal.kind === 'entry-move') {
    const entry = entries.entries.find(item => item.id === proposal.entry);
    if (!entry) return null;
    const text = entryText(entry);
    title = `改派给 ${name}`;
    lines = [
      `${text.text} ${text.target}`,
      `原属 ${instances.find(item => item.desired.id === entry.instance_id)?.desired.spec.name ?? '另一个容器'}，改后它不再有这个入口`,
      '地址与端口不变',
    ];
    action = '改派';
  } else if (proposal.kind === 'rule-move') {
    const link = rules.find(item => item.key === proposal.rule);
    const from = exits.find(item => item.id === link?.exit);
    if (!link || !exit) return null;
    title = `改由 ${exit.provider} 出网`;
    lines = [`${routeLabel(link.route)}：${from?.provider ?? '原网卡'} → ${exit.provider}`];
    action = '更换';
  } else {
    if (!exit) return null;
    if (!nic) {
      const option = network.data?.mount_options[exit.id];
      title = `接入出口 · ${exit.provider}`;
      lines = [
        `${exit.publication.spec.name}：为容器挂载一张出口网卡`,
        exit.publication.spec.route_presets.length
          ? `按发布预设写入路由 ${exit.publication.spec.route_presets.join(' ')}`
          : '挂载后再添加 L3 或 L4 出口规则',
      ];
      if (option?.transports.length) lines.push(`承载 ${transportName[option.transports[0]]}`);
      ready = !!option?.transports.length && !network.isPending;
      if (option?.error) lines.push(option.error);
      action = '接入';
    } else {
      title = `新增出口 · ${exit.provider}`;
      lines = [
        layer === 'L3'
          ? `${match} → ${exit.provider}`
          : `${match}${isDefault(prefix) ? '' : ` ${prefix}`} → ${exit.provider}`,
      ];
      ready = layer === 'L3' ? match.trim().length > 0 : !!parseService(match);
      fields = (
        <div className="cz-prop-fields">
          <SegmentedControl
            value={layer}
            options={[
              { value: 'L3' as Layer, label: 'L3 网段' },
              { value: 'L4' as Layer, label: 'L4 协议端口' },
            ]}
            ariaLabel="匹配方式"
            onChange={next => {
              setLayer(next);
              setMatch(defaultMatch(next));
              setLocalError(null);
            }}
          />
          <label className="wide">
            {layer === 'L3' ? '目标网段' : '协议/端口'}
            <input
              className="f mono"
              value={match}
              placeholder={layer === 'L3' ? '0.0.0.0/0' : 'tcp/443 或 udp/1000-2000'}
              onChange={event => setMatch(event.target.value)}
            />
          </label>
          {layer === 'L4' && (
            <label className="wide">
              目标网段
              <input className="f mono" value={prefix} onChange={event => setPrefix(event.target.value)} />
            </label>
          )}
        </div>
      );
      action = '新增';
    }
  }
  return (
    <section className="cz-proposal" aria-label="待确认的组合变更">
      <b>{title}</b>
      <ul>
        {lines.map(line => (
          <li key={line}>{line}</li>
        ))}
      </ul>
      {fields}
      {localError && <p className="cvm-notice">{localError}</p>}
      {apply.error && <ErrorBox error={apply.error} />}
      {network.error && <ErrorBox error={network.error} />}
      <footer>
        <button className="btn" onClick={onClose} disabled={apply.isPending}>
          取消
        </button>
        <button className="btn primary" disabled={!ready || apply.isPending || !catalog} onClick={() => apply.mutate()}>
          {apply.isPending ? '正在提交…' : action}
        </button>
      </footer>
    </section>
  );
}
