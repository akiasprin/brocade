import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchRevisions,
  fetchSnapshot,
  fetchUsage,
  fetchUsageNodeSeries,
  type UsageChainSample,
} from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { artifactPanel } from '../ui/artifact-panel';
import { InspectPane } from '../panes/inspect';
import {
  chainsOf,
  hopKey,
  isoProject,
  type AppIr,
  type ChainView,
  type IsoGeometry,
  type IsoPlate,
  type SystemIr,
} from './model';

// 视图变换：作用于内容外层 g 上的 translate 和 scale，单位为 viewBox 的用户单位。
// 不修改 viewBox 是有意的——viewBox 变化时 preserveAspectRatio 的居中偏移随之变化，
// 屏幕像素与用户单位的换算需要在每处重新计算。
interface Camera {
  x: number;
  y: number;
  k: number;
}
const HOME: Camera = { x: 0, y: 0, k: 1 };
const clampK = (k: number) => Math.min(2.6, Math.max(0.35, k));

/* 检视卡对应的对象。kind 的取值与 InspectPane 支持的类型一致。 */
interface Sel {
  kind: 'node' | 'link' | 'ingress' | 'hop';
  id: string;
}

// 服务端返回的时间戳形如 `2026-08-04 09:53:04+00`：使用空格分隔、时区只有两位。
// 两处都不符合 ISO 格式——直接调用 Date.parse 会得到 NaN，统计口径行会显示为无效值。
const tsMs = (t: string): number => Date.parse(t.replace(' ', 'T').replace(/([+-]\d{2})$/, '$1:00'));

