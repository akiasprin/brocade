// 编译台的外壳状态：当前所在页面、诊断气泡是否展开。
// 对象下钻保存在 wm.data.drill；产物栏状态由 ui/artifact-panel.ts 管理。

import { useSyncExternalStore } from 'react';

/** 顶栏导航的键。主工作流显示在顶栏上，其余收入「⋯」。 */
export type NavKey =
  | 'nodes'
  | 'chains'
  | 'tunnels'
  | 'users'
  | 'deploy'
  | 'topo'
  | 'settings'
  | 'usage'
  // 修改自身的登录密码。它不是功能页面而是身份区的操作，因此顶栏和「⋯」的
  // 页面列表中都不包含它——入口位于「⋯」的下半部分，与退出相邻。
  | 'password';

// 同一组键的运行时副本：地址栏中读取的是任意字符串，需要用它判断是否为有效页面。
// 顺序不影响结果，此处不决定顶栏的排列（排列由 forge/shell.tsx 的 NAV / MORE 决定）。
export const NAV_KEYS = [
  'nodes',
  'chains',
  'tunnels',
  'users',
  'deploy',
  'topo',
  'settings',
  'usage',
  'password',
] as const satisfies readonly NavKey[];

export const isNavKey = (value: string): value is NavKey => (NAV_KEYS as readonly string[]).includes(value);

export interface ForgeState {
  nav: NavKey;
  /* 诊断气泡。展开状态不持久化——刷新后应为收起，因此不写入 localStorage。 */
  diag: boolean;
}

export const DEFAULT_NAV: NavKey = 'nodes';

class ForgeStore {
  private state: ForgeState = { nav: DEFAULT_NAV, diag: false };
  private listeners = new Set<() => void>();

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): ForgeState => this.state;

  setNav(nav: NavKey) {
    if (this.state.nav === nav && !this.state.diag) return;
    this.state = { ...this.state, nav, diag: false };
    this.emit();
  }

  setDiag(diag: boolean) {
    if (this.state.diag === diag) return;
    this.state = { ...this.state, diag };
    this.emit();
  }

  toggleDiag() {
    this.setDiag(!this.state.diag);
  }

  private emit() {
    for (const listener of this.listeners) listener();
  }
}

export const forge = new ForgeStore();

export function useForge(): ForgeState {
  return useSyncExternalStore(forge.subscribe, forge.snapshot);
}
