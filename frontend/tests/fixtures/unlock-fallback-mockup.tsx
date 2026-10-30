// Mockup: 解锁报告里体现出口 DNS 策略的回退。Synthetic data only; never imported by the app.
// The real App runs on a stubbed Console. On the chain detail page each option portals its own head button and
// 「区域性服务解锁状态」 dialog in place of production's; on the list page it replaces the 解锁 table. 现状 is production.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/unlock-fallback-mockup.html?option=a
// Query: option=a|b|c|d|current, scene=unlock|mixed|same|stop|plain, page=detail|list, family=v4|v6|real (the lens the
//        dialog opens on), open=0 starts with the dialog closed, role=system-admin|readonly|public,
//        theme=dark|light (read by the HTML before modules load), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, ConsoleSnapshot, Hysteria2Settings, UnlockIpFamily, Whoami } from '../../src/api';
import type { ModelOp } from '../../src/draft';
import { App } from '../../src/app';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import { APPS, MACHINES, TENANT, chainSnapshot, e2eProbes, previewSnapshot } from './list-head-mockup-data';
import { agentState, linkHealth, nicView, pingLatest, reports, trafficItem } from './node-list-mockup-data';
import { settings } from './unlock-mockup-data';
import {
  SCENES,
  capabilitiesOf,
  nowSecs,
  unlockJob,
  type FallbackProbe,
  type SceneKey,
} from './unlock-fallback-mockup-data';
import { FallbackChainTable, FallbackUnlock, type Option } from './unlock-fallback-mockup-ui';
import '../../src/styles.css';
import '../../src/panes/node-unlock.css';
import './unlock-fallback-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
type Page = 'detail' | 'list';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 回退标记',
    note: '报告结构不变。常规流量不走这一族的服务保留读数，去掉状态色，名称后标「回退」，计数条上画空心、不计入可用数。点「回退」看命中的出口 DNS 策略。',
  },
  {
    value: 'b',
    name: 'B 让位',
    note: '这一族不是该服务常规路径时，磁贴主读数改为「走 IPv4」，点一下切到 IPv4 报告；回退取得的结论降为底行小字。',
  },
  {
    value: 'c',
    name: 'C 收纳',
    note: '磁贴只放常规流量会走这一族的服务；回退结果收进末尾一行紧凑条目，仍可点开查看。某个分类全部回退时，该分类整行不出现。',
  },
  {
    value: 'd',
    name: 'D 实际',
    note: '地址族切换增加「实际」并设为默认：每项服务按出口 DNS 策略取常规流量使用的地址族。AI 只看 IPv4 并标「仅 IPv4」，两族都会用到的服务两族都看，不一致时金色提示。IPv4 / IPv6 仍是原始报告，回退项按 A 标注；页头计数取实际视图。',
  },
  {
    value: 'current',
    name: '现状',
    note: '生产弹窗，未改动。IPv6 报告把经机器 DNS 回退取得的结果与常规结果同等显示并计数；列表对照表把它们计入双栈比较。',
  },
];
const COMMON_NOTE =
  '共同：回退项不计入可用数；失败处理为「停止连接」时，IPv6 的「已跳过」写明「DNS 策略只解析 IPv4」；列表对照表中回退的地址族不参与双栈比较，格内注「仅 v4」。数据假设 Console 随任务给出每项服务在出口命中的 DNS 策略（dns_paths）。';

/* ══ 合成后端 ══ */

const scene = pick<SceneKey>(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'unlock');
const page = pick<Page>(params.get('page'), ['detail', 'list'], 'detail');
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

const sceneChain = SCENES[scene].chain;
const sceneApp = APPS.find(app => app.chains.some(chain => chain.id === sceneChain))!;
const sceneChainName = sceneApp.chains.find(chain => chain.id === sceneChain)!.name;

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
const jobs = new Map<string, FallbackProbe | null>();
const jobOf = (chain: string, family: UnlockIpFamily) => {
  const key = `${chain}/${family}`;
  if (!jobs.has(key)) jobs.set(key, unlockJob(chain, family, scene, nowSecs()));
  return jobs.get(key) ?? null;
};

