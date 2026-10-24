// 线路 · 实验模式：全屏实验台。中间是等距台面——城市是板、机器是方块、测试节点是飞过台面的弧；
// 左边机器架、右边测试节点，底栏是组装条与日志。画法沿用拓扑页（.nd / .plate / .te / .stitch、玻璃浮层），
// 令牌与色板不变。Synthetic data only; never imported by the app.
import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type RefObject,
} from 'react';
import { CopyButton, copyStateIcon } from '../../src/ui/copy-button';
import { Icon } from '../../src/ui/icons';
import { RegionFlag } from '../../src/ui/region-flag';
import {
  CITIES,
  clientName,
  FLEET,
  fleetById,
  hopKm,
  HOUR,
  kmText,
  linkOf,
  loadOf,
  machineBlock,
  minRttMs,
  nameOf,
  pathIssue,
  pathKm,
  PROTOCOL_LABEL,
  protocolsOf,
  restartMachines,
  TEST_NODE_LIMIT,
  TEST_PORTS,
  TUNNEL_BOARD,
  TUNNELS,
  tunnelById,
  type CityCode,
  type FleetMachine,
  type HopState,
  type Scene,
  type TestNode,
  type TestNodeState,
  type Tunnel,
} from './test-node-mockup-data';
import {
  clockText,
  countdown,
  MockIcon,
  QrPopover,
  roleOf,
  RowMenu,
  shortId,
  stamp,
  SubscriptionCopy,
  useMockNow,
  type LabEvent,
  type TestNodeStore,
} from './test-node-mockup-ui';

export type LabPreset = 'none' | 'compose' | 'hover' | 'qr' | 'menu';

interface ComposeValue {
  path: string[];
  tunnel: string | null;
}

type Floating = { kind: 'qr' | 'menu'; id: string; anchor: HTMLElement };

const EMPTY: ComposeValue = { path: [], tunnel: null };
const HOP_LAMP: Record<HopState, string> = { wait: 'defer', run: 'run', ok: 'ok', fail: 'err' };
const HOP_TITLE: Record<HopState, string> = {
  wait: '等待 Agent 领取',
  run: '收敛中',
  ok: '已就绪',
  fail: '收敛失败',
};
const STATE_LAMP: Record<TestNodeState, string> = { ready: 'ok', deploying: 'run', failed: 'err', removing: 'idle' };
const RESTART_TITLE = '还没开测试入口：首次在它上面创建会重启 Xray，现有连接会断开';

function initialCompose(scene: Scene, preset: LabPreset): ComposeValue {
  if (scene.compose) return { path: scene.compose.path, tunnel: scene.compose.tunnel ?? null };
  if (preset === 'compose') return { path: ['hk-hkix', 'tw-hinet', 'us-gia'], tunnel: 'ob-warp' };
  return EMPTY;
}

const withoutMachine = ({ path, tunnel }: ComposeValue, machine: string): ComposeValue => ({
  path: path.filter(id => id !== machine),
  tunnel: path.length === 1 ? null : tunnel,
});

function matches(values: (string | null | undefined)[], query: string): boolean {
  const q = query.trim().toLowerCase();
  return !q || values.some(value => value?.toLowerCase().includes(q));
}

const machineMatches = (machine: FleetMachine, query: string) =>
  matches(
    [machine.name, machine.id, machine.ipv4, machine.ipv6, machine.country, CITIES[machine.city].name, machine.city],
    query,
  );

const tunnelMatches = (tunnel: Tunnel, query: string) =>
  matches([tunnel.name, tunnel.badge, tunnel.country, tunnel.exitIp, '隧道'], query);

interface Tag {
  text: string;
  tone: 'warn' | 'mute';
  title: string;
}

/** 机器身上值得在列表和台面上标出来的限制。 */
function tagsOf(machine: FleetMachine, testReady: (machine: string) => boolean): Tag[] {
  if (!machine.online) return [{ text: '离线', tone: 'mute', title: 'Agent 3 小时没有上报' }];
  const tags: Tag[] = [];
  if (machine.nat) tags.push({ text: 'NAT', tone: 'mute', title: '在 NAT 后，不能作入口' });
  if (!testReady(machine.id)) tags.push({ text: '需重启', tone: 'warn', title: RESTART_TITLE });
  if (machine.egress === false) tags.push({ text: '不出网', tone: 'mute', title: '不允许出网：作出口时要接隧道' });
  return tags;
}

/** 在当前路径里点这台机器会不会被拒绝；已在路径里的总能点（点了是移除）。 */
function blockOf(machine: FleetMachine, path: string[]): string | null {
  if (path.includes(machine.id)) return null;
  return machineBlock(machine, path.length === 0 ? 'entry' : 'later');
}

const degrees = (value: number, positive: string, negative: string) =>
  `${Math.abs(value).toFixed(2)}°${value >= 0 ? positive : negative}`;

