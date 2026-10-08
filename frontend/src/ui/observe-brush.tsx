/** 观测图表上的拖选放大与双击缩小。
 *
 * 机器详情观测页在外层放 ObserveBrushProvider；页内每张时间图（吞吐、Ping、KPI 展开的历史图）用
 * ObserveBrushPlot 包住自己的 ECharts 容器。按住左键横向拖动时，选区以时间存进本页共用的存储，
 * 每张图各自换算成像素画出同一时段：拖动所在的图带起止时刻标签，其余图画浅色选区，KPI 迷你趋势线
 * 画浅色同段。松开后交给页面切换范围，读取期间选区保持显示，切换完成或失败后清除。
 *
 * 选区层是 ECharts 容器的兄弟节点：不写进 ECharts option（notMerge 重设会清掉），也不放进 ECharts
 * 容器（dispose 会清空容器内的全部子节点）。指针移动只改样式，不触发 React 渲染。
 *
 * 没有 Provider 时（其他页面复用这些图表）只渲染容器，不响应拖选和双击。
 */
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import type { EChartsType } from 'echarts/core';
import {
  localDayStart,
  observeDurationText,
  observeSelection,
  observeSnapStep,
  observeSpanText,
  type ObserveSelection,
} from './observe-range';

/** 拖动距离不足该值（像素）按单击处理。 */
const DRAG_THRESHOLD_PX = 4;

/** 悬停提示的类名。拖动时隐藏：读数提示与选区标签同时出现会互相遮挡。 */
export const OBSERVE_TOOLTIP_CLASS = 'observe-chart-tip';

/** 图表最近一次写入的坐标范围。x 为毫秒时间戳；y 下界恒为 0。 */
export interface ObserveBrushExtent {
  minMs: number;
  maxMs: number;
  yMax: number;
}

interface BrushState {
  selection: ObserveSelection;
  /** 拖动所在的图；其余图画浅色选区、不带标签。 */
  source: object;
  /** 已松开，页面正在读取所选时段。 */
  pending: boolean;
}

interface BrushStore {
  get(): BrushState | null;
  set(state: BrushState | null): void;
  subscribe(listener: () => void): () => void;
}