const fmtBytes = (b: number): string => {
  if (!b) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = b;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${u[i]}`;
};

// ══════════════════════════════════════════════════════════════
// 检视台面：只用于查看。
//
// 每条线路是若干条有向链，需要表达流向和跳序，一条链一块板并沿 z 轴叠放。
// 不将一条线路的多条链合并到同一平面的原因：合并后会出现环（app-hk-01 即
// sg→au→jp→my→sg），无法分层。拆分到多块板后各自无环。
//
// 此处不提供编辑功能：连线操作和创建线路都在功能页中完成。画布只表示当前结构。
// ══════════════════════════════════════════════════════════════
export function TopoCanvas() {
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });
  // 线路的名称位于快照的 label 字段——/compile/{rev} 中没有该字段，
  // AppIr 只包含 app_id。视图切换器需要显示名称而非 id，因此需要读取快照。
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  // 每一跳的字节数：chain_samples 一行对应一条（链 × 接收方）。
  // 它是明细接口且按行数截断，因此覆盖的是最近一段时间而非整月——
  // 下方的统计口径说明即指该点。
  const usage = useQuery({
    queryKey: ['usage-chain'],
    queryFn: () => fetchUsage({ limit: '400' }),
    refetchInterval: 30_000,
  });
  /* 机器的本月总量和当前速率。该接口使用的才是月度口径。 */
  const series = useQuery({
    queryKey: ['usage-node-series'],
    queryFn: () => fetchUsageNodeSeries(),
    refetchInterval: 30_000,
  });

  const [view, setView] = useState('');
  const [sel, setSel] = useState<Sel | null>(null);
  /* 点击某条链路时其余链路降低对比度。再次点击取消。 */
  const [litChain, setLitChain] = useState<string | null>(null);
  const [cam, setCam] = useState<Camera>(HOME);
  const svg = useRef<SVGSVGElement>(null);
  /* 画布的可用区域。viewBox 取该值才能保持 1:1 的坐标关系。 */
  const [box, setBox] = useState({ w: 1200, h: 660 });

  const camRef = useRef(cam);
  useEffect(() => {
    camRef.current = cam;
  }, [cam]);
  const ptrs = useRef(new Map<number, { x: number; y: number }>());
  const pinch = useRef<{ d0: number; mid0: { x: number; y: number }; cam0: Camera } | null>(null);
  const gestureBroke = useRef(false);
  /* 执行过平移的那次操作不应同时触发选中 */
  const panMoved = useRef(false);

  const system = compile.data?.system as SystemIr | undefined;
  const apps = useMemo(() => (compile.data?.apps as AppIr[] | undefined) ?? [], [compile.data]);
  const labels = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of snapshot.data?.snapshot.apps ?? []) m.set(a.id, a.label || a.id);
    return m;
  }, [snapshot.data]);

  const app = apps.find(a => (a.app_id ?? '') === view) ?? null;
  const chains = useMemo(() => (app ? chainsOf(app).filter(c => c.head) : []), [app]);

  /* ── 用量：三张表 ── */
  const chainSamples = useMemo<UsageChainSample[]>(() => usage.data?.chain_samples ?? [], [usage.data]);
  // 某条链在某台机器上的字节数。IR 中该计数按「链 × 接收方」记录（hop_label =
  // {chain}@{node}），因此同一台机器的多条入边共用同一数值——只能取用一次，不能相加。
  const chainBytes = useMemo(() => {
    const m = new Map<string, number>();
    for (const s of chainSamples) {
      const k = `${s.chain_id}|${s.node_id}`;
      m.set(k, (m.get(k) ?? 0) + s.uplink_bytes + s.downlink_bytes);
    }
    return m;
  }, [chainSamples]);
  const nodeMonth = useMemo(() => {
    const m = new Map<string, { bytes: number; rate: number }>();
    for (const n of series.data?.nodes ?? []) {
      const last = n.buckets[n.buckets.length - 1];
      m.set(n.node_id, {
        bytes:
          n.month_user_uplink_bytes +
          n.month_user_downlink_bytes +
          n.month_relay_uplink_bytes +
          n.month_relay_downlink_bytes,
        rate: last
          ? (last.user_uplink_bytes + last.user_downlink_bytes + last.relay_uplink_bytes + last.relay_downlink_bytes) /
            30
          : 0,
      });
    }
    return m;
  }, [series.data]);
  // 采样实际覆盖的时间范围。统计口径由数据计算得出，不是固定值。
  // 变量名不使用 window——那会遮蔽全局的 window，导致下方绑定手势监听的代码失效。
  const sampleWin = useMemo(() => {
    if (!chainSamples.length) return null;
    const a = chainSamples.reduce(
      (min, s) => (s.window_start < min ? s.window_start : min),
      chainSamples[0].window_start,
    );
    const b = chainSamples.reduce((max, s) => (s.window_end > max ? s.window_end : max), chainSamples[0].window_end);
    const mins = Math.round((tsMs(b) - tsMs(a)) / 60000);
    return { mins: Number.isFinite(mins) ? mins : null, truncated: chainSamples.length >= 400 };
  }, [chainSamples]);

  /* ── 几何 ── */
  const geo = useMemo<IsoGeometry | null>(() => {
    if (!system) return null;
    const ids = system.nodes.map(n => n.id).sort();
    return isoProject(chains, ids, box);
  }, [system, chains, box]);

  // 画布在视口中的矩形，检视浮层据此限制边界。存入 state 而非渲染时读取：渲染期读取
  // 布局可能得到该帧尚未确定的值，且渲染期不应读取 ref。
  // 滚动不更新该值——当前实现中滚动本身也不触发渲染。
  const [canvasRect, setCanvasRect] = useState<{
    left: number;
    top: number;
    right: number;
    bottom: number;
  } | null>(null);

  useEffect(() => {
    const el = svg.current;
    if (!el) return;
    const measure = () => {
      const r = el.getBoundingClientRect();
      if (r.width > 0) {
        setBox(prev =>
          Math.abs(prev.w - r.width) < 4 && Math.abs(prev.h - r.height) < 4
            ? prev
            : { w: Math.round(r.width), h: Math.round(r.height) },
        );
        setCanvasRect(prev =>
          prev && Math.abs(prev.left - r.left) < 1 && Math.abs(prev.top - r.top) < 1
            ? prev
            : { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
        );
      }
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [system]);

  // 画布只剩线路视图，默认落在第一条线路上；当前选中的线路不存在时（初次加载、
  // 或选中的线路被删除）回退到第一条。与下方的视图重置同为渲染期 setState。
  if (apps.length > 0 && !apps.some(a => (a.app_id ?? '') === view)) {
    setView(apps[0].app_id ?? '');
  }

  // 切换视图后原有的视图变换和选中状态不再适用。在渲染期重置而非在 effect 中：在 effect 中
  // 重置会先提交一帧包含新视图和旧变换的内容，表现为画布位置跳变。React 对渲染期调用
  // 组件自身的 setState 有特殊处理，会丢弃本轮输出并重新渲染，不提交该帧。
  const [syncedView, setSyncedView] = useState(view);
  if (view !== syncedView) {
    setSyncedView(view);
    setCam(HOME);
    setSel(null);
    setLitChain(null);
  }

  // 捏合缩放和滚轮缩放。指针状态记录使用捕获阶段的原生监听；滚轮必须使用非 passive 的
  // 原生监听，React 的 onWheel 是 passive 的，preventDefault 不生效，会导致整页滚动。
  useEffect(() => {
    const el = svg.current;
    if (!el || !geo) return;
    /* viewBox 始终等于元素的像素尺寸，用户单位即像素，换算只需减去元素原点坐标 */
    const toUser = (p: { x: number; y: number }) => {
      const r = el.getBoundingClientRect();
      return { x: p.x - r.left, y: p.y - r.top };
    };
    const zoomAround = (anchor: { x: number; y: number }, k: number, cam0: Camera) => {
      const u = toUser(anchor);
      const c = { x: (u.x - cam0.x) / cam0.k, y: (u.y - cam0.y) / cam0.k };
      setCam({ k, x: u.x - k * c.x, y: u.y - k * c.y });
    };
    const down = (e: PointerEvent) => {
      // 上一轮未收到 pointerup 是常见情况（手势被系统接管、触点移出屏幕边缘、切换后台后返回），
      // 遗漏一次会在指针表中留下残留项，之后所有单指平移都会因判定为多指而被拦截。
      // 因此在新手势开始时清空指针表。
      if (e.isPrimary) {
        ptrs.current.clear();
        pinch.current = null;
        gestureBroke.current = false;
      }
      ptrs.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (ptrs.current.size === 2) {
        const [a, b] = [...ptrs.current.values()];
        pinch.current = {
          d0: Math.hypot(a.x - b.x, a.y - b.y) || 1,
          mid0: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
          cam0: camRef.current,
        };
        gestureBroke.current = true;
      }
    };
    const move = (e: PointerEvent) => {
      if (!ptrs.current.has(e.pointerId)) return;
      ptrs.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
      const g = pinch.current;
      if (!g || ptrs.current.size < 2) return;
      const [a, b] = [...ptrs.current.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y) || 1;
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      const k = clampK(g.cam0.k * (d / g.d0));
      const u0 = toUser(g.mid0);
      const c0 = { x: (u0.x - g.cam0.x) / g.cam0.k, y: (u0.y - g.cam0.y) / g.cam0.k };
      const u1 = toUser(mid);
      setCam({ k, x: u1.x - k * c0.x, y: u1.y - k * c0.y });
    };
    const up = (e: PointerEvent) => {
      ptrs.current.delete(e.pointerId);
      if (ptrs.current.size < 2) pinch.current = null;
      if (ptrs.current.size === 0) gestureBroke.current = false;
    };
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const cam0 = camRef.current;
      zoomAround({ x: e.clientX, y: e.clientY }, clampK(cam0.k * Math.exp(-e.deltaY * 0.0015)), cam0);
    };
    el.addEventListener('pointerdown', down, true);
    window.addEventListener('pointermove', move, true);
    window.addEventListener('pointerup', up, true);
    window.addEventListener('pointercancel', up, true);
    el.addEventListener('wheel', wheel, { passive: false });
    return () => {
      el.removeEventListener('pointerdown', down, true);
      window.removeEventListener('pointermove', move, true);
      window.removeEventListener('pointerup', up, true);
      window.removeEventListener('pointercancel', up, true);
      el.removeEventListener('wheel', wheel);
    };
  }, [geo]);

  if (!current || compile.isPending) return <Loading />;
  if (compile.error) return <ErrorBox error={compile.error} />;
  if (!system || !geo || system.nodes.length === 0)
    return <div id="stage-chip">还没有节点。先去「机器」面纳管一台。</div>;
  if (apps.length === 0) return <div id="stage-chip">还没有线路。先去「线路」面建一条。</div>;

  const nameOf = (id: string) =>
    app?.nodes.find(n => n.id === id)?.name || apps.flatMap(a => a.nodes).find(n => n.id === id)?.name || id;

  /* 在空白处拖动执行平移。不拦截节点自身的 pointerdown，通过位移阈值区分点击和拖动。 */
  const startPan = (e: React.PointerEvent) => {
    if (ptrs.current.size > 1) return;
    const start = { x: e.clientX, y: e.clientY };
    const cam0 = camRef.current;
    panMoved.current = false;
    const move = (ev: PointerEvent) => {
      if (gestureBroke.current) return;
      const dx = ev.clientX - start.x;
      const dy = ev.clientY - start.y;
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) panMoved.current = true;
      setCam({ k: cam0.k, x: cam0.x + dx, y: cam0.y + dy });
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  const pick = (s: Sel) => {
    if (panMoved.current) return;
    setSel(s);
    // 同时将产物栏切换到该机器的 xray.json。不主动展开面板，只在已展开时更新选中项——
    // 面板已关闭表示当前不需要查看。
    if (s.kind === 'node') {
      artifactPanel.select({ targetKind: 'node', targetId: s.id, artifactKind: 'xray' });
    }
  };

  // 内容坐标转视口坐标。浮层和引线位于视口坐标系中（它们是阅读内容，不应随缩放改变尺寸）。
  // 使用 canvasRect 和 cam 这两个 state，而非 svg.current / camRef.current：该函数在
  // 渲染期由 JSX 调用，而渲染期不应读取 ref 和布局。camRef 供指针事件使用
  // （指针事件是原生监听，闭包中的 cam 会过期），两者在渲染侧取值相同。
  const toViewport = (p: { x: number; y: number }) => {
    if (!canvasRect) return { x: 0, y: 0 };
    return { x: canvasRect.left + p.x * cam.k + cam.x, y: canvasRect.top + p.y * cam.k + cam.y };
  };

  // 选中对象在屏幕上的位置——引线连接到该点。每次重渲染都重新计算，
  // 平移和缩放都会触发重渲染，因此引线位置持续跟随。
  const anchorOf = (s: Sel): { x: number; y: number } => {
    const g = geo;
    const hub = (p: { x: number; y: number }) => ({ x: p.x, y: p.y + g.node.h / 2 });
    for (const pl of g.plates) {
      if (s.kind === 'node') {
        const gp = pl.pos.get(s.id);
        if (gp) return toViewport(hub(g.at(gp.gx, gp.gy, pl.z)));
      }
      if (s.kind === 'hop') {
        const h = pl.chain.hops.find(x => hopKey(x) === s.id);
        if (h) {
          const a = pl.pos.get(h.from);
          const b = pl.pos.get(h.to);
          if (a && b) {
            const pa = hub(g.at(a.gx, a.gy, pl.z));
            const pb = hub(g.at(b.gx, b.gy, pl.z));
            return toViewport({ x: (pa.x + pb.x) / 2, y: (pa.y + pb.y) / 2 });
          }
        }
      }
      if (s.kind === 'ingress' && pl.chain.ingress?.id === s.id && pl.chain.head) {
        const gp = pl.pos.get(pl.chain.head);
        if (gp) {
          const p = g.at(gp.gx, gp.gy, pl.z);
          return toViewport({ x: p.x, y: p.y + g.node.w * 0.5 + g.node.h + g.clientDrop });
        }
      }
    }
    return { x: 0, y: 0 };
  };

  const fit = () => {
    const c = geo;
    const k = clampK(Math.min(1, (box.w - 56) / Math.max(1, c.contentW), (box.h - 56) / Math.max(1, c.contentH)));
    setCam({
      k,
      x: (box.w - c.contentW * k) / 2 - k * c.x0,
      y: (box.h - c.contentH * k) / 2 - k * c.y0,
    });
  };

  const views = apps.map(a => ({
    id: a.app_id ?? '',
    label: labels.get(a.app_id ?? '') || a.app_id || '（未命名）',
    title: a.app_id ?? undefined,
  }));
  const homed = cam.x === 0 && cam.y === 0 && cam.k === 1;

  return (
    <>
      <div id="viewbar2">
        <SlideSeg items={views} value={view} onPick={setView} />
        <span className="sp" />
        <button className="btn" onClick={fit}>
          适应
        </button>
        <button className="btn" disabled={homed} onClick={() => setCam(HOME)}>
          1:1
        </button>
      </div>

      <div className="topo-deck">
        <svg
          id="toposvg"
          ref={svg}
          viewBox={`0 0 ${box.w} ${box.h}`}
          preserveAspectRatio="xMidYMid meet"
          onPointerDown={startPan}
        >
          <defs>
            <marker
              id="tarrow"
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
                stroke="var(--ink-4)"
                strokeWidth="1.1"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </marker>
          </defs>

          <g transform={`translate(${cam.x} ${cam.y}) scale(${cam.k})`}>
            <IsoView
              geo={geo}
              sel={sel}
              lit={litChain}
              nameOf={nameOf}
              bytesOf={(chain, node) => chainBytes.get(`${chain}|${node}`) ?? 0}
              onPickNode={id => pick({ kind: 'node', id })}
              onPickHop={h => pick({ kind: 'hop', id: hopKey(h) })}
              onPickIngress={id => pick({ kind: 'ingress', id })}
            />
          </g>
        </svg>

        {/* 该线路未使用的机器：绘制在网格内，不放在底部注释带中。
            它们是该图的组成部分——未被链路经过的机器与被经过的机器属于同一问题的两面；
            移到画布之外后需要额外注意到该区域的存在。

            不随 cam 变换：它是列表而非空间关系，缩放画布时应保持可读（原因同 Callout）。 */}
        {geo.idle.length > 0 && (
          <IdleStack ids={geo.idle} node={geo.node} nameOf={nameOf} monthOf={id => nodeMonth.get(id)?.bytes ?? 0} />
        )}

        {/* 检视卡挂在对象旁边，用一条虚线连回其对应的对象。
            曾使用右侧常驻栏：卡片与其对应的内容相隔半个屏幕，阅读时需要来回定位。 */}
        {sel && (
          <Callout
            sel={sel}
            title={sel.kind === 'node' ? nameOf(sel.id) : sel.id}
            anchor={anchorOf(sel)}
            bounds={canvasRect}
            onClose={() => setSel(null)}
          />
        )}
      </div>

      {/* 底部一条：注释带和图例。两者都不属于空间信息，不应随画布拖动。 */}
      <div className="topo-bottom">
        <div className="topo-strip">
          <div className="scope">
            <b>口径</b>
            <span>
              <i>链上读数</i>这条链在这台上跑的字节 · 近 {sampleWin?.mins ?? '—'} 分钟采样合计
              {sampleWin?.truncated && '（明细接口，已按行数截断）'}
            </span>
            <br />
            <span>
              <i>机器总量</i>本月累计
            </span>
          </div>
        </div>

        <div className="topo-legend">
          {chains.map(c => (
            <span
              key={c.id}
              className="chip"
              aria-pressed={litChain === c.id}
              title={c.id}
              onClick={() => setLitChain(litChain === c.id ? null : c.id)}
            >
              <span className="bar" />
              {c.name}
            </span>
          ))}
          <span className="sp" />
          <span className="note">
            光点 = 当前有流量，移动快慢 = 速率 · 竖虚线 = 同一台入口机器 · 拖动平移，滚轮缩放
          </span>
        </div>
      </div>
    </>
  );
}

// ══ 滑块式切换器 ══
// 滑块是一层独立的底色，随选中项移动。曾用背景色标记选中态：
// 切换时是瞬间变化，无法看出切换方向；滑块通过位移动画呈现该过程，便于跟随。
function SlideSeg({
  items,
  value,
  onPick,
}: {
  items: { id: string; label: string; title?: string }[];
  value: string;
  onPick: (id: string) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [thumb, setThumb] = useState<{ x: number; w: number } | null>(null);
  useLayoutEffect(() => {
    const host = box.current;
    const on = host?.querySelector<HTMLElement>('[aria-pressed="true"]');
    if (!host || !on) return;
    setThumb(prev =>
      prev && Math.abs(prev.x - on.offsetLeft) < 1 && Math.abs(prev.w - on.offsetWidth) < 1
        ? prev
        : { x: on.offsetLeft, w: on.offsetWidth },
    );
  }, [value, items]);
  return (
    <div className="slideseg" ref={box}>
      {thumb && <span className="slideseg-thumb" style={{ transform: `translateX(${thumb.x}px)`, width: thumb.w }} />}
      {items.map(it => (
        <button key={it.id} aria-pressed={it.id === value} title={it.title} onClick={() => onPick(it.id)}>
          {it.label}
        </button>
      ))}
    </div>
  );
}

// ══ 等距方块 ══
// 三个面用三档明度区分，不描边。简化的是描边和阴影，
// 而非压平为平面——立体形态需要保留。
function Cube({
  x,
  y,
  w,
  h,
  cls,
  onClick,
}: {
  x: number;
  y: number;
  w: number;
  h: number;
  cls?: string;
  onClick?: () => void;
}) {
  return (
    <g className={cls} onClick={onClick}>
      <path className="sideL" d={`M${x - w},${y} L${x},${y + w * 0.5} L${x},${y + w * 0.5 + h} L${x - w},${y + h} Z`} />
      <path className="sideR" d={`M${x + w},${y} L${x},${y + w * 0.5} L${x},${y + w * 0.5 + h} L${x + w},${y + h} Z`} />
      <path className="top" d={`M${x},${y - w * 0.5} L${x + w},${y} L${x},${y + w * 0.5} L${x - w},${y} Z`} />
    </g>
  );
}

/* ══ 线路：一条链一块板，沿 z 叠起来 ══ */
function IsoView({
  geo,
  sel,
  lit,
  nameOf,
  bytesOf,
  onPickNode,
  onPickHop,
  onPickIngress,
}: {
  geo: IsoGeometry;
  sel: Sel | null;
  lit: string | null;
  nameOf: (id: string) => string;
  bytesOf: (chain: string, node: string) => number;
  onPickNode: (id: string) => void;
  onPickHop: (h: ChainView['hops'][number]) => void;
  onPickIngress: (id: string) => void;
}) {
  const { node, at, clientDrop } = geo;
  const M = 0.72;
  const hub = (p: { x: number; y: number }) => ({ x: p.x, y: p.y + node.h / 2 });
  const dim = (chain: string) => !!lit && lit !== chain;

  // 竖虚线只连接各条链的入口机器。中间的机器在两条链中分别转发，
  // 两个实例之间没有流量，连线会使人误认为存在通路。
  const heads = new Map<string, IsoPlate[]>();
  for (const pl of geo.plates) {
    const head = pl.chain.head!;
    if (!heads.has(head)) heads.set(head, []);
    heads.get(head)!.push(pl);
  }

  return (
    <>
      {geo.plates.map(pl => {
        const c0 = at(-M, pl.y0 - M, pl.z);
        const c1 = at(pl.depth + M, pl.y0 - M, pl.z);
        const c2 = at(pl.depth + M, pl.y1 + M, pl.z);
        const c3 = at(-M, pl.y1 + M, pl.z);
        const total = pl.chain.hops.reduce((a, h) => a + bytesOf(pl.chain.id, h.to), 0);
        return (
          <g key={pl.chain.id} className={dim(pl.chain.id) ? 'iso-mute' : undefined}>
            <path
              className={`plate${lit === pl.chain.id ? ' hot' : ''}`}
              d={`M${c0.x},${c0.y} L${c1.x},${c1.y} L${c2.x},${c2.y} L${c3.x},${c3.y} Z`}
            />
            <text className={`plabel${lit === pl.chain.id ? ' hot' : ''}`} x={c3.x - 16} y={c3.y + 2} textAnchor="end">
              {pl.chain.name}
            </text>
            <text className="pmeta" x={c3.x - 16} y={c3.y + 18} textAnchor="end">
              {pl.chain.hops.length} 跳 · {fmtBytes(total)}
            </text>
          </g>
        );
      })}

      {/* 连线统一使用实线——一跳是配置决定的事实，本月是否有流量不改变其存在与否。
          当前是否有流量由动画光点表示。 */}
      {geo.plates.map(pl =>
        pl.chain.hops.map(h => {
          const ga = pl.pos.get(h.from);
          const gb = pl.pos.get(h.to);
          if (!ga || !gb) return null;
          const pa = hub(at(ga.gx, ga.gy, pl.z));
          const pb = hub(at(gb.gx, gb.gy, pl.z));
          const span = Math.abs(gb.gx - ga.gx);
          // 同一块板上的节点共线时，跨格的边会与中间各条单格的边完全重叠，
          // 三条边显示为一条。沿 -y 方向绘制弧线以避开中间的节点。
          const bowed = span > 1 && Math.abs(ga.gy - gb.gy) < 0.01;
          const cp = bowed ? hub(at((ga.gx + gb.gx) / 2, ga.gy - 0.58 * span, pl.z)) : null;
          const d = cp ? `M${pa.x},${pa.y} Q${cp.x},${cp.y} ${pb.x},${pb.y}` : `M${pa.x},${pa.y} L${pb.x},${pb.y}`;
          const id = `${pl.chain.id}|${h.from}|${h.to}`;
          const use = bytesOf(pl.chain.id, h.to);
          // 该机器在这条链中的入边数量。大于 1 时该数值无法分配到具体某条边上，
          // 边上只能标注为上限值。
          const amb = pl.chain.hops.filter(x => x.to === h.to).length > 1;
          const mid = cp
            ? { x: (pa.x + 2 * cp.x + pb.x) / 4, y: (pa.y + 2 * cp.y + pb.y) / 4 }
            : { x: (pa.x + pb.x) / 2, y: (pa.y + pb.y) / 2 };
          const label = `${amb ? '≤ ' : ''}${fmtBytes(use)}`;
          return (
            <g key={id} className={`te-wrap${dim(pl.chain.id) ? ' iso-mute' : ''}`}>
              <path
                id={`p-${id}`}
                className={`te${sel?.kind === 'hop' && sel.id === hopKey(h) ? ' focus' : ''}`}
                d={d}
                markerEnd="url(#tarrow)"
              />
              <Pulses path={`p-${id}`} bytes={use} />
              <g className="tag" transform={`translate(${mid.x - 14},${mid.y - 11})`}>
                <rect x={-label.length * 3 - 7} y={-10} width={label.length * 6 + 14} height={20} rx={4} />
                <text y={4} textAnchor="middle">
                  {label}
                </text>
              </g>
              <path className="te-hit" d={d} onClick={() => onPickHop(h)} />
            </g>
          );
        }),
      )}

      {/* 竖虚线和客户端 */}
      {[...heads.entries()].map(([head, list]) => {
        const sorted = [...list].sort((a, b) => a.z - b.z);
        const pt = (pl: IsoPlate) => {
          const gp = pl.pos.get(head)!;
          return hub(at(gp.gx, gp.gy, pl.z));
        };
        const low = sorted[0];
        const gp = low.pos.get(head)!;
        const base = at(gp.gx, gp.gy, low.z);
        const y0 = base.y + node.h / 2;
        const y1 = base.y + node.w * 0.5 + node.h + clientDrop;
        return (
          <g key={`st-${head}`}>
            {sorted.slice(0, -1).map((pl, i) => {
              const a = pt(pl);
              const b = pt(sorted[i + 1]);
              return <path key={i} className="stitch" d={`M${a.x},${a.y} L${b.x},${b.y}`} />;
            })}
            <path className="stitch" d={`M${base.x},${y0} L${base.x},${y1}`} />
            <circle
              className="cli-dot"
              cx={base.x}
              cy={y1 + 3}
              r={2.6}
              onClick={() => low.chain.ingress && onPickIngress(low.chain.ingress.id)}
            />
            <text className="cli-t" x={base.x} y={y1 + 18} textAnchor="middle">
              CLIENT
            </text>
          </g>
        );
      })}

      {/* 站点 */}
      {geo.plates.map(pl =>
        [...pl.pos.entries()].map(([id, gp]) => {
          const p = at(gp.gx, gp.gy, pl.z);
          const own = bytesOf(pl.chain.id, id);
          const val = id === pl.chain.head ? '' : fmtBytes(own);
          const hot = sel?.kind === 'node' && sel.id === id;
          return (
            <g key={`${pl.chain.id}|${id}`} className={`nd${hot ? ' hot' : ''}${dim(pl.chain.id) ? ' iso-mute' : ''}`}>
              <Cube x={p.x} y={p.y} w={node.w} h={node.h} onClick={() => onPickNode(id)} />
              {/* 名称和数值位于方块右侧、出边上方。左下方被竖虚线占用，
                  右下方被向下延伸的出边占用——只有右上方可用。 */}
              <text className="nm" x={p.x + node.w + 11} y={p.y - 16}>
                {nameOf(id)}
              </text>
              <text className="sub" x={p.x + node.w + 11} y={p.y - 3}>
                {val ? `${id} · ${val}` : id}
              </text>
              <title>
                {nameOf(id)}｜{pl.chain.name} 这条链 {fmtBytes(own)}
              </title>
            </g>
          );
        }),
      )}
    </>
  );
}

// 沿路径移动的光点。数量按流量、周期按速率确定，两者都限制在较小的范围内——
// 一条边上出现二十个光点时会呈现为虚线。
function Pulses({ path, bytes }: { path: string; bytes: number }) {
  if (!bytes) return null;
  const n = 2;
  const dur = 3.2;
  return (
    <>
      {Array.from({ length: n }, (_, i) => (
        <circle key={i} className="pulse" r={1.8}>
          <animateMotion dur={`${dur}s`} repeatCount="indefinite" begin={`${(-dur * i) / n}s`}>
            <mpath href={`#${path}`} />
          </animateMotion>
        </circle>
      ))}
    </>
  );
}

