// 组合台：入口 | 计算 | 出口，流量从左到右。
// - 访问入口：各机器发布的地址。共享地址按协议端口（L4）分给多个容器，独立地址整 IP（L3）交给一个容器；
//   入口投递到容器主网卡，不占网卡槽位，可以改派给同一用户的另一个容器。
// - 计算：提供算力的宿主，容器是其中的条目。首版不迁移计算，容器不能拖到别的宿主。
// - 出口：出口网络（网卡发布）。容器按目标网段（L3）或协议端口（L4）选择出网的机器，未匹配的流量走所在机器本机。
// 拖线头或在检查器点「＋ 入口 / ＋ 出口」再点目标，改动先画成虚线，在检查器确认后提交。
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import type { CvmEntry, CvmEntryBoard, CvmInstance, CvmNicCatalog, CvmWorkspace } from '../../../api';
import { stackHosts, statusText } from '../queries';
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

type Point = { x: number; y: number };
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
  selected,
  onSelect,
}: {
  workspace: CvmWorkspace;
  entries: CvmEntryBoard;
  catalog: CvmNicCatalog | undefined;
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  const instances = workspace.instances.filter(instance => instance.reserved && instance.desired.power !== 'deleted');
  const machines = entryMachines(entries, workspace.hosts);
  const exits = exitTiles(catalog, workspace.hosts);
  const rules = ruleLinks(instances);
  const writable = workspace.can_write;
  const board = useRef<HTMLDivElement>(null);
  const [geo, setGeo] = useState<{ anchors: Record<string, Point>; width: number; height: number }>({
    anchors: {},
    width: 0,
    height: 0,
  });
  const [hover, setHover] = useState<string | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [target, setTarget] = useState<Verdict | null>(null);
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [pick, setPick] = useState<Pick | null>(null);

  const measure = useCallback(() => {
    const root = board.current;
    if (!root) return;
    const box = root.getBoundingClientRect();
    const anchors: Record<string, Point> = {};
    root.querySelectorAll<HTMLElement>('[data-anchor]').forEach(element => {
      const rect = element.getBoundingClientRect();
      anchors[element.dataset.anchor!] = {
        x: rect.left + rect.width / 2 - box.left,
        y: rect.top + rect.height / 2 - box.top,
      };
    });
    setGeo(old => {
      const next = { anchors, width: box.width, height: box.height };
      return JSON.stringify(old) === JSON.stringify(next) ? old : next;
    });
  }, []);
  useLayoutEffect(measure);
  useEffect(() => {
    const root = board.current;
    if (!root || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(root);
    return () => observer.disconnect();
  }, [measure]);

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
  const curve = (a: Point, b: Point) => {
    const dx = Math.max(36, Math.abs(b.x - a.x) * 0.5);
    return `M${a.x} ${a.y} C${a.x + dx} ${a.y}, ${b.x - dx} ${b.y}, ${b.x} ${b.y}`;
  };
  const along = (a: Point, b: Point, t: number): Point => {
    const dx = Math.max(36, Math.abs(b.x - a.x) * 0.5);
    const p1 = { x: a.x + dx, y: a.y };
    const p2 = { x: b.x - dx, y: b.y };
    const u = 1 - t;
    return {
      x: u * u * u * a.x + 3 * u * u * t * p1.x + 3 * u * t * t * p2.x + t * t * t * b.x,
      y: u * u * u * a.y + 3 * u * u * t * p1.y + 3 * u * t * t * p2.y + t * t * t * b.y,
    };
  };

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
                {hostIds.has(machine.node) && <span className="cz-kind">也提供计算</span>}
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
            return (
              <div key={id} className="cz-host">
                <div className="cz-host-head">
                  <b>{name}</b>
                  {host && !host.fresh && <span className="cz-kind warn">离线</span>}
                  {caps && (
                    <span className="cz-host-free" title="已分配 / 可分配">
                      {used}/{caps.allocatable_vcpus} 核
                    </span>
                  )}
                </div>
                {caps && caps.allocatable_vcpus > 0 && (
                  <div className="cz-meter" aria-hidden="true">
                    <i style={{ width: `${Math.min(100, (used / caps.allocatable_vcpus) * 100)}%` }} />
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
                  <b>{exit.provider}</b>
                  <span className="cz-kind">{exit.publication.spec.name}</span>
                </div>
                <div className="cz-net-meta">
                  <span className="mono">
                    {exit.kind === 'private'
                      ? exit.publication.spec.destinations.join(' ')
                      : (exit.address ?? (exit.kind === 'dedicated' ? '独立地址' : '共享出口'))}
                  </span>
                  <span>{links.length ? `${links.length} 条` : ''}</span>
                </div>
                {verdict && !verdict.ok && <div className="cz-reason">{verdict.reason}</div>}
              </div>
            );
          })}
          {!exits.length && <p className="note cz-col-empty">还没有可用的出口网络</p>}
        </section>

        {/* ── 连线 ── */}
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
            const on = litRule(rule);
            const exit = exits.find(item => item.id === rule.exit);
            const pair = pairs.get(`${rule.instance}>${rule.exit}`) ?? [rule];
            const showLabel = on && !drag && pair[0] === rule && (focus === rule.instance || focusExit === rule.exit);
            const label = along(from, to, 0.5);
            const handle = along(from, to, 0.94);
            const lifted = proposedRule === rule.key || (drag?.kind === 'rule' && drag.rule === rule.key);
            const status = instanceOf(rule.instance)?.status ?? '';
            return (
              <g
                key={rule.key}
                className={`cz-wire out ${rule.layer} ${exit?.kind === 'private' ? 'private' : ''} ${on ? 'on' : 'off'} ${
                  lifted ? 'lifted' : ''
                } ${status}`}
              >
                <path d={curve(from, to)} />
                {showLabel && (
                  <text x={label.x} y={label.y - 7} textAnchor="middle">
                    {routeLabel(rule.route)}
                    {pair.length > 1 ? ` +${pair.length - 1}` : ''}
                  </text>
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
              const ends: [string, string] | null =
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
        <em title="未匹配出口规则的流量走所在机器本机">本机出口</em>
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
