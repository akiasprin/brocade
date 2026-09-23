import { useLayoutEffect, useState, useSyncExternalStore } from 'react';
import { prefersReducedMotion } from './motion';

export type PresencePhase = 'entering' | 'entered' | 'exiting';

export interface PresenceState {
  present: boolean;
  phase: PresencePhase;
}

interface InternalPresenceState {
  source: boolean;
  present: boolean;
  phase: PresencePhase;
}

const subscribeToMotionPreference = (listener: () => void): (() => void) => {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return () => undefined;
  const media = window.matchMedia('(prefers-reduced-motion: reduce)');
  media.addEventListener('change', listener);
  return () => media.removeEventListener('change', listener);
};

/** Keep a transient surface mounted long enough to play its exit animation. */
export function usePresence(show: boolean, durationMs: number): PresenceState {
  const reduce = useSyncExternalStore(subscribeToMotionPreference, prefersReducedMotion, () => true);
  const [state, setState] = useState<InternalPresenceState>(() => ({
    source: show,
    present: show,
    phase: show && !reduce ? 'entering' : show ? 'entered' : 'exiting',
  }));

  useLayoutEffect(() => {
    let active = true;
    let timer: number | undefined;

    queueMicrotask(() => {
      if (!active) return;

      if (show) {
        setState({ source: true, present: true, phase: reduce ? 'entered' : 'entering' });
        if (!reduce) {
          timer = window.setTimeout(() => {
            setState(current => (current.source ? { ...current, phase: 'entered' } : current));
          }, durationMs);
        }
        return;
      }

      if (reduce) {
        setState({ source: false, present: false, phase: 'exiting' });
        return;
      }

      setState(current =>
        current.source || current.present ? { source: false, present: true, phase: 'exiting' } : current,
      );
      timer = window.setTimeout(() => {
        setState(current => (current.source ? current : { ...current, present: false }));
      }, durationMs);
    });

    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [durationMs, reduce, show]);

  if (show === state.source) return { present: state.present, phase: state.phase };
  if (show) return { present: true, phase: reduce ? 'entered' : 'entering' };
  return { present: !reduce && state.present, phase: 'exiting' };
}
