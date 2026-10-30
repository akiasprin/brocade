// New heads for the 线路 / 用户 page header mockup. Never imported by the app.
//
// The head is the machine page's markup verbatim: .nd-sheet > .nd-paper > .nd-page-head with a .cg-plate identity
// (lamp in the corner), the .nd-tabs beam and .nd-tools. Below it the production pane renders unchanged with its own
// band header hidden; the new head drives the pane through those hidden controls (多选, 新建分组, 新增用户, the roster
// filter), so selection, creation and filtering stay the production behaviour.
import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { useQuery } from '@tanstack/react-query';
import {
  chainMembers,
  fetchE2eProbes,
  fetchNodes,
  fetchSnapshot,
  type E2eProbeItem,
  type SnapshotIngress,
} from '../../src/api';
import { ChainsPane } from '../../src/panes/chains';
import { UsersPane } from '../../src/panes/users';
import { isVisitor, useSession } from '../../src/session';
import { Empty } from '../../src/ui/bits';
import { Icon, type IconName } from '../../src/ui/icons';
import { byChain, headline, toneOf } from '../../src/ui/probe';
import type { Win } from '../../src/wm/store';

export type UsersLayout = 'span' | 'roster';

type Tone = 'ok' | 'warn' | 'bad' | 'idle';
type Lamp = { tone: Tone; why: string };

/* ══ 页头部件：与机器页相同的结构 ══ */

function PageIdentity({ icon, title, lamp }: { icon: IconName; title: string; lamp: Lamp | null }) {
  return (
    <div className="nd-page-identity">
      <span className="cg-plate">
        <Icon of={icon} size={18} />
        {lamp && (
          <span
            className={`cg-lamp ${lamp.tone === 'bad' ? 'err' : lamp.tone}`}
            role="img"
            aria-label={lamp.why}
            title={lamp.why}
          />
        )}
      </span>
      <div className="nd-ident-text">
        <div className="nd-ident-row">
          <h1 className="nd-id nd-name">{title}</h1>
        </div>
      </div>
    </div>
  );
}

type TabIcon = IconName | 'flask' | 'triangle';

interface TabSpec<K extends string> {
  key: K;
  label: string;
  icon: TabIcon;
  badge?: { tone: 'gold' | 'dim'; count: number };
}

/** 「测试」页签的烧瓶。图标集里还没有，落地时补进 ui/icons.tsx；笔画参数与 Icon 相同。 */
function FlaskIcon() {
  return (
    <span className="ui-icon nd-tab-ic" aria-hidden="true">
      <svg
        width={14}
        height={14}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M6 2 H10 M6.8 2 V6.3 L3.2 12.4 C2.8 13.2 3.3 14 4.2 14 H11.8 C12.7 14 13.2 13.2 12.8 12.4 L9.2 6.3 V2" />
        <path d="M4.7 10.4 H11.3" />
      </svg>
    </span>
  );
}

/** 「降级 / 需处理」页签的实心三角。机器页恢复旧标题栏后图标集里不再有它，这里保留一份。 */
function TriangleIcon() {
  return (
    <span className="ui-icon nd-tab-ic" aria-hidden="true">
      <svg width={14} height={14} viewBox="0 0 16 16">
        <path d="M8 2.4 L14.6 13.6 H1.4 Z" fill="currentColor" />
      </svg>
    </span>
  );
}

function PageTabs<K extends string>({
  label,
  tabs,
  active,
  onChange,
}: {
  label: string;
  tabs: readonly TabSpec<K>[];
  active: K;
  onChange: (next: K) => void;
}) {
  const base = useId();
  const move = (event: ReactKeyboardEvent<HTMLButtonElement>, current: K) => {
    const index = tabs.findIndex(tab => tab.key === current);
    let next: number | null = null;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % tabs.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + tabs.length) % tabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = tabs.length - 1;
    if (next === null) return;
    event.preventDefault();
    const key = tabs[next].key;
    onChange(key);
    requestAnimationFrame(() => document.getElementById(`${base}-${key}`)?.focus());
  };
  return (
    <div className="nd-tabs" role="tablist" aria-label={label}>
      <div className="nd-tabs-seg">
        {tabs.map(tab => (
          <button
            key={tab.key}
            id={`${base}-${tab.key}`}
            type="button"
            role="tab"
            aria-selected={active === tab.key}
            tabIndex={active === tab.key ? 0 : -1}
            onKeyDown={event => move(event, tab.key)}
            onClick={() => onChange(tab.key)}
          >
            {tab.icon === 'flask' ? (
              <FlaskIcon />
            ) : tab.icon === 'triangle' ? (
              <TriangleIcon />
            ) : (
              <Icon of={tab.icon} size={14} className="nd-tab-ic" />
            )}
            {tab.label}
            {tab.badge && tab.badge.count > 0 && (
              <span className={`nd-tab-badge ${tab.badge.tone}`}>{tab.badge.count}</span>
            )}
          </button>
        ))}
      </div>
    </div>
  );
}

