// 窄屏断点的唯一定义处。
// styles.css 中的 @media (max-width:820px) 必须与 NARROW_MAX 取值一致——布局需要在 JS 和
// CSS 两侧各判断一次（只渲染当前窗口这一行为无法由 CSS 表达），但两侧分别硬编码阈值会
// 产生不一致：修改其中一处会导致窗口已切换为窄屏布局而顶栏仍是桌面形态。
// 因此该数值只在此处定义一次，CSS 中有注释指向此处。
//
// 取 820 而非 768：iPad 竖屏（768）应使用窄屏布局，横屏（1024）保持桌面形态。
export const NARROW_MAX = 820;
export const NARROW_QUERY = `(max-width: ${NARROW_MAX}px)`;

import { useSyncExternalStore } from 'react';

const mql = (): MediaQueryList | null => (typeof window === 'undefined' ? null : window.matchMedia(NARROW_QUERY));

/* wm/store.ts 这类不在 React 中的模块直接调用它 */
export function isNarrow(): boolean {
  return mql()?.matches ?? false;
}

function subscribe(onChange: () => void): () => void {
  const m = mql();
  if (!m) return () => {};
  m.addEventListener('change', onChange);
  return () => m.removeEventListener('change', onChange);
}

export function useNarrow(): boolean {
  return useSyncExternalStore(subscribe, isNarrow, () => false);
}
