// Mockup: 解锁面板整体设计，重点是「回退」卡。Synthetic data only; never imported by the app.
// The real App runs on a stubbed Console. On the chain detail page each option portals its own head button and
// 「区域性服务解锁状态」 dialog in place of production's; on the machine page it replaces the 解锁 tab panel.
// 现状 is production (the landed A). Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/unlock-panel-mockup.html?option=c
// Query: option=a|b|c|d|current, scene=ai|youtube|stop|paths, page=chain|machine, family=v4|v6,
//        open=0 keeps the dialog closed, role=system-admin|readonly|public, theme=dark|light (read by the HTML before
//        modules load), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  Hysteria2Settings,
  NodeUnlockProbe,
  UnlockIpFamily,
  Whoami,
} from '../../src/api';
import type { ModelOp } from '../../src/draft';
import { App } from '../../src/app';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import { APPS, MACHINES, TENANT, chainSnapshot, e2eProbes, previewSnapshot } from './list-head-mockup-data';
import { agentState, linkHealth, nicView, pingLatest, reports, trafficItem } from './node-list-mockup-data';
import { settings } from './unlock-mockup-data';
import {
  SCENES,
  SCENE_CHAIN,
  SCENE_NODE,
  chainCapabilities,
  chainJob,
  nodeJob,
  nowSecs,
  type SceneKey,
} from './unlock-panel-mockup-data';
import { PanelDialog, PanelTab, type Option } from './unlock-panel-mockup-ui';
import '../../src/styles.css';
import '../../src/panes/node-unlock.css';
import './unlock-panel-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
type Page = 'chain' | 'machine';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 状态行',
    note: '磁贴布局不变。结论统一放到底部状态行，符号与文字在一起，右上角只留 YouTube 分项。回退卡的卡面、旗帜和地区与其他卡相同，状态行写「回退 · 地区不支持」，用中性的折返符号；点状态行看解析路径。',
  },
  {
    value: 'b',
    name: 'B 底部摘要',
    note: '磁贴同 A，IPv4 / IPv6 两个摘要切换放在底部，各自显示可用数和计数条，可直接对比两族。',
  },
  {
    value: 'c',
    name: 'C 分类卡',
    note: '一个分类一张卡，服务之间以细线分隔。顶部显示当前族统计，IPv4 / IPv6 切换放在底部；手机端的 AI 单项压缩为两行。',
  },
  {
    value: 'd',
    name: 'D 紧凑条目',
    note: '每项服务两行，地区靠右。顶部显示当前族统计，IPv4 / IPv6 切换放在底部，状态行同 A。',
  },
  { value: 'current', name: '现状', note: '生产版本（方案 A 落地后）。' },
];
const COMMON_NOTE =
  '共同：地址族切换都在底部；回退读数直接显示，卡面与旗帜保持完整；回退及停止连接项不计入统计，并注明数量；点状态行查看每条解析路径的原因、回答来源和实际连接。';

/* ══ 合成后端 ══ */

const scene = pick<SceneKey>(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'ai');
const page = pick<Page>(params.get('page'), ['chain', 'machine'], 'chain');
const role = pick(params.get('role'), ['system-admin', 'readonly', 'public'] as const, 'system-admin');
const who: Whoami =
  role === 'public'
    ? { operator_id: 'public', role: 'readonly', tenant_scope: null, token_prefix: null, masked_assets: true }
    : {
        operator_id: 'operator',
        role: role as AdminRole,
        tenant_scope: null,
        token_prefix: null,
        masked_assets: role === 'readonly',
      };
const editable = role === 'system-admin';
const session: Session = { who, initial: { node_count: MACHINES.length, chain_group_count: [] } };
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');
const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));

const sceneApp = APPS.find(app => app.chains.some(chain => chain.id === SCENE_CHAIN))!;
const sceneChainName = sceneApp.chains.find(chain => chain.id === SCENE_CHAIN)!.name;
const sceneNodeName = MACHINES.find(machine => machine.id === SCENE_NODE)!.name;
// Open the chain on IPv6 so the AI fallback is visible; either report can be selected explicitly.
const initialFamily: UnlockIpFamily =
  params.get('family') === 'v4' ? 'v4' : params.get('family') === 'v6' ? 'v6' : page === 'machine' ? 'v4' : 'v6';

