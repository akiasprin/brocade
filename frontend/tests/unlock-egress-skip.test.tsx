import { expect, it } from 'vitest';
import { verdictOf } from '../src/panes/unlock-status';

it('keeps absent egress IPv6 distinct from absent site AAAA and network failures', () => {
  expect(verdictOf({ status: 'skipped', code: 'no_ipv6_egress' })).toMatchObject({
    label: '已跳过',
    reason: '无 IPv6 出站',
  });
  expect(verdictOf({ status: 'skipped', code: 'no_ipv6_dns' }).reason).toBe('无 IPv6 地址解析');
  expect(verdictOf({ status: 'failed', code: 'network_error' }).label).toBe('检测失败');
});
