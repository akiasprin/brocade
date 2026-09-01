import { describe, expect, it } from 'vitest';
import { friendlyId, friendlyIdFromBytes, isFriendlyId } from '../src/friendly-id';

describe('可读模型 ID', () => {
  it('使用类型前缀和可发音的交替字母', () => {
    const bytes = new Uint8Array([8, 3, 9, 2, 12, 0]);
    const chain = friendlyIdFromBytes('chain', bytes);
    const ingress = friendlyIdFromBytes('ingress', bytes);

    expect(chain).toMatch(/^c-[a-z]{6}$/);
    expect(ingress).toBe(`i-${chain.slice(2)}`);
    expect(isFriendlyId('chain', chain)).toBe(true);
    expect(isFriendlyId('ingress', chain)).toBe(false);
  });

  it('避开当前快照已经占用的值', () => {
    const first = friendlyId('chain');
    const second = friendlyId('ingress', new Set([first]));
    expect(isFriendlyId('ingress', second)).toBe(true);
    expect(second.slice(2)).not.toBe(first.slice(2));
  });
});