// ══ 闲置机器：层叠显示 ══
// 它们之间没有关联，展开排列会占用空间，层叠显示正好表示这些机器未被使用。
function IdleStack({
  ids,
  node,
  nameOf,
  monthOf,
}: {
  ids: string[];
  node: { w: number; h: number };
  nameOf: (id: string) => string;
  monthOf: (id: string) => number;
}) {
  const rowH = node.h + 13;
  const listW = 250;
  const boxH = (ids.length - 1) * rowH + node.w + node.h + 10;
  const cx = node.w + 4;
  return (
    <div className="idle-wrap">
      <svg viewBox={`0 0 ${listW} ${boxH}`} width={listW} height={boxH}>
        {/* 从最下方开始绘制，上层才能覆盖下层的顶面，形成正确的层叠效果 */}
        {[...ids].reverse().map(id => {
          const k = ids.indexOf(id);
          const cy = node.w * 0.5 + 2 + k * rowH;
          return (
            <g key={id} className="idle-nd">
              <Cube x={cx} y={cy} w={node.w} h={node.h} />
              <text className="nm" x={cx + node.w + 12} y={cy + node.w * 0.5 + 1}>
                {nameOf(id)}
              </text>
              {/* 该机器可能在其他线路中承载流量，因此显示的是机器的本月总量 */}
              <text className="sub" x={listW} y={cy + node.w * 0.5 + 1} textAnchor="end">
                {id} · {fmtBytes(monthOf(id))}
              </text>
              <title>
                {nameOf(id)}｜这个线路的链没走到它｜这台机器本月 {fmtBytes(monthOf(id))}
              </title>
            </g>
          );
        })}
      </svg>
      <div className="idle-cap">这个线路没用到的机器 · {ids.length} 台</div>
    </div>
  );
}

