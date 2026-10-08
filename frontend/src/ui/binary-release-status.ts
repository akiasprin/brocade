import type { BinaryReleaseTargetStatus, BinaryReleaseStatus, BinaryVerification } from '../api';
type ReleaseTone = 'ok' | 'warn' | 'run' | 'err' | 'none';
export const BINARY_TARGET_STATUS: Record<BinaryReleaseTargetStatus, { tone: ReleaseTone; text: string }> = {
  pending: { tone: 'run', text: '待升级' },
  dispatched: { tone: 'run', text: '升级中' },
  succeeded: { tone: 'ok', text: '本次已升级' },
  unverified: { tone: 'err', text: '未通过验证' },
  'failed-recovered': { tone: 'err', text: '失败 · 原版本运行' },
  'failed-dirty': { tone: 'err', text: '失败 · 需处理' },
  unsupported: { tone: 'none', text: '不支持' },
  canceled: { tone: 'none', text: '已取消' },
};
export const BINARY_RELEASE_STATUS: Record<BinaryReleaseStatus, string> = {
  running: '发布中',
  halted: '已暂停',
  succeeded: '已完成',
  canceled: '已停止',
};
export const BINARY_VERIFICATION: Record<BinaryVerification, string> = {
  receipt: '执行回执',
  observed: '观测确认',
  legacy: '迁移记录',
};
