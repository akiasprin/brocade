// Shared pieces of the test node mockup: simulated backend with an event log, floating panels, icons and
// formatting. Synthetic data only; never imported by the app.
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { SegmentedControl } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { CopyButton, copyStateIcon } from '../../src/ui/copy-button';
import { Icon } from '../../src/ui/icons';
import {
  clientName,
  fleetById,
  HOUR,
  linkOf,
  minRttMs,
  nameOf,
  NOW,
  pathKm,
  PROTOCOL_LABEL,
  protocolsOf,
  restartMachines,
  TEST_NODE_TTL,
  TEST_SUBSCRIPTION_URL,
  tunnelById,
  type HopState,
  type Protocol,
  type TestNode,
} from './test-node-mockup-data';

/* ══ 图标：生产图标集里没有「测试」「二维码」「全屏」，落地时补进 ui/icons.tsx ══ */

const MOCK_ICONS = {
  flask: (
    <>
      <path d="M6.2 2.4 H9.8" />
      <path d="M6.9 2.4 V6.2 L3.3 12.3 A1.1 1.1 0 0 0 4.3 13.9 H11.7 A1.1 1.1 0 0 0 12.7 12.3 L9.1 6.2 V2.4" />
      <path d="M4.9 10.2 H11.1" />
    </>
  ),
  qr: (
    <>
      <rect x="2.4" y="2.4" width="4.4" height="4.4" rx="0.6" />
      <rect x="9.2" y="2.4" width="4.4" height="4.4" rx="0.6" />
      <rect x="2.4" y="9.2" width="4.4" height="4.4" rx="0.6" />
      <path d="M9.2 9.2 H11 V11 M13.6 9.2 V9.3 M11.4 13.6 H13.6 V11.8 M9.2 13.6 V13.5" />
    </>
  ),
  expand: <path d="M2.5 6 V2.5 H6 M10 2.5 H13.5 V6 M13.5 10 V13.5 H10 M6 13.5 H2.5 V10" />,
  shrink: <path d="M6 2.5 V6 H2.5 M13.5 6 H10 V2.5 M10 13.5 V10 H13.5 M2.5 10 H6 V13.5" />,
};

export function MockIcon({
  of,
  size = 16,
  className,
}: {
  of: keyof typeof MOCK_ICONS;
  size?: number;
  className?: string;
}) {
  return (
    <span className={`ui-icon${className ? ` ${className}` : ''}`} aria-hidden="true">
      <svg
        width={size}
        height={size}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {MOCK_ICONS[of]}
      </svg>
    </span>
  );
}

/* ══ 时钟与格式 ══ */

const STARTED = Date.now();

/** 演示时钟：从 NOW 起按真实时间走。 */
export function useMockNow(): number {
  return NOW + (useNow() - STARTED);
}

const mockTime = () => NOW + (Date.now() - STARTED);

const STAMP = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'Asia/Shanghai',
});

const CLOCK = new Intl.DateTimeFormat('zh-CN', {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
  timeZone: 'Asia/Shanghai',
});

export const stamp = (ms: number) => STAMP.format(ms).replace('/', '-');
export const clockText = (ms: number) => CLOCK.format(ms);

/** 剩余时间读成 时:分:秒；延长后可以超过 24 小时。 */
export function countdown(left: number): string {
  const total = Math.max(0, Math.floor(left / 1000));
  return [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60]
    .map(value => String(value).padStart(2, '0'))
    .join(':');
}

export type Role = '直出' | '入口' | '中转' | '出口';

export function roleOf(index: number, length: number, tunnel: boolean): Role {
  if (length === 1 && !tunnel) return '直出';
  if (index === 0) return '入口';
  if (index === length - 1 && !tunnel) return '出口';
  return '中转';
}

/** 显示用的短编号：去掉 t- 前缀并大写。 */
export const shortId = (id: string) => `#${id.slice(2).toUpperCase()}`;

/* ══ 模拟后端：下发、拨测、删除都按节拍推进，每次变化记一条日志 ══ */

export type EventTone = 'ok' | 'run' | 'bad' | 'warn';

export interface LabEvent {
  id: number;
  at: number;
  /** 相关的测试节点；机器级事件（例如重启 Xray）也挂在触发它的节点上。 */
  node: string | null;
  text: string;
  tone: EventTone | null;
}

type EventDraft = Omit<LabEvent, 'id'>;