function useSize<T extends HTMLElement>(): [RefObject<T | null>, { width: number; height: number }] {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    // ResizeObserver 在 observe 之后会先回调一次，初始尺寸也从这里来。
    const observer = new ResizeObserver(([entry]) =>
      setSize({ width: entry.contentRect.width, height: entry.contentRect.height }),
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  return [ref, size];
}

/* ══ 小图形：机器架和图例里的方块、隧道圆柱、负载曲线 ══ */

function MiniCube({ off = false }: { off?: boolean }) {
  return (
    <svg className={`lab-mini ${off ? 'idle-nd' : 'nd'}`} width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
      <path className="sideL" d="M1,4.5 L7,7.5 L7,12.5 L1,9.5 Z" />
      <path className="sideR" d="M13,4.5 L7,7.5 L7,12.5 L13,9.5 Z" />
      <path className="top" d="M7,1.5 L13,4.5 L7,7.5 L1,4.5 Z" />
    </svg>
  );
}

function MiniCylinder() {
  return (
    <svg className="lab-mini lab-cyl" width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
      <path className="side" d="M2,4 L2,10 A5,2.2 0 0 0 12,10 L12,4 Z" />
      <ellipse className="top" cx="7" cy="4" rx="5" ry="2.2" />
    </svg>
  );
}

function Spark({ values }: { values: number[] }) {
  const width = 44;
  const height = 14;
  const max = Math.max(...values, 1);
  const points = values
    .map(
      (value, index) =>
        `${((index / (values.length - 1)) * width).toFixed(1)},${(height - 1 - (value / max) * (height - 3)).toFixed(1)}`,
    )
    .join(' ');
  return (
    <svg className="lab-spark" width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true">
      <polyline points={points} />
    </svg>
  );
}

/* ══ 台面几何 ══
   与拓扑页同一套等距投影：x = (gx − gy)·cos30°·k，y = (gx + gy)·0.5·k。城市的示意坐标换算成格坐标，
   同城机器沿 gx 排开，所以一座城市是一块沿 x 轴的长板，和拓扑页里一条链一块板的画法一致。 */

const ISO_X = Math.cos(Math.PI / 6);
const ISO_Y = 0.5;
/** 同城相邻两台机器的间距（格）。 */
const STEP = 1.5;
/** 板边比机器多出的余量（格）。 */
const MARGIN = 0.55;

interface Grid {
  gx: number;
  gy: number;
}
interface Point {
  x: number;
  y: number;
}

/** 城市自西向东，台面上画板的顺序。 */
const CITY_ORDER = (Object.keys(CITIES) as CityCode[]).sort((a, b) => CITIES[a].board.x - CITIES[b].board.x);
/** 机器架里城市的顺序：常用的入口地区在前，再按离入口的远近往外排。 */
const RACK_ORDER: CityCode[] = ['HKG', 'MFM', 'TPE', 'TYO', 'SIN', 'LAX', 'FRA', 'LON'];
const machinesOf = (city: CityCode) => FLEET.filter(machine => machine.city === city);

/** 示意坐标换算成等距格坐标；同一块板上的第 index 个成员沿 gx 依次排开。 */
function gridAt({ x, y }: { x: number; y: number }, index: number): Grid {
  const u = x / ISO_X;
  const v = y / ISO_Y;
  return { gx: (u + v) / 2 + index * STEP, gy: (v - u) / 2 };
}

/** 机器与隧道在台面上的格坐标。 */
const GRID = new Map<string, Grid>([
  ...CITY_ORDER.flatMap(city =>
    machinesOf(city).map((machine, index) => [machine.id, gridAt(CITIES[city].board, index)] as const),
  ),
  ...TUNNELS.map((tunnel, index) => [tunnel.id, gridAt(TUNNEL_BOARD, index)] as const),
]);
const gridOf = (id: string): Grid => GRID.get(id) ?? { gx: 0, gy: 0 };

interface Plate {
  key: string;
  name: string;
  meta: string;
  members: string[];
  /** 上、右、下、左四个角；左角最靠西，板名挂在它左边。 */
  corners: Grid[];
}

function plateOf(key: string, name: string, meta: string, members: string[]): Plate {
  const first = gridOf(members[0]);
  const last = gridOf(members[members.length - 1]);
  return {
    key,
    name,
    meta,
    members,
    corners: [
      { gx: first.gx - MARGIN, gy: first.gy - MARGIN },
      { gx: last.gx + MARGIN, gy: last.gy - MARGIN },
      { gx: last.gx + MARGIN, gy: last.gy + MARGIN },
      { gx: first.gx - MARGIN, gy: first.gy + MARGIN },
    ],
  };
}

/** 一座城市一块板；隧道不属于任何城市，单独一块「外部」板。 */
const PLATES: Plate[] = [
  ...CITY_ORDER.map(city =>
    plateOf(
      city,
      CITIES[city].name,
      city,
      machinesOf(city).map(machine => machine.id),
    ),
  ),
  plateOf(
    'EXT',
    '隧道',
    '外部',
    TUNNELS.map(tunnel => tunnel.id),
  ),
];

/** 一个测试节点的各跳；接了隧道时最后一跳从出口机器连到隧道。 */
function hopsOf(path: string[], tunnel: string | null): { from: string; to: string }[] {
  const hops = path.slice(1).map((to, index) => ({ from: path[index], to }));
  return tunnel && path.length > 0 ? [...hops, { from: path[path.length - 1], to: tunnel }] : hops;
}

const EXTENT = (() => {
  const corners = PLATES.flatMap(plate => plate.corners);
  const xs = corners.map(corner => (corner.gx - corner.gy) * ISO_X);
  const ys = corners.map(corner => (corner.gx + corner.gy) * ISO_Y);
  return { x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) };
})();

/** 自后向前画：等距图里靠下的方块要压住靠上的。 */
const CUBE_ORDER = [...FLEET].sort((a, b) => {
  const ga = gridOf(a.id);
  const gb = gridOf(b.id);
  return ga.gx + ga.gy - (gb.gx + gb.gy);
});

interface Board {
  k: number;
  cube: { w: number; h: number };
  at: (grid: Grid) => Point;
  /** 步长太小时只留方块和城市名，读数交给机器架。 */
  names: boolean;
  subs: boolean;
}

function fitBoard(width: number, height: number): Board {
  // 左边留给城市名，右边留给机器名与读数，上边留给弧，下边留给 CLIENT 与图例。
  const pad =
    width < 560 ? { left: 52, right: 40, top: 40, bottom: 52 } : { left: 70, right: 112, top: 54, bottom: 64 };
  const spanX = EXTENT.x1 - EXTENT.x0;
  const spanY = EXTENT.y1 - EXTENT.y0;
  const room = { x: width - pad.left - pad.right, y: height - pad.top - pad.bottom };
  const k = Math.max(16, Math.min(92, room.x / spanX, room.y / spanY));
  const ox = pad.left + (room.x - spanX * k) / 2 - EXTENT.x0 * k;
  const oy = pad.top + (room.y - spanY * k) / 2 - EXTENT.y0 * k;
  const w = Math.round(Math.max(7, Math.min(15, k * 0.22)));
  return {
    k,
    cube: { w, h: Math.round(w * 0.76) },
    at: ({ gx, gy }) => ({ x: ox + (gx - gy) * ISO_X * k, y: oy + (gx + gy) * ISO_Y * k }),
    names: k >= 28,
    subs: k >= 42,
  };
}

/** 一跳画成弧：从顶面起飞、落在顶面，朝弦的上侧弯。同一对端点上的多条弧按车道错开，互不重叠。 */
function arc(a: Point, b: Point, lane: number) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const span = Math.hypot(dx, dy) || 1;
  // 弦的法线取朝上的一侧；竖直的弦向右弯。
  const flip = dx > 0 || (dx === 0 && dy > 0) ? 1 : -1;
  const normal = { x: (flip * dy) / span, y: (-flip * dx) / span };
  const lift = Math.max(4, Math.min(78, 10 + span * 0.17) + lane * 9);
  // 二次贝塞尔的弧顶离弦的距离是控制点的一半。
  const control = { x: (a.x + b.x) / 2 + normal.x * lift * 2, y: (a.y + b.y) / 2 + normal.y * lift * 2 };
  return {
    d: `M${a.x.toFixed(1)},${a.y.toFixed(1)} Q${control.x.toFixed(1)},${control.y.toFixed(1)} ${b.x.toFixed(1)},${b.y.toFixed(1)}`,
    mid: { x: (a.x + 2 * control.x + b.x) / 4, y: (a.y + 2 * control.y + b.y) / 4 },
  };
}

