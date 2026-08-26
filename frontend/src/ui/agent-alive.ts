// 判断机器的 agent 是否在线——纳管流程和 preview 两处使用同一实现。
//
// 提取为公共函数是因为该逻辑原本在两个文件中完全重复，包括注释。同一逻辑实现两份时，
// 修改其中一处会导致同一台机器在两个页面上显示不同状态。
//
// 使用三档而非两档：尚未使用 token 与已使用但无心跳需要区分——前者需要在机器上
// 执行命令，后者需要等待或排查，后续操作不同。
import { useNow } from './clock';
import type { NodeAgentStateItem } from '../api';

// 超过该时间没有心跳即判定为掉线。agent 的 APPLY 周期为 15 秒，此处留四倍余量。
const STALE_MS = 60_000;

export type AgentLiveness =
  { state: 'waiting' } | { state: 'polling'; onceOnline: boolean } | { state: 'online'; agoSec: number };

/** 传入 `undefined` 表示该机器尚未出现在列表中，返回 `null`。 */
export function useAgentLiveness(node: NodeAgentStateItem | undefined): AgentLiveness | null {
  // 使用 useNow 而非 Date.now()：该值表示距上次上报的时长，每秒变化，
  // 而在并发渲染下渲染期读取时钟可能在同一次提交中得到两个不同的值。
  const now = useNow();

  if (!node) return null;
  if (!node.token_last_used_at) return { state: 'waiting' };

  const poll = node.last_poll_at ? Date.parse(withZone(node.last_poll_at)) : NaN;
  if (Number.isNaN(poll) || now - poll > STALE_MS) {
    // 是否上报过依据 last_poll_at 判断，不由组件自行记录。服务端在每次拉取配置时写入该
    // 字段，因此它是完整的事实；而组件挂载期间记录的只是本次页面打开期间观察到的心跳——
    // 十分钟前上报过、当前掉线的机器会被判定为尚未启动，与实际相反。
    return { state: 'polling', onceOnline: !Number.isNaN(poll) };
  }
  return { state: 'online', agoSec: Math.max(0, Math.round((now - poll) / 1000)) };
}

// 服务端返回的是不带时区的本地时间字符串，按 UTC 解析。已带 Z 或 ±HH:MM 的保持原样。
function withZone(at: string): string {
  return at.endsWith('Z') || at.includes('+') ? at : `${at}Z`;
}
