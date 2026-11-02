import { useCallback, useEffect, useRef, type PointerEvent as ReactPointerEvent } from 'react';

/** Touch browsers may defer :active until release. Own only the transient visual state;
 * native scrolling, clicks, checkbox activation and mouse drag ordering stay in control. */
export function useCardPressFeedback() {
  const cleanup = useRef<(() => void) | null>(null);
  useEffect(() => () => cleanup.current?.(), []);
  return useCallback((event: ReactPointerEvent<HTMLElement>) => {
    cleanup.current?.();
    if (event.pointerType === 'mouse' || !event.isPrimary) return;
    const card = event.currentTarget;
    const { pointerId, clientX, clientY } = event;
    const clear = () => {
      card.removeAttribute('data-card-pointer-pressed');
      document.removeEventListener('pointermove', move, true);
      document.removeEventListener('pointerup', end, true);
      document.removeEventListener('pointercancel', end, true);
      document.removeEventListener('scroll', clear, true);
      document.removeEventListener('visibilitychange', visibility);
      window.removeEventListener('blur', clear);
      cleanup.current = null;
    };
    const move = (next: PointerEvent) => {
      if (next.pointerId === pointerId && Math.hypot(next.clientX - clientX, next.clientY - clientY) > 8) clear();
    };
    const end = (next: PointerEvent) => {
      if (next.pointerId === pointerId) clear();
    };
    const visibility = () => {
      if (document.visibilityState === 'hidden') clear();
    };
    card.setAttribute('data-card-pointer-pressed', 'true');
    cleanup.current = clear;
    document.addEventListener('pointermove', move, true);
    document.addEventListener('pointerup', end, true);
    document.addEventListener('pointercancel', end, true);
    document.addEventListener('scroll', clear, true);
    document.addEventListener('visibilitychange', visibility);
    window.addEventListener('blur', clear);
  }, []);
}