function Cube({ x, y, w, h }: Point & { w: number; h: number }) {
  return (
    <>
      <path className="sideL" d={`M${x - w},${y} L${x},${y + w * 0.5} L${x},${y + w * 0.5 + h} L${x - w},${y + h} Z`} />
      <path className="sideR" d={`M${x + w},${y} L${x},${y + w * 0.5} L${x},${y + w * 0.5 + h} L${x + w},${y + h} Z`} />
      <path className="top" d={`M${x},${y - w * 0.5} L${x + w},${y} L${x},${y + w * 0.5} L${x - w},${y} Z`} />
    </>
  );
}

function Cylinder({ x, y, r }: Point & { r: number }) {
  const ry = r * 0.42;
  const h = r * 0.8;
  return (
    <>
      <path
        className="side"
        d={`M${x - r},${y} L${x - r},${y + h} A${r},${ry} 0 0 0 ${x + r},${y + h} L${x + r},${y} Z`}
      />
      <ellipse className="top" cx={x} cy={y} rx={r} ry={ry} />
    </>
  );
}

// 沿弧移动的光点：可用的测试节点上有流量。两颗、周期固定，与拓扑页一致。
function Pulses({ path }: { path: string }) {
  const duration = 3.2;
  return (
    <>
      {[0, 1].map(index => (
        <circle key={index} className="pulse" r={1.7}>
          <animateMotion dur={`${duration}s`} repeatCount="indefinite" begin={`${(-duration * index) / 2}s`}>
            <mpath href={`#${path}`} />
          </animateMotion>
        </circle>
      ))}
    </>
  );
}

/* ══ 台面 ══ */

interface BoardProps {
  nodes: TestNode[];
  value: ComposeValue;
  testReady: (machine: string) => boolean;
  usage: Map<string, TestNode[]>;
  hover: string | null;
  peek: string | null;
  onHover: (id: string | null) => void;
  onPeek: (id: string | null) => void;
  onToggle: (machine: string) => void;
  onTunnel: (tunnel: string) => void;
}

function BoardView(props: BoardProps) {
  const [ref, size] = useSize<HTMLDivElement>();
  const board = useMemo(() => (size.width > 0 ? fitBoard(size.width, size.height) : null), [size.width, size.height]);
  const peeked = props.peek ? fleetById.get(props.peek) : undefined;
  return (
    <div ref={ref} className="lab-board" role="group" aria-label="台面">
      {board && <BoardSvg {...props} board={board} width={size.width} height={size.height} />}
      <Legend />
      {board && peeked && (
        <MachineCard
          machine={peeked}
          board={board}
          bounds={size}
          testReady={props.testReady(peeked.id)}
          nodes={props.usage.get(peeked.id) ?? []}
        />
      )}
    </div>
  );
}