/** The list mockup's Hysteria 2 wires predate bandwidth and masquerade; 链详情 reads both. */
function withDetailFields(view: ConsoleSnapshot): ConsoleSnapshot {
  for (const app of view.snapshot.apps)
    for (const ingress of app.ingresses) {
      const hy2 = ingress.wires.hysteria2 as Partial<Hysteria2Settings> | null | undefined;
      if (hy2)
        Object.assign(hy2, { bandwidth: hy2.bandwidth ?? {}, masquerade: hy2.masquerade ?? { kind: 'not-found' } });
    }
  return view;
}
const COMPILE_VIEW = {
  revision: 41,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: null,
  apps: null,
  redacted: false,
};

const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, refetchOnReconnect: false, retry: false },
  },
});

/** Jobs as the Console holds them, created on first read so their times are relative to page load. */
const jobs = new Map<string, NodeUnlockProbe | null>();
const jobOf = (owner: string, family: UnlockIpFamily) => {
  const key = `${owner}/${family}`;
  if (!jobs.has(key))
    jobs.set(
      key,
      owner === `node:${SCENE_NODE}` ? nodeJob(family, nowSecs()) : chainJob(owner, family, scene, nowSecs()),
    );
  return jobs.get(key) ?? null;
};

/** 「重新检测」：排队 2.5 秒后开始，再过 7 秒完成，上一次的结果挪到 previous，与 Console 相同。 */
function startJob(owner: string, family: UnlockIpFamily, refreshKey: unknown[]): NodeUnlockProbe {
  const last = jobOf(owner, family);
  const now = nowSecs();
  const shown = last?.results.length && !last.path_error ? last.results : (last?.previous_results ?? []);
  const queued = {
    ...(last ?? { node_id: SCENE_NODE, protocol: 'VLESS REALITY', family }),
    id: `mock-${owner}-${family}-${now}`,
    state: 'queued',
    trigger: 'manual',
    created_at: now,
    completed_at: null,
    next_run_at: now + 86_400,
    results: [],
    path_error: null,
    configuration_revision: 41,
    previous_results: shown,
    previous_completed_at: last?.completed_at ?? null,
  } as NodeUnlockProbe;
  const key = `${owner}/${family}`;
  jobs.set(key, queued);
  const refresh = () => void client.invalidateQueries({ queryKey: refreshKey });
  setTimeout(() => {
    if (jobs.get(key)?.id !== queued.id) return;
    jobs.set(key, { ...queued, state: 'running' });
    refresh();
  }, 2_500);
  setTimeout(() => {
    if (jobs.get(key)?.id !== queued.id) return;
    const done = nowSecs();
    jobs.set(key, { ...queued, state: 'complete', completed_at: done, next_run_at: done + 86_400, results: shown });
    refresh();
  }, 9_500);
  return queued;
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input instanceof Request ? input.url : input), location.origin);
  const path = url.pathname;
  const method = (init?.method ?? 'GET').toUpperCase();
  const now = nowSecs();
  const json = (body: unknown) => Response.json(body);
  const family: UnlockIpFamily = url.searchParams.get('family') === 'v6' ? 'v6' : 'v4';

  const chainUnlock = path.match(/^\/apps\/([^/]+)\/chains\/([^/]+)\/unlock-probe(\/capabilities)?$/);
  if (chainUnlock) {
    const [, app, chain, capabilities] = chainUnlock.map(part => (part ? decodeURIComponent(part) : part));
    await new Promise(resolve => setTimeout(resolve, method === 'POST' ? 280 : 40));
    if (capabilities) return json(chainCapabilities(chain));
    return json(method === 'POST' ? startJob(chain, family, ['chain-unlock-probe', app, chain]) : jobOf(chain, family));
  }
  const nodeUnlock = path.match(/^\/nodes\/([^/]+)\/unlock-probe(\/capabilities)?$/);
  if (nodeUnlock) {
    const [, node, capabilities] = nodeUnlock.map(part => (part ? decodeURIComponent(part) : part));
    await new Promise(resolve => setTimeout(resolve, method === 'POST' ? 280 : 40));
    if (capabilities)
      return json({ family_mode: 'split', ipv6_available: true, ipv6_reason: null, detection_mode: 'scheduled' });
    if (node !== SCENE_NODE) return json(null);
    return json(
      method === 'POST' ? startJob(`node:${node}`, family, ['node-unlock-probe', node]) : jobOf(`node:${node}`, family),
    );
  }
  if (path === '/chains/unlock-probes') {
    const all = APPS.flatMap(app =>
      app.chains.map(chain => ({
        app_id: app.id,
        chain_id: chain.id,
        capabilities: chainCapabilities(chain.id),
        probes: { v4: jobOf(chain.id, 'v4'), v6: jobOf(chain.id, 'v6'), auto: null },
      })),
    ).sort((a, b) => a.chain_id.localeCompare(b.chain_id));
    return json({ chains: all, next_cursor: null });
  }

  if (path === '/branding') return json({ site_name: 'Brocade', icon_data_url: null });
  if (path === '/auth/state') return json({ initialized: true, public_open: role === 'public' });
  if (path === '/bootstrap') return json(session);
  if (path === '/whoami') return json(who);
  if (path === '/revisions')
    return json({
      current_revision: 41,
      revisions: [41, 40, 39].map(id => ({
        id,
        created_at: '2026-10-23T10:00:00Z',
        author: 'operator',
        note: null,
        status: 'committed',
        current: id === 41,
        has_snapshot: true,
      })),
    });
  if (path.startsWith('/compile/')) return json(COMPILE_VIEW);
  if (path === '/model/preview' && method === 'POST') {
    const { ops } = JSON.parse(String(init?.body ?? '{}')) as { ops?: ModelOp[] };
    return json({
      snapshot: withDetailFields(previewSnapshot(chainSnapshot(who.masked_assets), ops ?? [])),
      compile: COMPILE_VIEW,
      artifacts: { revision: 41, artifacts: [] },
    });
  }
  if (path === '/deployments/verify' && method === 'POST')
    return json({
      revision_id: 41,
      converged: true,
      summary: {
        total_targets: 24,
        changed_targets: 0,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 0,
      },
      targets: [],
    });
  if (path === '/deployments') return json({ deployments: [] });
  if (path === '/grants/automation')
    return json({
      pending_jobs: 0,
      retrying_jobs: 0,
      failed_jobs: 0,
      max_attempts: 5,
      latest_revision_id: 41,
      oldest_pending_at: null,
      last_attempt_at: null,
      last_error: null,
    });
  if (path === '/notifications')
    return json({
      retention_days: 30,
      latest_event_id: 0,
      last_seen_event_id: 0,
      cleared_through_event_id: 0,
      unread_count: 0,
      active: [],
      events: [],
    });
  if (path === '/model/snapshot') return json(withDetailFields(chainSnapshot(who.masked_assets)));
  if (path === '/settings') return json(settings);
  if (path === '/tenants') return json({ tenants: [{ id: TENANT, name: '平台' }] });
  if (path === '/certs') return json({ groups: [], nodes: [] });
  if (path === '/nodes/agent-state')
    return json({
      nodes: MACHINES.map(machine => {
        const state = agentState(machine, now);
        return {
          ...state,
          // 机器解锁检测需要协议 v50 起的 Agent；示例里只给场景机器升级。
          ...(machine.id === SCENE_NODE ? { agent_protocol_version: 57 } : {}),
          public_ipv4: masked(state.public_ipv4),
          public_ipv6: masked(state.public_ipv6),
        };
      }),
    });
  if (path === '/node-traffic') return json({ nodes: MACHINES.map(machine => trafficItem(machine, now)) });
  if (path === '/load/nodes/nic')
    return json({
      nodes: MACHINES.filter(reports).map(machine => nicView(machine, now, numberParam(url, 'windows') || 24)),
    });
  if (path === '/ping-probe/nodes/latest')
    return json({ interval_secs: 10, nodes: MACHINES.flatMap(machine => pingLatest(machine, now) ?? []) });
  if (path === '/links/health') return json({ hops: linkHealth(MACHINES, now) });
  if (path === '/links/mtu') return json({ links: [], nodes: [], default_mtu: 1420 });
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });
  if (path === '/probes/e2e') return json(e2eProbes(now));
  if (path === '/users') return json({ users: [] });
  if (path === '/grant-probes/capability') return json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

