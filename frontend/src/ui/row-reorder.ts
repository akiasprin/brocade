import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';

type RowDrag = { source: string; target: string };

/** Pointer sorting only commits on release, so Escape and cancelled touch gestures leave edits intact. */
export function useRowReorder<T extends HTMLElement>(
  onMove: (source: string, target: string) => void,
  disabled: boolean,
) {
  const rootRef = useRef<T>(null);
  const cleanupRef = useRef<(() => void) | null>(null);
  const [drag, setDrag] = useState<RowDrag | null>(null);

  useEffect(() => () => cleanupRef.current?.(), []);

  const start = (event: ReactPointerEvent<HTMLElement>, source: string) => {
    const root = rootRef.current;
    if (disabled || !root || event.button !== 0 || event.isPrimary === false || cleanupRef.current) return;
    const rows = [...root.querySelectorAll<HTMLElement>('[data-reorder-id]')];
    if (rows.length < 2) return;
    event.preventDefault();
    const handle = event.currentTarget;
    handle.focus({ preventScroll: true });
    const pointer = event.pointerId;
    const originX = event.clientX;
    const originY = event.clientY;
    let lastY = originY;
    let started = false;
    let target = source;
    let frame = 0;
    let scrollBy = 0;
    let scroller: HTMLElement | null = root.parentElement;
    while (scroller) {
      const overflow = getComputedStyle(scroller).overflowY;
      if (/auto|scroll/.test(overflow) && scroller.scrollHeight > scroller.clientHeight) break;
      scroller = scroller.parentElement;
    }
    const scrollTarget = scroller ?? window;

    const locate = () => {
      // Rows stay in place during the gesture, avoiding target oscillation as the pointer rests.
      let distance = Infinity;
      for (const row of rows) {
        const rect = row.getBoundingClientRect();
        const next = Math.abs(lastY - (rect.top + rect.height / 2));
        if (next < distance) {
          distance = next;
          target = row.dataset.reorderId!;
        }
      }
      setDrag(current => (current?.source === source && current.target === target ? current : { source, target }));
    };

    const scroll = () => {
      frame = 0;
      if (!scrollBy) return;
      const before = scroller?.scrollTop ?? window.scrollY;
      scrollTarget.scrollBy(0, scrollBy);
      locate();
      if ((scroller?.scrollTop ?? window.scrollY) !== before) frame = requestAnimationFrame(scroll);
    };

    const move = (next: PointerEvent) => {
      if (next.pointerId !== pointer) return;
      if (!started && Math.hypot(next.clientX - originX, next.clientY - originY) < 4) return;
      started = true;
      next.preventDefault();
      lastY = next.clientY;
      locate();
      const bounds = scroller?.getBoundingClientRect();
      const top = bounds?.top ?? 0;
      const bottom = bounds?.bottom ?? window.innerHeight;
      scrollBy = lastY < top + 32 ? -12 : lastY > bottom - 32 ? 12 : 0;
      if (scrollBy && !frame) frame = requestAnimationFrame(scroll);
    };

    const cleanup = () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', release);
      window.removeEventListener('pointercancel', cancel);
      window.removeEventListener('keydown', key);
      window.removeEventListener('blur', cancel);
      cleanupRef.current = null;
    };
    const finish = (commit: boolean) => {
      cleanup();
      setDrag(null);
      if (commit && started && source !== target) onMove(source, target);
    };
    const release = (next: PointerEvent) => {
      if (next.pointerId === pointer) finish(true);
    };
    const cancel = () => finish(false);
    const key = (next: KeyboardEvent) => {
      if (next.key !== 'Escape') return;
      next.preventDefault();
      finish(false);
    };
    cleanupRef.current = cleanup;
    window.addEventListener('pointermove', move, { passive: false });
    window.addEventListener('pointerup', release);
    window.addEventListener('pointercancel', cancel);
    window.addEventListener('keydown', key);
    window.addEventListener('blur', cancel);
  };

  return { rootRef, drag, start };
}