function BoardSvg({
  nodes,
  value,
  testReady,
  hover,
  peek,
  onHover,
  onPeek,
  onToggle,
  onTunnel,
  board,
  width,
  height,
}: BoardProps & { board: Board; width: number; height: number }) {
  const { cube } = board;
  const at = (id: string) => board.at(gridOf(id));
  const draft = value.path;

  // 车道：同一对端点上较早的测试节点占低车道，新建的往上叠，已有的弧不跳动。
  const lanes = new Map<string, number>();
  const used = new Map<string, number>();
  for (const node of [...nodes].reverse()) {
    hopsOf(node.path, node.tunnel).forEach(({ from, to }, index) => {
      const key = [from, to].sort().join('|');
      const lane = used.get(key) ?? 0;
      used.set(key, lane + 1);
      lanes.set(`${node.id}|${index}`, lane);
    });
  }

  const entries = [...new Set([...nodes.map(node => node.path[0]), ...draft.slice(0, 1)])];
  const focusNode = hover ? nodes.find(node => node.id === hover) : undefined;
  const muted = (node: TestNode) => !!hover && hover !== node.id;

  return (
    <svg className="lab-svg" width={width} height={height} viewBox={`0 0 ${width} ${height}`}>
      <defs>
        {[
          ['lab-arrow', 'var(--ink-4)'],
          ['lab-arrow-hot', 'var(--gold-line)'],
        ].map(([id, color]) => (
          <marker
            key={id}
            id={id}
            viewBox="0 0 8 8"
            refX="7.4"
            refY="4"
            markerWidth="4.6"
            markerHeight="4.6"
            orient="auto-start-reverse"
          >
            <path
              d="M1,1.8 L7,4 L1,6.2"
              fill="none"
              stroke={color}
              strokeWidth="1.1"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </marker>
        ))}
      </defs>

      {PLATES.map(({ key, name, meta, members, corners }) => {
        const points = corners.map(corner => board.at(corner));
        const west = points[3];
        const hot = members.some(id => draft.includes(id) || value.tunnel === id);
        return (
          <g key={key} className="lab-plate">
            <path
              className={`plate${hot ? ' hot' : ''}`}
              d={`M${points.map(point => `${point.x.toFixed(1)},${point.y.toFixed(1)}`).join(' L')} Z`}
            />
            <text className={`plabel${hot ? ' hot' : ''}`} x={west.x - 9} y={west.y + 1} textAnchor="end">
              {name}
            </text>
            {board.names && (
              <text className="pmeta" x={west.x - 9} y={west.y + 14} textAnchor="end">
                {meta}
              </text>
            )}
          </g>
        );
      })}

      {CUBE_ORDER.map(machine => {
        const p = at(machine.id);
        const order = draft.indexOf(machine.id);
        const block = blockOf(machine, draft);
        const onPath = focusNode?.path.includes(machine.id) ?? false;
        const cls = [
          machine.online ? 'nd' : 'idle-nd',
          order >= 0 || onPath ? 'hot' : '',
          peek === machine.id ? 'peek' : '',
          block ? 'blocked' : '',
          focusNode && !onPath ? 'iso-mute' : '',
        ]
          .filter(Boolean)
          .join(' ');
        const toggle = () => {
          if (!block) onToggle(machine.id);
        };
        return (
          <g
            key={machine.id}
            className={cls}
            role="button"
            tabIndex={0}
            aria-label={`${machine.name}${order >= 0 ? `，路径第 ${order + 1} 台` : ''}${block ? `，${block}` : ''}`}
            aria-pressed={order >= 0}
            aria-disabled={!!block}
            onClick={toggle}
            onKeyDown={(event: ReactKeyboardEvent) => {
              if (event.key !== 'Enter' && event.key !== ' ') return;
              event.preventDefault();
              toggle();
            }}
            onPointerEnter={() => onPeek(machine.id)}
            onPointerLeave={() => onPeek(null)}
            onFocus={() => onPeek(machine.id)}
            onBlur={() => onPeek(null)}
          >
            <Cube x={p.x} y={p.y} w={cube.w} h={cube.h} />
          </g>
        );
      })}

      {TUNNELS.map(tunnel => {
        const p = at(tunnel.id);
        const on = value.tunnel === tunnel.id;
        const idle = draft.length === 0;
        const onPath = focusNode?.tunnel === tunnel.id;
        const toggle = () => {
          if (!idle) onTunnel(tunnel.id);
        };
        return (
          <g
            key={tunnel.id}
            className={`lab-cyl lab-tunnel${on || onPath ? ' hot' : ''}${idle ? ' blocked' : ''}${focusNode && !onPath ? ' iso-mute' : ''}`}
            role="button"
            tabIndex={0}
            aria-label={`隧道 ${tunnel.name}${on ? '，已接在路径末尾' : ''}${idle ? '，先选机器' : ''}`}
            aria-pressed={on}
            aria-disabled={idle}
            onClick={toggle}
            onKeyDown={(event: ReactKeyboardEvent) => {
              if (event.key !== 'Enter' && event.key !== ' ') return;
              event.preventDefault();
              toggle();
            }}
          >
            <title>{`${tunnel.name} · 出口 ${tunnel.country} ${tunnel.exitIp}`}</title>
            <Cylinder x={p.x} y={p.y} r={cube.w * 0.85} />
          </g>
        );
      })}

      {entries.map(id => {
        const p = at(id);
        const top = p.y + cube.w * 0.5 + cube.h + 2;
        const bottom = top + 18;
        const lit = draft[0] === id || focusNode?.path[0] === id;
        return (
          <g key={`client-${id}`} className={`lab-client${lit ? ' hot' : ''}`}>
            <path className="stitch" d={`M${p.x},${top} L${p.x},${bottom}`} />
            <circle className="cli-dot" cx={p.x} cy={bottom + 3} r={2.4} />
            {board.names && (
              <text className="cli-t" x={p.x} y={bottom + 16} textAnchor="middle">
                CLIENT
              </text>
            )}
          </g>
        );
      })}

      {nodes.map(node =>
        hopsOf(node.path, node.tunnel).map(({ from, to }, index) => {
          const id = `lab-arc-${node.id}-${index}`;
          const { d, mid } = arc(at(from), at(to), lanes.get(`${node.id}|${index}`) ?? 0);
          const tunnel = tunnelById.get(to);
          // 隧道那一跳只看出口机器有没有收敛好：隧道本身不归 Agent 管。
          const settled = node.hops[from] === 'ok' && (!!tunnel || node.hops[to] === 'ok');
          const failed = node.hops[to] === 'fail';
          const label = tunnel ? tunnel.badge : kmText(hopKm(from, to));
          return (
            <g
              key={id}
              className={`te-wrap${muted(node) ? ' iso-mute' : ''}${node.state === 'removing' ? ' lab-leaving' : ''}`}
            >
              <path
                id={id}
                className={`te lab-te${settled ? '' : ' pending'}${failed ? ' bad' : ''}${hover === node.id ? ' focus' : ''}`}
                d={d}
                markerEnd="url(#lab-arrow)"
              />
              {node.state === 'ready' && <Pulses path={id} />}
              <g className="tag" transform={`translate(${mid.x.toFixed(1)},${mid.y.toFixed(1)})`}>
                <rect x={-label.length * 3 - 7} y={-10} width={label.length * 6 + 14} height={20} rx={4} />
                <text y={4} textAnchor="middle">
                  {label}
                </text>
              </g>
              <path
                className="te-hit"
                d={d}
                onPointerEnter={() => onHover(node.id)}
                onPointerLeave={() => onHover(null)}
              />
            </g>
          );
        }),
      )}

      {hopsOf(draft, value.tunnel).map(({ from, to }) => {
        const { d } = arc(at(from), at(to), -1);
        return <path key={`draft-${to}`} className="lab-draft" d={d} markerEnd="url(#lab-arrow-hot)" />;
      })}

      {TUNNELS.map(tunnel => {
        const p = at(tunnel.id);
        const r = cube.w * 0.85;
        const dim = !!focusNode && focusNode.tunnel !== tunnel.id;
        return (
          <g key={`label-${tunnel.id}`} className={`lab-label${dim ? ' iso-mute' : ''}`} aria-hidden="true">
            {board.names && (
              <text className={`lab-nm${value.tunnel === tunnel.id ? ' hot' : ''}`} x={p.x + r + 6} y={p.y - 2}>
                {tunnel.badge}
              </text>
            )}
            {board.subs && (
              <text className="lab-sub" x={p.x + r + 6} y={p.y + 9}>
                {tunnel.country} · {tunnel.exitIp}
              </text>
            )}
          </g>
        );
      })}

      {FLEET.map(machine => {
        const p = at(machine.id);
        const order = draft.indexOf(machine.id);
        const tags = tagsOf(machine, testReady);
        const dim = !!focusNode && !focusNode.path.includes(machine.id);
        return (
          <g key={`label-${machine.id}`} className={`lab-label${dim ? ' iso-mute' : ''}`} aria-hidden="true">
            {board.names && (
              <text
                className={`lab-nm${order >= 0 ? ' hot' : ''}${machine.online ? '' : ' off'}`}
                x={p.x + cube.w + 6}
                y={p.y - 2}
              >
                {machine.short}
              </text>
            )}
            {board.subs && (
              <text className="lab-sub" x={p.x + cube.w + 6} y={p.y + 9}>
                {machine.online ? `${loadOf(machine)} Mb/s · ${machine.ping ?? '--'} ms` : ''}
                {tags.map(tag => (
                  <tspan key={tag.text} className={tag.tone}>
                    {machine.online ? ` · ${tag.text}` : tag.text}
                  </tspan>
                ))}
              </text>
            )}
            {order >= 0 && (
              <g className="lab-badge">
                <circle cx={p.x - cube.w - 9} cy={p.y + 3} r={7} />
                <text x={p.x - cube.w - 9} y={p.y + 6.5} textAnchor="middle">
                  {order + 1}
                </text>
              </g>
            )}
          </g>
        );
      })}
    </svg>
  );
}

function Legend() {
  return (
    <div className="lab-legend" aria-hidden="true">
      <span>
        <svg width="18" height="8">
          <path className="te" d="M1 4 H17" />
        </svg>
        可用
      </span>
      <span>
        <svg width="18" height="8">
          <path className="te lab-te pending" d="M1 4 H17" />
        </svg>
        下发中
      </span>
      <span>
        <svg width="18" height="8">
          <path className="lab-draft" d="M1 4 H17" />
        </svg>
        待创建
      </span>
      <span>
        <MiniCube off />
        离线
      </span>
      <span>
        <MiniCylinder />
        隧道
      </span>
    </div>
  );
}

