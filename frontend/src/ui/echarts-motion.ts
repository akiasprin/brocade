import { useEffect, useState, type RefObject } from 'react';

const ENTER_THRESHOLD = 0.35;
const ANIMATION_POINT_THRESHOLD = 10_000;

/**
 * Delay the first ECharts data paint until the canvas is actually visible. This keeps charts farther
 * down a detail page from finishing their entrance animation before the user scrolls to them.
 */
export function useEchartsViewportEntry(elementRef: RefObject<HTMLElement | null>): boolean {
  const [enteredViewport, setEnteredViewport] = useState(() => typeof IntersectionObserver === 'undefined');

  useEffect(() => {
    const element = elementRef.current;
    if (!element || enteredViewport) return;
    if (typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver(
      entries => {
        if (!entries.some(entry => entry.isIntersecting && entry.intersectionRatio >= ENTER_THRESHOLD)) return;
        observer.disconnect();
        setEnteredViewport(true);
      },
      { threshold: [ENTER_THRESHOLD] },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [elementRef, enteredViewport]);

  return enteredViewport;
}

/** Initial data sweeps in once; polling, theme changes, and later data updates remain still. */
export function echartsEntranceAnimation(firstDataPaint: boolean) {
  const reduceMotion =
    typeof window !== 'undefined' && window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
  const animate = firstDataPaint && !reduceMotion;
  return {
    animation: animate,
    // ECharts defaults to 2,000 and silently disables animation above it. One 24-hour Ping
    // series contains up to 8,640 exact 10-second samples, so keep the entrance sweep enabled
    // without sampling or changing the data rendered by the chart.
    animationThreshold: ANIMATION_POINT_THRESHOLD,
    animationDuration: animate ? 360 : 0,
    animationEasing: 'cubicInOut' as const,
    animationDurationUpdate: 0,
    animationEasingUpdate: 'linear' as const,
  };
}
