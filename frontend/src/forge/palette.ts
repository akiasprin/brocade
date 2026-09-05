// 综合色彩方案选择。每套方案同时定义两个角色色与中性底色：
// action 建立交互秩序并包含选择反馈，data 只表达连续数据。
// CSS 定义位于 styles.css 的 `:root[data-palette='…']` 令牌块。
// 属性必须设置在 <html> 上，才能覆盖 :root 的视觉令牌。

export type Palette = 'jinzi' | 'dailan' | 'songlv' | 'oufen' | 'xuanmo';

// 新会话以「黛蓝」进入；用户主动选择后仍尊重本地偏好。
// 旧版 nocturne / celadon / iris / yuanqing / rongyan 值不在 PALETTES 中，read 会自动回退到黛蓝。
const DEFAULT_PALETTE: Palette = 'dailan';

type PaletteOption = {
  key: Palette;
  name: string;
  description: string;
  action: string;
};

// 色点只显示最能代表方案的操作色；完整的角色色关系由 CSS 令牌表达。
export const PALETTES: PaletteOption[] = [
  {
    key: 'jinzi',
    name: '堇紫',
    description: '紫藤操作 · 紫罗兰数据',
    action: '#a394e8',
  },
  {
    key: 'dailan',
    name: '黛蓝',
    description: '黛蓝操作 · 雾青数据',
    action: '#8ca6dc',
  },
  {
    key: 'songlv',
    name: '松绿',
    description: '松绿操作 · 雾蓝数据',
    action: '#9cbc84',
  },
  {
    key: 'oufen',
    name: '藕粉',
    description: '藕粉操作 · 灰蓝数据',
    action: '#d49ab4',
  },
  {
    key: 'xuanmo',
    name: '玄墨',
    description: '灰白操作 · 矿物色数据',
    action: '#d1d1cd',
  },
];

const KEY = 'brocade-console:palette';

const read = (): Palette => {
  try {
    const v = localStorage.getItem(KEY);
    return PALETTES.some(option => option.key === v) ? (v as Palette) : DEFAULT_PALETTE;
  } catch {
    return DEFAULT_PALETTE;
  }
};

class PaletteStore {
  private value: Palette = read();
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

  snapshot = (): Palette => this.value;

  set(next: Palette) {
    this.value = next;
    try {
      localStorage.setItem(KEY, next);
    } catch {
      /* 写入失败时不做处理，本次选择仍然生效 */
    }
    this.paint();
    this.emit();
  }

  private paint() {
    document.documentElement.dataset.palette = this.value;
  }

  private emit() {
    for (const listener of this.listeners) listener();
  }
}

export const palette = new PaletteStore();