/** 日志只留最近这些条：实验台看的是眼前发生的事，历史去发布记录里查。 */
const EVENT_LIMIT = 30;

type SimNode = TestNode & { left?: Record<string, number>; restart?: string[] };

interface Sim {
  nodes: SimNode[];
  events: LabEvent[];
  /** 这次演示里已经重启过、开好测试入口的机器。 */
  opened: ReadonlySet<string>;
  seq: number;
}

const FAIL_REASON = '测试入口 UDP 39990 已被占用';

const pathText = (node: Pick<TestNode, 'path' | 'tunnel'>) =>
  [...node.path, ...(node.tunnel ? [node.tunnel] : [])].map(nameOf).join(' → ');

function probeMs(node: TestNode): number {
  // 示意读数：链路时延下限的两倍多一点，加上 TLS 握手与隧道的固定开销。
  const jitter = node.id.charCodeAt(node.id.length - 1) % 9;
  return Math.round(minRttMs(pathKm(node.path)) * 2.3 + 34 + (node.tunnel ? 58 : 0) + jitter);
}

function initialTicks(node: SimNode, machine: string, state: HopState): number {
  const restart = node.restart?.includes(machine) ?? false;
  // 各机器各自轮询领取，进度并行；等待领取的时长按机器错开，重启 Xray 的那台多花几拍。
  if (state === 'wait') return 2 + ((machine.charCodeAt(0) + machine.length * 3) % 5);
  return restart ? 6 : 2;
}

function advance(node: SimNode, now: number): SimNode | null {
  const left = { ...node.left };
  if (node.state === 'removing') {
    const ticks = (left.remove ?? 3) - 1;
    return ticks <= 0 ? null : { ...node, left: { ...left, remove: ticks } };
  }
  if (node.state === 'deploying') {
    const hops = { ...node.hops };
    for (const machine of node.path) {
      const state = hops[machine];
      if (state === 'ok' || state === 'fail') continue;
      const key = `hop:${machine}`;
      const ticks = (left[key] ?? initialTicks(node, machine, state)) - 1;
      if (ticks > 0) {
        left[key] = ticks;
        continue;
      }
      delete left[key];
      hops[machine] = state === 'wait' ? 'run' : node.failAt === machine ? 'fail' : 'ok';
    }
    const failed = node.path.find(machine => hops[machine] === 'fail');
    const done = node.path.every(machine => hops[machine] === 'ok');
    return {
      ...node,
      hops,
      left,
      state: failed ? 'failed' : done ? 'ready' : 'deploying',
      failure: failed ? { machine: failed, reason: FAIL_REASON } : null,
    };
  }
  if (node.probe?.status === 'running') {
    const ticks = (left.probe ?? 3) - 1;
    if (ticks > 0) return { ...node, left: { ...left, probe: ticks } };
    delete left.probe;
    const tunnel = node.tunnel ? tunnelById.get(node.tunnel) : undefined;
    const exit = fleetById.get(node.path.at(-1) ?? '');
    return {
      ...node,
      left,
      probe: {
        status: 'ok',
        exitIp: tunnel?.exitIp ?? exit?.ipv4 ?? exit?.ipv6 ?? undefined,
        country: tunnel?.country ?? exit?.country,
        ms: probeMs(node),
      },
    };
  }
  if (node.state === 'ready' && now >= node.expiresAt) return { ...node, state: 'removing' };
  return node;
}

const HOP_EVENT: Partial<Record<HopState, { text: string; tone: EventTone }>> = {
  run: { text: '领取，收敛中', tone: 'run' },
  ok: { text: '就绪', tone: 'ok' },
};

function probeText(probe: TestNode['probe']): string {
  return `拨测通过 · ${probe?.country ?? '--'} ${probe?.exitIp ?? ''} · 首字节 ${probe?.ms ?? '--'} ms`;
}

/** 一拍前后的差别写成日志。 */
function changes(before: SimNode, after: SimNode | null, at: number): EventDraft[] {
  const node = before.id;
  if (!after) return [{ at, node, text: '已从各机器撤下', tone: null }];
  const out: EventDraft[] = [];
  for (const machine of before.path) {
    const state = after.hops[machine];
    if (state === before.hops[machine]) continue;
    if (state === 'fail') {
      out.push({ at, node, text: `${nameOf(machine)} 收敛失败：${after.failure?.reason ?? ''}`, tone: 'bad' });
      continue;
    }
    const hop = HOP_EVENT[state];
    if (hop) out.push({ at, node, text: `${nameOf(machine)} ${hop.text}`, tone: hop.tone });
  }
  if (before.state === 'deploying' && after.state === 'ready') out.push({ at, node, text: '可用', tone: 'ok' });
  if (before.state === 'ready' && after.state === 'removing')
    out.push({ at, node, text: '到期，开始撤下', tone: 'warn' });
  if (before.probe?.status === 'running' && after.probe?.status === 'ok')
    out.push({ at, node, text: probeText(after.probe), tone: 'ok' });
  return out;
}

