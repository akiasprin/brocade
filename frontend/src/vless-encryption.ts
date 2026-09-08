import type { VlessEncryptionOptions } from './api';

export const DEFAULT_VLESS_ENCRYPTION: VlessEncryptionOptions = {
  appearance: 'native',
  ticket_lifetime: '600s',
  client_mode: '0rtt',
  server_padding: '',
  client_padding: '',
};

export function encryptionTicketError(value: string): string | null {
  const match = /^(\d+)(?:-(\d+))?s$/.exec(value);
  if (!match) return '请填写 600s 或 100-500s；0s 表示禁用会话恢复。';
  const from = Number(match[1]);
  const to = Number(match[2] ?? match[1]);
  return from > to || to > 65535 || (from === 0 && to !== 0)
    ? '时间范围须递增且不超过 65535 秒；禁用会话恢复请填 0s。'
    : null;
}

export function encryptionPaddingError(value: string): string | null {
  if (!value) return null;
  if (value.length > 4096) return 'Padding 规则不能超过 4096 字符。';
  const blocks = value.split('.');
  if (blocks.length % 2 === 0) return '须以填充段开始和结束，中间交替插入延迟段。';
  let total = 0;
  for (const [index, block] of blocks.entries()) {
    if (!/^\d+-\d+-\d+$/.test(block) || block.length >= 20) return '每段须为 概率-最小值-最大值，以英文句点分隔。';
    const [probability, min, max] = block.split('-').map(Number);
    if (probability > 100 || min > max || max > 2147483647) return '概率须为 0–100，范围须递增且不超过 2147483647。';
    if (index === 0 && (probability !== 100 || min < 35)) return '首段概率必须为 100%，最小长度至少 35 字节。';
    if (index % 2 === 0) {
      total += max;
      if (total > 65553) return '各填充段最大长度之和不能超过 65553 字节。';
    }
  }
  return null;
}
