import { useId, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import type {
  DestMatch,
  EgressDnsResolution,
  ExternalOutbound,
  HopMux,
  HopPool,
  NodeAgentStateItem,
  Rule,
  SnapshotApp,
  SnapshotStep,
} from '../api';
import { egressDnsIssue } from '../api';
import { MenuLayer, menuArrowKey } from '../ui/choice-menu';
import { HOP_WIRE_OPTIONS, hopWireLabel, type HopWireKind } from '../ui/format';
import { vpngateNodeEligibility } from '../vpngate-capability';
import { selectVpngatePool } from '../vpngate-selection';
import {
  buildMatch,
  defaultHopDial,
  dialKindOf,
  DnsPolicyDialog,
  DnsViewLink,
  EgressDnsChoice,
  ExternalOutboundEditor,
  egressDnsSelectorKey,
  formatHostPort,
  forwardAction,
  forwardPeers,
  hostOf,
  hopDialOf,
  HopRealityFields,
  HopTargetRow,
  isPinnedTerminalRule,
  isSniffingFallbackRule,
  listenerDialKindOf,
  listenerDialOf,
  listenerRefKey,
  ListenerReferenceRow,
  MATCH_KINDS,
  matchDependsOnSniffing,
  matchValues,
  MuxConfigDrawer,
  newEgressDns,
  NetworkMatchSelect,
  NextHopsPanel,
  orderedDnsPolicies,
  pinTerminalRules,
  ProxyNextHopRow,
  reusableListeners,
  reuseListenerAction,
  REVERSE_HOP_WIRE_OPTIONS,
  RuleDialField,
  RuleTargetPicker,
  sortedDnsPolicies,
  supportsEgressDns,
  under,
  useOverlayAddrs,
  type DialKind,
  type EgressDnsPolicy,
  type ForwardPeer,
  type MuxEditorState,
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

/**
 * 一台机器在新建链里看到的本机 DNS 解析策略：已有的按位置排序，新建的接在后面，按调整过的顺序重排，
 * 删掉的不再列出。弹层和提交时的 `reorder_node_egress_dns` 都用这一份，两处顺序不会不一致。
 */
export function wizardDnsPolicies(
  node: string,
  stored: EgressDnsPolicy[],
  changes: WizardDnsChange[],
  order: DestMatch[] | null,
): EgressDnsPolicy[] {
  const baseline = sortedDnsPolicies(stored.filter(policy => policy.node === node));
  const changeFor = (selector: DestMatch) =>
    changes.find(
      change => change.node === node && egressDnsSelectorKey(change.selector) === egressDnsSelectorKey(selector),
    );
  const pending = changes.flatMap((change, index) =>
    change.node === node &&
    change.resolution &&
    !baseline.some(policy => egressDnsSelectorKey(policy.selector) === egressDnsSelectorKey(change.selector))
      ? [{ node, position: baseline.length + index, selector: change.selector, resolution: change.resolution }]
      : [],
  );
  return orderedDnsPolicies([...baseline, ...pending], order).filter(policy => {
    const change = changeFor(policy.selector);
    return change ? change.resolution !== null : true;
  });
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
  dnsPolicies: EgressDnsPolicy[];
  dnsChanges: WizardDnsChange[];
  onDnsChange: (node: string, selector: DestMatch, resolution: EgressDnsResolution | null) => void;
  /** 机器 DNS 策略的新顺序；没有调整过的机器不在其中。 */
  dnsOrders: Record<string, DestMatch[]>;
  onDnsOrderChange: (node: string, selectors: DestMatch[]) => void;
  globalRelayMux: HopMux;
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
    dnsPolicies,
    dnsChanges,
    onDnsChange,
    dnsOrders,
    onDnsOrderChange,
    globalRelayMux,
  } = props;
  // 各机器独立折叠，与链路规则一致：默认展开入口和刚加入路径的机器。键是树中的位置（同一台机器
  // 出现两次时两处各自折叠）。
  const [expansion, setExpansion] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [fresh, setFresh] = useState<ReadonlySet<string>>(() => new Set());
  const [targetPicker, setTargetPicker] = useState<string | null>(null);
  const [creatingFor, setCreatingFor] = useState<{ source: string; index: number } | null>(null);
  const [muxEditor, setMuxEditor] = useState<(MuxEditorState & { source: string }) | null>(null);
  const [dnsDialog, setDnsDialog] = useState<{ node: string; focus: string | null; id: number } | null>(null);
  const overlayOf = useOverlayAddrs();
  const restart = () => {
    setExpansion(new Map());
    setFresh(new Set());
    setTargetPicker(null);
  };
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
  const dnsChangeFor = (id: string, selector: DestMatch) =>
    dnsChanges.find(
      candidate => candidate.node === id && egressDnsSelectorKey(candidate.selector) === egressDnsSelectorKey(selector),
    ) ?? null;
  const dnsPoliciesFor = (id: string): EgressDnsPolicy[] =>
    wizardDnsPolicies(id, dnsPolicies, dnsChanges, dnsOrders[id] ?? null);
  const dnsErrorFor = (id: string, selector: DestMatch) => {
    const resolution = dnsChangeFor(id, selector)?.resolution;
    return resolution ? egressDnsIssue(resolution) || null : null;
  };
  const dnsInvalid = (id: string) =>
    dnsChanges.some(change => change.node === id && !!change.resolution && !!egressDnsIssue(change.resolution));
  const openDns = (node: string, focus: string | null) =>
    setDnsDialog(previous => ({ node, focus, id: (previous?.id ?? 0) + 1 }));
  const moveDns = (id: string, selector: DestMatch, delta: number) => {
    const ordered = dnsPoliciesFor(id).map(policy => policy.selector);
    const index = ordered.findIndex(candidate => egressDnsSelectorKey(candidate) === egressDnsSelectorKey(selector));
    const target = index + delta;
    if (index < 0 || target < 0 || target >= ordered.length) return;
    [ordered[index], ordered[target]] = [ordered[target], ordered[index]];
    onDnsOrderChange(id, ordered);
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
    if (rule.a.t === 'forward' && rule.a.to && !next[rule.a.to]) {
      next[rule.a.to] = [fallbackFor(rule.a.to)];
      // 刚加入路径的机器自动展开，接着就能配置它的规则。
      const added = rule.a.to;
      setFresh(previous => new Set(previous).add(added));
    }
    commit(next);
  };
  const defaultForward = (source: string, target: string) => {
    const port = Number(portOf(target)) || freePortAcross(taken, [target], hopBase);
    return forwardAction(target, defaultHopDial({ peer: nodeOf(target), self: nodeOf(source), port }));
  };
  const setTarget = (source: string, index: number, value: string) => {
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
  const poolOf = (source: string, target: string): HopPool => {
    const rule = (tables[source] ?? []).find(candidate => candidate.a.t === 'forward' && candidate.a.to === target);
    return rule?.a.t === 'forward' ? rule.a.pool : { t: 'none' };
  };
  const setPool = (source: string, target: string, pool: HopPool) =>
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'forward' && rule.a.to === target
          ? { ...rule, a: forwardAction(target, rule.a.dial, pool) }
          : rule,
      ),
    });
  const setListenerPool = (source: string, listener: { chain: string; node: string }, pool: HopPool) =>
    commit({
      ...tables,
      [source]: (tables[source] ?? []).map(rule =>
        rule.a.t === 'reuse_listener' &&
        rule.a.listener.chain === listener.chain &&
        rule.a.listener.node === listener.node
          ? { ...rule, a: reuseListenerAction(listener, rule.a.dial, pool) }
          : rule,
      ),
    });
  const addRule = (source: string) => {
    const rows = [...(tables[source] ?? [fallbackFor(source)])];
    const terminal = rows.findIndex(isPinnedTerminalRule);
    rows.splice(terminal < 0 ? rows.length : terminal, 0, {
      m: buildMatch(MATCH_KINDS.some(kind => kind.t === 'geosite') ? 'geosite' : 'domain_keyword', ''),
      a: egressAllowed(source) ? { t: 'egress', send_through: null } : { t: 'block' },
    });
    commit({ ...tables, [source]: pinTerminalRules(rows) });
  };
  const addSniffingFallback = (source: string) =>
    commit({
      ...tables,
      [source]: pinTerminalRules([
        ...(tables[source] ?? [fallbackFor(source)]),
        { m: { t: 'sniffing_failed' }, a: fallbackFor(source).a },
      ]),
    });
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
    setFresh(previous => new Set(previous).add(target));
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
            restart();
            onRootChange(id);
            onTablesChange({ [id]: [fallbackFor(id)] });
          }}
        >
          <option value="">{available.length ? '— 选择入口节点 —' : '没有可用机器'}</option>
          {available.map(candidate => (
            <option key={candidate.node_id} value={candidate.node_id}>
              {candidate.name || candidate.node_id}（{candidate.node_id}）
            </option>
          ))}
        </select>
        {!available.length && <span className="note">先去「机器」纳管一台，再回来选择入口。</span>}
      </div>
    );
  }

  // 先统计每台机器在树中出现的次数，出现两次以上的每一处都标「共享配置」。遍历规则与下面的
  // renderNode 一致（相同的防环和防重复展开）。
  const occurrences = new Map<string, number>();
  {
    const seen = new Set<string>();
    const walk = (id: string, path: Set<string>) => {
      occurrences.set(id, (occurrences.get(id) ?? 0) + 1);
      if (seen.has(id) || path.has(id)) return;
      seen.add(id);
      const next = new Set(path).add(id);
      for (const rule of tables[id] ?? []) if (rule.a.t === 'forward' && rule.a.to) walk(rule.a.to, next);
    };
    walk(root, new Set());
  }

  const renderRules = (id: string, occurrence: string, rows: Rule[]) => {
    const name = nameOf(id);
    const peers = peersFor(id);
    const listeners = listenersFor(id);
    const vpngate = vpngateNodeEligibility(nodeOf(id) ?? undefined);
    const forwardTargets = [...new Set(rows.flatMap(rule => (rule.a.t === 'forward' && rule.a.to ? [rule.a.to] : [])))];
    const reverseTargets = forwardTargets.filter(target =>
      rows.some(rule => rule.a.t === 'forward' && rule.a.to === target && rule.a.dial.t === 'reverse'),
    );
    const normalTargets = forwardTargets.filter(target => !reverseTargets.includes(target));
    const referencedTargets = [
      ...new Map(
        rows.flatMap(rule =>
          rule.a.t === 'reuse_listener' ? [[listenerRefKey(rule.a.listener), rule.a.listener] as const] : [],
        ),
      ).values(),
    ];
    const proxyTargets = [...new Set(rows.flatMap(rule => (rule.a.t === 'proxy' ? [rule.a.outbound] : [])))];
    // 新建链的 REALITY 中转统一使用全局伪装站点：没有配置时不能选。
    const wireChoices = (reverse: boolean) =>
      (reverse ? REVERSE_HOP_WIRE_OPTIONS : HOP_WIRE_OPTIONS).map(option => ({
        kind: option.kind,
        label: option.label,
        disabled: ('disabled' in option && !!option.disabled) || (option.kind === 'reality' && !globalRealityReady),
      }));
    const globalReality = (
      <HopRealityFields
        hop={{ dest: realitySite.dest, names: realitySite.names.join(', ') }}
        title="新建链的中转 REALITY 使用全局伪装站点"
      />
    );
    const hasSniffingFallback = rows.some(isSniffingFallbackRule);
    const hasSniffingDependentRule = rows.some(rule => matchDependsOnSniffing(rule.m));
    return (
      <fieldset className="panel rule-editor rule-ro">
        <table className="tbl rule-table chain-rule-table">
          <tbody>
            {rows.map((rule, ruleIndex) => {
              const pinned = isPinnedTerminalRule(rule);
              const matchKind = MATCH_KINDS.find(candidate => candidate.t === rule.m.t);
              const target =
                rule.a.t === 'forward' ? rule.a.to : rule.a.t === 'reuse_listener' ? rule.a.listener.node : '';
              const listenerRef = rule.a.t === 'reuse_listener' ? rule.a.listener : null;
              const reference = listenerRef
                ? (listeners.find(candidate => candidate.key === listenerRefKey(listenerRef)) ?? null)
                : null;
              const pickerKey = `${occurrence}#${ruleIndex}`;
              const label = `${name} 第 ${ruleIndex + 1} 条`;
              const dnsResolution = rule.a.t === 'egress' ? dnsFor(id, rule.m) : null;
              const dnsSupported = supportsEgressDns(rule.m) && matchValues(rule.m).trim() !== '';
              return (
                <tr key={`${id}/${ruleIndex}`}>
                  <td className="mono dim" style={{ width: 24 }}>
                    {ruleIndex + 1}
                  </td>
                  <td className="rule-match-cell">
                    <select
                      className="f"
                      aria-label={`${label}匹配条件`}
                      value={rule.m.t}
                      disabled={rule.m.t === 'any'}
                      title={rule.m.t === 'any' ? '主干兜底，固定在末尾' : undefined}
                      onChange={event =>
                        patch(id, ruleIndex, { ...rule, m: buildMatch(event.target.value as Rule['m']['t'], '') })
                      }
                    >
                      {MATCH_KINDS.map(kind => (
                        <option
                          key={kind.t}
                          value={kind.t}
                          disabled={
                            kind.t === 'any' ||
                            (kind.t === 'sniffing_failed' && rows.some(row => row.m.t === kind.t && row !== rule))
                          }
                        >
                          {kind.label}
                        </option>
                      ))}
                    </select>
                    {matchKind?.list && rule.m.t !== 'any' && (
                      <input
                        className="f mono"
                        aria-label={`${label}匹配取值`}
                        placeholder={matchKind.hint}
                        value={matchValues(rule.m)}
                        spellCheck={false}
                        onChange={event =>
                          patch(id, ruleIndex, { ...rule, m: buildMatch(rule.m.t, event.target.value) })
                        }
                      />
                    )}
                    {rule.m.t === 'network' && (
                      <NetworkMatchSelect
                        label={`${label}传输层`}
                        value={rule.m.v}
                        onChange={value => patch(id, ruleIndex, { ...rule, m: { t: 'network', v: value } })}
                      />
                    )}
                  </td>
                  <td className="rule-action-cell">
                    <div className="rule-action-fields">
                      <select
                        className={`f rule-action-select rule-action-${rule.a.t === 'block' ? 'block' : rule.a.t === 'egress' ? 'egress' : 'forward'}`}
                        aria-label={`${label}动作`}
                        value={rule.a.t === 'proxy' || rule.a.t === 'reuse_listener' ? 'forward' : rule.a.t}
                        onChange={event => {
                          const action = event.target.value as 'forward' | 'egress' | 'block';
                          setAction(id, ruleIndex, action);
                          // 与链路规则一致：改成转发后直接打开目标菜单。
                          setTargetPicker(action === 'forward' ? pickerKey : null);
                        }}
                      >
                        <option value="forward">转发给</option>
                        <option value="egress" disabled={!egressAllowed(id)}>
                          从本机出网
                        </option>
                        <option value="block">拒绝</option>
                      </select>
                      {(rule.a.t === 'forward' || rule.a.t === 'proxy' || rule.a.t === 'reuse_listener') && (
                        <RuleTargetPicker
                          label={`${label}转发目标`}
                          action={rule.a}
                          open={targetPicker === pickerKey}
                          onOpenChange={open => setTargetPicker(open ? pickerKey : null)}
                          disabled={false}
                          peers={peers}
                          outbounds={availableOutbounds}
                          outboundUnavailable={outbound => outboundUnavailable(id, outbound)}
                          vpngate={vpngate}
                          tenant={tenant}
                          listeners={listeners}
                          apps={listenerApps}
                          chainId={chainId}
                          onNode={peer => setTarget(id, ruleIndex, `node:${peer}`)}
                          onOutbound={outbound => setTarget(id, ruleIndex, `outbound:${outbound}`)}
                          onVpngate={(country, serverIds) => {
                            if (!tenant || !vpngate.eligible) return;
                            const pool = selectVpngatePool(outbounds, tenant, country, serverIds);
                            if (!outbounds.some(candidate => candidate.id === pool.id)) onOutboundCreated(pool);
                            setTarget(id, ruleIndex, `outbound:${pool.id}`);
                          }}
                          onListener={listener => setListenerTarget(id, ruleIndex, listener)}
                          onCustom={() => setCreatingFor({ source: id, index: ruleIndex })}
                        />
                      )}
                      {rule.a.t === 'forward' && rule.a.to && (
                        <RuleDialField
                          label={`从 ${name} 到 ${nameOf(target)} 的连接方式`}
                          current={dialKindOf(rule.a.dial, nodeOf(target))}
                          customHost={hostOf(rule.a.dial)}
                          target={nodeOf(target)}
                          self={nodeOf(id)}
                          overlay={overlayOf(target)}
                          targetName={nameOf(target)}
                          onPick={kind => setDial(id, target, kind)}
                          onCustom={host => setCustomHost(id, target, host)}
                        />
                      )}
                      {rule.a.t === 'reuse_listener' &&
                        reference &&
                        (reference.local ? (
                          <span className="listener-local-route mono">
                            本机内部 · 回环:{reference.step.hop_in?.port}
                          </span>
                        ) : (
                          <RuleDialField
                            listener
                            label={`从 ${name} 到 ${reference.nodeName} 的连接方式`}
                            current={listenerDialKindOf(rule.a.dial)}
                            customHost={rule.a.dial.t === 'addr' ? rule.a.dial.v : ''}
                            target={nodeOf(reference.ref.node)}
                            self={null}
                            overlay={overlayOf(reference.ref.node)}
                            targetName={reference.nodeName}
                            onPick={kind =>
                              setListenerDial(
                                id,
                                reference.ref,
                                kind as 'public_ipv4' | 'public_ipv6' | 'overlay' | 'custom',
                              )
                            }
                            onCustom={host => setListenerCustomHost(id, reference.ref, host)}
                          />
                        ))}
                      {rule.a.t === 'egress' && (
                        <EgressDnsChoice
                          custom={!!dnsResolution}
                          supported={dnsSupported}
                          label={`DNS 解析方式（新建链：${label}）`}
                          title={
                            dnsSupported
                              ? `修改 ${name} 的本机 DNS 解析策略`
                              : '自定义 DNS 只支持填写了内容的域名类规则'
                          }
                          invalid={dnsInvalid(id)}
                          onChoose={custom => {
                            if (!custom) {
                              onDnsChange(id, rule.m, null);
                              return;
                            }
                            onDnsChange(id, rule.m, dnsFor(id, rule.m) ?? newEgressDns());
                            // 新策略还没有 DNS 地址：直接打开弹层，停在这一条上。
                            openDns(id, egressDnsSelectorKey(rule.m));
                          }}
                          onView={() => openDns(id, dnsResolution ? egressDnsSelectorKey(rule.m) : null)}
                        />
                      )}
                    </div>
                  </td>
                  <td className="rule-ops-cell">
                    <button
                      type="button"
                      className="btn"
                      aria-label={`${label}上移`}
                      title={pinned ? '终结规则位置固定' : '上移'}
                      disabled={pinned || ruleIndex === 0}
                      onClick={() => move(id, ruleIndex, -1)}
                    >
                      ↑
                    </button>
                    <button
                      type="button"
                      className="btn"
                      aria-label={`${label}下移`}
                      title={
                        pinned
                          ? '终结规则位置固定'
                          : isPinnedTerminalRule(rows[ruleIndex + 1])
                            ? '不能移动到终结规则之后'
                            : '下移'
                      }
                      disabled={pinned || isPinnedTerminalRule(rows[ruleIndex + 1])}
                      onClick={() => move(id, ruleIndex, 1)}
                    >
                      ↓
                    </button>
                    <button
                      type="button"
                      className="btn danger"
                      aria-label={`删除${label}`}
                      title={rule.m.t === 'any' ? '主干兜底，不能删除' : `删除第 ${ruleIndex + 1} 条规则`}
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
        {normalTargets.length + reverseTargets.length + referencedTargets.length + proxyTargets.length > 0 && (
          <NextHopsPanel>
            {normalTargets.map(target => {
              const wire = wireOf(target);
              const pool = poolOf(id, target);
              const dialedDirectly = rows.some(
                rule => rule.a.t === 'forward' && rule.a.to === target && rule.a.dial.t !== 'overlay',
              );
              return (
                <HopTargetRow
                  key={target}
                  badge="本链"
                  title={target}
                  name={nameOf(target)}
                  portLabel="目标端口"
                  port={portOf(target)}
                  portPlaceholder={String(hopBase)}
                  portAriaLabel={`${nameOf(target)} 的中转监听端口`}
                  portIssue={portIssueOf(target)}
                  onPort={port => onPortChange(target, port)}
                  wire={wire}
                  wireOptions={wireChoices(false)}
                  wireAriaLabel={`${nameOf(target)} 的中转协议`}
                  wireMark={dialedDirectly && wire === 'none' ? '明文直连会暴露 UUID 和目标地址。' : null}
                  onWire={kind => onWireChange(target, kind)}
                  pool={pool}
                  poolAriaLabel={`从 ${name} 到 ${nameOf(target)} 的连接复用`}
                  onPool={next => setPool(id, target, next)}
                  onMux={() =>
                    setMuxEditor({
                      source: id,
                      to: target,
                      targetName: nameOf(target),
                      followGlobal: !(pool.t === 'mux' && pool.v),
                      value: pool.t === 'mux' && pool.v ? { ...pool.v } : { ...globalRelayMux },
                    })
                  }
                  readOnly={false}
                  security={wire === 'reality' && globalReality}
                />
              );
            })}
            {reverseTargets.length > 0 && (
              <HopTargetRow
                badge="反向"
                title={id}
                name={name}
                detail={`${reverseTargets.map(nameOf).join('、')} 从这里接入`}
                portLabel="本机端口"
                port={portOf(id)}
                portPlaceholder={String(hopBase)}
                portAriaLabel={`${name} 的中转监听端口`}
                portIssue={portIssueOf(id)}
                onPort={port => onPortChange(id, port)}
                wire={wireOf(id)}
                wireOptions={wireChoices(true)}
                wireAriaLabel={`${name} 的中转协议`}
                wireMark={wireOf(id) === 'none' ? '明文接入可能暴露 UUID 和目标地址。' : null}
                onWire={kind => onWireChange(id, kind)}
                readOnly={false}
                security={wireOf(id) === 'reality' && globalReality}
              />
            )}
            {referencedTargets.map(listener => {
              const target = listeners.find(candidate => candidate.key === listenerRefKey(listener)) ?? null;
              const referenceRule = rows.find(
                rule =>
                  rule.a.t === 'reuse_listener' &&
                  rule.a.listener.chain === listener.chain &&
                  rule.a.listener.node === listener.node,
              );
              const pool: HopPool = referenceRule?.a.t === 'reuse_listener' ? referenceRule.a.pool : { t: 'none' };
              return (
                <ListenerReferenceRow
                  key={listenerRefKey(listener)}
                  listener={listener}
                  target={target}
                  dial={referenceRule?.a.t === 'reuse_listener' ? referenceRule.a.dial : null}
                  pool={pool}
                  poolAriaLabel={`从 ${name} 到 ${target?.nodeName ?? listener.node} 的连接复用`}
                  onPool={next => setListenerPool(id, listener, next)}
                  onMux={() =>
                    setMuxEditor({
                      source: id,
                      to: listenerRefKey(listener),
                      listener,
                      targetName: target ? `${target.nodeName} · TCP ${target.step.hop_in?.port}` : listener.node,
                      followGlobal: !(pool.t === 'mux' && pool.v),
                      value: pool.t === 'mux' && pool.v ? { ...pool.v } : { ...globalRelayMux },
                    })
                  }
                  readOnly={false}
                />
              );
            })}
            {proxyTargets.map(outboundId => {
              const outbound = availableOutbounds.find(candidate => candidate.id === outboundId) ?? null;
              return (
                <ProxyNextHopRow
                  key={`external-${outboundId}`}
                  outboundId={outboundId}
                  outbound={outbound}
                  nodeId={id}
                  nodeName={name}
                  unavailable={outbound ? outboundUnavailable(id, outbound) : null}
                  editable
                />
              );
            })}
          </NextHopsPanel>
        )}
        <div className="toolbar rule-editor-toolbar">
          <button type="button" className="btn" onClick={() => addRule(id)}>
            ＋ 加一条
          </button>
          {hasSniffingDependentRule && !hasSniffingFallback && (
            <button
              type="button"
              className="btn"
              onClick={() => addSniffingFallback(id)}
              title="域名/Geosite 规则依赖嗅探；匹配条件与转发、从本机出网或拒绝动作独立配置。"
            >
              ＋ 嗅探失败兜底
            </button>
          )}
          <span className="sp" />
          {dnsPoliciesFor(id).length > 0 && !rows.some(rule => rule.a.t === 'egress') && (
            <DnsViewLink invalid={dnsInvalid(id)} onOpen={() => openDns(id, null)} />
          )}
        </div>
      </fieldset>
    );
  };

  const rendered = new Set<string>();
  const renderNode = (id: string, incoming: string, path: string[]): ReactNode => {
    const repeated = rendered.has(id) || path.includes(id);
    if (!repeated) rendered.add(id);
    const occurrence = [...path, id].join('>');
    const defaultExpanded = !repeated && (id === root || fresh.has(id));
    const expanded = expansion.get(occurrence) ?? defaultExpanded;
    const toggle = () =>
      setExpansion(previous => new Map(previous).set(occurrence, !(previous.get(occurrence) ?? defaultExpanded)));
    const rows = tables[id] ?? [fallbackFor(id)];
    const spineIndex = spine.indexOf(id);
    const entryEdges = members.flatMap(source =>
      (tables[source] ?? []).flatMap(rule =>
        rule.a.t === 'forward' && rule.a.to === id ? [{ source, dial: rule.a.dial }] : [],
      ),
    );
    const listened = entryEdges.length > 0 || rows.some(rule => rule.a.t === 'forward' && rule.a.dial.t === 'reverse');
    const childEdges = repeated
      ? []
      : rows
          .filter(rule => rule.a.t === 'forward' && rule.a.to)
          .sort((left, right) => Number(right.m.t === 'any') - Number(left.m.t === 'any'));
    return (
      <div
        className={`chain-rule-node${expanded ? ' open' : ''}`}
        style={{ '--depth': path.length } as CSSProperties}
        key={occurrence}
      >
        <div
          role="button"
          tabIndex={0}
          className="chain-rule-node-head"
          aria-expanded={expanded}
          onClick={toggle}
          onKeyDown={event => {
            if (event.target !== event.currentTarget) return;
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              toggle();
            }
          }}
        >
          <span className="disc" aria-hidden="true">
            {expanded ? '▾' : '▸'}
          </span>
          <span className="who">
            <b title={id}>{nameOf(id)}</b>
            {(occurrences.get(id) ?? 0) > 1 && (
              <span className="st" title="这台机器在这条链上出现多次，共用同一张规则表">
                共享配置
              </span>
            )}
            {id === root && <span className="st b-role">入口</span>}
            {incoming && <span className="st">{incoming}</span>}
          </span>
          <span className="hopctl" onClick={event => event.stopPropagation()}>
            {/* 与链路规则一致：只有主干上、入口之后的机器提供移除；支路由上游的转发规则管理。
                入口的 × 是向导独有的「换一台当入口」。 */}
            {id === root
              ? !fixedHead && (
                  <button
                    type="button"
                    className="del-ctl"
                    aria-label="换一台当入口"
                    title="换一台机器当入口，已写的规则一并清空"
                    onClick={() => {
                      restart();
                      onRootChange(null);
                      onTablesChange({});
                    }}
                  >
                    ×
                  </button>
                )
              : spineIndex > 0 && (
                  <button
                    type="button"
                    className="del-ctl"
                    aria-label={`把 ${nameOf(id)} 移出本链`}
                    title="从这条链移除这台机器，其整张规则表一并删除"
                    onClick={() => removeNode(id)}
                  >
                    ×
                  </button>
                )}
          </span>
          <div className="meta">
            <span className="m-rules">{rows.length} 条规则</span>
            <div className="m-hop" title={listened ? `${nameOf(id)} 实际监听的中转端口与承载协议` : undefined}>
              <span className="mono">{listened ? `${portOf(id)} / ${hopWireLabel(wireOf(id))}` : '—'}</span>
            </div>
          </div>
        </div>
        {/* 折叠的机器仍然挂载规则表：打开中的目标菜单、未提交的输入都在里面。再次出现的位置共用同一张表，
            只在展开时挂载。 */}
        {(!repeated || expanded) && (
          <div className="chain-rule-body" hidden={!expanded}>
            {renderRules(id, occurrence, rows)}
          </div>
        )}
        <div className="chain-rule-children">
          {childEdges.map(rule =>
            rule.a.t === 'forward' && rule.a.to
              ? renderNode(
                  rule.a.to,
                  rule.m.t === 'any' ? '' : `${rule.m.t}${matchValues(rule.m) ? `=${matchValues(rule.m)}` : ''}`,
                  [...path, id],
                )
              : null,
          )}
        </div>
      </div>
    );
  };
  const tail = spine.at(-1);
  const tailPeers = tail ? peersFor(tail).filter(peer => !peer.blocked && !memberSet.has(peer.id)) : [];
  const creatingRule = creatingFor ? tables[creatingFor.source]?.[creatingFor.index] : undefined;
  return (
    <>
      <div className="node-chain-use chain-tree wzr-tree">
        <div className="chain-rule-tree">{renderNode(root, '', [])}</div>
        {tail && <AppendHopPicker tailName={nameOf(tail)} peers={tailPeers} onPick={appendHop} />}
      </div>
      {dnsDialog && (
        <DnsPolicyDialog
          key={dnsDialog.id}
          nodeName={nameOf(dnsDialog.node)}
          policies={dnsPoliciesFor(dnsDialog.node)}
          resolutionOf={selector => dnsFor(dnsDialog.node, selector)}
          onChange={(selector, resolution) => onDnsChange(dnsDialog.node, selector, resolution)}
          onMove={(selector, delta) => moveDns(dnsDialog.node, selector, delta)}
          errorFor={selector => dnsErrorFor(dnsDialog.node, selector)}
          focus={dnsDialog.focus}
          readOnly={false}
          disabled={false}
          onClose={() => setDnsDialog(null)}
        />
      )}
      {creatingFor !== null && (
        <ExternalOutboundEditor
          tenantId={tenant}
          existing={null}
          existingIds={new Set(outbounds.map(outbound => outbound.id))}
          initialEntryMode="manual"
          deferStage
          onClose={() => setCreatingFor(null)}
          onSaved={outbound => {
            if (!creatingRule) return;
            onOutboundCreated(outbound);
            patch(creatingFor.source, creatingFor.index, { ...creatingRule, a: { t: 'proxy', outbound: outbound.id } });
            setCreatingFor(null);
          }}
        />
      )}
      {muxEditor && (
        <MuxConfigDrawer
          targetName={muxEditor.targetName || nameOf(muxEditor.to)}
          globalValue={globalRelayMux}
          state={muxEditor}
          readOnly={false}
          onChange={next => setMuxEditor({ ...next, source: muxEditor.source })}
          onClose={() => setMuxEditor(null)}
          onApply={() => {
            const pool: HopPool = muxEditor.followGlobal ? { t: 'mux' } : { t: 'mux', v: { ...muxEditor.value } };
            if (muxEditor.listener) setListenerPool(muxEditor.source, muxEditor.listener, pool);
            else setPool(muxEditor.source, muxEditor.to, pool);
            setMuxEditor(null);
          }}
        />
      )}
    </>
  );
}

function AppendHopPicker({
  tailName,
  peers,
  onPick,
}: {
  tailName: string;
  peers: Pick<ForwardPeer, 'id' | 'name'>[];
  onPick: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const anchor = useRef<HTMLButtonElement>(null);
  const id = useId();
  const search = query.trim().toLowerCase();
  const visible = peers.filter(peer => `${peer.name} ${peer.id}`.toLowerCase().includes(search));
  return (
    <div className="wzr-addhop">
      <button
        ref={anchor}
        type="button"
        className="btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-describedby={`${id}-hint`}
        disabled={peers.length === 0}
        onClick={() => {
          setQuery('');
          setOpen(true);
        }}
      >
        ＋ 添加下一跳
      </button>
      <span className="wzr-addhop-hint" id={`${id}-hint`}>
        {peers.length ? (
          <>
            接在 <b>{tailName}</b> 之后
          </>
        ) : (
          '没有可添加的机器'
        )}
      </span>
      {open && peers.length > 0 && (
        <MenuLayer anchor={anchor} label="添加下一跳" onClose={() => setOpen(false)}>
          <div
            id={id}
            className="dialog-surface choice-menu wzr-hop-menu"
            role="menu"
            aria-label="添加下一跳"
            onKeyDown={menuArrowKey}
          >
            <div className="choice-menu-section">接在 {tailName} 之后</div>
            <input
              autoFocus
              className="f external-target-search"
              aria-label="搜索可添加的机器"
              placeholder="搜索机器名称或 ID"
              value={query}
              onChange={event => setQuery(event.target.value)}
            />
            {visible.map(peer => (
              <button
                key={peer.id}
                type="button"
                role="menuitem"
                onClick={() => {
                  setOpen(false);
                  onPick(peer.id);
                }}
              >
                <span className="wzr-hop-copy">
                  <b>{peer.name || peer.id}</b>
                  <small>{peer.id}</small>
                </span>
                <span aria-hidden="true">＋</span>
              </button>
            ))}
            {visible.length === 0 && <span className="external-target-empty">没有匹配的机器</span>}
          </div>
        </MenuLayer>
      )}
    </div>
  );
}