function record(sim: Sim, drafts: EventDraft[]): Sim {
  if (drafts.length === 0) return sim;
  const added = drafts.map((draft, index) => ({ ...draft, id: sim.seq + index + 1 }));
  return {
    ...sim,
    seq: sim.seq + added.length,
    events: [...added.reverse(), ...sim.events].slice(0, EVENT_LIMIT),
  };
}

/** 场景里已有的测试节点补一段过去的日志，打开就有内容可看。 */
function history(nodes: TestNode[]): EventDraft[] {
  const SECOND = 1000;
  const drafts = nodes.flatMap(node => {
    const at = node.createdAt;
    const id = node.id;
    const out: EventDraft[] = [{ at, node: id, text: `创建：${pathText(node)}`, tone: null }];
    node.path.forEach((machine, index) => {
      const state = node.hops[machine];
      const when = at + (3 + index * 2) * SECOND;
      if (state === 'ok') out.push({ at: when, node: id, text: `${nameOf(machine)} 就绪`, tone: 'ok' });
      if (state === 'run') out.push({ at: when, node: id, text: `${nameOf(machine)} 领取，收敛中`, tone: 'run' });
      if (state === 'fail')
        out.push({
          at: when,
          node: id,
          text: `${nameOf(machine)} 收敛失败：${node.failure?.reason ?? ''}`,
          tone: 'bad',
        });
    });
    if (node.state === 'ready') out.push({ at: at + 12 * SECOND, node: id, text: '可用', tone: 'ok' });
    if (node.probe?.status === 'ok')
      out.push({ at: at + 6 * 60 * SECOND, node: id, text: probeText(node.probe), tone: 'ok' });
    return out;
  });
  return drafts.filter(draft => draft.at <= NOW).sort((a, b) => a.at - b.at);
}

export interface TestNodeStore {
  nodes: TestNode[];
  events: LabEvent[];
  testReady: (machine: string) => boolean;
  create: (path: string[], tunnel: string | null) => void;
  remove: (id: string) => void;
  extend: (id: string) => void;
  probe: (id: string) => void;
  retry: (id: string) => void;
}

export function useTestNodes(initial: () => TestNode[], running: boolean): TestNodeStore {
  const [sim, setSim] = useState<Sim>(() => {
    const nodes = initial();
    return record({ nodes, events: [], opened: new Set(), seq: 0 }, history(nodes));
  });

  useEffect(() => {
    if (!running) return;
    const handle = window.setInterval(() => {
      const now = mockTime();
      setSim(current => {
        const drafts: EventDraft[] = [];
        const nodes = current.nodes.flatMap(node => {
          const next = advance(node, now);
          drafts.push(...changes(node, next, now));
          return next ?? [];
        });
        return record({ ...current, nodes }, drafts);
      });
    }, 700);
    return () => window.clearInterval(handle);
  }, [running]);

  /** 改一个节点并记一条日志；节点已不在（例如刚被删掉）时什么也不做。 */
  const act = (
    id: string,
    change: (node: SimNode) => SimNode,
    text: (node: SimNode) => string,
    tone: EventTone | null,
  ) =>
    setSim(current => {
      const target = current.nodes.find(node => node.id === id);
      if (!target) return current;
      const next = change(target);
      return record({ ...current, nodes: current.nodes.map(node => (node.id === id ? next : node)) }, [
        { at: mockTime(), node: id, text: text(next), tone },
      ]);
    });

  return {
    nodes: sim.nodes,
    events: sim.events,
    testReady: machine => (fleetById.get(machine)?.testReady ?? true) || sim.opened.has(machine),
    create: (path, tunnel) => {
      const now = mockTime();
      const id = `t-${Math.floor(Math.random() * 0x10000)
        .toString(16)
        .padStart(4, '0')}`;
      setSim(current => {
        const restart = restartMachines(path)
          .map(machine => machine.id)
          .filter(machine => !current.opened.has(machine));
        const node: SimNode = {
          id,
          path,
          tunnel,
          createdAt: now,
          expiresAt: now + TEST_NODE_TTL,
          state: 'deploying',
          hops: Object.fromEntries(path.map(machine => [machine, 'wait' as const])),
          failure: null,
          probe: null,
          restart,
        };
        return record(
          { ...current, nodes: [node, ...current.nodes], opened: new Set([...current.opened, ...restart]) },
          [
            { at: now, node: node.id, text: `创建：${pathText(node)}`, tone: null },
            ...restart.map(machine => ({
              at: now,
              node: node.id,
              text: `${nameOf(machine)} 重启 Xray，开测试入口`,
              tone: 'warn' as const,
            })),
          ],
        );
      });
    },
    remove: id =>
      act(
        id,
        node => ({ ...node, state: 'removing' }),
        () => '删除，开始撤下',
        null,
      ),
    extend: id =>
      act(
        id,
        node => ({ ...node, expiresAt: Math.min(node.expiresAt + TEST_NODE_TTL, mockTime() + 7 * 24 * HOUR) }),
        node => `延长到 ${stamp(node.expiresAt)}`,
        null,
      ),
    probe: id =>
      act(
        id,
        node => ({ ...node, probe: { status: 'running' } }),
        () => '拨测中',
        'run',
      ),
    retry: id =>
      act(
        id,
        node => ({
          ...node,
          state: 'deploying',
          failure: null,
          failAt: undefined,
          hops: Object.fromEntries(node.path.map(machine => [machine, node.hops[machine] === 'ok' ? 'ok' : 'wait'])),
        }),
        () => '重试下发',
        'run',
      ),
  };
}

