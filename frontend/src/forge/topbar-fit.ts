import { useLayoutEffect, type RefObject } from 'react';

// Measure actual content so long account names, status changes and the artifact rail obey the same
// priorities. The top bar must not become a CSS size container: that traps its account menu.
const FIT_STEPS = ['account', 'traffic', 'brand', 'meta', 'label'];
const MIN_GAP = 16;

export function useTopbarFit(ref: RefObject<HTMLDivElement | null>, narrow: boolean) {
  useLayoutEffect(() => {
    const top = ref.current;
    if (narrow || !top) return;
    const nav = top.querySelector<HTMLElement>('nav');
    const spacer = top.querySelector<HTMLElement>(':scope > .sp');
    if (!nav || !spacer) return;
    const fits = () =>
      nav.scrollWidth <= nav.clientWidth + 1 &&
      top.scrollWidth <= top.clientWidth + 1 &&
      spacer.getBoundingClientRect().width >= MIN_GAP;
    const fit = () => {
      const applied: string[] = [];
      top.dataset.fit = '';
      for (const step of FIT_STEPS) {
        if (fits()) break;
        applied.push(step);
        top.dataset.fit = applied.join(' ');
      }
    };
    fit();
    const resize = new ResizeObserver(fit);
    resize.observe(top);
    const content = new MutationObserver(fit);
    content.observe(top, { childList: true, subtree: true, characterData: true });
    return () => {
      resize.disconnect();
      content.disconnect();
      delete top.dataset.fit;
    };
  }, [ref, narrow]);
}
