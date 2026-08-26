// 明暗主题切换。
//
// 原定为默认暗色且不提供主题按钮——现已提供按钮，但默认仍为暗色，
// 按钮只负责记录用户的选择。
// 不跟随 `prefers-color-scheme`：本工具用于运维，在同一台设备上因时间不同而切换模式
// 的成本高于一次选择错误。
//
// 综合色彩方案可选：见 palette.ts（更多菜单里的色板），色值定义在 styles.css 的
// `[data-palette='…']` 令牌块。
//
// 属性设置在 <html> 上而非 body：CSS 变量需要在 :root 上覆盖，在 body 上无法覆盖。

export type Theme = 'dark' | 'light';

const KEY = 'brocade-console:theme';

const read = (): Theme => {
  try {
    return localStorage.getItem(KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
};

class ThemeStore {
  private value: Theme = read();
  private listeners = new Set<() => void>();

  constructor() {
    this.paint();
    /* 其他标签页修改后同步更新 */
    addEventListener('storage', e => {
      if (e.key !== KEY) return;
      this.value = read();
      this.paint();
      this.emit();
    });
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): Theme => this.value;

  toggle() {
    this.value = this.value === 'light' ? 'dark' : 'light';
    try {
      localStorage.setItem(KEY, this.value);
    } catch {
      /* 写入失败时不做处理，本次选择仍然生效 */
    }
    this.paint();
    this.emit();
  }

  private paint() {
    document.documentElement.dataset.theme = this.value;
  }

  private emit() {
    for (const listener of this.listeners) listener();
  }
}

export const theme = new ThemeStore();
