import {
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type RefObject,
} from 'react';

type Offset = { x: number; y: number };
type Gesture = {
  id: string;
  pointer: number;
  handle: HTMLButtonElement;
  start: Offset;
  scroll: Offset;
  origin: Offset;
  minimum: Offset;
  moved: boolean;
};
const DIRECTIONS: Record<string, Offset> = {
  ArrowLeft: { x: -1, y: 0 },
  ArrowRight: { x: 1, y: 0 },
  ArrowUp: { x: 0, y: -1 },
  ArrowDown: { x: 0, y: 1 },
};

/** View-only offsets: moving a card never changes machine order, rules, or the draft. */
export function useFlowCardDrag(
  well: RefObject<HTMLDivElement | null>,
  machines: readonly { id: string }[],
  compact: boolean,
) {
  const [offsets, setOffsets] = useState<Record<string, Offset>>({});
  const [dragging, setDragging] = useState<string | null>(null);
  const gesture = useRef<Gesture | null>(null);
  const moved = useRef(false);
  const ids = useMemo(() => new Set(machines.map(machine => machine.id)), [machines]);

  const place = (id: string, offset: Offset) => {
    if (!ids.has(id)) return;
    setOffsets(current => {
      if (current[id]?.x === offset.x && current[id]?.y === offset.y) return current;
      const next: Record<string, Offset> = {};
      for (const key of ids) if (current[key]) next[key] = current[key];
      next[id] = offset;
      return next;
    });
  };

  const minimumFor = (handle: HTMLButtonElement, origin: Offset): Offset | null => {
    const root = well.current;
    const card = handle.closest<HTMLElement>('.flow-machine');
    const columns = root?.querySelector<HTMLElement>('.flow-columns');
    if (!root || !card || !columns) return null;
    const bounds = root.getBoundingClientRect();
    const rect = card.getBoundingClientRect();
    return {
      x:
        parseFloat(getComputedStyle(root).paddingLeft) -
        (rect.left - bounds.left - root.clientLeft + root.scrollLeft - origin.x),
      y: columns.offsetTop - (rect.top - bounds.top - root.clientTop + root.scrollTop - origin.y),
    };
  };

  const finish = (revert: boolean) => {
    const active = gesture.current;
    if (!active) return;
    gesture.current = null;
    if (revert && active.moved) place(active.id, active.origin);
    setDragging(null);
    if (active.handle.hasPointerCapture(active.pointer)) active.handle.releasePointerCapture(active.pointer);
  };

  const start = (id: string, event: PointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0 || !event.isPrimary || gesture.current) return;
    moved.current = false;
    if (compact) return;
    const root = well.current;
    const origin = offsets[id] ?? { x: 0, y: 0 };
    const minimum = minimumFor(event.currentTarget, origin);
    if (!root || !minimum) return;
    gesture.current = {
      id,
      pointer: event.pointerId,
      handle: event.currentTarget,
      start: { x: event.clientX, y: event.clientY },
      scroll: { x: root.scrollLeft, y: root.scrollTop },
      origin,
      minimum,
      moved: false,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const move = (event: PointerEvent<HTMLButtonElement>) => {
    const active = gesture.current;
    const root = well.current;
    if (!active || active.pointer !== event.pointerId || !root) return;
    if (compact) {
      finish(true);
      return;
    }
    const dx = event.clientX - active.start.x + root.scrollLeft - active.scroll.x;
    const dy = event.clientY - active.start.y + root.scrollTop - active.scroll.y;
    if (!active.moved && Math.hypot(dx, dy) < 4) return;
    event.preventDefault();
    if (!active.moved) active.handle.focus({ preventScroll: true });
    active.moved = true;
    moved.current = true;
    setDragging(active.id);
    place(active.id, {
      x: Math.max(active.minimum.x, active.origin.x + dx),
      y: Math.max(active.minimum.y, active.origin.y + dy),
    });
  };

  const end = (event: PointerEvent<HTMLButtonElement>) => {
    if (gesture.current?.pointer === event.pointerId) finish(false);
  };
  const cancel = (event: PointerEvent<HTMLButtonElement>) => {
    if (gesture.current?.pointer === event.pointerId) finish(true);
  };

  const keyDown = (id: string, event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === 'Escape' && gesture.current?.id === id) {
      event.preventDefault();
      finish(true);
      return;
    }
    if (compact || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === 'Home') {
      event.preventDefault();
      place(id, { x: 0, y: 0 });
      return;
    }
    const direction = DIRECTIONS[event.key];
    if (!direction) return;
    const origin = offsets[id] ?? { x: 0, y: 0 };
    const minimum = minimumFor(event.currentTarget, origin);
    if (!minimum) return;
    event.preventDefault();
    const step = event.shiftKey ? 1 : 10;
    place(id, {
      x: Math.max(minimum.x, origin.x + direction.x * step),
      y: Math.max(minimum.y, origin.y + direction.y * step),
    });
  };

  const consumeClick = (event: MouseEvent<HTMLButtonElement>) => {
    const skip = moved.current && event.detail > 0;
    moved.current = false;
    return skip;
  };

  return { offsets, dragging, start, move, end, cancel, keyDown, consumeClick };
}