/* ══ 浮层定位与收起 ══ */

function useFloating(anchor: HTMLElement | null, floating: RefObject<HTMLElement | null>, align: 'start' | 'end') {
  const [place, setPlace] = useState<{ style: CSSProperties; below: boolean }>({
    style: { visibility: 'hidden' },
    below: true,
  });
  useLayoutEffect(() => {
    const update = () => {
      const node = floating.current;
      if (!anchor || !node) return;
      const rect = anchor.getBoundingClientRect();
      const margin = 10;
      const gap = 6;
      const left = Math.min(
        window.innerWidth - node.offsetWidth - margin,
        Math.max(margin, align === 'start' ? rect.left : rect.right - node.offsetWidth),
      );
      const below = rect.bottom + gap + node.offsetHeight <= window.innerHeight - margin;
      const top = below ? rect.bottom + gap : Math.max(margin, rect.top - gap - node.offsetHeight);
      setPlace({ style: { left, top }, below });
    };
    update();
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);
    return () => {
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
    };
  }, [anchor, floating, align]);
  return place;
}

/** 点浮层和锚点之外的地方、或按 Esc 收起。 */
function useDismiss(floating: RefObject<HTMLElement | null>, anchor: HTMLElement | null, onClose: () => void) {
  const close = useRef(onClose);
  useLayoutEffect(() => {
    close.current = onClose;
  });
  useEffect(() => {
    const down = (event: PointerEvent) => {
      const target = event.target as Node;
      if (floating.current?.contains(target) || anchor?.contains(target)) return;
      close.current();
    };
    const key = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.stopPropagation();
      close.current();
      anchor?.focus();
    };
    document.addEventListener('pointerdown', down, true);
    window.addEventListener('keydown', key, true);
    return () => {
      document.removeEventListener('pointerdown', down, true);
      window.removeEventListener('keydown', key, true);
    };
  }, [floating, anchor]);
}

/* ══ 二维码与更多操作 ══
   浮层挂在调用方给的容器里：实验台进入浏览器全屏后，只有全屏元素内部的内容可见。 */

