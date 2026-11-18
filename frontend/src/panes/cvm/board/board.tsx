// 组合台：入口 | 计算 | 出口，流量从左到右。
// - 访问入口：各机器发布的地址。共享地址按协议端口（L4）分给多个容器，独立地址整 IP（L3）交给一个容器；
//   入口投递到容器主网卡，不占网卡槽位，可以改派给同一用户的另一个容器。
// - 计算：提供算力的宿主，容器是其中的条目。首版不迁移计算，容器不能拖到别的宿主。
// - 出口：出口网络（网卡发布）。容器按目标网段（L3）或协议端口（L4）选择出网的机器，未匹配的流量走所在机器本机。
// 拖线头或在检查器点「＋ 入口 / ＋ 出口」再点目标，改动先画成虚线，在检查器确认后提交。
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import {
  entryStateText,
  type CvmChainCatalog,
  type CvmEntry,
  type CvmEntryBoard,
  type CvmInstance,
  type CvmNicCatalog,
  type CvmWorkspace,
} from '../../../api';
import { RegionFlag } from '../../../ui/region-flag';
import { NetworkLogsButton } from '../network-logs';
import { capacity, stackHosts, statusText } from '../queries';
import { Inspector, type Proposal, type Pick } from './inspector';
import {
  entryMachines,
  entryText,
  exitTiles,
  isDefault,
  nicFor,
  routeLabel,
  ruleLinks,
  type AddressRow,
  type RuleLink,
} from './model';
import { along, curve, useAnchorGeometry, WireLabel, type Point } from './wires';

/** 入口的来源：已有入口（改派），或共享地址的空闲端口、空闲的独立地址（新增）。 */
export type EntrySource = { type: 'entry'; entry: string } | { type: 'port' | 'whole'; row: string };
type Drag =
  | { kind: 'entry'; source: EntrySource; from: Point; at: Point }
  | { kind: 'rule'; instance: string; rule: string | null; from: Point; at: Point };
type Verdict = { id: string; ok: boolean; reason: string | null };

const sourceAnchor = (source: EntrySource) =>
  source.type === 'entry' ? `entry:${source.entry}` : `${source.type}:${source.row}`;