interface MenuItem {
  icon: IconName;
  label: string;
  note: string;
  disabled?: boolean;
  onSelect: () => void;
}

/** The machine page's ⋯: a fixed menu right-aligned with the trigger, closed by any click or Escape. */
function MoreMenu({ items }: { items: MenuItem[] }) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    const escape = (event: KeyboardEvent) => event.key === 'Escape' && close();
    document.addEventListener('click', close);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', escape);
    };
  }, [open]);
  useLayoutEffect(() => {
    if (!open) return;
    const trigger = triggerRef.current?.getBoundingClientRect();
    const menu = menuRef.current;
    if (!trigger || !menu) return;
    menu.style.top = `${trigger.bottom + 6}px`;
    menu.style.left = `${Math.max(10, Math.min(innerWidth - menu.offsetWidth - 10, trigger.right - menu.offsetWidth))}px`;
  }, [open]);
  return (
    <div className="fg-menuwrap">
      <button
        ref={triggerRef}
        type="button"
        className="btn fg-more"
        aria-label="更多操作"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={event => {
          event.stopPropagation();
          setOpen(value => !value);
        }}
      >
        <Icon of="more" size={14} className="nd-tool-icon" />
      </button>
      {open &&
        createPortal(
          <div
            ref={menuRef}
            className="fg-menu action-menu"
            data-node-action-menu
            role="menu"
            onClick={() => setOpen(false)}
          >
            {items.map(item => (
              <button key={item.label} role="menuitem" disabled={item.disabled} onClick={item.onSelect}>
                <Icon of={item.icon} size={14} className="action-menu-icon" />
                <span>
                  {item.label}
                  <small>{item.note}</small>
                </span>
              </button>
            ))}
          </div>,
          document.body,
        )}
    </div>
  );
}

/* ══ 与生产页面的桥：读取并点击被隐藏的原控件 ══ */

/** Re-reads `read` (a module-level function) from the production subtree whenever it mutates; the value must
 *  be JSON-serialisable. The first reading arrives from the observer callback, not from the effect body. */
function useDomReading<T>(rootRef: RefObject<HTMLElement | null>, read: (root: HTMLElement) => T): T | null {
  const [value, setValue] = useState<T | null>(null);
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    let last = '';
    const update = () => {
      const next = read(root);
      const key = JSON.stringify(next);
      if (key === last) return;
      last = key;
      setValue(next);
    };
    const observer = new MutationObserver(update);
    observer.observe(root, { subtree: true, childList: true, attributes: true, characterData: true });
    const first = requestAnimationFrame(update);
    return () => {
      cancelAnimationFrame(first);
      observer.disconnect();
    };
  }, [rootRef, read]);
  return value;
}

const textOf = (element: Element | null | undefined) => element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

/* ══ 线路 ══ */

type ChainTab = 'all' | 'attention';

/** 生产 `chainAttentionTone` 的同一判定：模型事实优先，其次是端到端探测。 */
function chainTone(
  disabled: boolean,
  ingress: SnapshotIngress | null,
  probe: E2eProbeItem | undefined,
  users: number,
): Tone {
  if (disabled || !ingress) return 'bad';
  const probeTone = toneOf(probe);
  if (probeTone === 'down') return 'bad';
  if (probeTone === 'odd') return 'warn';
  if (probeTone === 'none' || users === 0) return 'idle';
  return 'ok';
}