const KIND_CN: Record<Sel['kind'], string> = {
  node: '机器',
  link: 'overlay 链路',
  ingress: '接入面',
  hop: '转发',
};

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

// 挂在对象旁的检视卡，以及一条连回该对象的虚线。
// 卡片位于视口坐标系而非画布坐标系：它是阅读内容，不应随缩放改变尺寸。
function Callout({
  sel,
  title,
  anchor,
  bounds,
  onClose,
}: {
  sel: Sel;
  title: string;
  anchor: { x: number; y: number };
  // 画布在视口中的矩形。卡片以该矩形而非整个视口为边界——视图条位于画布上沿，
  // 以视口为边界时卡片会位于其下方，标题栏被完全遮挡。
  bounds: { left: number; top: number; right: number; bottom: number } | null;
  onClose: () => void;
}) {
  const el = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 300, h: 220 });
  useEffect(() => {
    const node = el.current;
    if (!node) return;
    const measure = () => {
      const r = node.getBoundingClientRect();
      setSize(prev =>
        Math.abs(prev.w - r.width) < 2 && Math.abs(prev.h - r.height) < 2 ? prev : { w: r.width, h: r.height },
      );
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(node);
    return () => ro.disconnect();
  }, [sel.kind, sel.id]);

  const M = 12;
  const BAR = 56;
  const LEGEND = 40;
  const x0 = (bounds?.left ?? 0) + M;
  const x1 = (bounds?.right ?? window.innerWidth) - M;
  const y0 = (bounds?.top ?? 0) + BAR;
  const y1 = (bounds?.bottom ?? window.innerHeight) - LEGEND;
  let left: number;
  let top: number;
  if (sel.kind === 'node') {
    /* 节点方向的卡片向左右偏移。上下方被名称和数值占用。 */
    const gap = 58;
    const flip = anchor.x + gap + size.w > x1;
    left = clamp(flip ? anchor.x - gap - size.w : anchor.x + gap, x0, Math.max(x0, x1 - size.w));
    top = clamp(anchor.y - 72, y0, Math.max(y0, y1 - size.h));
  } else {
    /* 边方向的卡片向上下偏移：其锚点位于两台机器之间，向左右偏移会遮挡其中一台。 */
    const above = anchor.y - 18 - size.h;
    const below = anchor.y + 52;
    left = clamp(anchor.x - size.w / 2, x0, Math.max(x0, x1 - size.w));
    top = clamp(above >= y0 ? above : below, y0, Math.max(y0, y1 - size.h));
  }

  // 引线从卡片朝向锚点的那条边引出，起点限制在该边的范围内——
  // 从卡片中心引出会穿过卡片内的文字。
  const cx = left + size.w / 2;
  const cy = top + size.h / 2;
  const dx = anchor.x - cx;
  const dy = anchor.y - cy;
  const from =
    Math.abs(dx) > Math.abs(dy)
      ? { x: dx < 0 ? left : left + size.w, y: clamp(anchor.y, top + 14, top + size.h - 14) }
      : { x: clamp(anchor.x, left + 14, left + size.w - 14), y: dy < 0 ? top : top + size.h };
  const bend = clamp(Math.abs(anchor.x - from.x) * 0.42, 34, 120);
  const c1 = from.x + (anchor.x >= from.x ? bend : -bend);
  const c2 = anchor.x - (anchor.x >= from.x ? bend : -bend);

  return (
    <>
      <svg className="topo-leader" aria-hidden="true">
        <path
          className="topo-leader-line"
          d={`M${from.x} ${from.y} C ${c1} ${from.y}, ${c2} ${anchor.y}, ${anchor.x} ${anchor.y}`}
        />
        <circle className="topo-leader-dot" cx={anchor.x} cy={anchor.y} r="3" />
      </svg>
      <div className="topo-callout" ref={el} style={{ left, top }}>
        <div className="topo-callout-head">
          <span className="fw-kind">{KIND_CN[sel.kind]}</span>
          <span className="fw-title">{title}</span>
          <button className="btn" onClick={onClose} title="关掉">
            ✕
          </button>
        </div>
        <div className="topo-callout-body">
          <InspectPane kind={sel.kind} id={sel.id} />
        </div>
      </div>
    </>
  );
}
