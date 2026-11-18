import { useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { matchGeosite } from '../../api/geosite';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import { RegionFlag } from '../../ui/region-flag';
import { useNarrow } from '../../ui/viewport';
import { flowMatch, hostPort, type FlowMachine, type FlowModel, type FlowRule } from './model';
import { flowGeositeSets, parseFlowTarget, traceFlow } from './trace';
import { useFlowCardDrag } from './use-card-drag';
import { FlowTraceBar } from './trace-bar';
import './style.css';

export function FlowModeToggle({
  visual,
  onChange,
  name,
  controls,
  disabled = false,
}: {
  visual: boolean;
  onChange: () => void;
  name: string;
  controls: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      className={`flow-mode${visual ? ' is-visual' : ''}`}
      role="switch"
      aria-checked={visual}
      aria-label={`${name}：链路可视化`}
      aria-controls={controls}
      title={`当前：${visual ? '链路可视化' : '列表编辑'}；点击切换到${visual ? '列表编辑' : '链路可视化'}`}
      onClick={onChange}
    >
      <span className="flow-mode-thumb" aria-hidden="true" />
      <svg className="flow-mode-list" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M6 4h7M6 8h7M6 12h7" />
        <circle cx="3" cy="4" r=".7" />
        <circle cx="3" cy="8" r=".7" />
        <circle cx="3" cy="12" r=".7" />
      </svg>
      <svg className="flow-mode-visual" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M2 8h4c2 0 2-4 4-4h4M6 8c2 0 2 4 4 4h4m-2-10 2 2-2 2m0 4 2 2-2 2" />
      </svg>
    </button>
  );
}

type Selection = { kind: 'machine'; id: string } | { kind: 'rule' | 'hop'; id: string };
type Wire = { rule: FlowRule; d: string; x: number; y: number };

function actionLabel(row: FlowRule, model: FlowModel) {
  if (row.target) return `→ ${model.machines.find(machine => machine.id === row.target)?.name ?? '缺少机器'}`;
  const action = row.rule.a;
  if (action.t === 'egress') return '本机出网';
  if (action.t === 'block') return '拒绝';
  if (action.t === 'proxy')
    return model.outbounds.find(outbound => outbound.id === action.outbound)?.name || action.outbound;
  return '缺少目标';
}

function FlowInspector({
  selection,
  model,
  onReveal,
  onClose,
  anchor,
}: {
  selection: Selection;
  model: FlowModel;
  onReveal: (machine: FlowMachine, rule?: FlowRule) => void;
  onClose: () => void;
  anchor: React.RefObject<HTMLElement | null>;
}) {
  const narrow = useNarrow();
  const rule =
    selection.kind !== 'machine'
      ? model.machines.flatMap(machine => machine.rules).find(row => row.id === selection.id)
      : undefined;
  const machine = model.machines.find(
    machine => machine.id === (selection.kind === 'machine' ? selection.id : rule?.owner),
  );
  if (!machine) return null;
  const hop = rule?.hop;
  const reveal = () => {
    onClose();
    onReveal(machine, rule);
  };
  const title =
    selection.kind === 'machine'
      ? machine.name
      : selection.kind === 'hop'
        ? `${machine.name} ${rule ? actionLabel(rule, model) : ''}`
        : rule
          ? flowMatch(rule.rule.m)
          : machine.name;
  return (
    <DialogLayer
      {...(narrow ? { mode: 'drawer' as const } : { mode: 'popover' as const, anchor })}
      label="链路检视"
      className="flow-inspector"
      onClose={onClose}
    >
      <section className="dialog-surface">
        <header>
          <div>
            <small>
              {selection.kind === 'machine'
                ? '机器'
                : selection.kind === 'hop'
                  ? '连接'
                  : rule?.generated
                    ? '编译器补齐'
                    : `规则 ${(rule?.index ?? 0) + 1}`}{' '}
              · {machine.name}
            </small>
            <b>{title}</b>
          </div>
          <DialogClose className="sub-dialog-close" aria-label="关闭链路检视">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <dl>
          {rule && (
            <>
              <dt>处理</dt>
              <dd>
                <strong className="flow-inspector-action" data-action={rule.rule.a.t}>
                  {actionLabel(rule, model)}
                </strong>
              </dd>
            </>
          )}
          {rule?.rule.a.t === 'egress' && rule.rule.a.send_through && (
            <>
              <dt>源地址</dt>
              <dd className="flow-inspector-address">{rule.rule.a.send_through}</dd>
            </>
          )}
          {rule?.rule.a.t === 'proxy' &&
            model.outbounds
              .filter(outbound => rule.rule.a.t === 'proxy' && outbound.id === rule.rule.a.outbound)
              .map(outbound => (
                <div className="flow-proxy-facts" key={outbound.id}>
                  <dt>代理</dt>
                  <dd>
                    {hostPort(outbound.address, outbound.port)} · {outbound.protocol.toUpperCase()}
                  </dd>
                </div>
              ))}
          {hop && (
            <>
              <dt>连接</dt>
              <dd>
                <strong className="flow-inspector-value">{hop.label}</strong>
                {hop.plaintext && <em>明文连接</em>}
              </dd>
              <dt>目标</dt>
              <dd className="flow-inspector-address">{hop.endpoint ?? '保存到草稿后查看编译结果'}</dd>
              <dt>承载</dt>
              <dd>{hop.wire ?? '待编译'}</dd>
              <dt>复用</dt>
              <dd>{hop.pool ?? '待编译'}</dd>
            </>
          )}
          {selection.kind === 'machine' && (
            <>
              <dt>监听</dt>
              <dd>
                <strong className="flow-inspector-value">{machine.listen}</strong>
              </dd>
              <dt>规则</dt>
              <dd>
                <strong className="flow-inspector-number">{machine.rules.filter(row => !row.generated).length}</strong>{' '}
                条
              </dd>
            </>
          )}
          {machine.reference && (
            <>
              <dt>规则属于</dt>
              <dd>引用 · {machine.reference}</dd>
            </>
          )}
          {(!rule || rule.rule.a.t === 'egress') && (
            <>
              <dt>机器地址</dt>
              <dd className="flow-inspector-address">{machine.address || '未上报'}</dd>
              <dt>域名策略</dt>
              <dd>
                {machine.domainStrategy ? (
                  <code className="flow-inspector-token">{machine.domainStrategy}</code>
                ) : (
                  '未提供'
                )}
              </dd>
              <dt>DNS</dt>
              <dd>
                {machine.dnsPending
                  ? 'DNS 有未保存修改，保存到草稿后查看'
                  : machine.dns.length === 0
                    ? '机器默认 DNS'
                    : machine.dns.map(policy => (
                        <div className="flow-dns" key={policy.position}>
                          <span className="flow-dns-heading">
                            <span className="flow-dns-index">D{policy.position + 1}</span>
                            <strong>{flowMatch(policy.selector)}</strong>
                          </span>
                          <code>
                            {(Array.isArray(policy.resolution.address)
                              ? policy.resolution.address
                              : [policy.resolution.address]
                            )
                              .map(address => hostPort(address, policy.resolution.port))
                              .join(' / ')}
                          </code>
                          <small>
                            <b>{policy.resolution.transport.toUpperCase()}</b>
                            <span>·</span>
                            <code>{policy.resolution.address_strategy}</code>
                          </small>
                        </div>
                      ))}
              </dd>
            </>
          )}
          {rule?.shadowedBy && (
            <>
              <dt>不会命中</dt>
              <dd className="flow-inspector-warning">已被本机规则 {rule.shadowedBy} 完全覆盖</dd>
            </>
          )}
        </dl>
        <footer>
          <button type="button" className="btn" onClick={reveal}>
            {machine.reference ? '打开所属线路规则' : '在规则表中查看'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

export function RuleFlow({
  model,
  loading = false,
  focusNode,
  onReveal,
}: {
  model: FlowModel;
  loading?: boolean;
  focusNode?: string;
  onReveal: (machine: FlowMachine, rule?: FlowRule) => void;
}) {
  const [query, setQuery] = useState('');
  const [network, setNetwork] = useState<'tcp' | 'udp'>('tcp');
  const [selection, setSelection] = useState<Selection | null>(null);
  const anchor = useRef<HTMLElement | null>(null);
  const well = useRef<HTMLDivElement>(null);
  const pendingHeight = useRef<number | null>(null);
  useLayoutEffect(() => {
    const root = well.current;
    if (!root) return;
    if (loading) {
      const rememberHeight = () => {
        pendingHeight.current = Number.parseFloat(getComputedStyle(root).height);
      };
      rememberHeight();
      if (typeof ResizeObserver === 'undefined') return;
      const observer = new ResizeObserver(rememberHeight);
      observer.observe(root);
      return () => observer.disconnect();
    }
    const from = pendingHeight.current;
    pendingHeight.current = null;
    if (
      from === null ||
      root.offsetWidth === 0 ||
      !root.animate ||
      window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    )
      return;
    // Animate CSS heights: offsetHeight would count padding/borders twice with content-box.
    const style = getComputedStyle(root);
    const to = Number.parseFloat(style.height);
    if (Math.abs(from - to) < 1) return;
    const animation = root.animate([{ height: `${from}px` }, { height: `${to}px` }], {
      duration: 260,
      easing: style.getPropertyValue('--motion-ease-out').trim(),
    });
    return () => animation.cancel();
  }, [loading]);
  const [geometry, setGeometry] = useState<{ width: number; height: number; wires: Wire[]; compact: boolean }>({
    width: 0,
    height: 0,
    wires: [],
    compact: false,
  });
  const cardDrag = useFlowCardDrag(well, model.machines, geometry.compact);
  const target = query.trim() ? parseFlowTarget(query, network) : null;
  const domain = target && !target.ip ? target.host : '';
  const sets = useMemo(() => flowGeositeSets(model), [model]);
  const [settledDomain, setSettledDomain] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => setSettledDomain(domain), 200);
    return () => clearTimeout(timer);
  }, [domain]);
  const geosite = useQuery({
    queryKey: ['routing-geosite', domain, sets],
    queryFn: ({ signal }) => matchGeosite(domain, sets, signal),
    enabled: !!domain && domain === settledDomain && sets.length > 0,
    staleTime: 30_000,
    gcTime: 60_000,
    retry: false,
  });
  const trace = target ? traceFlow(model, target, { matches: geosite.data, error: geosite.error?.message }) : null;
  const columns = useMemo(() => {
    const columns = new Map<number, FlowMachine[]>();
    const weight = (machine: FlowMachine) => {
      const incoming = model.machines.flatMap(item => item.rules).filter(row => row.target === machine.id);
      return incoming.length ? incoming.reduce((sum, row) => sum + row.index, 0) / incoming.length : -1;
    };
    for (const machine of model.machines) columns.set(machine.depth, [...(columns.get(machine.depth) ?? []), machine]);
    return [...columns].sort(([a], [b]) => a - b).map(([, machines]) => machines.sort((a, b) => weight(a) - weight(b)));
  }, [model]);
  const measure = useCallback(() => {
    const root = well.current;
    if (!root || root.offsetWidth === 0) return;
    const bounds = root.getBoundingClientRect();
    const relative = (element: Element) => {
      const rect = element.getBoundingClientRect();
      return {
        x: rect.left - bounds.left - root.clientLeft + root.scrollLeft,
        y: rect.top - bounds.top - root.clientTop + root.scrollTop,
        w: rect.width,
        h: rect.height,
      };
    };
    const outlets = new Map(
      [...root.querySelectorAll<HTMLElement>('[data-flow-outlet]')].map(element => [
        element.dataset.flowOutlet,
        element,
      ]),
    );
    const inlets = new Map(
      [...root.querySelectorAll<HTMLElement>('[data-flow-inlet]')].map(element => [element.dataset.flowInlet, element]),
    );
    const wires: Wire[] = [];
    for (const rule of model.machines.flatMap(machine => machine.rules)) {
      const outlet = outlets.get(rule.id);
      const inlet = rule.target ? inlets.get(rule.target) : null;
      if (!outlet || !inlet) continue;
      const a = relative(outlet);
      const b = relative(inlet);
      const x1 = a.x + a.w / 2;
      const y1 = a.y + a.h / 2;
      const x2 = b.x;
      const y2 = b.y + 24;
      const dx = Math.max(30, (x2 - x1) / 2);
      wires.push({
        rule,
        d: `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}`,
        x: (x1 + x2) / 2,
        y: (y1 + y2) / 2,
      });
    }
    const columns = root.querySelector<HTMLElement>('.flow-columns');
    const style = getComputedStyle(root);
    const cards = [...root.querySelectorAll<HTMLElement>('.flow-machine')].map(relative);
    setGeometry({
      width: Math.max(
        root.clientWidth,
        (columns?.scrollWidth ?? 0) + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight),
        ...cards.map(card => card.x + card.w + parseFloat(style.paddingRight)),
      ),
      height: Math.max(
        (columns?.offsetTop ?? 0) + (columns?.scrollHeight ?? 0) + parseFloat(style.paddingBottom),
        ...cards.map(card => card.y + card.h + parseFloat(style.paddingBottom)),
      ),
      wires,
      compact: columns ? getComputedStyle(columns).flexDirection === 'column' : false,
    });
  }, [model]);
  useLayoutEffect(() => {
    measure();
  }, [measure, cardDrag.offsets]);
  useLayoutEffect(() => {
    const root = well.current;
    if (!root || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(root);
    root.querySelectorAll('.flow-machine').forEach(card => observer.observe(card));
    return () => observer.disconnect();
  }, [measure]);
  const open = (selection: Selection, element: HTMLElement) => {
    anchor.current = element;
    setSelection(selection);
  };
  const chips = [
    ...new Map(geometry.wires.filter(wire => wire.rule.hop).map(wire => [wire.rule.hop!.id, wire])).values(),
  ];
  // Reserve layout using known machines/rules, never placeholder routing facts. On phones the
  // same machines form a single vertical column, so reserve their combined height instead.
  const columnHeights = loading
    ? columns.map(
        machines =>
          machines.reduce((height, machine) => height + 60 + Math.max(1, machine.rules.length) * 32, 0) +
          Math.max(0, machines.length - 1) * 18,
      )
    : [];
  const phoneHeight = loading
    ? model.machines.reduce((height, machine) => height + 60 + Math.max(1, machine.rules.length) * 40, 0) +
      Math.max(0, model.machines.length - 1) * 12 +
      94
    : 0;
  return (
    <div className="rule-flow">
      <div
        className={`flow-well${loading ? ' flow-well-loading' : ''}`}
        ref={well}
        aria-busy={loading || undefined}
        style={
          loading
            ? ({
                '--flow-loading-height': `${Math.max(220, 124 + Math.max(0, ...columnHeights))}px`,
                '--flow-loading-phone-height': `${Math.max(220, phoneHeight)}px`,
              } as CSSProperties)
            : undefined
        }
      >
        {loading ? (
          <span className="flow-loading-label" role="status">
            加载中…
          </span>
        ) : (
          <>
            <FlowTraceBar
              model={model}
              query={query}
              onQueryChange={setQuery}
              network={network}
              onNetworkChange={setNetwork}
              target={target}
              trace={trace}
            />
            <svg className="flow-wires" width={geometry.width} height={geometry.height} aria-hidden="true">
              {geometry.wires.map(wire => (
                <path
                  key={wire.rule.id}
                  d={wire.d}
                  className={`${trace?.rules.has(wire.rule.id) ? 'is-hit' : ''} ${selection?.id === wire.rule.id ? 'is-selected' : ''} ${wire.rule.shadowedBy ? 'is-shadowed' : ''}`}
                  onClick={() => {
                    const button = [...(well.current?.querySelectorAll<HTMLElement>('[data-flow-row]') ?? [])].find(
                      element => element.dataset.flowRow === wire.rule.id,
                    );
                    if (button) {
                      button.focus();
                      open({ kind: 'hop', id: wire.rule.id }, button);
                    }
                  }}
                />
              ))}
            </svg>
            <div className="flow-columns">
              {columns.map((machines, index) => (
                <div className="flow-column" key={index}>
                  {machines.map(machine => (
                    <article
                      className={`flow-machine${machine.id === model.entry ? ' is-entry' : ''}${machine.node === focusNode ? ' is-self' : ''}${trace?.machines.has(machine.id) ? ' is-lit' : ''}${cardDrag.dragging === machine.id ? ' is-dragging' : ''}`}
                      data-flow-machine={machine.id}
                      key={machine.id}
                      style={
                        {
                          '--flow-node-x': `${cardDrag.offsets[machine.id]?.x ?? 0}px`,
                          '--flow-node-y': `${cardDrag.offsets[machine.id]?.y ?? 0}px`,
                        } as CSSProperties
                      }
                    >
                      <button
                        type="button"
                        className="flow-machine-head"
                        data-flow-inlet={machine.id}
                        title={geometry.compact ? '检视机器' : '拖动移动；方向键微调，Home 恢复位置；点击检视'}
                        aria-description={
                          geometry.compact ? undefined : '可拖动调整位置，也可用方向键微调，Home 恢复位置'
                        }
                        onPointerDown={event => cardDrag.start(machine.id, event)}
                        onPointerMove={cardDrag.move}
                        onPointerUp={cardDrag.end}
                        onPointerCancel={cardDrag.cancel}
                        onLostPointerCapture={cardDrag.cancel}
                        onKeyDown={event => cardDrag.keyDown(machine.id, event)}
                        onClick={event => {
                          if (!cardDrag.consumeClick(event))
                            open({ kind: 'machine', id: machine.id }, event.currentTarget);
                        }}
                      >
                        <span>
                          {machine.country && <RegionFlag code={machine.country} />}
                          <b>{machine.name}</b>
                          {machine.node === focusNode && <i className="flow-self">本机</i>}
                          <small className="flow-machine-facts">
                            <span className="flow-listen" title={machine.listen}>
                              {machine.listenPorts.length
                                ? `${machine.id === model.entry ? '' : '中转 '}${machine.listenPorts.map(port => `:${port}`).join(' / ')}`
                                : machine.listen}
                            </span>
                            {machine.id !== model.entry && <span aria-hidden="true">/</span>}
                            <span className="flow-machine-role">
                              {machine.id === model.entry
                                ? '入口'
                                : machine.reference
                                  ? '引用'
                                  : machine.rules.some(row => row.target)
                                    ? '中转'
                                    : '出口'}
                            </span>
                          </small>
                        </span>
                        {machine.reference && <span className="flow-reference">引用 · {machine.reference}</span>}
                        {!machine.reachable && <span className="flow-reference">未从入口连接</span>}
                      </button>
                      <ol>
                        {machine.rules.map(row => (
                          <li
                            key={row.id}
                            className={`${trace?.rules.has(row.id) ? 'is-hit' : ''}${trace?.uncertain === row.id ? ' is-uncertain' : ''}${row.shadowedBy ? ' is-shadowed' : ''}`}
                          >
                            <button
                              type="button"
                              data-flow-row={row.id}
                              onClick={event => open({ kind: 'rule', id: row.id }, event.currentTarget)}
                              title={`${flowMatch(row.rule.m)} → ${actionLabel(row, model)}${row.shadowedBy ? `；被规则 ${row.shadowedBy} 覆盖` : ''}`}
                            >
                              <span className="flow-index">{row.generated ? '+' : row.index + 1}</span>
                              <span className="flow-match">{flowMatch(row.rule.m)}</span>
                              <span className={`flow-action is-${row.rule.a.t}`}>
                                {row.generated && <i title="编译器补齐">补齐</i>}
                                {row.shadowedBy && <i>不会命中</i>}
                                {row.target ? (
                                  <>
                                    <span className="flow-goto">
                                      {actionLabel(row, model)}
                                      <small>
                                        {row.hop?.label}
                                        {row.hop?.plaintext && ' · 明文'}
                                      </small>
                                    </span>
                                    <span className="flow-outlet" data-flow-outlet={row.id} />
                                  </>
                                ) : (
                                  actionLabel(row, model)
                                )}
                              </span>
                            </button>
                          </li>
                        ))}
                        {machine.rules.length === 0 && (
                          <li className="flow-empty">暂无规则，保存到草稿后查看编译补齐</li>
                        )}
                      </ol>
                    </article>
                  ))}
                </div>
              ))}
            </div>
            {chips.map(wire => (
              <button
                type="button"
                key={wire.rule.hop!.id}
                className={`flow-hop${wire.rule.hop!.plaintext ? ' is-plain' : ''}${trace?.hops.has(wire.rule.hop!.id) ? ' is-hit' : ''}`}
                style={{ left: wire.x, top: wire.y }}
                title={`${wire.rule.hop!.label} · ${wire.rule.hop!.wire ?? '待编译'}`}
                onClick={event => open({ kind: 'hop', id: wire.rule.id }, event.currentTarget)}
              >
                {wire.rule.hop!.label}
                {wire.rule.hop!.plaintext && <span>明文</span>}
              </button>
            ))}
          </>
        )}
      </div>
      {selection && (
        <FlowInspector
          selection={selection}
          model={model}
          anchor={anchor}
          onReveal={onReveal}
          onClose={() => setSelection(null)}
        />
      )}
    </div>
  );
}
