// 窗口管理器：不依赖框架的状态模块，React 只按快照渲染窗框。
// 规则：窗口按 key 去重，重复打开时只置顶不新建；窗口归属于台面，切换台面时整层隐藏、
// 切回时恢复；拖动过程只修改 DOM 样式，pointerup 时才提交到状态。
//
// 窄屏（见 ui/viewport.ts）下不使用窗口形态：一次只显示 active 的一个，
// 填满顶栏与底部 tab 之间的区域，不支持拖动和高度测量。窗口仍正常创建和存在，
// 因为 win.data 保存窗内导航状态（列表 ↔ 详情 ↔ 向导），不使用的只是几何属性。

import { isNarrow } from '../ui/viewport';

export type Floor = 'desk' | 'topo';

export interface Win {
  id: number;
  /* 去重标识：功能窗为 tab:nodes，检视窗为 node:hk-01 这类 */
  key: string;
  title: string;
  x: number;
  y: number;
  w: number;
  h: number;
  z: number;
  min: boolean;
  /* 创建时所在的台面；可见条件为 home === floor && !min */
  home: Floor;
  // 窗内导航状态（列表 ↔ 详情 ↔ 向导）。保存在此而非组件内，使切换台面后窗口被过滤再切回时
  // 仍显示原有页面。其中有两个约定键：`drill` 是面板私有的下钻状态，结构由各面板定义、
  // 外壳不解析；`crumb` 见 `CrumbSeg`，外壳据此渲染顶部面包屑。
  data: Record<string, unknown>;
}

// 面包屑的一段。面板写入 `win.data.crumb`，外壳据此渲染。
// *
// * 外壳不解析 `drill` 的内容，只在点击时将该值原样写回 `win.data.drill`。
// * 因此新增面板不需要修改外壳——否则外壳需要逐个解析各面板的私有类型，
// * 每增加一层下钻都需要两侧同时修改。
// *
// * 不带 `drill` 的段表示当前位置，不可点击。顶层那一段（机器、项目等）由外壳补全，
// * 面板只写入自身的下钻层级。
export interface CrumbSeg {
  label: string;
  drill?: unknown;
}

export interface WmSnapshot {
  wins: readonly Win[];
  floor: Floor;
  activeId: number | null;
}

export interface OpenOptions {
  w?: number;
  h?: number;
}

/* 顶栏下边界：窗口标题栏不得进入顶栏下方 */
const TOP_MIN = 64;
/* 拖出视口时至少保留该宽度，以便重新拖回 */
const EDGE_KEEP = 90;
/* 内容为空的窗口也需保证标题栏可见 */
const MIN_H = 120;
// 窄屏下新建窗口使用的宽度。不能按 innerWidth 计算：几何属性会按 key 存入 localStorage，
// 在移动端打开过的页面在桌面端会变为 270px 宽，且该状态会持久保留。
const DESK_DEFAULT_W = 880;

interface PersistedLayout {
  floor?: Floor;
  rects?: Record<string, { x: number; y: number; w: number; h: number }>;
}