/** The realtime channels stay silent; every reading falls back to the polled queries. */
class SilentEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  readonly readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  constructor(readonly url: string) {
    super();
  }
  close() {}
}
Object.defineProperty(window, 'EventSource', { value: SilentEventSource, configurable: true });

/* ══ 插槽：在生产页面里给方案部件留位置 ══ */

type Spot = { parent: Element; before: Node | null };

/**
 * Keeps one element at the spot `locate` names while `enabled`, putting it back whenever the production pane
 * re-renders around it, and returns it as a portal target.
 */
function useSlot(enabled: boolean, className: string, locate: (slot: HTMLElement) => Spot | null) {
  const [slot] = useState(() => {
    const element = document.createElement('div');
    element.className = className;
    return element;
  });
  const [placed, setPlaced] = useState(false);
  const locateRef = useRef(locate);
  useLayoutEffect(() => {
    locateRef.current = locate;
  });
  useEffect(() => {
    if (!enabled) return;
    const place = () => {
      const spot = locateRef.current(slot);
      if (!spot) {
        slot.remove();
        setPlaced(false);
        return;
      }
      if (slot.parentNode !== spot.parent || (spot.before !== null && slot.nextSibling !== spot.before))
        spot.parent.insertBefore(slot, spot.before);
      setPlaced(true);
    };
    place();
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      slot.remove();
      setPlaced(false);
    };
  }, [enabled, slot]);
  return enabled && placed ? slot : null;
}