function QrMatrix({ text }: { text: string }) {
  // 示意图案：真实落地需要二维码编码器（新依赖或内置实现），这里只画出定位块与随机模块。
  const size = 29;
  let seed = 0;
  for (const char of text) seed = (Math.imul(seed, 31) + char.charCodeAt(0)) | 0;
  const random = () => {
    seed = (Math.imul(seed ^ (seed >>> 15), 2246822507) + 3266489909) | 0;
    return ((seed >>> 0) % 1000) / 1000;
  };
  const finder = (row: number, col: number) =>
    [
      [0, 0],
      [0, size - 7],
      [size - 7, 0],
    ].find(([top, left]) => row >= top - 1 && row <= top + 7 && col >= left - 1 && col <= left + 7);
  let d = '';
  for (let row = 0; row < size; row += 1) {
    for (let col = 0; col < size; col += 1) {
      const zone = finder(row, col);
      let dark: boolean;
      if (zone) {
        const r = row - zone[0];
        const c = col - zone[1];
        const inside = r >= 0 && r <= 6 && c >= 0 && c <= 6;
        dark = inside && (r === 0 || r === 6 || c === 0 || c === 6 || (r >= 2 && r <= 4 && c >= 2 && c <= 4));
      } else if (row === 6 || col === 6) dark = (row + col) % 2 === 0;
      else dark = random() > 0.52;
      if (dark) d += `M${col} ${row}h1v1h-1z`;
    }
  }
  return (
    <svg className="tn-qr-code" viewBox={`-2 -2 ${size + 4} ${size + 4}`} role="img" aria-label="分享链接二维码">
      <rect x={-2} y={-2} width={size + 4} height={size + 4} fill="#fff" />
      <path d={d} fill="#111" shapeRendering="crispEdges" />
    </svg>
  );
}

export function QrPopover({
  node,
  anchor,
  container,
  onClose,
}: {
  node: TestNode;
  anchor: HTMLElement;
  container: Element | null;
  onClose: () => void;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const protocols = protocolsOf(node);
  const [protocol, setProtocol] = useState<Protocol>(protocols[0]);
  const { style } = useFloating(anchor, panel, 'end');
  useDismiss(panel, anchor, onClose);
  const uri = linkOf(node, protocol);
  return createPortal(
    <div ref={panel} className="tn-float tn-qr" role="dialog" aria-label={`${clientName(node)} 二维码`} style={style}>
      {protocols.length > 1 && (
        <SegmentedControl
          ariaLabel="协议"
          value={protocol}
          options={protocols.map(item => ({ value: item, label: PROTOCOL_LABEL[item] }))}
          onChange={setProtocol}
        />
      )}
      <QrMatrix text={uri} />
      <span className="tn-qr-name">{clientName(node)}</span>
      <CopyButton className="btn sm tn-qr-copy" text={uri} label="复制链接">
        {(state, label) => (
          <>
            <Icon of={copyStateIcon(state)} size={12} />
            {label}
          </>
        )}
      </CopyButton>
    </div>,
    container ?? document.body,
  );
}

export function RowMenu({
  node,
  anchor,
  container,
  now,
  onExtend,
  onPromote,
  onDelete,
  onClose,
}: {
  node: TestNode;
  anchor: HTMLElement;
  container: Element | null;
  now: number;
  onExtend: () => void;
  onPromote: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  const menu = useRef<HTMLDivElement>(null);
  const { style } = useFloating(anchor, menu, 'end');
  useDismiss(menu, anchor, onClose);
  const extended = Math.min(node.expiresAt + TEST_NODE_TTL, now + 7 * 24 * HOUR);
  return createPortal(
    <div ref={menu} className="fg-menu action-menu tn-menu" role="menu" style={style} onClick={onClose}>
      <button type="button" role="menuitem" disabled={node.state === 'removing'} onClick={onExtend}>
        <Icon of="clock" size={14} className="action-menu-icon" />
        <span>
          延长 24 小时<small>改到 {stamp(extended)} 到期</small>
        </span>
      </button>
      <button type="button" role="menuitem" disabled={node.state !== 'ready'} onClick={onPromote}>
        <Icon of="chains" size={14} className="action-menu-icon" />
        <span>
          转为线路<small>带着这条路径打开建链向导</small>
        </span>
      </button>
      <hr />
      <button type="button" role="menuitem" className="dg" disabled={node.state === 'removing'} onClick={onDelete}>
        <Icon of="trash" size={14} className="action-menu-icon" />
        <span>
          删除<small>立即撤下各机器上的测试配置</small>
        </span>
      </button>
    </div>,
    container ?? document.body,
  );
}

export function SubscriptionCopy() {
  return (
    <CopyButton
      className="btn tn-sub"
      text={TEST_SUBSCRIPTION_URL}
      label="订阅"
      title="复制测试订阅：只含测试节点，到期的自动消失"
      aria-label="复制测试订阅地址"
    >
      {(state, label) => (
        <>
          <Icon of={state === 'idle' ? 'subscription' : copyStateIcon(state)} size={13} />
          {label}
        </>
      )}
    </CopyButton>
  );
}
