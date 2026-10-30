// Mockup: 常用服务的解锁情况 on the 线路 list page. Synthetic data only; never imported by the app.
// The real App and shell run on a stubbed Console. Each option portals its parts into the production list: into every
// chain card (A 标志条, B 常用, C the selected service), into the page title bar (B 常用服务, D view switch) and
// right under it (C service bar, D table). Everything else, including 链详情 and its 解锁 dialog, is production.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/chain-unlock-mockup.html?option=a#/chains
// Query: option=a|b|c|d|current, scene=typical|calm, role=system-admin|readonly|public, theme=dark|light (read by
//        the HTML before modules load), nav=0 hides the review bar.
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
import { MACHINES, TENANT, chainSnapshot, e2eProbes, previewSnapshot } from './list-head-mockup-data';
import { agentState, linkHealth, nicView, pingLatest, reports, trafficItem } from './node-list-mockup-data';
import { settings } from './unlock-mockup-data';
import { SCENES, nowSecs, unlockCapabilities, unlockJob, type SceneKey } from './chain-unlock-mockup-data';
import {
  DEFAULT_PINNED,
  LensBar,
  LensLine,
  PinnedPart,
  PinnedPicker,
  SERVICES,
  StripPart,
  UnlockTable,
  ViewSwitch,
  type DisplayService,
  type Option,
  type View,
} from './chain-unlock-mockup-ui';
import '../../src/styles.css';
import '../../src/panes/node-unlock.css';
import './chain-unlock-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 标志条',
    note: '每张链卡底部一行十三个服务标志，按 AI、流媒体、社区分三段，顺序与解锁磁贴相同。标志下的短条是结论：绿色可用、金色部分可用、红色不可用、灰色检测失败；不可用的标志同时变暗。悬停看结论和地区。',
  },
  {
    value: 'b',
    name: 'B 常用',
    note: '每张链卡列出常用服务及其返回的地区和结论符号，默认 ChatGPT、Claude、Gemini、Netflix、Disney+、YouTube Premium、Spotify、TikTok；标题栏「常用服务」可增减，按浏览器保存。分流到不同出口时（例如 AI 走美国、流媒体走日本）在卡上直接可见。',
  },
  {
    value: 'c',
    name: 'C 筛选',
    note: '链卡上方是服务选择条，一次看一项服务：每张卡在名称下写出该服务的结论和地区，不可用或未取得结果的链减淡；选择条右端统计可用的链数。所选服务按浏览器保存。',
  },
  {
    value: 'd',
    name: 'D 对照表',
    note: '标题栏增加「卡片 / 解锁」视图切换。解锁视图是链 × 服务的对照表：每格是结论符号和服务返回的地区，末列是检测时间或状态；按分组分段，与卡片同序，可切换 IPv4 / IPv6。点一行进入链详情。',
  },
  { value: 'current', name: '现状', note: '生产页面，未改动。链详情标题栏的「解锁」按钮打开完整报告。' },
];
const COMMON_NOTE =
  '共同：列表默认读取 IPv4 结果；检测中、超时或检测客户端失败时显示上一次结果并减淡，上方一行写明状态；线路已变更时结果保持正常亮度，状态以金色标出；与上次不同的服务加金点。点链卡仍进入链详情，「解锁」弹窗不变。';

/* ══ 合成后端 ══ */

const scene = pick<SceneKey>(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'typical');
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
const session: Session = { who, initial: { node_count: MACHINES.length, chain_group_count: [] } };
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');
const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));

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
const jobOf = (chain: string, family: UnlockIpFamily) => {
  const key = `${chain}/${family}`;
  if (!jobs.has(key)) jobs.set(key, unlockJob(chain, family, scene, nowSecs()));
  return jobs.get(key) ?? null;
};