function MachineCard({
  machine,
  board,
  bounds,
  testReady,
  nodes,
}: {
  machine: FleetMachine;
  board: Board;
  bounds: { width: number; height: number };
  testReady: boolean;
  nodes: TestNode[];
}) {
  const p = board.at(gridOf(machine.id));
  const width = 256;
  const gap = board.cube.w + 18;
  const left = p.x + gap + width <= bounds.width - 8 ? p.x + gap : Math.max(8, p.x - gap - width);
  const top = Math.max(8, Math.min(p.y - 40, bounds.height - 248));
  const city = CITIES[machine.city];
  const protocols = protocolsOf({ path: [machine.id] });
  return (
    <div className="lab-card" style={{ left, top, width }} role="tooltip">
      <header className="lab-card-head">
        <i className={`cg-lamp ${machine.online ? 'ok' : 'idle'}`} aria-hidden="true" />
        <b>{machine.name}</b>
        <span className="sp" />
        <span className="mono">{machine.id}</span>
      </header>
      <dl className="lab-card-body">
        <dt>地址</dt>
        <dd className="mono">{[machine.ipv4, machine.ipv6].filter(Boolean).join(' · ')}</dd>
        <dt>位置</dt>
        <dd>
          {city.name} <span className="mono">{machine.city}</span>
          <span className="mono lab-card-dim">
            {' '}
            {degrees(city.lat, 'N', 'S')} {degrees(city.lon, 'E', 'W')}
          </span>
        </dd>
        <dt>负载</dt>
        <dd>
          {machine.online ? (
            <>
              <Spark values={machine.nicMbps} />
              <span className="mono">{loadOf(machine)} Mb/s</span>
            </>
          ) : (
            '离线'
          )}
        </dd>
        <dt>延迟</dt>
        <dd className="mono">{machine.ping === null ? '--' : `${machine.ping} ms`}</dd>
        <dt>测试入口</dt>
        <dd className={testReady ? undefined : 'warn'}>
          {testReady ? (
            <span className="mono">
              :{TEST_PORTS.vless} :{TEST_PORTS.hy2} :{TEST_PORTS.anytls}
            </span>
          ) : (
            '未开 · 首次使用重启 Xray'
          )}
        </dd>
        <dt>协议</dt>
        <dd>{protocols.map(protocol => PROTOCOL_LABEL[protocol]).join(' · ')}</dd>
        {machine.nat && (
          <>
            <dt>NAT</dt>
            <dd>不能作入口</dd>
          </>
        )}
        {machine.egress === false && (
          <>
            <dt>出网</dt>
            <dd>不允许，作出口时接隧道</dd>
          </>
        )}
        <dt>测试节点</dt>
        <dd className="mono">{nodes.length > 0 ? nodes.map(node => shortId(node.id)).join(' ') : '--'}</dd>
      </dl>
    </div>
  );
}

/* ══ 机器架 ══ */

function Rack({
  value,
  query,
  testReady,
  usage,
  peek,
  onQuery,
  onPeek,
  onMachine,
  onTunnel,
}: {
  value: ComposeValue;
  query: string;
  testReady: (machine: string) => boolean;
  usage: Map<string, TestNode[]>;
  peek: string | null;
  onQuery: (query: string) => void;
  onPeek: (id: string | null) => void;
  onMachine: (id: string) => void;
  onTunnel: (id: string) => void;
}) {
  const groups = RACK_ORDER.map(city => ({
    city,
    machines: machinesOf(city).filter(machine => machineMatches(machine, query)),
  })).filter(group => group.machines.length > 0);
  const tunnels = TUNNELS.filter(tunnel => tunnelMatches(tunnel, query));
  const online = FLEET.filter(machine => machine.online).length;
  return (
    <aside className="lab-pane lab-rack" aria-label="机器">
      <header className="lab-pane-head">
        <h2>机器</h2>
        <span className="lab-pane-meta">
          {online}/{FLEET.length} 在线
        </span>
      </header>
      <span className="user-list-search lab-search">
        <Icon of="search" size={13} className="user-list-search-icon" />
        <input
          type="search"
          value={query}
          aria-label="搜索机器或隧道"
          placeholder="名称、城市或 IP"
          autoComplete="off"
          spellCheck={false}
          onChange={event => onQuery(event.target.value)}
        />
        {query && (
          <button type="button" aria-label="清空搜索" title="清空" onClick={() => onQuery('')}>
            <Icon of="close" size={13} />
          </button>
        )}
      </span>
      <div className="lab-rack-cols" aria-hidden="true">
        <span />
        <span>名称</span>
        <span className="lab-cell-spark">12 分钟</span>
        <span>Mb/s</span>
        <span>ms</span>
        <span title="经过它的测试节点">测</span>
      </div>
      <div className="lab-rack-body">
        {groups.map(({ city, machines }) => (
          <section key={city} className="lab-rack-group" aria-label={CITIES[city].name}>
            <h3 className="lab-rack-city">
              <RegionFlag code={machines[0].country} />
              {CITIES[city].name}
              <span>{city}</span>
            </h3>
            {machines.map(machine => {
              const order = value.path.indexOf(machine.id);
              const block = blockOf(machine, value.path);
              const tags = tagsOf(machine, testReady);
              const uses = usage.get(machine.id)?.length ?? 0;
              return (
                <button
                  key={machine.id}
                  type="button"
                  className={`lab-rack-row${peek === machine.id ? ' peek' : ''}${machine.online ? '' : ' off'}`}
                  aria-pressed={order >= 0}
                  aria-disabled={!!block}
                  title={block ?? (tags.map(tag => tag.title).join('；') || undefined)}
                  onClick={() => {
                    if (!block) onMachine(machine.id);
                  }}
                  onPointerEnter={() => onPeek(machine.id)}
                  onPointerLeave={() => onPeek(null)}
                  onFocus={() => onPeek(machine.id)}
                  onBlur={() => onPeek(null)}
                >
                  <span className="lab-slot">
                    {order >= 0 ? <b>{order + 1}</b> : <MiniCube off={!machine.online} />}
                  </span>
                  <span className="lab-rack-name">
                    <span className="lab-rack-short">{machine.short}</span>
                    {/* 一行只放一个标记，按严重程度取第一个；其余写在提示里，悬停的读数卡里也有。 */}
                    {tags.slice(0, 1).map(tag => (
                      <em key={tag.text} className={`lab-tag ${tag.tone}`}>
                        {tag.text}
                      </em>
                    ))}
                  </span>
                  <span className="lab-cell-spark">
                    {machine.online ? <Spark values={machine.nicMbps} /> : <span className="lab-none">--</span>}
                  </span>
                  <span className="lab-num">{machine.online ? loadOf(machine) : '--'}</span>
                  <span className="lab-num">{machine.ping ?? '--'}</span>
                  <span className="lab-num lab-uses">{uses > 0 ? uses : ''}</span>
                </button>
              );
            })}
          </section>
        ))}
        {tunnels.length > 0 && (
          <section className="lab-rack-group" aria-label="隧道">
            <h3 className="lab-rack-city">
              隧道<span>出口机器之后</span>
            </h3>
            {tunnels.map(tunnel => {
              const on = value.tunnel === tunnel.id;
              const idle = value.path.length === 0;
              return (
                <button
                  key={tunnel.id}
                  type="button"
                  className="lab-rack-row tunnel"
                  aria-pressed={on}
                  aria-disabled={idle}
                  title={idle ? '先选机器，隧道接在最后一台之后' : tunnel.name}
                  onClick={() => {
                    if (!idle) onTunnel(tunnel.id);
                  }}
                >
                  <span className="lab-slot">{on ? <b>⇢</b> : <MiniCylinder />}</span>
                  <span className="lab-rack-name">
                    <span className="lab-rack-short">{tunnel.badge}</span>
                    <em className="lab-rack-sub">{tunnel.name}</em>
                  </span>
                  <span className="lab-rack-exit">
                    <RegionFlag code={tunnel.country} />
                    {tunnel.exitIp}
                  </span>
                </button>
              );
            })}
          </section>
        )}
        {groups.length === 0 && tunnels.length === 0 && <p className="lab-empty">没有匹配的机器</p>}
      </div>
    </aside>
  );
}

