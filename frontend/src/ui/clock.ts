// 计时器：所有相对时间显示共用同一个 interval。
// *
// * 相对时间原本在渲染时计算一次，因此只在其他原因触发重渲染时更新——
// * 机器页每 5 秒 refetch 一次，该值即每五秒跳变一次，中间保持不变，表现为界面无响应。
// *
// * 只使用一个定时器而非每个组件一个：一屏十六台机器会产生十六个 setInterval，
// * 且各自的相位不同——同一秒内部分已更新、部分未更新，同一列数值会出现不一致。
// * 共用一个后所有数值在同一帧内更新。
// *
// * 没有订阅者时定时器自动停止：该定时器挂在窗口上，不应在无使用方时仍每秒执行。
import { useSyncExternalStore } from 'react';

let now = Date.now();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (timer === null) {
    timer = setInterval(() => {
      now = Date.now();
      for (const fn of listeners) fn();
    }, 1000);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

const snapshot = () => now;

/** 每秒更新一次的当前时间。用它计算相对时间，数值会自动更新。 */
export function useNow(): number {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}