/** Clicks `selector` once it exists; production controls are driven the way a person would. */
function useClickOnce(enabled: boolean, selector: () => HTMLElement | null, then?: () => void) {
  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(() => {
      const target = selector();
      if (!target) return;
      clearInterval(timer);
      target.click();
      then?.();
    }, 120);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled]);
}

const familyButton = (root: ParentNode | null, family: UnlockIpFamily) =>
  [...(root?.querySelectorAll<HTMLButtonElement>('.unlock-footer .segsw button') ?? [])].find(
    button => button.textContent === (family === 'v4' ? 'IPv4' : 'IPv6'),
  ) ?? null;

function Overlay({ option }: { option: Choice }) {
  const own = option !== 'current';
  // 机器页：先打开「解锁」页签，方案与现状都落在这里。
  useClickOnce(
    page === 'machine',
    () =>
      [...document.querySelectorAll<HTMLButtonElement>('#root [role="tab"]')].find(tab =>
        tab.textContent?.includes('解锁'),
      ) ?? null,
  );
  // 链详情页头：放在生产「解锁」按钮之前；生产按钮由 CSS 隐去。
  const head = useSlot(own && page === 'chain', 'mk-head-slot', slot => {
    const tools = document.querySelector('#root .chain-detail-head .nd-tools');
    // 插槽本身插入后也是子节点：以第一个非插槽节点为锚，否则每次检查都会再插一次。
    return tools ? { parent: tools, before: [...tools.childNodes].find(node => node !== slot) ?? null } : null;
  });
  // 机器「解锁」页签：放在生产面板之前；生产面板由 CSS 隐去。
  const tab = useSlot(own && page === 'machine', 'mk-tab-slot', () => {
    const panel = document.querySelector('#root .nd-tab-panel > .node-unlock:not(.pm-tab)');
    return panel?.parentElement ? { parent: panel.parentElement, before: panel } : null;
  });
  // 现状：打开生产弹窗，切到与方案相同的地址族。
  useClickOnce(
    !own && page === 'chain' && params.get('open') !== '0',
    () => document.querySelector<HTMLButtonElement>('#root .chain-detail-head .nd-tools > .unlock-head-btn'),
    () => {
      const timer = setInterval(() => {
        const button = familyButton(document.querySelector('.chain-unlock-dialog'), initialFamily);
        if (!button) return;
        clearInterval(timer);
        button.click();
      }, 120);
    },
  );
  useClickOnce(!own && page === 'machine' && initialFamily === 'v6', () =>
    familyButton(document.querySelector('#root .node-unlock'), 'v6'),
  );
  // 切到方案时关掉现状打开的生产弹窗。
  useEffect(() => {
    if (own)
      document
        .querySelector<HTMLButtonElement>('.chain-unlock-dialog:not([data-pm-option]) .sub-dialog-close')
        ?.click();
  }, [own]);
  return (
    <>
      {head &&
        own &&
        createPortal(
          <PanelDialog
            key={`${option}/${scene}`}
            option={option}
            surface={{ kind: 'chain', app: sceneApp.id, chain: SCENE_CHAIN, name: sceneChainName }}
            editable={editable}
            initialOpen={params.get('open') !== '0'}
            initialFamily={initialFamily}
          />,
          head,
        )}
      {tab &&
        own &&
        createPortal(
          <PanelTab
            key={`${option}/${scene}`}
            option={option}
            surface={{ kind: 'node', node: SCENE_NODE, name: sceneNodeName }}
            editable={editable}
            initialFamily={initialFamily}
          />,
          tab,
        )}
    </>
  );
}

