import { useEffect } from 'react';
import { wm, type CrumbSeg, type Win } from './store';

// 将当前所在层级同步给外壳顶部的面包屑。
// *
// * 通过监听 drill 同步，而非在 `go()` 中写入。下钻状态的来源不止点击：地址栏直达
// * （`#/nodes/node/sg-relay`）、浏览器后退、从其他面板跳转，都是直接写入
// * `win.data.drill`，不经过面板自身的 `go`。写在 `go` 中时，这些路径进入后
// * 面包屑会停留在上一层——表现为从列表进入时正确，刷新后上一层丢失。
export function useCrumb(win: Win, segs: CrumbSeg[]) {
  /* 依赖使用序列化后的值：`segs` 每次渲染都是新数组，按引用比较会导致每帧都写入一次。 */
  const key = JSON.stringify(segs);
  useEffect(() => {
    if (JSON.stringify(win.data.crumb ?? []) === key) return;
    wm.patchData(win.id, { crumb: JSON.parse(key) as CrumbSeg[] });
  }, [win.id, key, win.data.crumb]);
}
