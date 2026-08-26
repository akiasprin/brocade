// 产物面板的状态。它既不是窗口也不属于画布——是工作台右侧的独立面板，
// 在两个台面上都存在（配置输出适合侧栏形态）。
// 因此状态保存在此处，而非放入窗口管理器。

import { isNarrow } from './viewport';

export interface ArtifactSel {
  targetKind: string;
  targetId: string;
  artifactKind: string;
}

export interface ArtifactPanelState {
  open: boolean;
  sel: ArtifactSel | null;
}

const KEY = 'brocade-console:artifact-panel';

class ArtifactPanelStore {
  // 默认收起。产物是修改后的编译结果，用于核对而非编辑——进入控制台的主要目的是修改模型，
  // 默认占用三分之一台面显示未被查看的 json 会压缩中间栏的宽度。
  // 需要查看时通过顶栏按钮打开，或从检视页点击某一份进入（`show` 会自动展开）。
  private state: ArtifactPanelState = { open: false, sel: null };
  private listeners = new Set<() => void>();

  constructor() {
    // 窄屏下该面板是全屏的，进入时展开会遮挡整个控制台。
    // 因此移动端一律以收起状态开始，不读取存储的状态——存储的状态通常是桌面端的
    // 侧栏展开偏好，在此含义不同。需要查看产物时从 ☰ 菜单打开。
    if (isNarrow()) return;
    try {
      const saved = localStorage.getItem(KEY);
      /* 记录手动展开过的状态——默认收起不表示不允许展开，而是不自动展开。 */
      if (saved === 'open') this.state = { open: true, sel: null };
    } catch {
      /* 写入失败时使用默认值：收起 */
    }
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): ArtifactPanelState => this.state;

  /* 在检视窗中点击某份产物：面板未展开时展开并选中该产物 */
  show(sel: ArtifactSel) {
    this.state = { open: true, sel };
    this.commit();
  }

  select(sel: ArtifactSel) {
    this.state = { ...this.state, sel };
    this.commit();
  }

  toggle() {
    this.state = { ...this.state, open: !this.state.open };
    this.commit();
  }

  close() {
    this.state = { ...this.state, open: false };
    this.commit();
  }

  private commit() {
    try {
      localStorage.setItem(KEY, this.state.open ? 'open' : 'closed');
    } catch {
      /* 写入失败时不做处理 */
    }
    for (const listener of this.listeners) listener();
  }
}

export const artifactPanel = new ArtifactPanelStore();