/* ══ 测试节点 ══ */

function Run({
  node,
  now,
  focused,
  qrOpen,
  menuOpen,
  onHover,
  onQr,
  onMenu,
  onProbe,
  onRetry,
}: {
  node: TestNode;
  now: number;
  focused: boolean;
  qrOpen: boolean;
  menuOpen: boolean;
  onHover: (id: string | null) => void;
  onQr: (anchor: HTMLElement) => void;
  onMenu: (anchor: HTMLElement) => void;
  onProbe: () => void;
  onRetry: () => void;
}) {
  const left = node.expiresAt - now;
  const done = node.path.filter(machine => node.hops[machine] === 'ok').length;
  const probe = node.probe;
  const km = pathKm(node.path);
  return (
    <article
      className={`lab-run is-${node.state}${focused ? ' focus' : ''}`}
      data-node={node.id}
      aria-label={clientName(node)}
      onPointerEnter={() => onHover(node.id)}
      onPointerLeave={() => onHover(null)}
      onFocus={() => onHover(node.id)}
      onBlur={() => onHover(null)}
    >
      <div className="lab-run-line">
        <i className={`cg-lamp ${STATE_LAMP[node.state]}`} aria-hidden="true" />
        <b className="lab-run-id">{shortId(node.id)}</b>
        <span className="lab-run-path" title={clientName(node)}>
          {node.path.map((id, index) => (
            <Fragment key={id}>
              {index > 0 && <i>→</i>}
              {nameOf(id)}
            </Fragment>
          ))}
          {node.tunnel && (
            <>
              <i>→</i>
              <em>{tunnelById.get(node.tunnel)?.badge}</em>
            </>
          )}
        </span>
        <span
          className={`lab-ttl${left < HOUR && node.state !== 'removing' ? ' soon' : ''}`}
          title={node.state === 'removing' ? undefined : `${stamp(node.expiresAt)} 到期，到期自动删除`}
        >
          {node.state === 'removing' ? '--:--:--' : countdown(left)}
        </span>
        <button
          type="button"
          className="btn sm lab-run-more"
          aria-label={`${clientName(node)} 的更多操作`}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onClick={event => onMenu(event.currentTarget)}
        >
          <Icon of="more" size={14} />
        </button>
      </div>
      <div className="lab-run-line sub">
        {node.state === 'ready' && (
          <>
            <span className="lab-run-links">
              {protocolsOf(node).map(protocol => (
                <CopyButton
                  key={protocol}
                  className="btn sm tn-link"
                  text={linkOf(node, protocol)}
                  aria-label={`复制 ${PROTOCOL_LABEL[protocol]} 分享链接`}
                  title={`复制 ${PROTOCOL_LABEL[protocol]} 分享链接`}
                >
                  {/* 平时只写协议名，省下的宽度留给拨测读数；复制后才出现对勾或失败记号。 */}
                  {state => (
                    <>
                      {state !== 'idle' && <Icon of={copyStateIcon(state)} size={11} />}
                      {PROTOCOL_LABEL[protocol]}
                    </>
                  )}
                </CopyButton>
              ))}
              <button
                type="button"
                className="btn sm tn-qr-btn"
                aria-label="二维码"
                title="二维码"
                aria-expanded={qrOpen}
                onClick={event => onQr(event.currentTarget)}
              >
                <MockIcon of="qr" size={13} />
              </button>
            </span>
            <span className="sp" />
            {probe?.status === 'running' && (
              <span className="tn-probe run">
                <i className="cg-lamp run" aria-hidden="true" />
                拨测中
              </span>
            )}
            {probe?.status === 'ok' && (
              <span
                className="tn-probe ok"
                title={`拨测：出口 ${probe.country} ${probe.exitIp}，首字节 ${probe.ms} ms；链路往返下限 ${Math.round(minRttMs(km))} ms`}
              >
                <RegionFlag code={probe.country} />
                <span className="mono lab-probe-ip">{probe.exitIp}</span>
                <b className="mono">{probe.ms}ms</b>
              </span>
            )}
            {probe?.status === 'fail' && <span className="tn-probe bad">不通 · {probe.detail}</span>}
            <button
              type="button"
              className="btn sm tn-probe-btn"
              disabled={probe?.status === 'running'}
              aria-label="拨测"
              title="从控制台用这个测试节点的凭据连一次，看通不通、从哪里出网"
              onClick={onProbe}
            >
              <Icon of="diag" size={12} />
            </button>
          </>
        )}
        {node.state === 'deploying' && (
          <>
            {node.path.map(id => (
              <span
                key={id}
                className="lab-hop"
                data-hop={node.hops[id]}
                title={`${nameOf(id)} · ${HOP_TITLE[node.hops[id]]}`}
              >
                <i className={`cg-lamp ${HOP_LAMP[node.hops[id]]}`} aria-hidden="true" />
                {fleetById.get(id)?.short ?? id}
              </span>
            ))}
            <span className="sp" />
            <span className="lab-run-note mono">
              {done}/{node.path.length}
            </span>
          </>
        )}
        {node.state === 'failed' && node.failure && (
          <>
            <span className="lab-run-fail" title={node.failure.reason}>
              {nameOf(node.failure.machine)}：{node.failure.reason}
            </span>
            <span className="sp" />
            <button type="button" className="btn sm" onClick={onRetry}>
              重试
            </button>
          </>
        )}
        {node.state === 'removing' && <span className="lab-run-note">撤下中</span>}
      </div>
    </article>
  );
}