/** 降级页签里卡片名称下的一行原因；措辞沿用探测横幅的 `headline`。 */
function chainReason(retiredNames: string[], ingress: SnapshotIngress | null, probe: E2eProbeItem | undefined): string {
  if (retiredNames.length > 0) return `含退役机器 ${retiredNames.join('、')}，整链停用`;
  if (!ingress) return '缺接入面，用户无法接入';
  const tone = toneOf(probe);
  if (probe && (tone === 'down' || tone === 'odd')) return headline(probe);
  return '没有要处理的';
}

const CHAIN_LIST_WIN: Win = {
  id: -101,
  key: 'tab:chains',
  title: '线路',
  x: 0,
  y: 0,
  w: 0,
  h: 0,
  z: 0,
  min: false,
  home: 'desk',
  data: { drill: { p: 'list' } },
} as Win;

type ChainHeader =
  | { selecting: false; multiDisabled: boolean; createDisabled: boolean }
  | { selecting: true; deleteLabel: string; deleteDisabled: boolean; deleteTitle: string };

function readChainHeader(root: HTMLElement): ChainHeader | null {
  const buttons = [...root.querySelectorAll<HTMLButtonElement>('.chain-list-panel > header button')];
  if (buttons.length === 0) return null;
  if (buttons.some(button => textOf(button) === '取消')) {
    const remove = buttons.find(button => button.classList.contains('danger'));
    return {
      selecting: true,
      deleteLabel: textOf(remove),
      deleteDisabled: remove?.disabled ?? true,
      deleteTitle: remove?.title ?? '',
    };
  }
  return {
    selecting: false,
    multiDisabled: buttons.find(button => textOf(button) === '多选')?.disabled ?? true,
    createDisabled: buttons.find(button => textOf(button).includes('新建分组'))?.disabled ?? true,
  };
}

function clickChainHeader(root: HTMLElement | null, match: (button: HTMLButtonElement) => boolean) {
  [...(root?.querySelectorAll<HTMLButtonElement>('.chain-list-panel > header button') ?? [])].find(match)?.click();
}

