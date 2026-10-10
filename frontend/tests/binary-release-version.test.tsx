import { describe, expect, it } from 'vitest';
import { binaryReleaseVersion } from '../src/ui/binary-release-version';

describe('二进制发布版本', () => {
  it('Agent 显示源码提交而不是二进制摘要，并保留 dirty 状态', () => {
    const revision = '7fd6c7d6e37773422426b119e300e961c1099840';
    expect(binaryReleaseVersion('agent', revision)).toBe('7fd6c7d6e377');
    expect(binaryReleaseVersion('agent', `${revision}-dirty`)).toBe('7fd6c7d6e377 · dirty');
  });

  it('Xray 使用相同的源码提交显示规则', () => {
    const revision = '77ae5216cf2132fcf300fac1a6a4998ea54378ba';
    expect(binaryReleaseVersion('xray', revision)).toBe('77ae5216cf21');
    expect(binaryReleaseVersion('xray', `${revision}-dirty`)).toBe('77ae5216cf21 · dirty');
  });

  it('旧发布版本和 Xray 版本保持兼容', () => {
    expect(binaryReleaseVersion('agent', '0.2.0')).toBe('v0.2.0');
    expect(binaryReleaseVersion('xray', 'Xray 26.9.1')).toBe('v26.9.1');
  });
});