/** 链详情里的「重新检测」：排队 2.5 秒后开始，再过 7 秒完成，上一次的结果挪到 previous，与 Console 相同。 */
function startJob(app: string, chain: string, family: UnlockIpFamily): NodeUnlockProbe {
  const last = jobOf(chain, family);
  const now = nowSecs();
  const shown = last?.results.length && !last.path_error ? last.results : (last?.previous_results ?? []);
  const queued: NodeUnlockProbe = {
    ...(last ?? { node_id: '', protocol: 'VLESS REALITY', app_id: app, chain_id: chain, family }),
    id: `mock-${chain}-${family}-${now}`,
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
  const key = `${chain}/${family}`;
  jobs.set(key, queued);
  const refresh = () => void client.invalidateQueries({ queryKey: ['chain-unlock-probe', app, chain] });
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

  const unlock = path.match(/^\/apps\/([^/]+)\/chains\/([^/]+)\/unlock-probe(\/capabilities)?$/);
  if (unlock) {
    const [, app, chain, capabilities] = unlock.map(part => (part ? decodeURIComponent(part) : part));
    await new Promise(resolve => setTimeout(resolve, method === 'POST' ? 280 : 40));
    if (capabilities) return json(unlockCapabilities(chain));
    const family: UnlockIpFamily = url.searchParams.get('family') === 'v6' ? 'v6' : 'v4';
    return json(method === 'POST' ? startJob(app, chain, family) : jobOf(chain, family));
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
        return { ...state, public_ipv4: masked(state.public_ipv4), public_ipv6: masked(state.public_ipv6) };
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

/* ══ 插槽：在生产线路页里给方案部件留位置 ══ */

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

interface CardSlot {
  element: HTMLElement;
  app: string;
  chain: string;
}

/** One slot inside every chain card; CSS `order` places it, so React's own children never move around it. */
function useCardSlots(enabled: boolean): CardSlot[] {
  const [slots, setSlots] = useState<CardSlot[]>([]);
  useEffect(() => {
    if (!enabled) return;
    const root = document.getElementById('root')!;
    const sync = () => {
      const next: CardSlot[] = [];
      for (const card of root.querySelectorAll<HTMLElement>('.chain-card[data-order-app][data-order-id]')) {
        let element = card.querySelector<HTMLElement>(':scope > .mk-card-slot');
        if (!element) {
          element = document.createElement('div');
          element.className = 'mk-card-slot';
          card.appendChild(element);
        }
        next.push({ element, app: card.dataset.orderApp!, chain: card.dataset.orderId! });
      }
      setSlots(previous =>
        previous.length === next.length &&
        previous.every((slot, index) => slot.element === next[index].element && slot.chain === next[index].chain)
          ? previous
          : next,
      );
    };
    sync();
    const observer = new MutationObserver(sync);
    observer.observe(root, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      for (const element of root.querySelectorAll('.mk-card-slot')) element.remove();
      setSlots([]);
    };
  }, [enabled]);
  return enabled ? slots : [];
}

const listHead = () => document.querySelector<HTMLElement>('#root .chain-list-panel > header');

function Overlay({
  option,
  pinned,
  onPinned,
  lens,
  onLens,
  view,
  onView,
  family,
  onFamily,
}: {
  option: Choice;
  pinned: DisplayService[];
  onPinned: (next: DisplayService[]) => void;
  lens: DisplayService;
  onLens: (next: DisplayService) => void;
  view: View;
  onView: (next: View) => void;
  family: UnlockIpFamily;
  onFamily: (next: UnlockIpFamily) => void;
}) {
  const cards = useCardSlots(option === 'a' || option === 'b' || option === 'c');
  // 标题栏：放在「多选」之前，与生产按钮同一排。
  const head = useSlot(option === 'b' || option === 'd', 'mk-head-slot', () => {
    const header = listHead();
    if (!header) return null;
    const first = [...header.children].find(child => child.tagName === 'BUTTON');
    return { parent: header, before: first ?? null };
  });
  // 标题栏正下方：选择条与对照表。
  const under = useSlot(option === 'c' || option === 'd', 'mk-under-slot', slot => {
    const header = listHead();
    if (!header?.parentElement) return null;
    return {
      parent: header.parentElement,
      before: header.nextSibling === slot ? slot.nextSibling : header.nextSibling,
    };
  });
  return (
    <>
      {cards.map(slot =>
        createPortal(
          option === 'a' ? (
            <StripPart app={slot.app} chain={slot.chain} />
          ) : option === 'b' ? (
            <PinnedPart app={slot.app} chain={slot.chain} pinned={pinned} />
          ) : (
            <LensLine app={slot.app} chain={slot.chain} lens={lens} />
          ),
          slot.element,
          `${slot.app}/${slot.chain}`,
        ),
      )}
      {head &&
        createPortal(
          option === 'b' ? (
            <PinnedPicker pinned={pinned} onChange={onPinned} />
          ) : (
            <ViewSwitch view={view} onView={onView} family={family} onFamily={onFamily} />
          ),
          head,
        )}
      {under &&
        createPortal(
          option === 'c' ? (
            <LensBar lens={lens} onLens={onLens} />
          ) : view === 'table' ? (
            <UnlockTable family={family} />
          ) : null,
          under,
        )}
    </>
  );
}

/* ══ 审阅外壳 ══ */

const STORE = 'mk-chain-unlock:';
function useStored<T>(name: string, fallback: T, valid: (value: unknown) => value is T) {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = JSON.parse(localStorage.getItem(STORE + name) ?? 'null');
      return valid(raw) ? raw : fallback;
    } catch {
      return fallback;
    }
  });
  const update = (next: T) => {
    setValue(next);
    try {
      localStorage.setItem(STORE + name, JSON.stringify(next));
    } catch {
      /* 存不下只影响下次打开的默认值 */
    }
  };
  return [value, update] as const;
}
const isService = (value: unknown): value is DisplayService => SERVICES.includes(value as DisplayService);
const isServices = (value: unknown): value is DisplayService[] => Array.isArray(value) && value.every(isService);