export function MockChainPage() {
  const { who } = useSession();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const probes = useQuery({ queryKey: ['e2e-probes'], queryFn: () => fetchE2eProbes(), refetchInterval: 60_000 });
  const [tab, setTab] = useState<ChainTab>('all');
  const bodyRef = useRef<HTMLDivElement>(null);
  const header = useDomReading(bodyRef, readChainHeader);

  const probeOf = byChain(probes.data?.chains);
  const nodeList = nodes.data?.nodes ?? [];
  const retired = new Set(nodeList.filter(node => node.retired_at).map(node => node.node_id));
  const nameOf = (id: string) => nodeList.find(node => node.node_id === id)?.name || id;
  const rows = (snapshot.data?.snapshot.apps ?? []).flatMap(app =>
    app.chains.map(chain => {
      const ingress = app.ingresses.find(item => item.chain === chain.id) ?? null;
      const ingressIds = new Set(app.ingresses.filter(item => item.chain === chain.id).map(item => item.id));
      const users = new Set(
        app.grants.filter(grant => ingressIds.has(grant.ingress)).map(grant => `${grant.tenant}/${grant.user}`),
      ).size;
      const retiredMembers = chainMembers(app, chain.id).filter(member => retired.has(member));
      const probe = probeOf.get(chain.id);
      return {
        key: `${app.id}/${chain.id}`,
        tone: chainTone(retiredMembers.length > 0, ingress, probe, users),
        why: chainReason(retiredMembers.map(nameOf), ingress, probe),
      };
    }),
  );
  const count = (tone: Tone) => rows.filter(row => row.tone === tone).length;
  const bad = count('bad');
  const warn = count('warn');
  const idle = count('idle');
  const attention = bad + warn;
  const lamp: Lamp | null =
    rows.length === 0
      ? null
      : {
          tone: bad > 0 ? 'bad' : warn > 0 ? 'warn' : idle === rows.length ? 'idle' : 'ok',
          why:
            [bad > 0 && `${bad} 条不可用`, warn > 0 && `${warn} 条有警告`, idle > 0 && `${idle} 条未探测或无人使用`]
              .filter(Boolean)
              .join(' · ') || '全部连通',
        };

  // 原因行插在生产链卡的标题之后；生产组件重渲染时由观察器补回。只在降级页签里显示。
  const reasonKey = JSON.stringify(rows.map(row => [row.key, row.tone, row.why]));
  useEffect(() => {
    const root = bodyRef.current;
    if (!root) return;
    const reasons = new Map(
      (JSON.parse(reasonKey) as [string, Tone, string][]).map(([key, tone, why]) => [key, { tone, why }]),
    );
    const place = () => {
      for (const card of root.querySelectorAll<HTMLElement>('.chain-card[data-route-focus^="chain:"]')) {
        const reason = reasons.get(card.dataset.routeFocus!.slice('chain:'.length));
        const head = card.querySelector(':scope > .chain-card-head');
        if (!reason || !head) continue;
        let line = card.querySelector<HTMLElement>(':scope > .lh-why');
        if (!line) {
          line = document.createElement('p');
          head.after(line);
        }
        const className = `lh-why ${reason.tone}`;
        if (line.className !== className) line.className = className;
        if (line.textContent !== reason.why) {
          line.textContent = reason.why;
          line.title = reason.why;
        }
      }
    };
    place();
    const observer = new MutationObserver(place);
    observer.observe(root, { subtree: true, childList: true });
    return () => observer.disconnect();
  }, [reasonKey]);

  const tabs: TabSpec<ChainTab>[] = [
    { key: 'all', label: '全部', icon: 'chains' },
    { key: 'attention', label: '降级', icon: 'triangle', badge: { tone: 'gold', count: attention } },
  ];
  const changeTab = (next: ChainTab) => {
    if (next === tab) return;
    if (header?.selecting) clickChainHeader(bodyRef.current, button => textOf(button) === '取消');
    setTab(next);
  };

  return (
    <div className="nd-sheet nd-page lh-chain-page" data-tab={tab}>
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head">
          <PageIdentity icon="chains" title="线路" lamp={lamp} />
          {rows.length > 0 && <PageTabs label="链分组" tabs={tabs} active={tab} onChange={changeTab} />}
          {!isVisitor(who) && header && (
            <div className="nd-tools">
              <div className="nd-acts">
                {header.selecting ? (
                  <>
                    <button
                      className="btn"
                      type="button"
                      onClick={() => clickChainHeader(bodyRef.current, button => textOf(button) === '取消')}
                    >
                      取消
                    </button>
                    <button
                      className="btn danger"
                      type="button"
                      disabled={header.deleteDisabled}
                      title={header.deleteTitle}
                      onClick={() => clickChainHeader(bodyRef.current, button => button.classList.contains('danger'))}
                    >
                      {header.deleteLabel}
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      className="btn primary"
                      type="button"
                      disabled={header.createDisabled}
                      onClick={() => clickChainHeader(bodyRef.current, button => textOf(button).includes('新建分组'))}
                    >
                      新建分组
                    </button>
                    {rows.length > 0 && (
                      <MoreMenu
                        items={[
                          {
                            icon: 'check',
                            label: '多选删除',
                            note: '勾选后删除链：接入面和授权一并删除，进草稿',
                            disabled: header.multiDisabled,
                            onSelect: () => clickChainHeader(bodyRef.current, button => textOf(button) === '多选'),
                          },
                        ]}
                      />
                    )}
                  </>
                )}
              </div>
            </div>
          )}
        </header>
        <div className="nd-paper-body" ref={bodyRef}>
          {tab === 'attention' && attention === 0 && <Empty>没有降级的链。</Empty>}
          <ChainsPane win={CHAIN_LIST_WIN} />
        </div>
      </div>
    </div>
  );
}

/* ══ 用户 ══ */

type RosterTab = 'all' | 'attention' | 'test' | 'disabled';

const ROSTER_TABS: { key: RosterTab; label: string; icon: TabIcon }[] = [
  { key: 'all', label: '全部', icon: 'users' },
  { key: 'attention', label: '需处理', icon: 'triangle' },
  { key: 'test', label: '测试', icon: 'flask' },
  { key: 'disabled', label: '已停用', icon: 'dash' },
];

type RosterReading = {
  counts: number[];
  active: number;
  createDisabled: boolean;
  createTitle: string;
  hasList: boolean;
};