function createBrushStore(): BrushStore {
  let state: BrushState | null = null;
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set: next => {
      state = next;
      for (const listener of listeners) listener();
    },
    subscribe: listener => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

interface BrushContextValue {
  store: BrushStore;
  select(selection: ObserveSelection, source: object): void;
  zoomOut(): void;
}

const BrushContext = createContext<BrushContextValue | null>(null);

export function ObserveBrushProvider({
  rangeKey,
  pending,
  onSelect,
  onZoomOut,
  children,
}: {
  /** 当前显示范围的键。变化表示切换完成。 */
  rangeKey: string | number;
  /** 页面正在读取新范围。由真变假（完成或失败）时清除选区。 */
  pending: boolean;
  /** 返回 false 表示没有发起切换（与当前范围相同或正在读取同一范围），选区立即清除。 */
  onSelect: (startUnixSecs: number, endUnixSecs: number) => boolean;
  onZoomOut: () => void;
  children: ReactNode;
}) {
  const [store] = useState(createBrushStore);
  // 回调随每次渲染更新，Context 的值保持不变，指针事件读到的总是最新的回调。
  const handlers = useRef({ onSelect, onZoomOut });
  useEffect(() => {
    handlers.current = { onSelect, onZoomOut };
  });
  useEffect(() => {
    if (!pending && store.get()?.pending) store.set(null);
  }, [store, pending, rangeKey]);
  const value = useMemo<BrushContextValue>(
    () => ({
      store,
      select: (selection, source) => {
        store.set({ selection, source, pending: true });
        if (!handlers.current.onSelect(selection.startUnixSecs, selection.endUnixSecs)) store.set(null);
      },
      zoomOut: () => handlers.current.onZoomOut(),
    }),
    [store],
  );
  return <BrushContext.Provider value={value}>{children}</BrushContext.Provider>;
}

interface PlotRect {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

/** 绘图区在图表容器内的像素范围，由坐标轴两端换算得到（公开 API，不读 ECharts 内部的 grid 模型）。 */
function plotRect(chart: EChartsType, extent: ObserveBrushExtent): PlotRect | null {
  const finder = { xAxisIndex: 0, yAxisIndex: 0 };
  const origin = chart.convertToPixel(finder, [extent.minMs, 0]);
  const corner = chart.convertToPixel(finder, [extent.maxMs, extent.yMax]);
  if (!Array.isArray(origin) || !Array.isArray(corner)) return null;
  const [left, bottom] = origin;
  const [right, top] = corner;
  if (![left, right, top, bottom].every(Number.isFinite) || right - left < 1 || bottom - top < 1) return null;
  return { left, right, top, bottom };
}

/** 当前坐标范围与绘图区；图表尚未写入数据时为 null。 */
function measure(chartRef: RefObject<EChartsType | null>, extentRef: RefObject<ObserveBrushExtent | null>) {
  const chart = chartRef.current;
  const extent = extentRef.current;
  if (!chart || !extent || extent.maxMs <= extent.minMs) return null;
  const rect = plotRect(chart, extent);
  return rect ? { chart, extent, rect } : null;
}

const inside = (rect: PlotRect, x: number, y: number) =>
  x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;

/** 拖选标签：起止时刻 + 时长；补足到 1 分钟时说明这一点。今天以外的日期写出来。 */
function selectionText(selection: ObserveSelection, nowUnixSecs: number): [string, string] {
  const { startUnixSecs, endUnixSecs } = selection;
  const date =
    localDayStart(startUnixSecs) !== localDayStart(endUnixSecs) ||
    localDayStart(startUnixSecs) !== localDayStart(nowUnixSecs);
  return [
    observeSpanText(startUnixSecs, endUnixSecs, { seconds: selection.step < 60, date }),
    selection.widened ? '最短 1 分钟' : observeDurationText(endUnixSecs - startUnixSecs),
  ];
}

/**
 * 图表容器。`className` 是原有的尺寸类（`.ndtp-ec` / `.history-chart` / `.ping-probe-chart`），
 * ECharts 挂在内层铺满的 `hostRef` 上。
 */
export function ObserveBrushPlot({
  className,
  hostRef,
  chartRef,
  extentRef,
}: {
  className: string;
  hostRef: RefObject<HTMLDivElement | null>;
  chartRef: RefObject<EChartsType | null>;
  extentRef: RefObject<ObserveBrushExtent | null>;
}) {
  const brush = useContext(BrushContext);
  const plotRef = useRef<HTMLDivElement>(null);
  const bandRef = useRef<HTMLDivElement>(null);
  const tagRef = useRef<HTMLDivElement>(null);
  // 本图作为选区来源的标识。
  const [token] = useState(() => ({}));
  const endDrag = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!brush) return;
    let tipsHidden = false;
    const paint = () => {
      const band = bandRef.current;
      const tag = tagRef.current;
      const plot = plotRef.current;
      if (!band || !tag || !plot) return;
      const state = brush.store.get();
      const measured = state ? measure(chartRef, extentRef) : null;
      if (!state || !measured) {
        band.hidden = true;
        tag.hidden = true;
        tipsHidden = false;
        return;
      }
      const { chart, extent, rect } = measured;
      // 拖动开始时收起本图的提示与十字线：按下后浏览器不再派发鼠标移动的兼容事件，
      // ECharts 收不到移动，十字线和高亮的数据点会停在按下的位置。
      if (!state.pending && !tipsHidden) {
        chart.dispatchAction({ type: 'hideTip' });
        chart.dispatchAction({ type: 'updateAxisPointer', currTrigger: 'leave' });
        tipsHidden = true;
      }
      const xOf = (unixSecs: number) =>
        Math.min(
          rect.right,
          Math.max(
            rect.left,
            rect.left + ((unixSecs * 1000 - extent.minMs) / (extent.maxMs - extent.minMs)) * (rect.right - rect.left),
          ),
        );
      const left = xOf(state.selection.startUnixSecs);
      const right = xOf(state.selection.endUnixSecs);
      band.hidden = false;
      band.style.left = `${left}px`;
      band.style.width = `${Math.max(1, right - left)}px`;
      band.style.top = `${rect.top}px`;
      band.style.height = `${rect.bottom - rect.top}px`;
      band.toggleAttribute('data-linked', state.source !== token);
      band.toggleAttribute('data-pending', state.pending);
      if (state.source !== token) {
        tag.hidden = true;
        return;
      }
      const [span, detail] = selectionText(state.selection, Math.floor(Date.now() / 1000));
      const spanText = document.createElement('span');
      spanText.textContent = span;
      const detailText = document.createElement('span');
      detailText.className = 'observe-brush-tag-detail';
      detailText.textContent = detail;
      tag.replaceChildren(spanText, detailText);
      tag.hidden = false;
      // 标签压在 x 轴刻度行上、居中于选区，不遮挡曲线；超出图宽时贴边。
      const width = tag.offsetWidth;
      tag.style.left = `${Math.min(plot.clientWidth - width - 2, Math.max(2, (left + right) / 2 - width / 2))}px`;
      tag.style.top = `${rect.bottom + 4}px`;
    };
    paint();
    return brush.store.subscribe(paint);
  }, [brush, chartRef, extentRef, token]);

  useEffect(() => () => endDrag.current?.(), []);

  if (!brush) {
    return (
      <div className={`${className} observe-plot`}>
        <div ref={hostRef} className="observe-plot-canvas" />
      </div>
    );
  }

  const localX = (event: { clientX: number }) => event.clientX - (plotRef.current?.getBoundingClientRect().left ?? 0);
  const localY = (event: { clientY: number }) => event.clientY - (plotRef.current?.getBoundingClientRect().top ?? 0);

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    // 触屏的横向滑动留给悬停读数，不启用拖选。
    if (event.button !== 0 || event.pointerType === 'touch' || endDrag.current) return;
    const measured = measure(chartRef, extentRef);
    if (!measured) return;
    const { extent, rect } = measured;
    const anchorX = localX(event);
    if (!inside(rect, anchorX, localY(event))) return;
    event.preventDefault();
    const width = rect.right - rect.left;
    const step = observeSnapStep((extent.maxMs - extent.minMs) / 1000 / width);
    const secondsAt = (x: number) =>
      (extent.minMs +
        ((Math.min(rect.right, Math.max(rect.left, x)) - rect.left) / width) * (extent.maxMs - extent.minMs)) /
      1000;
    const anchor = secondsAt(anchorX);
    const pointerId = event.pointerId;
    let active = false;
    let selection: ObserveSelection | null = null;

    const move = (next: PointerEvent) => {
      if (next.pointerId !== pointerId) return;
      const x = localX(next);
      if (!active) {
        if (Math.abs(x - anchorX) < DRAG_THRESHOLD_PX) return;
        active = true;
        document.body.classList.add('observe-brushing');
      }
      selection = observeSelection(anchor, secondsAt(x), step, Math.floor(Date.now() / 1000));
      brush.store.set({ selection, source: token, pending: false });
    };
    const up = (next: PointerEvent) => {
      if (next.pointerId === pointerId) finish(true);
    };
    const cancel = (next: PointerEvent) => {
      if (next.pointerId === pointerId) finish(false);
    };
    const key = (next: KeyboardEvent) => {
      if (next.key !== 'Escape') return;
      next.preventDefault();
      finish(false);
    };
    const teardown = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', cancel);
      window.removeEventListener('keydown', key);
      document.body.classList.remove('observe-brushing');
      endDrag.current = null;
    };
    const finish = (apply: boolean) => {
      teardown();
      if (!active) return;
      if (apply && selection) brush.select(selection, token);
      else brush.store.set(null);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', cancel);
    window.addEventListener('keydown', key);
    endDrag.current = teardown;
  };

  // 光标只在绘图区内变成十字，坐标轴与标题区保持默认。
  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (endDrag.current || event.pointerType === 'touch') return;
    const measured = measure(chartRef, extentRef);
    plotRef.current?.toggleAttribute(
      'data-brush-hot',
      Boolean(measured && inside(measured.rect, localX(event), localY(event))),
    );
  };
  const onPointerLeave = () => plotRef.current?.removeAttribute('data-brush-hot');
  const onDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const measured = measure(chartRef, extentRef);
    if (!measured || !inside(measured.rect, localX(event), localY(event))) return;
    event.preventDefault();
    brush.zoomOut();
  };

  return (
    <div
      ref={plotRef}
      className={`${className} observe-plot`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerLeave={onPointerLeave}
      onDoubleClick={onDoubleClick}
    >
      <div ref={hostRef} className="observe-plot-canvas" />
      <div className="observe-brush-layer" aria-hidden="true">
        <div ref={bandRef} className="observe-brush-band" hidden />
        <div ref={tagRef} className="observe-brush-tag" hidden />
      </div>
    </div>
  );
}

