import { useEffect, useRef, useSyncExternalStore, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react';
import { wm, type Win } from '../wm/store';
import { useNarrow } from './viewport';

export function useWm() {
  return useSyncExternalStore(wm.subscribe, wm.snapshot);
}

// `filter` 供新外壳使用：编译台的功能页面本身也是 wm 中的窗口（只是不渲染窗框），
// 在拓扑视图打开检视窗时需要将它们过滤掉，否则会重复渲染相同内容。
// 不传入时保持旧外壳的行为。
export function WinLayer({ render, filter }: { render: (win: Win) => ReactNode; filter?: (win: Win) => boolean }) {
  const snap = useWm();
  const narrow = useNarrow();

  useEffect(() => {
    const onResize = () => wm.clampAll();
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  const visible = snap.wins.filter(w => w.home === snap.floor && !w.min && (!filter || filter(w)));
  const topmost = (pool: Win[]) => pool.reduce<Win | null>((top, w) => (!top || w.z > top.z ? w : top), null);
  // 窄屏下一次只显示一个：优先显示当前活动窗口，活动窗口关闭后取 z 值最高的。
  // 其余窗口不被销毁，只是不渲染——窗内导航状态（win.data）保持不变。
  const shown = narrow ? (visible.find(w => w.id === snap.activeId) ?? topmost(visible)) : null;
  // 检视窗在窄屏下是覆盖在下层之上的半屏面板，因此下层需要一并渲染，
  // 否则半屏面板下方显示为空白台面，会被理解为渲染异常。
  const backdrop =
    shown && !shown.key.startsWith('tab:') ? topmost(visible.filter(w => w.key.startsWith('tab:'))) : null;
  const list = narrow ? [backdrop, shown].filter((w): w is Win => !!w) : visible;

  return (
    <div id="winlayer" className={narrow ? 'narrow' : undefined}>
      {list.map(w => (
        <WindowFrame key={w.id} win={w} active={snap.activeId === w.id} narrow={narrow} render={render} />
      ))}
    </div>
  );
}

function WindowFrame({
  win,
  active,
  narrow,
  render,
}: {
  win: Win;
  active: boolean;
  narrow: boolean;
  render: (win: Win) => ReactNode;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const fitter = useRef<HTMLDivElement>(null);

  // 窗口高度等于内容高度。测量的是内容层（高度不受限），而非窗体的 body——
  // body 的 scrollHeight 始终不小于窗口高度，以它测量会导致窗口只增不减。
  // 窄屏下窗口高度由 CSS 决定，不进行测量。
  useEffect(() => {
    if (narrow) return;
    const content = fitter.current;
    if (!content) return;
    const measure = () => {
      const head = content.parentElement?.previousElementSibling?.getBoundingClientRect().height ?? 0;
      const pad = content.parentElement ? parseFloat(getComputedStyle(content.parentElement).paddingTop) * 2 : 0;
      wm.fitHeight(win.id, content.scrollHeight + head + pad + 2);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(content);
    return () => ro.disconnect();
  }, [win.id, narrow]);

  /* 拖动过程只修改 DOM 样式，释放时才将位置提交到状态 */
  const onHeadPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (narrow) return;
    if ((e.target as HTMLElement).closest('button, a, input, select, label')) return;
    const el = frame.current;
    if (!el) return;
    const head = e.currentTarget;
    const dx = e.clientX - win.x;
    const dy = e.clientY - win.y;
    let last = { x: win.x, y: win.y };
    head.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      last = wm.clampPos(win, ev.clientX - dx, ev.clientY - dy);
      el.style.left = `${last.x}px`;
      el.style.top = `${last.y}px`;
    };
    const up = () => {
      head.removeEventListener('pointermove', move);
      head.removeEventListener('pointerup', up);
      head.removeEventListener('pointercancel', up);
      wm.moveTo(win.id, last.x, last.y);
    };
    head.addEventListener('pointermove', move);
    head.addEventListener('pointerup', up);
    head.addEventListener('pointercancel', up);
  };

  const isTab = win.key.startsWith('tab:');
  const kind = isTab ? '功能窗' : '检视窗';
  // 窄屏下功能窗的显示由底部 tab bar 控制，标题栏不应再提供关闭按钮；
  // 检视窗和诊断窗是临时覆盖层，关闭按钮是唯一的退出方式，需要保留。
  const showClose = !narrow || !isTab;

  return (
    <div
      ref={frame}
      className={`fw ${isTab ? 'fw-tab' : 'fw-insp'}${active ? ' active' : ''}`}
      style={narrow ? { zIndex: win.z } : { left: win.x, top: win.y, width: win.w, height: win.h, zIndex: win.z }}
      onPointerDownCapture={() => {
        if (!active) wm.bring(win.id);
      }}
    >
      <div className="fw-head" onPointerDown={onHeadPointerDown}>
        <span className="fw-kind">{kind}</span>
        <span className="fw-title">
          {kind} · {win.title}
        </span>
        {!narrow && (
          <button title="最小化成顶栏小签" onClick={() => wm.minimize(win.id)}>
            —
          </button>
        )}
        {showClose && (
          <button title="关闭" onClick={() => wm.close(win.id)}>
            ✕
          </button>
        )}
      </div>
      <div className="fw-body">
        <div ref={fitter}>{render(win)}</div>
      </div>
    </div>
  );
}