function ReviewBar({ option, onOption }: { option: Choice; onOption: (next: Choice) => void }) {
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    next.set(name, value);
    location.search = next.toString();
  };
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          线路 · 常用服务解锁<small>MOCKUP</small>
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
            <span className="mk-ctl-label">场景</span>
            <select value={scene} onChange={event => reload('scene', event.target.value)}>
              {(Object.keys(SCENES) as SceneKey[]).map(key => (
                <option key={key} value={key}>
                  {SCENES[key]}
                </option>
              ))}
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">身份</span>
            <select value={role} onChange={event => reload('role', event.target.value)}>
              <option value="system-admin">管理员</option>
              <option value="readonly">只读</option>
              <option value="public">访客</option>
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
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
  );
  const [pinned, setPinned] = useStored('pinned', DEFAULT_PINNED, isServices);
  const [lens, setLens] = useStored<DisplayService>('lens', 'chatgpt', isService);
  const [view, setView] = useStored<View>(
    'view',
    'table',
    (value): value is View => value === 'cards' || value === 'table',
  );
  const [family, setFamily] = useState<UnlockIpFamily>('v4');
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    history.replaceState(history.state, '', `?${next}${location.hash}`);
    document.documentElement.dataset.mkOption = option;
  }, [option]);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkView = option === 'd' ? view : 'cards';
  }, [option, view]);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkNav = nav ? 'true' : 'false';
  }, [nav]);
  return (
    <>
      {nav && <ReviewBar option={option} onOption={setOption} />}
      <Overlay
        option={option}
        pinned={pinned}
        onPinned={setPinned}
        lens={lens}
        onLens={setLens}
        view={view}
        onView={setView}
        family={family}
        onFamily={setFamily}
      />
    </>
  );
}

if (!location.hash) history.replaceState(null, '', `${location.search}#/chains`);
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
