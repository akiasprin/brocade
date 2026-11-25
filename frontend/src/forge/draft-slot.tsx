import { createContext, useContext, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

/* 顶栏正下方的草稿条位置。线路、机器的 DraftBar 直接渲染在这里；页面自带的待发布条（如容器）
   通过 DraftSlot 落到同一处，紧贴顶栏、与页面纸面分开，而不是挤进纸面顶部。
   没有外壳的场景（组件测试、独立预览）拿不到插槽，就地渲染。 */
export const DraftSlotContext = createContext<HTMLElement | null>(null);

export function DraftSlot({ children }: { children: ReactNode }) {
  const slot = useContext(DraftSlotContext);
  return slot ? createPortal(children, slot) : children;
}
