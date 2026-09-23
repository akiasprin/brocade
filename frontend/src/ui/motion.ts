import { flushSync } from 'react-dom';

export type MotionTransitionKind = 'appearance' | 'theme-dark' | 'theme-light';

export interface MotionOrigin {
  x: number;
  y: number;
}

let activeTransition: ViewTransition | null = null;
let transitionSequence = 0;

const clearTransitionState = () => {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  delete root.dataset.motionTransition;
  root.style.removeProperty('--motion-origin-x');
  root.style.removeProperty('--motion-origin-y');
  root.style.removeProperty('--motion-reveal-radius');
};

/** Stop an appearance snapshot before unrelated page content changes underneath it. */
export function cancelVisualTransition(): void {
  transitionSequence += 1;
  activeTransition?.skipTransition();
  activeTransition = null;
  clearTransitionState();
}

export function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined' && window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
}

/**
 * Resolve a pointer origin while keeping keyboard-triggered transitions anchored to the control.
 * Synthetic keyboard clicks report (0, 0), which would otherwise make the reveal start in the
 * viewport corner instead of at the button that owns the action.
 */
export function motionOriginFor(target: Element, clientX: number, clientY: number): MotionOrigin {
  if (clientX !== 0 || clientY !== 0) return { x: clientX, y: clientY };
  const rect = target.getBoundingClientRect();
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
}

/**
 * Use the browser's snapshot transition when it is available. State still changes immediately on
 * the fallback path, so theme and appearance controls do not depend on animation support.
 */
export function runVisualTransition(update: () => void, kind: MotionTransitionKind, origin?: MotionOrigin): void {
  if (typeof document === 'undefined') {
    update();
    return;
  }

  if (!document.startViewTransition || prefersReducedMotion() || document.visibilityState === 'hidden') {
    update();
    return;
  }

  activeTransition?.skipTransition();
  const sequence = ++transitionSequence;
  const root = document.documentElement;
  root.dataset.motionTransition = kind;

  if (origin && typeof window !== 'undefined') {
    const x = Math.min(Math.max(origin.x, 0), window.innerWidth);
    const y = Math.min(Math.max(origin.y, 0), window.innerHeight);
    const radius = Math.hypot(Math.max(x, window.innerWidth - x), Math.max(y, window.innerHeight - y));
    root.style.setProperty('--motion-origin-x', `${Math.round(x)}px`);
    root.style.setProperty('--motion-origin-y', `${Math.round(y)}px`);
    root.style.setProperty('--motion-reveal-radius', `${Math.ceil(radius)}px`);
  }

  let updated = false;
  const apply = () => {
    updated = true;
    // The browser captures its new snapshot as soon as this callback returns. React may otherwise
    // defer an external-store or state update until the event completes, yielding two identical
    // snapshots followed by an abrupt live-DOM change instead of a transition.
    flushSync(update);
  };

  try {
    const transition = document.startViewTransition(apply);
    activeTransition = transition;
    void transition.finished
      .catch(() => undefined)
      .finally(() => {
        if (sequence !== transitionSequence) return;
        activeTransition = null;
        clearTransitionState();
      });
  } catch {
    if (!updated) update();
    if (sequence === transitionSequence) {
      activeTransition = null;
      clearTransitionState();
    }
  }
}