function readRoster(root: HTMLElement): RosterReading | null {
  const create = [...root.querySelectorAll<HTMLButtonElement>('.user-roster-head-actions button')][0];
  if (!create) return null;
  const segments = [...root.querySelectorAll<HTMLButtonElement>('.user-roster-filter button')];
  return {
    counts: segments.map(button => Number(textOf(button.querySelector('i')) || 0)),
    active: Math.max(
      0,
      segments.findIndex(button => button.getAttribute('aria-pressed') === 'true'),
    ),
    createDisabled: create.disabled,
    createTitle: create.title,
    hasList: segments.length > 0,
  };
}

/** Keeps a portal target as the first child of the production roster column (名册栏 only). */
function useRosterSlot(rootRef: RefObject<HTMLElement | null>, enabled: boolean) {
  const [slot] = useState(() => {
    const element = document.createElement('div');
    element.className = 'lh-roster-slot';
    return element;
  });
  useEffect(() => {
    const root = rootRef.current;
    if (!enabled || !root) return;
    const place = () => {
      const roster = root.querySelector('.user-split-roster');
      if (!roster || roster.firstChild === slot) return;
      roster.insertBefore(slot, roster.firstChild);
    };
    place();
    const observer = new MutationObserver(place);
    observer.observe(root, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      slot.remove();
    };
  }, [rootRef, enabled, slot]);
  return enabled ? slot : null;
}

export function MockUsersPage({ layout, user }: { layout: UsersLayout; user: string | null }) {
  const { who } = useSession();
  const bodyRef = useRef<HTMLDivElement>(null);
  const roster = useDomReading(bodyRef, readRoster);
  const slot = useRosterSlot(bodyRef, layout === 'roster');
  const win = useMemo(
    () =>
      ({
        id: -102,
        key: 'tab:users',
        title: '用户',
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        z: 0,
        min: false,
        home: 'desk',
        data: { drill: user ? { p: 'user', id: user } : { p: 'list' } },
      }) as Win,
    [user],
  );

  const counts = roster?.counts ?? [];
  const attention = counts[1] ?? 0;
  const active = ROSTER_TABS[roster?.active ?? 0].key;
  const lamp: Lamp | null =
    !roster?.hasList || (counts[0] ?? 0) === 0
      ? null
      : attention > 0
        ? { tone: 'warn', why: `${attention} 人需处理：流量用尽、未生效或已到期` }
        : { tone: 'ok', why: '没有需要处理的用户' };
  const tabs: TabSpec<RosterTab>[] = ROSTER_TABS.map((tab, index) => ({
    ...tab,
    badge:
      tab.key === 'all' ? undefined : { tone: tab.key === 'attention' ? 'gold' : 'dim', count: counts[index] ?? 0 },
  }));
  const choose = (next: RosterTab) => {
    const index = ROSTER_TABS.findIndex(tab => tab.key === next);
    bodyRef.current?.querySelectorAll<HTMLButtonElement>('.user-roster-filter button')[index]?.click();
  };
  const head = (
    <>
      <PageIdentity icon="users" title="用户" lamp={lamp} />
      {roster?.hasList && <PageTabs label="用户分组" tabs={tabs} active={active} onChange={choose} />}
      {!isVisitor(who) && roster && (
        <div className="nd-tools">
          <div className="nd-acts">
            <button
              className="btn primary"
              type="button"
              disabled={roster.createDisabled}
              title={roster.createTitle || undefined}
              onClick={() =>
                bodyRef.current?.querySelector<HTMLButtonElement>('.user-roster-head-actions button')?.click()
              }
            >
              新增用户
            </button>
          </div>
        </div>
      )}
    </>
  );
  const pane = (
    <div ref={bodyRef} className="lh-users-body">
      <UsersPane win={win} bare />
    </div>
  );

  if (layout === 'roster')
    return (
      <div className="lh-users-page lh-users-roster">
        {pane}
        {slot &&
          createPortal(
            <div className="nd-sheet nd-page lh-roster-sheet">
              <div className="nd-paper">
                <header className="nd-page-head">{head}</header>
              </div>
            </div>,
            slot,
          )}
      </div>
    );
  return (
    <PageFrame className="lh-users-page lh-users-span" head={head}>
      {pane}
    </PageFrame>
  );
}

function PageFrame({ className, head, children }: { className: string; head: ReactNode; children: ReactNode }) {
  return (
    <div className={`nd-sheet nd-page ${className}`}>
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head">{head}</header>
        <div className="nd-paper-body">{children}</div>
      </div>
    </div>
  );
}