function Runs({
  store,
  now,
  hover,
  floating,
  listRef,
  onHover,
  onOpen,
}: {
  store: TestNodeStore;
  now: number;
  hover: string | null;
  floating: Floating | null;
  listRef: RefObject<HTMLDivElement | null>;
  onHover: (id: string | null) => void;
  onOpen: (kind: Floating['kind'], id: string, anchor: HTMLElement) => void;
}) {
  return (
    <aside className="lab-pane lab-runs" aria-label="测试节点">
      <header className="lab-pane-head">
        <h2>测试节点</h2>
        <span className="lab-pane-meta">
          {store.nodes.length}/{TEST_NODE_LIMIT}
        </span>
        <span className="sp" />
        <span className="lab-pane-meta">剩余</span>
      </header>
      <div ref={listRef} className="lab-runs-body">
        {store.nodes.length === 0 && <p className="lab-empty">还没有测试节点</p>}
        {store.nodes.map(node => (
          <Run
            key={node.id}
            node={node}
            now={now}
            focused={hover === node.id}
            qrOpen={floating?.kind === 'qr' && floating.id === node.id}
            menuOpen={floating?.kind === 'menu' && floating.id === node.id}
            onHover={onHover}
            onQr={anchor => onOpen('qr', node.id, anchor)}
            onMenu={anchor => onOpen('menu', node.id, anchor)}
            onProbe={() => store.probe(node.id)}
            onRetry={() => store.retry(node.id)}
          />
        ))}
      </div>
    </aside>
  );
}

/* ══ 底栏：组装条与日志 ══ */

function Compose({
  value,
  confirm,
  testReady,
  limitReached,
  onChange,
  onConfirm,
  onCreate,
}: {
  value: ComposeValue;
  confirm: boolean;
  testReady: (machine: string) => boolean;
  limitReached: boolean;
  onChange: (value: ComposeValue) => void;
  onConfirm: (confirm: boolean) => void;
  onCreate: (value: ComposeValue) => void;
}) {
  const { path, tunnel } = value;
  const issue = path.length === 0 ? null : pathIssue(path, tunnel);
  const restarts = restartMachines(path).filter(machine => !testReady(machine.id));
  const ready = path.length > 0 && !issue && !limitReached;
  const km = pathKm(path);
  const create = () => {
    if (!ready) return;
    if (restarts.length > 0 && !confirm) {
      onConfirm(true);
      return;
    }
    onCreate(value);
  };
  const tunnelInfo = tunnel ? tunnelById.get(tunnel) : undefined;

  return (
    <section className="lab-compose" aria-label="组装">
      <div className="lab-compose-path">
        <span className="lab-dock-label">路径</span>
        {path.length === 0 && <span className="lab-compose-hint">点选机器，第一台是入口</span>}
        {/* 每一跳的连线和它指向的记号放在一组里，折行时不会把连线单独留在行尾。 */}
        {path.map((id, index) => (
          <span key={id} className="lab-step">
            {index > 0 && (
              <span className="lab-hopline">
                <span>{kmText(hopKm(path[index - 1], id))}</span>
              </span>
            )}
            <span className="lab-token">
              <b className="lab-token-no">{index + 1}</b>
              {nameOf(id)}
              <em>{roleOf(index, path.length, !!tunnel)}</em>
              <button
                type="button"
                className="lab-x"
                aria-label={`从路径移除 ${nameOf(id)}`}
                title="移除"
                onClick={() => onChange(withoutMachine(value, id))}
              >
                ×
              </button>
            </span>
          </span>
        ))}
        {tunnelInfo && (
          <span className="lab-step">
            <span className="lab-hopline" />
            <span className="lab-token tunnel">
              <b className="lab-token-no">⇢</b>
              {tunnelInfo.badge}
              <em>隧道</em>
              <button
                type="button"
                className="lab-x"
                aria-label={`去掉隧道 ${tunnelInfo.name}`}
                title="移除"
                onClick={() => onChange({ path, tunnel: null })}
              >
                ×
              </button>
            </span>
          </span>
        )}
      </div>
      {confirm ? (
        <div className="tn-confirm" role="alert">
          <Icon of="warn" size={14} className="tn-confirm-icon" />
          <span className="tn-confirm-text">
            {restarts.map(machine => machine.name).join('、')} 还没有测试入口，创建时会重启它的 Xray，现有连接会断开。
          </span>
          <span className="tn-confirm-actions">
            <button type="button" className="btn" onClick={() => onConfirm(false)}>
              取消
            </button>
            <button type="button" className="btn danger" onClick={create}>
              重启并创建
            </button>
          </span>
        </div>
      ) : (
        <div className="lab-compose-foot">
          <span className="lab-metric" title="相邻机器之间的大圆距离之和">
            <i className="math">L</i>
            {path.length > 1 ? kmText(km) : '--'}
          </span>
          <span className="lab-metric" title="光在光纤里约每毫秒 200 km，往返时延不会低于这个值；不含隧道之后的路程">
            <i className="math">Δt</i>
            {path.length > 1 ? `≥ ${Math.round(minRttMs(km))} ms` : '--'}
          </span>
          <span className="lab-metric">
            <i>协议</i>
            {path.length > 0
              ? protocolsOf({ path })
                  .map(protocol => PROTOCOL_LABEL[protocol])
                  .join(' · ')
              : '--'}
          </span>
          <span className="lab-metric">
            <i>有效期</i>24 h
          </span>
          {restarts.length > 0 && (
            <span className="lab-metric warn" title={RESTART_TITLE}>
              <i>需重启</i>
              {restarts.map(machine => machine.name).join('、')}
            </span>
          )}
          <span className="lab-compose-actions">
            {issue ? (
              <span className="lab-note bad">{issue}</span>
            ) : limitReached ? (
              <span className="lab-note">最多同时保留 {TEST_NODE_LIMIT} 个</span>
            ) : null}
            <button type="button" className="btn" disabled={path.length === 0} onClick={() => onChange(EMPTY)}>
              清空
            </button>
            <button type="button" className="btn primary lab-create" disabled={!ready} onClick={create}>
              创建
            </button>
          </span>
        </div>
      )}
    </section>
  );
}

