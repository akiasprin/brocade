import { Fragment, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import type {
  DestMatch,
  EgressDnsResolution,
  ExternalOutbound,
  NodeAgentStateItem,
  Rule,
  SnapshotApp,
  SnapshotStep,
} from '../api';
import { HOP_WIRE_OPTIONS, hopWireLabel, type HopWireKind } from '../ui/format';
import { Icon, PanelTitle } from '../ui/icons';
import { vpngateNodeEligibility } from '../vpngate-capability';
import { isVpngateOutbound, selectVpngatePool } from '../vpngate-selection';
import { navigate } from '../forge/route';
import { VpngateRuleMenu } from './vpngate-rule-menu';
import {
  buildMatch,
  defaultHopDial,
  dialKindOf,
  dialUnavailable,
  DIAL_LABEL,
  DIAL_ORDER,
  MachineEgressDnsControls,
  egressDnsSelectorKey,
  externalProtocolBadge,
  formatHostPort,
  forwardAction,
  forwardPeers,
  hostOf,
  hopDialOf,
  listenerDialKindOf,
  listenerDialOf,
  LISTENER_DIAL_ORDER,
  isPinnedTerminalRule,
  MATCH_KINDS,
  matchValues,
  pinTerminalRules,
  placeTargetMenu,
  reusableListeners,
  reuseListenerAction,
  supportsEgressDns,
  under,
  type DialKind,
  type ReusableListener,
} from './rules';
import { freePortAcross, type PortOwners } from './ports';
import {
  reachableWizardTables,
  wizardEgressRule,
  wizardMembers,
  wizardSpine,
  type WizardRuleTables,
} from './chain-wizard-graph';

export interface WizardDnsChange {
  node: string;
  selector: DestMatch;
  resolution: EgressDnsResolution | null;
}

interface WizardPathEditorProps {
  root: string | null;
  tables: WizardRuleTables;
  onRootChange: (root: string | null) => void;
  onTablesChange: (tables: WizardRuleTables) => void;
  fixedHead: boolean;
  nodes: NodeAgentStateItem[];
  app: SnapshotApp | null;
  apps: SnapshotApp[];
  chainId: string;
  tenant: string;
  outbounds: ExternalOutbound[];
  onOutboundCreated: (outbound: ExternalOutbound) => void;
  taken: Map<string, PortOwners>;
  hopBase: number;
  portOf: (host: string) => string;
  wireOf: (host: string) => HopWireKind;
  onPortChange: (host: string, port: string) => void;
  onWireChange: (host: string, wire: HopWireKind) => void;
  portIssueOf: (host: string) => string | null;
  egressAllowed: (node: string) => boolean;
  globalRealityReady: boolean;
  realitySite: { dest: string; names: string[] };
  entryProtocolCount: number;
  dnsPolicies: { node: string; selector: DestMatch; resolution: EgressDnsResolution }[];
  dnsChanges: WizardDnsChange[];
  onDnsChange: (node: string, selector: DestMatch, resolution: EgressDnsResolution | null) => void;
}

/** The creation page edits the same Rule/Step graph as the chain detail page. */
export function WizardPathEditor(props: WizardPathEditorProps) {
  const {
    root,
    tables,
    onRootChange,
    onTablesChange,
    fixedHead,
    nodes,
    app,
    apps,
    chainId,
    tenant,
    outbounds,
    onOutboundCreated,
    taken,
    hopBase,
    portOf,
    wireOf,
    onPortChange,
    onWireChange,
    portIssueOf,
    egressAllowed,
    globalRealityReady,
    realitySite,
    entryProtocolCount,
    dnsPolicies,
    dnsChanges,
    onDnsChange,
  } = props;
  const [open, setOpen] = useState<string | null>(root);
  const [targetPicker, setTargetPicker] = useState<{ source: string; index: number } | null>(null);
  const [targetPickerView, setTargetPickerView] = useState<
    { t: 'targets' } | { t: 'vpngate' } | { t: 'listener-chains' } | { t: 'listener-endpoints'; chain: string }
  >({ t: 'targets' });
  const [targetQuery, setTargetQuery] = useState('');
  const [targetMenuPlacement, setTargetMenuPlacement] = useState<ReturnType<typeof placeTargetMenu> | null>(null);
  const targetPickerRoot = useRef<HTMLSpanElement>(null);
  const targetMenu = useRef<HTMLFieldSetElement>(null);
  useEffect(() => {
    if (!targetPicker) return;
    const closeOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && targetPickerRoot.current?.contains(event.target)) return;
      if (event.target instanceof Node && targetMenu.current?.contains(event.target)) return;
      setTargetPicker(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setTargetPicker(null);
    };
    document.addEventListener('pointerdown', closeOutside, true);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOutside, true);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [targetPicker]);
  useLayoutEffect(() => {
    if (!targetPicker) return;
    const place = () => {
      const anchor = targetPickerRoot.current?.getBoundingClientRect();
      if (!anchor) return;
      const wantedHeight = targetMenu.current ? Math.min(520, targetMenu.current.scrollHeight + 2) : 520;
      setTargetMenuPlacement(current => {
        const next = placeTargetMenu(anchor, current?.below, wantedHeight);
        return current && JSON.stringify(current) === JSON.stringify(next) ? current : next;
      });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    window.visualViewport?.addEventListener('resize', place);
    window.visualViewport?.addEventListener('scroll', place);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
      window.visualViewport?.removeEventListener('resize', place);
      window.visualViewport?.removeEventListener('scroll', place);
    };
  }, [targetPicker, targetPickerView, targetQuery]);
  const nodeOf = (id: string) => nodes.find(node => node.node_id === id) ?? null;
  const nameOf = (id: string) => nodeOf(id)?.name || id;
  const dnsFor = (id: string, selector: DestMatch): EgressDnsResolution | null => {
    const key = egressDnsSelectorKey(selector);
    const change = dnsChanges.find(
      candidate => candidate.node === id && egressDnsSelectorKey(candidate.selector) === key,
    );
    if (change) return change.resolution;
    return (
      dnsPolicies.find(candidate => candidate.node === id && egressDnsSelectorKey(candidate.selector) === key)
        ?.resolution ?? null
    );
  };
  const fallbackFor = (id: string): Rule =>
    egressAllowed(id) ? wizardEgressRule() : { m: { t: 'any' }, a: { t: 'block' } };
  const members = wizardMembers(root, tables);
  const spine = wizardSpine(root, tables);
  const memberSet = new Set(members);
  const steps: SnapshotStep[] = members.map(id => ({
    chain: chainId,
    node: id,
    accept: null,
    hop_in: null,
    rules: tables[id] ?? [],
  }));
  const peersFor = (source: string) =>
    forwardPeers({
      nodeId: source,
      sourceChain: chainId,
      spine,
      tenant,
      steps,
      app,
      drafts: tables,
      nodes,
    });
  const sourceApp: SnapshotApp = {
    ...(app ?? { id: chainId, label: '', ingresses: [], fronts: [], grants: [] }),
    chains: [...(app?.chains ?? []), { id: chainId, tenant, name: '' }],
    steps: [...(app?.steps ?? []), ...steps],
  };
  const listenerApps = [...apps.filter(candidate => candidate.id !== sourceApp.id), sourceApp];
  const listenersFor = (source: string): ReusableListener[] =>
    reusableListeners({
      apps: listenerApps,
      sourceApp: sourceApp.id,
      sourceChain: chainId,
      sourceNode: source,
      sourceRules: tables[source] ?? [],
      sourceDrafts: tables,
      nodes,
    }).filter(candidate => candidate.ref.chain !== chainId);
  const availableOutbounds = outbounds.filter(
    outbound => tenant === outbound.tenant || (outbound.protocol.t !== 'warp' && under(tenant, outbound.tenant)),
  );
  const outboundUnavailable = (source: string, outbound: ExternalOutbound): string | null => {
    if (outbound.protocol.t !== 'vpngate') return null;
    const state = vpngateNodeEligibility(nodeOf(source) ?? undefined);
    return state.eligible ? null : state.reason;
  };
  const commit = (next: WizardRuleTables) => onTablesChange(reachableWizardTables(root, next));
  const patch = (source: string, index: number, rule: Rule) => {
    const next = {
      ...tables,
      [source]: pinTerminalRules((tables[source] ?? []).map((row, i) => (i === index ? rule : row))),
    };
    if (rule.a.t === 'forward' && rule.a.to && !next[rule.a.to]) next[rule.a.to] = [fallbackFor(rule.a.to)];
    commit(next);
  };
  const defaultForward = (source: string, target: string) => {
    const port = Number(portOf(target)) || freePortAcross(taken, [target], hopBase);
    return forwardAction(target, defaultHopDial({ peer: nodeOf(target), self: nodeOf(source), port }));
  };
  const setTarget = (source: string, index: number, value: string) => {
    setTargetPicker(null);
    setTargetPickerView({ t: 'targets' });
    const rule = tables[source]?.[index];
    if (!rule) return;
    if (value.startsWith('outbound:')) {
      patch(source, index, { ...rule, a: { t: 'proxy', outbound: value.slice(9) } });
      return;
    }
    const target = value.startsWith('node:') ? value.slice(5) : '';
    if (!target) return;
    const same = tables[source]?.find(
      (candidate, i) => i !== index && candidate.a.t === 'forward' && candidate.a.to === target,
    );
    patch(source, index, {
      ...rule,
      a: same?.a.t === 'forward' ? forwardAction(target, same.a.dial, same.a.pool) : defaultForward(source, target),
    });
  };
  const setListenerTarget = (source: string, index: number, listener: ReusableListener) => {
    const rule = tables[source]?.[index];
    if (!rule || listener.blocked) return;
    const same = tables[source]?.find(
      (candidate, i) =>
        i !== index &&
        candidate.a.t === 'reuse_listener' &&
        candidate.a.listener.chain === listener.ref.chain &&
        candidate.a.listener.node === listener.ref.node,
    );
    const peer = nodeOf(listener.ref.node);
    const dial =
      same?.a.t === 'reuse_listener'
        ? same.a.dial
        : listenerDialOf(
            listener.local ? 'overlay' : peer?.public_ipv4 && !peer.public_ipv4_nat ? 'public_ipv4' : 'overlay',
          );
    patch(source, index, {
      ...rule,
      a: reuseListenerAction(listener.ref, dial, same?.a.t === 'reuse_listener' ? same.a.pool : undefined),
    });
    setTargetPicker(null);
    setTargetPickerView({ t: 'targets' });
  };
  const setAction = (source: string, index: number, action: 'forward' | 'egress' | 'block') => {
    const rule = tables[source]?.[index];
    if (!rule) return;
    if (action !== 'forward') {
      patch(source, index, {
        ...rule,
        a: action === 'egress' ? { t: 'egress', send_through: null } : { t: 'block' },
      });
      return;
    }
    const peer = peersFor(source).find(candidate => !candidate.blocked);
    const outbound = availableOutbounds.find(candidate => candidate.protocol.t !== 'vpngate');
    if (peer) {
      const same = tables[source]?.find(candidate => candidate.a.t === 'forward' && candidate.a.to === peer.id);
      patch(source, index, {
        ...rule,
        a: same?.a.t === 'forward' ? forwardAction(peer.id, same.a.dial, same.a.pool) : defaultForward(source, peer.id),
      });
    } else if (outbound)
      patch(source, index, {
        ...rule,
        a: { t: 'proxy', outbound: outbound.id },
      });
    else patch(source, index, { ...rule, a: forwardAction('', { t: 'overlay' }) });
  };
  const setDial = (source: string, target: string, kind: DialKind) => {
    const dial = hopDialOf(kind, nodeOf(target), Number(portOf(target)) || hopBase);
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'forward' && rule.a.to === target
          ? { ...rule, a: forwardAction(target, dial, rule.a.pool) }
          : rule,
      ),
    });
  };
  const setListenerDial = (
    source: string,
    listener: { chain: string; node: string },
    kind: 'public_ipv4' | 'public_ipv6' | 'overlay' | 'custom',
  ) =>
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'reuse_listener' &&
        rule.a.listener.chain === listener.chain &&
        rule.a.listener.node === listener.node
          ? { ...rule, a: reuseListenerAction(listener, listenerDialOf(kind), rule.a.pool) }
          : rule,
      ),
    });
  const setListenerCustomHost = (source: string, listener: { chain: string; node: string }, host: string) =>
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'reuse_listener' &&
        rule.a.listener.chain === listener.chain &&
        rule.a.listener.node === listener.node
          ? { ...rule, a: reuseListenerAction(listener, { t: 'addr', v: host }, rule.a.pool) }
          : rule,
      ),
    });
  const setCustomHost = (source: string, target: string, host: string) => {
    const dial = { t: 'addr' as const, v: formatHostPort(host, Number(portOf(target)) || hopBase) };
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'forward' && rule.a.to === target
          ? { ...rule, a: forwardAction(target, dial, rule.a.pool) }
          : rule,
      ),
    });
  };
  const setPool = (source: string, target: string, useMux: boolean) =>
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'forward' && rule.a.to === target
          ? { ...rule, a: forwardAction(target, rule.a.dial, useMux ? { t: 'mux' } : { t: 'none' }) }
          : rule,
      ),
    });
  const addRule = (source: string) => {
    const rows = [...(tables[source] ?? [fallbackFor(source)])];
    const terminal = rows.findIndex(isPinnedTerminalRule);
    rows.splice(terminal < 0 ? rows.length : terminal, 0, {
      m: { t: 'domain_suffix', v: [] },
      a: egressAllowed(source) ? { t: 'egress', send_through: null } : { t: 'block' },
    });
    commit({ ...tables, [source]: pinTerminalRules(rows) });
  };
  const move = (source: string, index: number, delta: number) => {
    const rows = [...(tables[source] ?? [])];
    const next = index + delta;
    if (next < 0 || next >= rows.length || isPinnedTerminalRule(rows[index]) || isPinnedTerminalRule(rows[next]))
      return;
    [rows[index], rows[next]] = [rows[next], rows[index]];
    commit({ ...tables, [source]: rows });
  };
  const remove = (source: string, index: number) =>
    commit({ ...tables, [source]: (tables[source] ?? []).filter((_, i) => i !== index) });
  const removeNode = (target: string) => {
    const next: WizardRuleTables = {};
    for (const [source, rows] of Object.entries(tables)) {
      next[source] = rows.flatMap(rule => {
        if (rule.a.t !== 'forward' || rule.a.to !== target) return [rule];
        return rule.m.t === 'any' ? [{ ...rule, a: fallbackFor(source).a }] : [];
      });
    }
    commit(next);
  };
  const appendHop = (target: string) => {
    const source = spine.at(-1);
    if (!source) return;
    const rows = tables[source] ?? [fallbackFor(source)];
    const next = {
      ...tables,
      [source]: rows.map(rule => (rule.m.t === 'any' ? { ...rule, a: defaultForward(source, target) } : rule)),
      [target]: tables[target] ?? [fallbackFor(target)],
    };
    commit(next);
    setOpen(source);
  };

  if (!root) {
    const available = nodes.filter(candidate => !candidate.retired_at);
    return (
      <div className="wzr-empty">
        <label htmlFor="chain-wizard-entry">入口节点</label>
        <select
          className="f"
          id="chain-wizard-entry"
          aria-label="选择入口节点"
          value=""
          onChange={event => {
            const id = event.target.value;
            if (!id) return;
            onRootChange(id);
            onTablesChange({ [id]: [fallbackFor(id)] });
            setOpen(id);
          }}
        >
          <option value="">{available.length ? '— 选择入口节点 —' : '没有可用机器'}</option>
          {available.map(candidate => (
            <option key={candidate.node_id} value={candidate.node_id}>
              {candidate.name || candidate.node_id}（{candidate.node_id}）
            </option>
          ))}
        </select>
        <span className="note">
          {available.length ? '接入面开在这台机器上，用户从这里接入。' : '先去「机器」纳管一台，再回来选择入口。'}
        </span>
      </div>
    );
  }

  const rendered = new Set<string>();
  const renderNode = (id: string, depth: number, incoming: string, ancestors: Set<string>): React.ReactNode => {
    const repeated = rendered.has(id) || ancestors.has(id);
    if (!repeated) rendered.add(id);
    const rows = tables[id] ?? [fallbackFor(id)];
    const expanded = open === id && !repeated;
    const index = spine.indexOf(id);
    const role = index === 0 ? '入口' : index > 0 && index === spine.length - 1 ? '出口' : index > 0 ? '中转' : '支路';
    const entryEdges = members.flatMap(source =>
      (tables[source] ?? []).flatMap(rule =>
        rule.a.t === 'forward' && rule.a.to === id ? [{ source, dial: rule.a.dial }] : [],
      ),
    );
    const listened = entryEdges.length > 0 || rows.some(rule => rule.a.t === 'forward' && rule.a.dial.t === 'reverse');
    const childEdges = rows
      .filter(rule => rule.a.t === 'forward' && rule.a.to)
      .sort((left, right) => Number(right.m.t === 'any') - Number(left.m.t === 'any'));
    const nextAncestors = new Set(ancestors);
    nextAncestors.add(id);
    const peers = peersFor(id);
    return (
      <div
        className={`chain-rule-node${expanded ? ' open' : ''}`}
        style={{ '--depth': depth } as CSSProperties}
        key={`${id}/${incoming}`}
      >
        <div
          role="button"
          tabIndex={0}
          className="chain-rule-node-head"
          aria-expanded={expanded}
          onClick={() => setOpen(open === id ? null : id)}
          onKeyDown={event => {
            if (event.target !== event.currentTarget) return;
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              setOpen(open === id ? null : id);
            }
          }}
        >
          <span className="disc" aria-hidden="true">
            {expanded ? '▾' : '▸'}
          </span>
          <span className="who">
            <b title={id}>{nameOf(id)}</b>
            <span className={`st${role === '入口' ? ' b-role' : role === '出口' ? ' st-succeeded' : ''}`}>{role}</span>
            {incoming && <span className="st">{incoming}</span>}
            {repeated && <span className="st">共享配置</span>}
          </span>
          <span className="hopctl" onClick={event => event.stopPropagation()}>
            {id === root ? (
              !fixedHead && (
                <button
                  type="button"
                  className="del-ctl"
                  aria-label="换一台当入口"
                  onClick={() => {
                    onRootChange(null);
                    onTablesChange({});
                    setOpen(null);
                  }}
                >
                  ×
                </button>
              )
            ) : (
              <button
                type="button"
                className="del-ctl"
                aria-label={`把 ${nameOf(id)} 移出本链`}
                onClick={() => removeNode(id)}
              >
                ×
              </button>
            )}
          </span>
          <div className="meta">
            <span className="m-rules">{rows.length} 条规则</span>
            <span className="m-hop mono">
              {listened
                ? `${portOf(id)} / ${hopWireLabel(wireOf(id))}`
                : id === root
                  ? `${entryProtocolCount} 种接入协议`
                  : '—'}
            </span>
          </div>
        </div>
        {!repeated && (
          <>
            <div className="chain-rule-body" hidden={!expanded}>
              <fieldset className="panel rule-editor rule-ro wzr-editor">
                <div className="toolbar wzr-rule-heading">
                  <b className="mono">
                    {chainId} / {id}
                  </b>
                  <span className="note">规则自上而下匹配，末位「任意」兜底。</span>
                  <span className="sp" />
                </div>
                <table className="tbl rule-table wzr-table">
                  <tbody>
                    {rows.map((rule, ruleIndex) => {
                      const pinned = isPinnedTerminalRule(rule);
                      const matchKind = MATCH_KINDS.find(candidate => candidate.t === rule.m.t);
                      const target =
                        rule.a.t === 'forward' ? rule.a.to : rule.a.t === 'reuse_listener' ? rule.a.listener.node : '';
                      const dialKind = rule.a.t === 'forward' ? dialKindOf(rule.a.dial, nodeOf(target)) : null;
                      const externalId = rule.a.t === 'proxy' ? rule.a.outbound : null;
                      const external = externalId
                        ? availableOutbounds.find(candidate => candidate.id === externalId)
                        : null;
                      const pickerOpen = targetPicker?.source === id && targetPicker.index === ruleIndex;
                      const targetMatches = (...parts: Array<string | null | undefined>) =>
                        !targetQuery.trim() || parts.join(' ').toLowerCase().includes(targetQuery.trim().toLowerCase());
                      const visibleNextPeers = peers.filter(
                        peer => !peer.blocked && peer.where === 'next' && targetMatches(peer.id, peer.name),
                      );
                      const visibleInsidePeers = peers.filter(
                        peer => !peer.blocked && peer.where === 'inside' && targetMatches(peer.id, peer.name),
                      );
                      const visibleForkPeers = peers.filter(
                        peer => !peer.blocked && peer.where === 'outside' && targetMatches(peer.id, peer.name),
                      );
                      const visibleBlockedPeers = peers.filter(
                        peer => peer.blocked && targetMatches(peer.id, peer.name, peer.blocked),
                      );
                      const visibleOutbounds = availableOutbounds.filter(
                        outbound =>
                          outbound.protocol.t !== 'vpngate' &&
                          targetMatches(outbound.id, outbound.name, outbound.address, outbound.protocol.t),
                      );
                      const listeners = listenersFor(id);
                      const visibleListenerApps = listenerApps.flatMap(candidateApp => {
                        const chains = candidateApp.chains.flatMap(candidateChain => {
                          if (candidateChain.id === chainId) return [];
                          const candidates = listeners.filter(listener => listener.ref.chain === candidateChain.id);
                          const matches =
                            targetMatches(
                              candidateApp.id,
                              candidateApp.label,
                              candidateChain.id,
                              candidateChain.name,
                            ) ||
                            candidates.some(candidate =>
                              targetMatches(
                                candidate.nodeName,
                                candidate.ref.node,
                                String(candidate.step.hop_in?.port ?? ''),
                                candidate.blocked,
                              ),
                            );
                          return candidates.length && matches ? [{ chain: candidateChain, candidates }] : [];
                        });
                        return chains.length ? [{ app: candidateApp, chains }] : [];
                      });
                      const listenerRef = rule.a.t === 'reuse_listener' ? rule.a.listener : null;
                      const reference = listenerRef
                        ? listeners.find(
                            candidate =>
                              candidate.ref.chain === listenerRef.chain && candidate.ref.node === listenerRef.node,
                          )
                        : null;
                      return (
                        <tr key={`${id}/${ruleIndex}`}>
                          <td className="mono dim">{rule.m.t === 'any' ? '*' : ruleIndex + 1}</td>
                          <td className="rule-match-cell">
                            <select
                              className="f"
                              aria-label={`${nameOf(id)} 第 ${ruleIndex + 1} 条匹配条件`}
                              value={rule.m.t}
                              disabled={rule.m.t === 'any'}
                              onChange={event =>
                                patch(id, ruleIndex, {
                                  ...rule,
                                  m: buildMatch(event.target.value as Rule['m']['t'], ''),
                                })
                              }
                            >
                              {MATCH_KINDS.map(kind => (
                                <option
                                  key={kind.t}
                                  value={kind.t}
                                  disabled={
                                    kind.t === 'any' ||
                                    (kind.t === 'sniffing_failed' &&
                                      rows.some(row => row.m.t === kind.t && row !== rule))
                                  }
                                >
                                  {kind.label}
                                </option>
                              ))}
                            </select>
                            {matchKind?.list && rule.m.t !== 'any' && (
                              <input
                                className="f"
                                aria-label={`${nameOf(id)} 第 ${ruleIndex + 1} 条匹配取值`}
                                placeholder={matchKind.hint}
                                value={matchValues(rule.m)}
                                onChange={event =>
                                  patch(id, ruleIndex, { ...rule, m: buildMatch(rule.m.t, event.target.value) })
                                }
                              />
                            )}
                            {rule.m.t === 'network' && (
                              <select
                                className="f"
                                aria-label="传输层"
                                value={rule.m.v}
                                onChange={event =>
                                  patch(id, ruleIndex, {
                                    ...rule,
                                    m: { t: 'network', v: event.target.value as 'tcp' | 'udp' },
                                  })
                                }
                              >
                                <option value="tcp">TCP</option>
                                <option value="udp">UDP</option>
                              </select>
                            )}
                          </td>
                          <td className="rule-action-cell">
                            <select
                              className={`f rule-action-select rule-action-${rule.a.t === 'block' ? 'block' : rule.a.t === 'egress' ? 'egress' : 'forward'}`}
                              aria-label={`${nameOf(id)} 第 ${ruleIndex + 1} 条动作`}
                              value={rule.a.t === 'proxy' || rule.a.t === 'reuse_listener' ? 'forward' : rule.a.t}
                              onChange={event =>
                                setAction(id, ruleIndex, event.target.value as 'forward' | 'egress' | 'block')
                              }
                            >
                              <option value="forward">转发给</option>
                              <option value="egress" disabled={!egressAllowed(id)}>
                                从本机出网
                              </option>
                              <option value="block">拒绝</option>
                            </select>
                            {(rule.a.t === 'forward' || rule.a.t === 'proxy' || rule.a.t === 'reuse_listener') && (
                              <>
                                <span
                                  className="external-target-picker"
                                  ref={pickerOpen ? targetPickerRoot : undefined}
                                >
                                  <button
                                    type="button"
                                    className={`external-target-trigger${rule.a.t === 'forward' ? ' node-target' : rule.a.t === 'reuse_listener' ? ' listener-target' : ''}`}
                                    aria-label={`${nameOf(id)} 第 ${ruleIndex + 1} 条转发目标`}
                                    aria-expanded={pickerOpen}
                                    onClick={event => {
                                      if (pickerOpen) {
                                        setTargetPicker(null);
                                        return;
                                      }
                                      setTargetMenuPlacement(
                                        placeTargetMenu(event.currentTarget.getBoundingClientRect()),
                                      );
                                      setTargetPickerView({ t: 'targets' });
                                      setTargetQuery('');
                                      setTargetPicker({ source: id, index: ruleIndex });
                                    }}
                                  >
                                    <span
                                      className={`external-target-kind${external ? ' external' : rule.a.t === 'reuse_listener' ? ' listener' : ''}`}
                                    >
                                      {rule.a.t === 'reuse_listener'
                                        ? '引用'
                                        : rule.a.t === 'forward'
                                          ? 'NODE'
                                          : external
                                            ? externalProtocolBadge(external.protocol.t)
                                            : '未选'}
                                    </span>
                                    <span className="external-target-copy">
                                      <b>{target ? nameOf(target) : external?.name || '选择内部节点或代理出站'}</b>
                                      {reference && (
                                        <small>
                                          {reference.blocked
                                            ? `不可发布 · ${reference.blocked}`
                                            : `引用子树 · ${reference.ownerName} · ${reference.references} 处使用`}
                                        </small>
                                      )}
                                    </span>
                                    <span className="external-target-chevron">⌄</span>
                                  </button>
                                  {pickerOpen &&
                                    targetMenuPlacement &&
                                    typeof document !== 'undefined' &&
                                    createPortal(
                                      <fieldset
                                        ref={targetMenu}
                                        className={`external-target-menu${targetMenuPlacement.below ? ' below' : ''}`}
                                        style={{
                                          left: targetMenuPlacement.left,
                                          top: targetMenuPlacement.top,
                                          width: targetMenuPlacement.width,
                                          maxHeight: targetMenuPlacement.maxHeight,
                                        }}
                                      >
                                        {targetPickerView.t === 'targets' ? (
                                          <>
                                            <input
                                              className="f external-target-search"
                                              placeholder="搜索机器或代理出站"
                                              value={targetQuery}
                                              onChange={event => setTargetQuery(event.target.value)}
                                            />
                                            <span className="external-target-menu-label">本链已有监听节点</span>
                                            {[...visibleNextPeers, ...visibleInsidePeers].map(peer => (
                                              <button
                                                type="button"
                                                key={peer.id}
                                                className={rule.a.t === 'forward' && target === peer.id ? 'on' : ''}
                                                onClick={() => setTarget(id, ruleIndex, `node:${peer.id}`)}
                                              >
                                                <span className="external-target-kind">NODE</span>
                                                <span className="external-target-copy">
                                                  <b>{peer.name || '未命名节点'}</b>
                                                </span>
                                                <span className="external-target-where">
                                                  {peer.where === 'next' ? '当前下游' : '链内其它节点'}
                                                </span>
                                              </button>
                                            ))}
                                            <span className="external-target-menu-label">在机器上新建本链监听</span>
                                            {visibleForkPeers.map(peer => (
                                              <button
                                                type="button"
                                                key={peer.id}
                                                className={rule.a.t === 'forward' && target === peer.id ? 'on' : ''}
                                                title="保存时在这台机器创建本链监听和一棵空规则子树"
                                                onClick={() => setTarget(id, ruleIndex, `node:${peer.id}`)}
                                              >
                                                <span className="external-target-kind">NODE</span>
                                                <span className="external-target-copy">
                                                  <b>{peer.name || '未命名节点'}</b>
                                                </span>
                                                <span className="external-target-where">加入本链</span>
                                              </button>
                                            ))}
                                            {visibleBlockedPeers.length > 0 && (
                                              <span className="external-target-menu-label">不可用的机器</span>
                                            )}
                                            {visibleBlockedPeers.map(peer => (
                                              <button type="button" disabled key={peer.id} title={peer.blocked ?? ''}>
                                                <span className="external-target-kind">NODE</span>
                                                <span className="external-target-copy">
                                                  <b>{peer.name || '未命名节点'}</b>
                                                </span>
                                                <span className="external-target-where">不能选</span>
                                              </button>
                                            ))}
                                            <span className="external-target-menu-label">代理出站</span>
                                            {targetMatches('VPN Gate', 'VPNGate', '国家', '地区', '节点池') && (
                                              <button
                                                type="button"
                                                aria-label="VPN Gate"
                                                disabled={
                                                  !tenant || !vpngateNodeEligibility(nodeOf(id) ?? undefined).eligible
                                                }
                                                title={
                                                  vpngateNodeEligibility(nodeOf(id) ?? undefined).reason ?? undefined
                                                }
                                                onClick={() => {
                                                  setTargetPickerView({ t: 'vpngate' });
                                                  setTargetQuery('');
                                                }}
                                              >
                                                <span className="external-target-kind external">VG</span>
                                                <span className="external-target-copy">
                                                  <b>VPN Gate</b>
                                                  <small>
                                                    {vpngateNodeEligibility(nodeOf(id) ?? undefined).eligible
                                                      ? '按地区自动管理，或手动选择节点池'
                                                      : `不可接入 · ${vpngateNodeEligibility(nodeOf(id) ?? undefined).reason}`}
                                                  </small>
                                                </span>
                                                <span className="external-target-where">地区 ›</span>
                                              </button>
                                            )}
                                            {visibleOutbounds.map(outbound => (
                                              <span className="external-target-option" key={outbound.id}>
                                                <button
                                                  type="button"
                                                  className={`external-target-option-select${external?.id === outbound.id ? ' on' : ''}`}
                                                  disabled={!!outboundUnavailable(id, outbound)}
                                                  title={outboundUnavailable(id, outbound) ?? undefined}
                                                  onClick={() => setTarget(id, ruleIndex, `outbound:${outbound.id}`)}
                                                >
                                                  <span className="external-target-kind external">
                                                    {externalProtocolBadge(outbound.protocol.t)}
                                                  </span>
                                                  <span className="external-target-copy">
                                                    <b>{outbound.name}</b>
                                                  </span>
                                                  <span className="external-target-where">共享资源</span>
                                                </button>
                                                <button
                                                  type="button"
                                                  className="external-target-manage"
                                                  aria-label={`打开隧道 ${outbound.name}`}
                                                  onClick={() => {
                                                    setTargetPicker(null);
                                                    navigate('tunnels', {
                                                      p: outbound.protocol.t === 'warp' ? 'warp' : 'custom',
                                                      id: outbound.id,
                                                    });
                                                  }}
                                                >
                                                  查看
                                                </button>
                                              </span>
                                            ))}
                                            <button
                                              type="button"
                                              className="external-target-new"
                                              aria-label="管理隧道"
                                              onClick={() => {
                                                setTargetPicker(null);
                                                navigate('tunnels');
                                              }}
                                            >
                                              <Icon of="tunnels" size={12} className="external-target-new-icon" />
                                              <span className="external-target-copy">
                                                <b>管理隧道</b>
                                              </span>
                                              <span className="external-target-where">新建、编辑和删除</span>
                                            </button>
                                            <button
                                              type="button"
                                              className="external-target-new external-target-custom"
                                              onClick={() => {
                                                setTargetPickerView({ t: 'listener-chains' });
                                                setTargetQuery('');
                                              }}
                                            >
                                              <Icon of="tunnels" size={12} className="external-target-new-icon" />
                                              <span className="external-target-copy">
                                                <b>自定义</b>
                                              </span>
                                              <span className="external-target-where">复用已有监听</span>
                                            </button>
                                            {visibleNextPeers.length +
                                              visibleInsidePeers.length +
                                              visibleForkPeers.length +
                                              visibleBlockedPeers.length +
                                              visibleOutbounds.length +
                                              Number(targetMatches('VPN Gate', 'VPNGate', '国家', '地区', '节点池')) ===
                                              0 && <span className="external-target-empty">没有匹配项</span>}
                                          </>
                                        ) : targetPickerView.t === 'vpngate' ? (
                                          <VpngateRuleMenu
                                            selected={external && isVpngateOutbound(external) ? external : null}
                                            onBack={() => setTargetPickerView({ t: 'targets' })}
                                            onSelect={(country, serverIds) => {
                                              const pool = selectVpngatePool(outbounds, tenant, country, serverIds);
                                              if (!outbounds.some(candidate => candidate.id === pool.id))
                                                onOutboundCreated(pool);
                                              setTarget(id, ruleIndex, `outbound:${pool.id}`);
                                            }}
                                          />
                                        ) : targetPickerView.t === 'listener-chains' ? (
                                          <>
                                            <div className="external-target-menu-nav">
                                              <button
                                                type="button"
                                                aria-label="返回目标列表"
                                                onClick={() => {
                                                  setTargetPickerView({ t: 'targets' });
                                                  setTargetQuery('');
                                                }}
                                              >
                                                ←
                                              </button>
                                              <span>
                                                <b>自定义 · 复用已有监听</b>
                                                <small>先选择 App 和链</small>
                                              </span>
                                            </div>
                                            <input
                                              autoFocus
                                              className="f external-target-search"
                                              placeholder="跨 App 搜索链、机器或端口"
                                              value={targetQuery}
                                              onChange={event => setTargetQuery(event.target.value)}
                                            />
                                            {visibleListenerApps.map(group => (
                                              <Fragment key={group.app.id}>
                                                <span className="external-target-menu-label">
                                                  APP · {group.app.label || group.app.id}
                                                </span>
                                                {group.chains.map(({ chain, candidates }) => (
                                                  <button
                                                    type="button"
                                                    key={chain.id}
                                                    onClick={() => {
                                                      setTargetPickerView({
                                                        t: 'listener-endpoints',
                                                        chain: chain.id,
                                                      });
                                                      setTargetQuery('');
                                                    }}
                                                  >
                                                    <span className="external-target-kind listener">链</span>
                                                    <span className="external-target-copy">
                                                      <b>{chain.name || chain.id}</b>
                                                      <small>{chain.id}</small>
                                                    </span>
                                                    <span className="external-target-where">
                                                      {candidates.length} 个监听端点
                                                    </span>
                                                  </button>
                                                ))}
                                              </Fragment>
                                            ))}
                                            {visibleListenerApps.length === 0 && (
                                              <span className="external-target-empty">没有匹配的链</span>
                                            )}
                                          </>
                                        ) : (
                                          <>
                                            <div className="external-target-menu-nav">
                                              <button
                                                type="button"
                                                aria-label="返回链列表"
                                                onClick={() => {
                                                  setTargetPickerView({ t: 'listener-chains' });
                                                  setTargetQuery('');
                                                }}
                                              >
                                                ←
                                              </button>
                                              <span>
                                                <b>
                                                  {listenerApps
                                                    .flatMap(candidate => candidate.chains)
                                                    .find(chain => chain.id === targetPickerView.chain)?.name ||
                                                    targetPickerView.chain}
                                                </b>
                                                <small>
                                                  {listenerApps.find(candidate =>
                                                    candidate.chains.some(chain => chain.id === targetPickerView.chain),
                                                  )?.label || '未知 App'}
                                                </small>
                                              </span>
                                            </div>
                                            <input
                                              autoFocus
                                              className="f external-target-search"
                                              placeholder="搜索监听端点、机器或端口"
                                              value={targetQuery}
                                              onChange={event => setTargetQuery(event.target.value)}
                                            />
                                            <span className="external-target-menu-label">链上监听端点</span>
                                            {listeners
                                              .filter(
                                                candidate =>
                                                  candidate.ref.chain === targetPickerView.chain &&
                                                  targetMatches(
                                                    candidate.nodeName,
                                                    candidate.ref.node,
                                                    String(candidate.step.hop_in?.port ?? ''),
                                                    candidate.blocked,
                                                  ),
                                              )
                                              .map(candidate => (
                                                <button
                                                  type="button"
                                                  key={candidate.key}
                                                  disabled={!!candidate.blocked}
                                                  title={
                                                    candidate.blocked ?? '只保存引用；端口、安全参数和规则由源线路维护'
                                                  }
                                                  className={
                                                    rule.a.t === 'reuse_listener' &&
                                                    rule.a.listener.chain === candidate.ref.chain &&
                                                    rule.a.listener.node === candidate.ref.node
                                                      ? 'on'
                                                      : ''
                                                  }
                                                  onClick={() => setListenerTarget(id, ruleIndex, candidate)}
                                                >
                                                  <span className="external-target-kind listener">
                                                    {candidate.local ? '本机' : '监听'}
                                                  </span>
                                                  <span className="external-target-copy">
                                                    <b>
                                                      {candidate.nodeName} · TCP {candidate.step.hop_in?.port}
                                                    </b>
                                                    <small>{candidate.step.rules.length} 条规则</small>
                                                  </span>
                                                  <span className="external-target-where">
                                                    {candidate.blocked ?? `${candidate.references} 处引用`}
                                                  </span>
                                                </button>
                                              ))}
                                            {!listeners.some(
                                              candidate =>
                                                candidate.ref.chain === targetPickerView.chain &&
                                                targetMatches(
                                                  candidate.nodeName,
                                                  candidate.ref.node,
                                                  String(candidate.step.hop_in?.port ?? ''),
                                                  candidate.blocked,
                                                ),
                                            ) && (
                                              <span className="external-target-empty">这条链没有匹配的监听端点</span>
                                            )}
                                          </>
                                        )}
                                      </fieldset>,
                                      document.body,
                                    )}
                                </span>
                                {rule.a.t === 'forward' && (
                                  <>
                                    <select
                                      className="f wzr-dial"
                                      aria-label={`从 ${nameOf(id)} 到 ${nameOf(target)} 的连接方式`}
                                      value={dialKind ?? 'overlay'}
                                      onChange={event => setDial(id, target, event.target.value as DialKind)}
                                    >
                                      {DIAL_ORDER.map(kind => (
                                        <option
                                          key={kind}
                                          value={kind}
                                          disabled={dialUnavailable(kind, nodeOf(target), nodeOf(id))}
                                        >
                                          {DIAL_LABEL[kind]}
                                        </option>
                                      ))}
                                    </select>
                                    {dialKind === 'custom' && (
                                      <>
                                        <input
                                          className="f wzr-custom"
                                          aria-label={`到 ${nameOf(target)} 的自定义主机地址`}
                                          placeholder="主机地址"
                                          value={hostOf(rule.a.dial)}
                                          aria-invalid={!hostOf(rule.a.dial).trim()}
                                          onChange={event => setCustomHost(id, target, event.target.value)}
                                        />
                                        {!hostOf(rule.a.dial).trim() && (
                                          <span className="note warn">填写自定义主机地址</span>
                                        )}
                                      </>
                                    )}
                                    {(dialKind === 'public_ipv4' || dialKind === 'public_ipv6') &&
                                      rule.a.dial.t === 'addr' && (
                                        <span className="mono dim wzr-dial-address">{hostOf(rule.a.dial)}</span>
                                      )}
                                    {rule.a.dial.t === 'reverse' && (
                                      <span className="note">由 {nameOf(target)} 连接本机</span>
                                    )}
                                  </>
                                )}
                                {rule.a.t === 'reuse_listener' &&
                                  reference &&
                                  (reference.local ? (
                                    <span className="listener-local-route mono">
                                      本机内部 · 回环:{reference.step.hop_in?.port}
                                    </span>
                                  ) : (
                                    <>
                                      <select
                                        className="f wzr-dial"
                                        aria-label={`从 ${nameOf(id)} 到 ${reference.nodeName} 的连接方式`}
                                        value={listenerDialKindOf(rule.a.dial)}
                                        onChange={event =>
                                          setListenerDial(
                                            id,
                                            reference.ref,
                                            event.target.value as 'public_ipv4' | 'public_ipv6' | 'overlay' | 'custom',
                                          )
                                        }
                                      >
                                        {LISTENER_DIAL_ORDER.map(kind => (
                                          <option
                                            key={kind}
                                            value={kind}
                                            disabled={dialUnavailable(kind, nodeOf(reference.ref.node), null)}
                                          >
                                            {DIAL_LABEL[kind]}
                                          </option>
                                        ))}
                                      </select>
                                      {rule.a.dial.t === 'addr' && (
                                        <input
                                          className="f wzr-custom"
                                          aria-label={`到 ${reference.nodeName} 的自定义主机地址`}
                                          placeholder="主机地址"
                                          value={rule.a.dial.v}
                                          aria-invalid={!rule.a.dial.v.trim()}
                                          onChange={event =>
                                            setListenerCustomHost(id, reference.ref, event.target.value)
                                          }
                                        />
                                      )}
                                    </>
                                  ))}
                              </>
                            )}
                            {rule.a.t === 'egress' && (
                              <span className="egress-dns-reference">
                                <MachineEgressDnsControls
                                  resolution={dnsFor(id, rule.m)}
                                  supported={supportsEgressDns(rule.m)}
                                  onChange={resolution => onDnsChange(id, rule.m, resolution)}
                                  readOnly={false}
                                  nodeName={nameOf(id)}
                                  accessibleSuffix={`（新建链：${nameOf(id)} 第 ${ruleIndex + 1} 条）`}
                                />
                              </span>
                            )}
                          </td>
                          <td className="wzr-controls">
                            <button
                              type="button"
                              className="btn"
                              aria-label="上移规则"
                              disabled={pinned || ruleIndex === 0}
                              onClick={() => move(id, ruleIndex, -1)}
                            >
                              ↑
                            </button>
                            <button
                              type="button"
                              className="btn"
                              aria-label="下移规则"
                              disabled={pinned || isPinnedTerminalRule(rows[ruleIndex + 1])}
                              onClick={() => move(id, ruleIndex, 1)}
                            >
                              ↓
                            </button>
                            <button
                              type="button"
                              className="btn danger"
                              aria-label="删除规则"
                              disabled={rule.m.t === 'any'}
                              onClick={() => remove(id, ruleIndex)}
                            >
                              删
                            </button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
                {Array.from(
                  new Set(rows.flatMap(rule => (rule.a.t === 'forward' && rule.a.to ? [rule.a.to] : []))),
                ).map(target => {
                  const action = rows.find(rule => rule.a.t === 'forward' && rule.a.to === target)?.a;
                  if (!action || action.t !== 'forward') return null;
                  const host = action.dial.t === 'reverse' ? id : target;
                  const reverse = action.dial.t === 'reverse';
                  const wire = wireOf(host);
                  const issue = portIssueOf(host);
                  return (
                    <div
                      className="panel listener-reference-panel hop-target-panel wzr-listener"
                      key={`${id}/${target}`}
                    >
                      <header>
                        <PanelTitle of="chains">本链监听</PanelTitle>
                        <span className="hint">端口和协议属于目标监听；拨号属于当前链边</span>
                      </header>
                      <div className="listener-reference-list">
                        <div className="listener-reference-row hop-target-row">
                          <div className="listener-reference-main">
                            <span className="external-target-kind node">本链</span>
                            <span className="listener-reference-copy">
                              <b>
                                {nameOf(host)} · TCP {portOf(host)}
                              </b>
                            </span>
                          </div>
                          <div className="listener-reference-facts hop-target-facts">
                            <label className="hopfld hop-target-port">
                              <small>目标端口</small>
                              <input
                                className="f mono"
                                inputMode="numeric"
                                aria-label={`${nameOf(host)} 的中转监听端口`}
                                value={portOf(host)}
                                aria-invalid={!!issue}
                                onChange={event => onPortChange(host, event.target.value)}
                              />
                            </label>
                            <label className="hopfld hop-target-wire">
                              <small>承载协议</small>
                              <select
                                className="f"
                                aria-label={`${nameOf(host)} 的中转协议`}
                                value={wire}
                                onChange={event => onWireChange(host, event.target.value as HopWireKind)}
                              >
                                {HOP_WIRE_OPTIONS.map(option => (
                                  <option
                                    key={option.kind}
                                    value={option.kind}
                                    disabled={
                                      (reverse && !option.reverseOk) ||
                                      (option.kind === 'reality' && !globalRealityReady)
                                    }
                                  >
                                    {option.label}
                                  </option>
                                ))}
                              </select>
                            </label>
                            <label className="hopfld hop-target-pool">
                              <small>连接复用</small>
                              <select
                                className="f"
                                aria-label={`从 ${nameOf(id)} 到 ${nameOf(target)} 的连接复用`}
                                value={action.pool.t}
                                disabled={reverse}
                                onChange={event => setPool(id, target, event.target.value === 'mux')}
                              >
                                <option value="none">每次新建</option>
                                <option value="mux">Mux 复用</option>
                              </select>
                            </label>
                          </div>
                          <p className="hop-target-note">
                            <span>
                              端口开在 {nameOf(host)}，连接由 {nameOf(reverse ? target : id)} 发起；目标端口必须唯一。
                            </span>
                            <span>
                              {action.pool.t === 'mux' ? '当前多条流复用同一条隧道。' : '当前每条业务流单独建连。'}
                            </span>
                          </p>
                        </div>
                      </div>
                      {wire === 'reality' && (
                        <p className="note">中转 REALITY 使用全局伪装站点：{realitySite.dest || '未配置'}。</p>
                      )}
                      {issue && <p className="note warn">{issue}</p>}
                      {(action.dial.t === 'addr' || reverse) && wire === 'none' && (
                        <p className="note warn">明文直连会暴露 UUID 和目标地址；请改用加密承载或 WireGuard。</p>
                      )}
                    </div>
                  );
                })}
                <div className="toolbar">
                  <button type="button" className="btn" onClick={() => addRule(id)}>
                    ＋ 加一条
                  </button>
                </div>
              </fieldset>
            </div>
            <div className="chain-rule-children">
              {childEdges.map(rule =>
                rule.a.t === 'forward' && rule.a.to
                  ? renderNode(
                      rule.a.to,
                      depth + 1,
                      rule.m.t === 'any' ? '' : `${rule.m.t}${matchValues(rule.m) ? `=${matchValues(rule.m)}` : ''}`,
                      nextAncestors,
                    )
                  : null,
              )}
            </div>
          </>
        )}
      </div>
    );
  };
  const tail = spine.at(-1);
  const tailPeers = tail ? peersFor(tail).filter(peer => !peer.blocked && !memberSet.has(peer.id)) : [];
  return (
    <>
      <div className="node-chain-use chain-tree wzr-tree">
        <div className="chain-rule-tree">{renderNode(root, 0, '', new Set())}</div>
        <div className="wzr-addhop">
          <select
            className="f"
            aria-label="在路径末尾添加机器"
            value=""
            disabled={tailPeers.length === 0}
            onChange={event => {
              if (event.target.value) appendHop(event.target.value);
            }}
          >
            <option value="">{tailPeers.length ? '＋ 在主干末尾加一跳…' : '没有可添加的主干机器'}</option>
            {tailPeers.map(peer => (
              <option key={peer.id} value={peer.id}>
                {peer.name || peer.id}（{peer.id}）
              </option>
            ))}
          </select>
          <span className="note">点机器名展开它的规则表。</span>
        </div>
      </div>
    </>
  );
}