/** 「重新检测」：排队 2.5 秒后开始，再过 7 秒完成，上一次的结果挪到 previous，与 Console 相同。 */
function startJob(app: string, chain: string, family: UnlockIpFamily): FallbackProbe {
  const last = jobOf(chain, family);
  const now = nowSecs();
  const shown = last?.results.length && !last.path_error ? last.results : (last?.previous_results ?? []);
  const queued = {
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
  } as FallbackProbe;
  const key = `${chain}/${family}`;
  jobs.set(key, queued);
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ['chain-unlock-probe', app, chain] });
    void client.invalidateQueries({ queryKey: ['chain-unlock-probes'] });
  };
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
    if (capabilities) return json(capabilitiesOf(chain, scene));
    const family: UnlockIpFamily = url.searchParams.get('family') === 'v6' ? 'v6' : 'v4';
    return json(method === 'POST' ? startJob(app, chain, family) : jobOf(chain, family));
  }
  if (path === '/chains/unlock-probes') {
    const all = APPS.flatMap(app =>
      app.chains.map(chain => ({
        app_id: app.id,
        chain_id: chain.id,
        capabilities: capabilitiesOf(chain.id, scene),
        probes: { v4: jobOf(chain.id, 'v4'), v6: jobOf(chain.id, 'v6'), auto: null },
      })),
    ).sort((a, b) => a.chain_id.localeCompare(b.chain_id));
    const after = url.searchParams.get('after');
    const remaining = all.filter(item => !after || item.chain_id > after);
    const chains = remaining.slice(0, 100);
    return json({ chains, next_cursor: remaining.length > 100 ? chains.at(-1)!.chain_id : null });
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

const initialLens = (option: Choice) => {
  const family = params.get('family');
  if (family === 'v4' || family === 'v6') return family;
  return option === 'd' ? ('real' as const) : ('v6' as const);
};

function Overlay({ option }: { option: Choice }) {
  const own = option !== 'current';
  // 链详情页头：放在生产「解锁」按钮之前；生产按钮由 CSS 隐去。
  const head = useSlot(own && page === 'detail', 'mk-head-slot', slot => {
    const tools = document.querySelector('#root .chain-detail-head .nd-tools');
    // 插槽本身插入后也是子节点：以第一个非插槽节点为锚，否则每次检查都会再插一次。
    return tools ? { parent: tools, before: [...tools.childNodes].find(node => node !== slot) ?? null } : null;
  });
  // 线路列表的解锁视图：放在生产对照表之前；生产表由 CSS 隐去。
  const list = useSlot(own && page === 'list', 'mk-list-slot', () => {
    const table = document.querySelector('#root .chain-unlock-scroll:not(.mk-list-slot *)');
    return table?.parentElement ? { parent: table.parentElement, before: table } : null;
  });
  // 切到方案时关掉现状打开的生产弹窗；现状则打开它一次，与方案同样落在弹窗上。
  useEffect(() => {
    if (page !== 'detail') return;
    if (own) {
      document.querySelector<HTMLButtonElement>('.chain-unlock-dialog:not([data-fb-option]) .sub-dialog-close')?.click();
      return;
    }
    if (params.get('open') === '0') return;
    const timer = setInterval(() => {
      const button = document.querySelector<HTMLButtonElement>('#root .chain-detail-head .nd-tools > .unlock-head-btn');
      if (!button) return;
      button.click();
      clearInterval(timer);
    }, 120);
    return () => clearInterval(timer);
  }, [own]);
  return (
    <>
      {head &&
        own &&
        createPortal(
          <FallbackUnlock
            key={`${option}/${scene}`}
            option={option}
            app={sceneApp.id}
            chain={sceneChain}
            name={sceneChainName}
            editable={editable}
            initialOpen={params.get('open') !== '0'}
            initialLens={initialLens(option)}
          />,
          head,
        )}
      {list && own && createPortal(<FallbackChainTable apps={APPS} />, list)}
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
          解锁报告 · DNS 回退<small>MOCKUP</small>
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
                { value: 'detail' as Page, label: '链详情' },
                { value: 'list' as Page, label: '线路列表' },
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
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
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

if (!location.hash || params.has('page') || params.has('scene'))
  history.replaceState(
    null,
    '',
    `${location.search}${page === 'list' ? '#/chains' : `#/chains/chain/${sceneApp.id}/${sceneChain}`}`,
  );
localStorage.setItem('brocade-console:chain-view', 'unlock');
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