class WindowManager {
  private wins: Win[] = [];
  private floor: Floor = 'desk';
  private activeId: number | null = null;
  private seq = 0;
  private storageKey: string | null = null;
  private saved: PersistedLayout = {};
  private listeners = new Set<() => void>();
  private snap: WmSnapshot = { wins: [], floor: 'desk', activeId: null };

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): WmSnapshot => this.snap;

  // 登录时调用：布局按操作者分键恢复，切换用户时重新开始。
  // 同一用户重复调用时为空操作：init 会清空 wins，而 React 的子组件 effect 先于
  // 父组件 effect 执行——外壳在父组件中调用 init、工作区在子组件中打开窗口，
  // 没有该保护时刚打开的窗口会被随后的 init 清除，而子组件的依赖未变化不会再次打开，
  // 界面会停留在加载状态。切换用户（operator 变化）时仍然整体重新初始化。
  init(operator: string) {
    /* 键中包含布局版本：默认窗口形态变更时递增版本，旧布局整体失效 */
    const key = `brocade-console:wm:v4:${operator}`;
    if (this.storageKey === key) return;
    this.storageKey = key;
    this.wins = [];
    this.activeId = null;
    this.saved = {};
    try {
      this.saved = JSON.parse(localStorage.getItem(key) ?? '{}') as PersistedLayout;
    } catch {
      /* 存储的布局数据损坏时重新初始化 */
    }
    this.floor = this.saved.floor === 'topo' ? 'topo' : 'desk';
    this.commit();
  }

  open(key: string, title: string, options: OpenOptions = {}): Win {
    let win = this.wins.find(w => w.key === key);
    if (!win) {
      const w = options.w ?? (isNarrow() ? DESK_DEFAULT_W : Math.min(880, window.innerWidth - 120));
      /* 初始值只用于首帧，渲染完成后 fitHeight 会按实际内容调整 */
      const h = options.h ?? MIN_H;
      const cascade = this.wins.filter(x => x.home === this.floor).length % 4;
      const rect = this.saved.rects?.[key] ?? {
        x: 96 + cascade * 46,
        y: 112 + cascade * 34,
        w,
        h,
      };
      win = { id: ++this.seq, key, title, ...rect, z: 0, min: false, home: this.floor, data: {} };
      this.clampWin(win);
      this.wins.push(win);
    }
    win.min = false;
    win.title = title;
    this.raise(win);
    this.commit();
    return win;
  }

  // 高度随内容变化：窗口不应是包含少量内容的大容器。
  // 只调整高度不调整宽度——宽度变化会导致内容重排，重排又改变高度，形成循环。
  fitHeight(id: number, needed: number) {
    /* 窄屏下窗口高度由 CSS 决定，测量结果不被使用，且会存入布局并影响桌面端 */
    if (isNarrow()) return;
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    const max = Math.max(MIN_H, window.innerHeight - win.y - 16);
    const next = Math.round(Math.min(Math.max(needed, MIN_H), max));
    if (Math.abs(next - win.h) < 2) return;
    win.h = next;
    this.commit();
  }

  // 只修改传入的键。`setData` 要求调用方展开完整的 data，而 effect 中的 data
  // 通常来自上一轮渲染的闭包——用它覆盖会清除其他位置刚写入的字段。
  patchData(id: number, patch: Record<string, unknown>) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    win.data = { ...win.data, ...patch };
    this.commit();
  }

  /* 窗内导航：整体替换该窗口的 data（就地重渲染，不创建子窗口） */
  setData(id: number, data: Record<string, unknown>) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    win.data = data;
    this.commit();
  }

  bring(id: number) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    this.raise(win);
    this.commit();
  }

  minimize(id: number) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    win.min = true;
    if (this.activeId === id) this.activeId = null;
    this.commit();
  }

  restore(id: number) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    win.min = false;
    this.raise(win);
    this.commit();
  }

  close(id: number) {
    this.wins = this.wins.filter(w => w.id !== id);
    if (this.activeId === id) this.activeId = this.wins.at(-1)?.id ?? null;
    this.commit();
  }

  setFloor(floor: Floor) {
    if (this.floor === floor) return;
    this.floor = floor;
    this.commit();
  }

  moveTo(id: number, x: number, y: number) {
    const win = this.wins.find(w => w.id === id);
    if (!win) return;
    const p = this.clampPos(win, x, y);
    win.x = p.x;
    win.y = p.y;
    this.commit();
  }

  clampAll() {
    // 窄屏下不按几何属性布局窗口。此处不提前 return 时，移动端软键盘每次弹出都会触发 resize，
    // 将每个窗口的 x 限制到 innerWidth-90 并存储——导致桌面端布局被移动端修改。
    if (isNarrow()) return;
    for (const win of this.wins) this.clampWin(win);
    this.commit();
  }

  clampPos(win: { w: number }, x: number, y: number): { x: number; y: number } {
    return {
      x: Math.min(Math.max(-win.w + EDGE_KEEP, x), window.innerWidth - EDGE_KEEP),
      y: Math.min(Math.max(TOP_MIN, y), window.innerHeight - 46),
    };
  }

  private clampWin(win: Win) {
    if (isNarrow()) return;
    const p = this.clampPos(win, win.x, win.y);
    win.x = p.x;
    win.y = p.y;
  }

  private raise(win: Win) {
    win.z = 20 + ++this.seq;
    this.activeId = win.id;
  }

  private commit() {
    if (this.storageKey) {
      const rects = { ...(this.saved.rects ?? {}) };
      for (const w of this.wins) rects[w.key] = { x: w.x, y: w.y, w: w.w, h: w.h };
      this.saved = { floor: this.floor, rects };
      try {
        localStorage.setItem(this.storageKey, JSON.stringify(this.saved));
      } catch {
        /* 写入失败时不做处理，布局丢失不影响功能 */
      }
    }
    this.snap = {
      wins: this.wins.map(w => ({ ...w })),
      floor: this.floor,
      activeId: this.activeId,
    };
    for (const listener of this.listeners) listener();
  }
}

export const wm = new WindowManager();

/* 供冒烟脚本和调试访问（与 playground 全局 S 的做法一致） */
if (typeof window !== 'undefined') {
  (window as unknown as { __wm?: WindowManager }).__wm = wm;
}
