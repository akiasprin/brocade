// 编译台的外壳状态：当前所在页面、诊断气泡是否展开。
//
// v1 中此处保存的是左树的选中项。移除左树后，选路改由各页面自身的列表进入
// （机器页点击行下钻，链路页点击链），该状态已保存在 wm.data.drill 中，
// 外壳不再保存副本——同一状态保存两份会出现不一致。
//
// 产物栏的展开状态不在此处：它由 ui/artifact-panel.ts 管理，新旧外壳共用一份，
// 且写入 localStorage。

import { useSyncExternalStore } from 'react';

/** 顶栏导航的键。主工作流显示在顶栏上，其余收入「⋯」。 */
export type NavKey =
  | 'nodes'
  | 'chains'
  | 'users'
  | 'deploy'
  | 'topo'
  | 'settings'
  | 'tenants'
  | 'usage'
  | 'operators'
  | 'links'
  // 修改自身的登录密码。它不是功能页面而是身份区的操作，因此顶栏和「⋯」的
  // 页面列表中都不包含它——入口位于「⋯」的下半部分，与退出相邻。
  | 'password';

// 同一组键的运行时副本：地址栏中读取的是任意字符串，需要用它判断是否为有效页面。
// 顺序不影响结果，此处不决定顶栏的排列（排列由 forge/shell.tsx 的 NAV / MORE 决定）。
export const NAV_KEYS = [
  'nodes',
  'chains',
  'users',
  'deploy',
  'topo',
  'settings',
  'tenants',
  'usage',
  'operators',
  'links',
  'password',
] as const satisfies readonly NavKey[];

export const isNavKey = (value: string): value is NavKey => (NAV_KEYS as readonly string[]).includes(value);

export interface ForgeState {
  nav: NavKey;
  /* 诊断气泡。展开状态不持久化——刷新后应为收起，因此不写入 localStorage。 */
  diag: boolean;
}

const KEY = 'brocade-console:forge-nav';

class ForgeStore {
  private state: ForgeState = { nav: 'nodes', diag: false };
  private listeners = new Set<() => void>();

  constructor() {
    // 记录上次所在的页面——刷新后回到机器列表会要求每次重新导航。
    // 只记录所在页面，不记录页面内的层级：后者是临时状态。
    try {
      const saved = localStorage.getItem(KEY);
      if (saved && isNavKey(saved)) this.state = { ...this.state, nav: saved };
    } catch {
      /* 读取失败时使用默认值 */
    }
  }

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
    try {
      localStorage.setItem(KEY, nav);
    } catch {
      /* 写入失败时不做处理 */
    }
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