export function CompositionBoard({
  workspace,
  entries,
  catalog,
  chains,
  selected,
  onSelect,
}: {
  workspace: CvmWorkspace;
  entries: CvmEntryBoard;
  catalog: CvmNicCatalog | undefined;
  chains: CvmChainCatalog | undefined;
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  const instances = workspace.instances.filter(instance => instance.reserved && instance.desired.power !== 'deleted');
  const machines = entryMachines(entries, workspace.hosts);
  const exits = exitTiles(catalog, workspace.hosts);
  const rules = ruleLinks(instances);
  const writable = workspace.can_write;
  const { root: board, geo } = useAnchorGeometry<HTMLDivElement>();
  const [hover, setHover] = useState<string | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [target, setTarget] = useState<Verdict | null>(null);
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [pick, setPick] = useState<Pick | null>(null);

  const local = (event: { clientX: number; clientY: number }): Point => {
    const box = board.current!.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };
  const instanceOf = (id: string) => instances.find(item => item.desired.id === id);
  const entryOf = (id: string) => entries.entries.find(item => item.id === id);
  const rowOf = (key: string) => machines.flatMap(machine => machine.addresses).find(row => row.key === key);

  /* ── 判定：只做界面上的预检，最终以服务端为准 ── */
  const judgePill = (source: EntrySource, id: string): Verdict => {
    const instance = instanceOf(id);
    if (!instance) return { id, ok: false, reason: '容器不可用' };
    if (source.type === 'entry') {
      const entry = entryOf(source.entry);
      if (entry?.instance_id === id) return { id, ok: false, reason: '已经是这个容器' };
      const owner = entry && instanceOf(entry.instance_id);
      if (owner && (owner.tenant_id !== instance.tenant_id || owner.user_id !== instance.user_id))
        return { id, ok: false, reason: '只能改派给同一用户' };
      if (entry?.state === 'releasing') return { id, ok: false, reason: '入口正在撤销' };
    } else {
      const row = rowOf(source.row);
      if (
        row &&
        !row.publication.spec.grants.some(g => g.tenant_id === instance.tenant_id && g.user_id === instance.user_id)
      )
        return { id, ok: false, reason: '未获授权' };
    }
    return { id, ok: true, reason: null };
  };
  const judgeExit = (rule: string | null, exit: string, instanceId: string): Verdict => {
    const link = rule ? rules.find(item => item.key === rule) : null;
    const instance = instanceOf(instanceId);
    if (exit.startsWith('chain:')) {
      const chain = chains?.chains.find(c => `chain:${c.id}` === exit);
      if (!instance || !chain) return { id: exit, ok: false, reason: '线路目录读取中或已失效' };
      if (link?.exit === exit) return { id: exit, ok: false, reason: '当前就是这里' };
      const owner = chains?.owner;
      if (!owner || owner.tenant_id !== instance.tenant_id || owner.user_id !== instance.user_id)
        return { id: exit, ok: false, reason: '正在检查容器使用者授权' };
      if (!chain.authorized) return { id: exit, ok: false, reason: `${owner.name} 未获授权` };
      if (chain.unavailable_reason) return { id: exit, ok: false, reason: chain.unavailable_reason };
      if (!chains?.supported) return { id: exit, ok: false, reason: chains?.reason ?? '宿主尚不支持线路出口' };
      if (link?.route.destination.includes(':') && !chain.ipv6)
        return { id: exit, ok: false, reason: '线路未确认 IPv6 落地' };
      return { id: exit, ok: true, reason: null };
    }
    const tile = exits.find(item => item.id === exit);
    if (!instance || !tile) return { id: exit, ok: false, reason: '不可用' };
    if (link?.exit === exit) return { id: exit, ok: false, reason: '当前就是这里' };
    if (link && !nicFor(instance, exit)) return { id: exit, ok: false, reason: '需先接入这个出口' };
    if (!tile.publication.spec.grants.some(g => g.tenant_id === instance.tenant_id && g.user_id === instance.user_id))
      return { id: exit, ok: false, reason: '未获授权' };
    if (link && !tile.publication.spec.destinations.some(net => net === link.route.destination || isDefault(net)))
      return { id: exit, ok: false, reason: '不通这个网段' };
    return { id: exit, ok: true, reason: null };
  };

  /* ── 拖动 ── */
  const onPointerMove = (event: PointerEvent) => {
    if (!drag || !board.current) return;
    setDrag(old => (old ? { ...old, at: local(event) } : old));
    const hit = document.elementFromPoint(event.clientX, event.clientY);
    if (drag.kind === 'entry') {
      const pill = hit?.closest<HTMLElement>('[data-pill]');
      setTarget(pill ? judgePill(drag.source, pill.dataset.pill!) : null);
    } else {
      const tile = hit?.closest<HTMLElement>('[data-exit]');
      setTarget(tile ? judgeExit(drag.rule, tile.dataset.exit!, drag.instance) : null);
    }
  };
  /* 松手后浏览器会在起点与落点的共同父元素（台面）上补发 click；这一次不能当成「点空白取消选中」。 */
  const justDropped = useRef(false);
  const onPointerUp = () => {
    if (!drag) return;
    justDropped.current = true;
    window.setTimeout(() => (justDropped.current = false), 0);
    const verdict = target;
    setDrag(null);
    setTarget(null);
    if (!verdict?.ok) return;
    if (drag.kind === 'entry') {
      onSelect(verdict.id);
      setProposal(
        drag.source.type === 'entry'
          ? { kind: 'entry-move', instance: verdict.id, entry: drag.source.entry }
          : { kind: 'entry-new', instance: verdict.id, row: drag.source.row, whole: drag.source.type === 'whole' },
      );
    } else {
      onSelect(drag.instance);
      setProposal(
        drag.rule
          ? { kind: 'rule-move', instance: drag.instance, rule: drag.rule, exit: verdict.id }
          : { kind: 'rule-new', instance: drag.instance, exit: verdict.id },
      );
    }
  };
  useEffect(() => {
    if (!drag) return;
    addEventListener('pointermove', onPointerMove);
    addEventListener('pointerup', onPointerUp);
    return () => {
      removeEventListener('pointermove', onPointerMove);
      removeEventListener('pointerup', onPointerUp);
    };
  });
  const begin = (event: ReactPointerEvent, next: Drag) => {
    if (!writable || event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    setProposal(null);
    setPick(null);
    setDrag(next);
  };
  const startEntry = (event: ReactPointerEvent, source: EntrySource) => {
    const from = geo.anchors[sourceAnchor(source)];
    if (from) begin(event, { kind: 'entry', source, from, at: local(event) });
  };
  const startRule = (event: ReactPointerEvent, instance: string, rule: string | null) => {
    const from = geo.anchors[`out:${instance}`];
    if (!from) return;
    onSelect(instance);
    begin(event, { kind: 'rule', instance, rule, from, at: local(event) });
  };

  /* ── 点选（不拖动的路径）：先在检查器里选「＋ 入口 / ＋ 出口」，再点目标 ── */
  const pickSource = (source: EntrySource) => {
    if (pick?.kind !== 'entry') return false;
    const verdict = judgePill(source, pick.instance);
    if (verdict.ok)
      setProposal(
        source.type === 'entry'
          ? { kind: 'entry-move', instance: pick.instance, entry: source.entry }
          : { kind: 'entry-new', instance: pick.instance, row: source.row, whole: source.type === 'whole' },
      );
    setPick(null);
    return true;
  };

  /* ── 焦点：悬停或选中某个容器时，只点亮它的入口与出口 ── */
  const hoverEntry = hover?.startsWith('entry:') ? entryOf(hover.slice(6)) : null;
  const focus =
    (drag?.kind === 'rule' ? drag.instance : null) ??
    (hover?.startsWith('pill:') ? hover.slice(5) : null) ??
    hoverEntry?.instance_id ??
    selected;
  const focusExit = hover?.startsWith('exit:') ? hover.slice(5) : null;
  const litEntry = (entry: CvmEntry) => (focusExit ? false : focus ? entry.instance_id === focus : true);
  const litRule = (rule: RuleLink) => (focusExit ? rule.exit === focusExit : focus ? rule.instance === focus : true);
  const quiet = !!drag || !!pick;
  const selectedInstance = instances.find(item => item.desired.id === selected) ?? null;
  const proposalFor = proposal && proposal.instance === selectedInstance?.desired.id ? proposal : null;
  const proposedEntry = proposal?.kind === 'entry-move' ? proposal.entry : null;
  const proposedRule = proposal?.kind === 'rule-move' ? proposal.rule : null;
  const entryRow = (entry: CvmEntry) => {
    const owner = instanceOf(entry.instance_id);
    const text = entryText(entry);
    const lifted =
      proposedEntry === entry.id ||
      (drag?.kind === 'entry' && drag.source.type === 'entry' && drag.source.entry === entry.id);
    return (
      <div
        key={entry.id}
        className={`cz-map ${text.layer} ${entry.state} ${!quiet && !litEntry(entry) ? 'dim' : ''} ${lifted ? 'lifted' : ''} ${
          pick?.kind === 'entry' ? 'pickable' : ''
        }`}
        onPointerEnter={() => setHover(`entry:${entry.id}`)}
        onPointerLeave={() => setHover(null)}
        onClick={() => {
          if (!pickSource({ type: 'entry', entry: entry.id })) onSelect(entry.instance_id);
        }}
        title={entry.error ?? undefined}
      >
        <span className={`cz-layer ${text.layer}`}>{text.badge}</span>
        <span className="mono cz-map-addr">
          {entry.mode.kind === 'l3' ? entry.visible_address : entry.mode.listen_port}
        </span>
        <span className="cz-map-to">
          {entry.state !== 'active' && (
            <i className={`cz-state-dot ${entry.state}`} role="img" aria-label={entryStateText[entry.state]} />
          )}
          {owner?.desired.spec.name ?? '其他容器'}
          {entry.mode.kind === 'l4' && <span className="mono">:{entry.mode.target_port}</span>}
        </span>
        {writable && entry.state !== 'releasing' && (
          <span
            className="cz-socket out"
            data-anchor={`entry:${entry.id}`}
            title="拖到另一个容器，把这个入口改派给它"
            onPointerDown={event => startEntry(event, { type: 'entry', entry: entry.id })}
          />
        )}
        {(!writable || entry.state === 'releasing') && (
          <span className="cz-socket out idle" data-anchor={`entry:${entry.id}`} />
        )}
      </div>
    );
  };
  const freeRow = (row: AddressRow, type: 'port' | 'whole', label: string, detail: string, mono: boolean) => {
    const source: EntrySource = { type, row: row.key };
    const proposed = proposal?.kind === 'entry-new' && proposal.row === row.key;
    return (
      <div
        key={`${type}:${row.key}`}
        className={`cz-map free ${proposed ? 'proposed' : ''} ${pick?.kind === 'entry' ? 'pickable' : ''}`}
        onClick={() => pickSource(source)}
      >
        <span className={mono ? 'mono cz-map-addr' : 'cz-map-addr'}>{label}</span>
        <span className="cz-map-to">{detail}</span>
        <span
          className="cz-socket out free"
          data-anchor={sourceAnchor(source)}
          title="拖到容器上，新增一个入口"
          onPointerDown={event => startEntry(event, source)}
        />
      </div>
    );
  };
  const hosts = stackHosts(workspace);
  const hostIds = new Set(hosts.map(host => host.node_id));
  // Rules between the same container and exit share one wire path; label the pair once.
  const pairs = new Map<string, RuleLink[]>();
  for (const rule of rules) {
    const key = `${rule.instance}>${rule.exit}`;
    pairs.set(key, [...(pairs.get(key) ?? []), rule]);
  }
  const orphanHosts = [...new Set(instances.map(item => item.node_id))].filter(id => !hostIds.has(id));

  return (
    <div className={`cz-layout ${drag ? 'dragging' : ''}`}>
      <div
        className="cz-board"
        ref={board}
        onClick={event => {
          if (justDropped.current || event.target !== event.currentTarget) return;
          onSelect(null);
          setProposal(null);
          setPick(null);
        }}
      >
        {/* ── 访问入口 ── */}
        <section className="cz-col cz-entries" aria-label="访问入口">
          <header className="cz-col-head">
            <b>访问入口</b>
            <small>L3 整 IP · L4 协议端口</small>
          </header>
          {machines.map(machine => (
            <div className="cz-addr" key={machine.node}>
              <div className="cz-addr-head">
                <b>{machine.name}</b>
                {hostIds.has(machine.node) && <span className="cz-kind">兼计算</span>}
                <NetworkLogsButton
                  iconOnly
                  title="入口机器网络日志"
                  allowed={workspace.can_manage_network}
                  machines={[{ node_id: machine.node, name: machine.name, role: '入口' }]}
                />
              </div>
              {machine.addresses.map(row => {
                const visible = row.offer.public_address ?? row.offer.address;
                const open = writable && row.publication.admitting;
                if (row.offer.kind === 'shared')
                  return (
                    <div className="cz-addr-group" key={row.key}>
                      <div className="cz-ip">
                        <span className="mono">{visible}</span>
                        <small>
                          共享 · {row.offer.port_start}–{row.offer.port_end}
                        </small>
                      </div>
                      {row.entries.map(entryRow)}
                      {open && freeRow(row, 'port', '＋ 端口', 'L4', false)}
                    </div>
                  );
                if (row.entries.length) return row.entries.map(entryRow);
                if (row.usage?.whole_taken)
                  return (
                    <div key={row.key} className="cz-map taken">
                      <span className="mono cz-map-addr">{visible}</span>
                      <span className="cz-map-to">已占用</span>
                    </div>
                  );
                return open ? (
                  freeRow(row, 'whole', visible, '整 IP 空闲', true)
                ) : (
                  <div key={row.key} className="cz-map taken">
                    <span className="mono cz-map-addr">{visible}</span>
                    <span className="cz-map-to">停止新增</span>
                  </div>
                );
              })}
              {machine.orphans.map(entryRow)}
            </div>
          ))}
          {!machines.length && <p className="note cz-col-empty">还没有发布访问入口地址</p>}
        </section>

        {/* ── 计算 ── */}
        <section className="cz-col cz-compute" aria-label="计算">
          <header className="cz-col-head">
            <b>计算</b>
            <small>{hosts.length} 台宿主</small>
          </header>
          {[
            ...hosts.map(host => ({ id: host.node_id, name: host.name, host })),
            ...orphanHosts.map(id => ({ id, name: id, host: null })),
          ].map(({ id, name, host }) => {
            const here = instances.filter(item => item.node_id === id);
            const caps = host?.capabilities;
            const used = here.reduce((sum, item) => sum + item.desired.spec.resources.vcpus, 0);
            const memory = here.reduce((sum, item) => sum + item.desired.spec.resources.memory_bytes, 0);
            return (
              <div key={id} className="cz-host">
                <div className="cz-host-head">
                  <b>{name}</b>
                  {host && !host.fresh && <span className="cz-kind warn">离线</span>}
                  <NetworkLogsButton
                    iconOnly
                    title="宿主网络配置日志"
                    allowed={workspace.can_manage_network}
                    machines={[{ node_id: id, name, role: '计算' }]}
                  />
                </div>
                {caps && (
                  <div className="cz-cap" title="逻辑已分配 / 宿主基准 · 允许超售">
                    {[
                      {
                        label: 'vCPU',
                        used,
                        total: caps.allocatable_vcpus,
                        text: `${used} / ${caps.allocatable_vcpus}`,
                      },
                      {
                        label: '内存',
                        used: memory,
                        total: caps.allocatable_memory_bytes,
                        text: `${capacity(memory)} / ${capacity(caps.allocatable_memory_bytes)}`,
                      },
                    ].map(row => (
                      <span key={row.label}>
                        <small>{row.label}</small>
                        <b>
                          {row.text}
                          {row.used > row.total ? ' · 超售' : ''}
                        </b>
                        <i className="cz-meter" aria-hidden="true">
                          <i style={{ width: `${row.total > 0 ? Math.min(100, (row.used / row.total) * 100) : 0}%` }} />
                        </i>
                      </span>
                    ))}
                  </div>
                )}
                <div className="cz-pills">
                  {here.map(instance => {
                    const verdict = drag?.kind === 'entry' && target?.id === instance.desired.id ? target : null;
                    const lit =
                      (focus === null || focus === instance.desired.id) &&
                      (focusExit === null ||
                        rules.some(rule => rule.instance === instance.desired.id && rule.exit === focusExit));
                    return (
                      <Pill
                        key={instance.desired.id}
                        instance={instance}
                        selected={selected === instance.desired.id}
                        dim={!quiet && !lit}
                        drop={
                          verdict
                            ? verdict.ok
                              ? 'ok'
                              : 'no'
                            : drag?.kind === 'entry' || pick?.kind === 'entry'
                              ? 'can'
                              : null
                        }
                        reason={verdict && !verdict.ok ? verdict.reason : null}
                        local={
                          !rules.some(
                            rule =>
                              rule.instance === instance.desired.id &&
                              isDefault(rule.route.destination) &&
                              !rule.route.service,
                          )
                        }
                        writable={writable}
                        onSelect={() => onSelect(selected === instance.desired.id ? null : instance.desired.id)}
                        onHover={value => setHover(value ? `pill:${instance.desired.id}` : null)}
                        onOut={event => startRule(event, instance.desired.id, null)}
                      />
                    );
                  })}
                  {!here.length && <span className="cz-empty">空闲</span>}
                </div>
              </div>
            );
          })}
        </section>

        {/* ── 出口 ── */}
        <section className="cz-col cz-exits" aria-label="出口">
          <header className="cz-col-head">
            <b>出口</b>
            <small>L3 网段 · L4 协议端口</small>
          </header>
          {exits.map(exit => {
            const links = rules.filter(item => item.exit === exit.id);
            const verdict = drag?.kind === 'rule' && target?.id === exit.id ? target : null;
            const proposed =
              (proposal?.kind === 'rule-new' || proposal?.kind === 'rule-move') && proposal.exit === exit.id;
            const lit = focusExit ? focusExit === exit.id : focus ? links.some(rule => rule.instance === focus) : true;
            return (
              <div
                key={exit.id}
                data-exit={exit.id}
                className={`cz-net ${verdict ? (verdict.ok ? 'drop-ok' : 'drop-no') : ''} ${
                  drag?.kind === 'rule' || pick?.kind === 'rule' ? 'droppable' : ''
                } ${proposed ? 'drop-ok' : ''} ${!quiet && !proposed && !lit ? 'dim' : ''}`}
                onPointerEnter={() => setHover(`exit:${exit.id}`)}
                onPointerLeave={() => setHover(null)}
                onClick={() => {
                  if (pick?.kind !== 'rule') return;
                  const result = judgeExit(null, exit.id, pick.instance);
                  if (result.ok) setProposal({ kind: 'rule-new', instance: pick.instance, exit: exit.id });
                  setPick(null);
                }}
              >
                <span className="cz-socket" data-anchor={`exit:${exit.id}`} />
                <div className="cz-net-head">
                  <b>{exit.publication.spec.name}</b>
                  <span className="cz-kind">
                    {exit.kind === 'private' ? '内网' : exit.kind === 'shared' ? '共享 NAT' : '独立地址'}
                  </span>
                  <NetworkLogsButton
                    iconOnly
                    title="出口网络日志"
                    allowed={workspace.can_manage_network}
                    machines={[{ node_id: exit.publication.spec.provider_node, name: exit.provider, role: '出口' }]}
                  />
                </div>
                <div className="cz-net-sub">{exit.provider}</div>
                <div className="cz-net-meta">
                  <span className="mono">
                    {exit.kind === 'private'
                      ? exit.publication.spec.destinations.join(' ')
                      : (exit.address ?? (exit.kind === 'dedicated' ? '独立地址' : '共享出口'))}
                  </span>
                  <span>{links.length ? `${links.length} 条规则` : '未使用'}</span>
                </div>
                {verdict && !verdict.ok && <div className="cz-reason">{verdict.reason}</div>}
              </div>
            );
          })}
          {!exits.length && <p className="note cz-col-empty">还没有可用的出口网络</p>}
          <div className="cz-net cz-chains" aria-label="线路出口">
            <div className="cz-net-head">
              <b>线路</b>
              <span className="cz-kind">主网卡 · TCP/UDP</span>
            </div>
            {(chains?.chains ?? []).map(chain => {
              const id = `chain:${chain.id}`;
              const verdict = (drag?.kind === 'rule' || pick?.kind === 'rule') && target?.id === id ? target : null;
              const links = rules.filter(r => r.exit === id);
              const proposed =
                (proposal?.kind === 'rule-new' || proposal?.kind === 'rule-move') && proposal.exit === id;
              return (
                <button
                  type="button"
                  key={id}
                  data-exit={id}
                  title={
                    chain.unavailable_reason ??
                    (chain.authorized
                      ? `${chains?.owner?.name ?? '使用者'} 已获授权`
                      : chains?.owner
                        ? `${chains.owner.name} 未获授权`
                        : '选择容器检查授权')
                  }
                  className={`cz-chain-row ${chain.reachable === false ? 'failed' : ''} ${verdict ? (verdict.ok ? 'drop-ok' : 'drop-no') : ''} ${proposed ? 'drop-ok' : ''}`}
                  onPointerEnter={() => setHover(`exit:${id}`)}
                  onPointerLeave={() => setHover(null)}
                  onClick={() => {
                    if (pick?.kind !== 'rule') return;
                    const result = judgeExit(null, id, pick.instance);
                    setTarget(result);
                    if (result.ok) {
                      setProposal({ kind: 'rule-new', instance: pick.instance, exit: id });
                      setPick(null);
                    }
                  }}
                >
                  <span className="cz-socket" data-anchor={`exit:${id}`} />
                  <RegionFlag code={chain.country} />
                  <span className="cz-chain-name">{chain.name}</span>
                  <small>
                    {chain.reachable === false ? '不通' : chain.rtt_ms !== null ? `${chain.rtt_ms} ms` : '未拨测'}
                  </small>
                  <small>
                    {links.length
                      ? `${links.length} 条`
                      : chain.unavailable_reason
                        ? '不支持'
                        : chain.authorized
                          ? '可用'
                          : '未授权'}
                  </small>
                  {verdict && !verdict.ok && <span className="cz-reason">{verdict.reason}</span>}
                </button>
              );
            })}
            {!chains?.chains.length && <p className="note">选择容器后显示其使用者有授权的线路。</p>}
            {!!chains?.hidden_unauthorized && <p className="note">另有 {chains.hidden_unauthorized} 条未授权线路</p>}
            {chains?.reason && selected && <p className="note">{chains.reason}</p>}
          </div>
        </section>

        {/* ── 连线：线在卡片下层，标签、把手、待确认与拖动中的线浮在卡片之上 ── */}
        <svg className="cz-wires" width={geo.width} height={geo.height} aria-hidden="true">
          {entries.entries.map(entry => {
            const from = geo.anchors[`entry:${entry.id}`];
            const to = geo.anchors[`in:${entry.instance_id}`];
            if (!from || !to) return null;
            const lifted =
              proposedEntry === entry.id ||
              (drag?.kind === 'entry' && drag.source.type === 'entry' && drag.source.entry === entry.id);
            return (
              <g
                key={entry.id}
                className={`cz-wire in ${entryText(entry).layer} ${litEntry(entry) ? 'on' : 'off'} ${lifted ? 'lifted' : ''} ${entry.state}`}
              >
                <path d={curve(from, to)} />
              </g>
            );
          })}
          {rules.map(rule => {
            const from = geo.anchors[`out:${rule.instance}`];
            const to = rule.exit ? geo.anchors[`exit:${rule.exit}`] : undefined;
            if (!from || !to) return null;
            const exit = exits.find(item => item.id === rule.exit);
            const lifted = proposedRule === rule.key || (drag?.kind === 'rule' && drag.rule === rule.key);
            const status = instanceOf(rule.instance)?.status ?? '';
            return (
              <g
                key={rule.key}
                className={`cz-wire out ${rule.layer} ${exit?.kind === 'private' ? 'private' : ''} ${
                  litRule(rule) ? 'on' : 'off'
                } ${lifted ? 'lifted' : ''} ${status}`}
              >
                <path d={curve(from, to)} />
              </g>
            );
          })}
        </svg>
        <svg className="cz-wires cz-wires-over" width={geo.width} height={geo.height} aria-hidden="true">
          {rules.map(rule => {
            const from = geo.anchors[`out:${rule.instance}`];
            const to = rule.exit ? geo.anchors[`exit:${rule.exit}`] : undefined;
            if (!from || !to) return null;
            const on = litRule(rule);
            const pair = pairs.get(`${rule.instance}>${rule.exit}`) ?? [rule];
            const showLabel = on && !drag && pair[0] === rule && (focus === rule.instance || focusExit === rule.exit);
            const lifted = proposedRule === rule.key || (drag?.kind === 'rule' && drag.rule === rule.key);
            const handle = along(from, to, 0.86);
            return (
              <g key={rule.key}>
                {showLabel && (
                  <WireLabel
                    at={along(from, to, 0.5)}
                    text={`${routeLabel(rule.route)}${pair.length > 1 ? ` +${pair.length - 1}` : ''}`}
                  />
                )}
                {writable && selected === rule.instance && !drag && !lifted && (
                  <circle
                    className="cz-handle"
                    cx={handle.x}
                    cy={handle.y}
                    r={6}
                    onPointerDown={event => startRule(event, rule.instance, rule.key)}
                  >
                    <title>拖到另一个出口即可更换</title>
                  </circle>
                )}
              </g>
            );
          })}
          {proposal &&
            (() => {
              const ends: [string, string] =
                proposal.kind === 'entry-move'
                  ? [`entry:${proposal.entry}`, `in:${proposal.instance}`]
                  : proposal.kind === 'entry-new'
                    ? [`${proposal.whole ? 'whole' : 'port'}:${proposal.row}`, `in:${proposal.instance}`]
                    : [`out:${proposal.instance}`, `exit:${proposal.exit}`];
              const from = geo.anchors[ends[0]];
              const to = geo.anchors[ends[1]];
              return from && to ? (
                <g className="cz-wire proposed on">
                  <path d={curve(from, to)} />
                </g>
              ) : null;
            })()}
          {drag && (
            <g className="cz-wire live on">
              <path d={curve(drag.from, drag.at)} />
              <circle cx={drag.at.x} cy={drag.at.y} r={5} />
            </g>
          )}
        </svg>
      </div>
      <Inspector
        workspace={workspace}
        instance={selectedInstance}
        instances={instances}
        entries={entries}
        catalog={catalog}
        chains={chains}
        machines={machines}
        exits={exits}
        rules={rules}
        pick={pick}
        setPick={setPick}
        proposal={proposalFor}
        onSettle={() => setProposal(null)}
      />
    </div>
  );
}

function Pill({
  instance,
  selected,
  dim,
  drop,
  reason,
  local,
  writable,
  onSelect,
  onHover,
  onOut,
}: {
  instance: CvmInstance;
  selected: boolean;
  dim: boolean;
  drop: 'ok' | 'no' | 'can' | null;
  reason: string | null;
  local: boolean;
  writable: boolean;
  onSelect: () => void;
  onHover: (value: boolean) => void;
  onOut: (event: ReactPointerEvent) => void;
}) {
  const resources = instance.desired.spec.resources;
  const memory =
    resources.memory_bytes >= 1024 ** 3
      ? `${resources.memory_bytes / 1024 ** 3}G`
      : `${resources.memory_bytes / 1024 ** 2}M`;
  return (
    <div
      role="button"
      tabIndex={0}
      data-pill={instance.desired.id}
      className={`cz-pill ${selected ? 'on' : ''} ${dim ? 'dim' : ''} ${drop ? `drop-${drop}` : ''}`}
      title={reason ?? statusText[instance.status]}
      onClick={onSelect}
      onPointerEnter={() => onHover(true)}
      onPointerLeave={() => onHover(false)}
      onKeyDown={event => event.key === 'Enter' && onSelect()}
      aria-pressed={selected}
    >
      <span className="cz-port in" data-anchor={`in:${instance.desired.id}`} />
      <span className={`cz-dot ${instance.status} ${instance.desired.power}`} />
      <b>{instance.desired.spec.name}</b>
      <small>
        {resources.vcpus}C·{memory}
      </small>
      {reason ? (
        <em className="bad">{reason}</em>
      ) : local ? (
        <em title="未匹配出口规则的流量走所在机器本机">本机</em>
      ) : null}
      <span
        className={`cz-port out ${writable ? '' : 'idle'}`}
        data-anchor={`out:${instance.desired.id}`}
        title={writable ? '拖到出口，新增出口规则' : undefined}
        onPointerDown={onOut}
      />
    </div>
  );
}