/* ══ 审阅外壳 ══ */

function ReviewBar({ option, onOption }: { option: Choice; onOption: (next: Choice) => void }) {
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    next.set(name, value);
    next.delete('family');
    location.search = next.toString();
  };
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          解锁面板 · 整体设计<small>MOCKUP</small>
        </span>
        <div className="mk-controls">
          <label className="mk-ctl">
            <span className="mk-ctl-label">方案</span>
            <SegmentedControl
              value={option}
              options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
              ariaLabel="方案"
              onChange={onOption}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">页面</span>
            <SegmentedControl
              value={page}
              options={[
                { value: 'chain' as Page, label: '链路弹窗' },
                { value: 'machine' as Page, label: '机器页签' },
              ]}
              ariaLabel="页面"
              onChange={value => reload('page', value)}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">场景</span>
            <select value={scene} onChange={event => reload('scene', event.target.value)}>
              {(Object.keys(SCENES) as SceneKey[]).map(key => (
                <option key={key} value={key}>
                  {SCENES[key].name}
                </option>
              ))}
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">主题</span>
            <select
              value={document.documentElement.dataset.theme}
              onChange={event => reload('theme', event.target.value)}
            >
              <option value="dark">深</option>
              <option value="light">浅</option>
            </select>
          </label>
        </div>
      </div>
      <p className="mk-scene-note">
        <b>{entry.name}</b>
        {entry.note}
        {option !== 'current' && <span className="mk-common">{COMMON_NOTE}</span>}
      </p>
    </div>
  );
}

function Root({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={session}>{children}</SessionProvider>
    </QueryClientProvider>
  );
}

function Mockup() {
  const [option, setOption] = useState<Choice>(
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'c'),
  );
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    history.replaceState(history.state, '', `?${next}${location.hash}`);
    document.documentElement.dataset.mkOption = option;
  }, [option]);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkNav = nav ? 'true' : 'false';
  }, [nav]);
  return (
    <>
      {nav && <ReviewBar option={option} onOption={setOption} />}
      <Overlay key={option} option={option} />
    </>
  );
}

history.replaceState(
  null,
  '',
  `${location.search}${page === 'machine' ? `#/nodes/node/${SCENE_NODE}` : `#/chains/chain/${sceneApp.id}/${SCENE_CHAIN}`}`,
);
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
createRoot(document.getElementById('mk-root')!).render(
  <Root>
    <Mockup />
  </Root>,
);