function Log({ events }: { events: LabEvent[] }) {
  return (
    <section className="lab-log" aria-label="日志">
      <span className="lab-dock-label">日志</span>
      <ol className="lab-log-list" role="log">
        {events.map(event => (
          <li key={event.id} className={event.tone ?? undefined}>
            <time>{clockText(event.at)}</time>
            <b>{event.node ? shortId(event.node) : ''}</b>
            <span>{event.text}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}

/* ══ 顶栏 ══ */

function Bar({
  store,
  now,
  full,
  onFull,
  onExit,
}: {
  store: TestNodeStore;
  now: number;
  full: boolean;
  onFull: () => void;
  onExit: () => void;
}) {
  const counts: [TestNodeState, string, string][] = [
    ['ready', 'ok', '可用'],
    ['deploying', 'run', '下发'],
    ['failed', 'err', '失败'],
  ];
  return (
    <header className="lab-pane lab-bar">
      <button type="button" className="btn lab-back" title="回到线路（Esc）" onClick={onExit}>
        <Icon of="back" size={13} />
        线路
      </button>
      <span className="lab-title">
        <MockIcon of="flask" size={14} />
        实验模式
      </span>
      <span className="lab-readout">
        {counts.map(([state, lamp, label]) => {
          const count = store.nodes.filter(node => node.state === state).length;
          return count > 0 ? (
            <span key={state} className={state === 'failed' ? 'bad' : undefined}>
              <i className={`cg-lamp ${lamp}`} aria-hidden="true" />
              {label} {count}
            </span>
          ) : null;
        })}
      </span>
      <span className="sp" />
      <time className="lab-clock" title="控制台时间">
        {clockText(now)}
        <small>UTC+8</small>
      </time>
      <SubscriptionCopy />
      <button
        type="button"
        className="btn lab-full"
        aria-pressed={full}
        aria-label={full ? '退出全屏' : '全屏'}
        title={full ? '退出全屏' : '全屏'}
        onClick={onFull}
      >
        <MockIcon of={full ? 'shrink' : 'expand'} size={14} />
      </button>
    </header>
  );
}

/* ══ 普通模式里的入口：线路标题栏上的开关 ══ */

export function LabSwitch({ count, onEnter }: { count: number; onEnter: () => void }) {
  return (
    <button
      type="button"
      className="btn lab-switch"
      title={count > 0 ? `进入实验模式：${count} 个测试节点` : '进入实验模式'}
      onClick={onEnter}
    >
      <MockIcon of="flask" size={13} />
      实验模式
      {count > 0 && <span className="lab-switch-count">{count}</span>}
    </button>
  );
}

/* ══ 实验台 ══ */

export function LabStage({
  store,
  scene,
  preset,
  onExit,
  onPromote,
}: {
  store: TestNodeStore;
  scene: Scene;
  preset: LabPreset;
  onExit: () => void;
  onPromote: (node: TestNode) => void;
}) {
  const now = useMockNow();
  const list = useRef<HTMLDivElement>(null);
  const [stage, setStage] = useState<HTMLDivElement | null>(null);
  const [value, setValue] = useState<ComposeValue>(() => initialCompose(scene, preset));
  const [confirm, setConfirm] = useState(!!scene.compose?.confirm);
  const [query, setQuery] = useState('');
  const [peek, setPeek] = useState<string | null>(preset === 'hover' ? 'hk-hkix' : null);
  const [hover, setHover] = useState<string | null>(() =>
    preset === 'hover' ? (store.nodes.find(node => node.state === 'ready')?.id ?? null) : null,
  );
  const [floating, setFloating] = useState<Floating | null>(null);
  const [full, setFull] = useState(false);

  const change = (next: ComposeValue) => {
    setConfirm(false);
    setValue(next);
  };
  const toggleMachine = (machine: string) =>
    change(
      value.path.includes(machine) ? withoutMachine(value, machine) : { ...value, path: [...value.path, machine] },
    );
  const toggleTunnel = (tunnel: string) => change({ ...value, tunnel: value.tunnel === tunnel ? null : tunnel });

  const usage = useMemo(() => {
    const map = new Map<string, TestNode[]>();
    for (const node of store.nodes) {
      if (node.state === 'removing') continue;
      for (const machine of node.path) map.set(machine, [...(map.get(machine) ?? []), node]);
    }
    return map;
  }, [store.nodes]);

  useLayoutEffect(() => {
    if (preset !== 'qr' && preset !== 'menu') return;
    const first = store.nodes.find(node => node.state === 'ready');
    const anchor = first
      ? list.current?.querySelector<HTMLElement>(
          `[data-node="${first.id}"] ${preset === 'qr' ? '.tn-qr-btn' : '.lab-run-more'}`,
        )
      : null;
    if (first && anchor) setFloating({ kind: preset, id: first.id, anchor });
    // 只在挂载时按预设打开一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const sync = () => setFull(document.fullscreenElement !== null);
    document.addEventListener('fullscreenchange', sync);
    return () => document.removeEventListener('fullscreenchange', sync);
  }, []);

  useEffect(() => {
    // Esc 回到线路。浮层自己在捕获阶段先收走 Esc；全屏时 Esc 由浏览器用来退出全屏，不会到这里。
    const key = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      if ((event.target as HTMLElement | null)?.closest('input, textarea, select')) return;
      onExit();
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [onExit]);

  const toggleFull = () => {
    const request = document.fullscreenElement ? document.exitFullscreen() : stage?.requestFullscreen();
    request?.catch(error => console.warn('浏览器拒绝了全屏请求', error));
  };
  const open = (kind: Floating['kind'], id: string, anchor: HTMLElement) =>
    setFloating(current => (current?.anchor === anchor ? null : { kind, id, anchor }));
  const target = floating ? store.nodes.find(node => node.id === floating.id) : undefined;

  return (
    <div ref={setStage} className="lab-stage" aria-label="实验模式">
      <div className="lab-grid">
        <Bar store={store} now={now} full={full} onFull={toggleFull} onExit={onExit} />
        <Rack
          value={value}
          query={query}
          testReady={store.testReady}
          usage={usage}
          peek={peek}
          onQuery={setQuery}
          onPeek={setPeek}
          onMachine={toggleMachine}
          onTunnel={toggleTunnel}
        />
        <BoardView
          nodes={store.nodes}
          value={value}
          testReady={store.testReady}
          usage={usage}
          hover={hover}
          peek={peek}
          onHover={setHover}
          onPeek={setPeek}
          onToggle={toggleMachine}
          onTunnel={toggleTunnel}
        />
        <Runs
          store={store}
          now={now}
          hover={hover}
          floating={floating}
          listRef={list}
          onHover={setHover}
          onOpen={open}
        />
        <footer className="lab-pane lab-dock">
          <Compose
            value={value}
            confirm={confirm}
            testReady={store.testReady}
            limitReached={store.nodes.length >= TEST_NODE_LIMIT}
            onChange={change}
            onConfirm={setConfirm}
            onCreate={next => {
              store.create(next.path, next.tunnel);
              setConfirm(false);
              setValue(EMPTY);
            }}
          />
          <Log events={store.events} />
        </footer>
      </div>
      {floating && target && floating.kind === 'qr' && (
        <QrPopover node={target} anchor={floating.anchor} container={stage} onClose={() => setFloating(null)} />
      )}
      {floating && target && floating.kind === 'menu' && (
        <RowMenu
          node={target}
          anchor={floating.anchor}
          container={stage}
          now={now}
          onClose={() => setFloating(null)}
          onExtend={() => store.extend(target.id)}
          onPromote={() => onPromote(target)}
          onDelete={() => store.remove(target.id)}
        />
      )}
    </div>
  );
}
