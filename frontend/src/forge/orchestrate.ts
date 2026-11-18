// 容器版把「线路」「容器」并成顶栏的一个入口「编排」。
// 两页仍是各自的 NavKey：地址、窗口名、权限与页面结构都不变，分组只决定顶栏怎么呈现与点击后去哪里。

import type { NavKey } from './state';

/** 组内顺序即子页切换的顺序：线路优先。 */
export const ORCHESTRATE_MEMBERS = ['chains', 'cvm'] as const satisfies readonly NavKey[];
export type OrchestrateMember = (typeof ORCHESTRATE_MEMBERS)[number];

export const ORCHESTRATE_LABEL = '编排';

const STORAGE_KEY = 'brocade-console:orchestrate-view';

export const isOrchestrateMember = (nav: NavKey): nav is OrchestrateMember =>
  (ORCHESTRATE_MEMBERS as readonly NavKey[]).includes(nav);

/** 从别的页面点「编排」时回到上次停留的子页；没有记录或存储不可用时进入线路。 */
export function lastOrchestrateView(): OrchestrateMember {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved === 'cvm' ? 'cvm' : 'chains';
  } catch {
    return 'chains';
  }
}

export function rememberOrchestrateView(nav: OrchestrateMember) {
  try {
    localStorage.setItem(STORAGE_KEY, nav);
  } catch {
    /* Preference storage may be unavailable; the entry then falls back to 线路. */
  }
}