/** KPI 迷你趋势线上画出同一时段（浅色、无边线）。`rectRef` 指向 SVG 里的一个 rect，坐标系宽 `width`。 */
export function useObserveBrushSpark(
  rectRef: RefObject<SVGRectElement | null>,
  rangeStartUnixSecs: number,
  rangeEndUnixSecs: number,
  width: number,
) {
  const brush = useContext(BrushContext);
  useEffect(() => {
    if (!brush) return;
    const paint = () => {
      const rect = rectRef.current;
      if (!rect) return;
      const state = brush.store.get();
      const span = rangeEndUnixSecs - rangeStartUnixSecs;
      if (!state || span <= 0) {
        rect.setAttribute('visibility', 'hidden');
        return;
      }
      const at = (unixSecs: number) => Math.min(1, Math.max(0, (unixSecs - rangeStartUnixSecs) / span)) * width;
      const left = at(state.selection.startUnixSecs);
      rect.setAttribute('x', left.toFixed(2));
      rect.setAttribute('width', Math.max(0.3, at(state.selection.endUnixSecs) - left).toFixed(2));
      rect.setAttribute('visibility', 'visible');
    };
    paint();
    return brush.store.subscribe(paint);
  }, [brush, rectRef, rangeStartUnixSecs, rangeEndUnixSecs, width]);
}
