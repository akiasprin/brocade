import { describe, expect, it } from 'vitest';
import { appId, appIdFromBytes, isModelId, modelIdPair, modelIdPairFromBytes, modelIdsArePaired } from '../src/model-id';

describe('关联模型 ID', () => {
  it('分组使用 app 加四位十六进制随机值', () => {
    expect(appIdFromBytes(new Uint8Array([0x8f, 0x3a]))).toBe('app-8f3a');
    expect(isModelId('app', 'app-8f3a')).toBe(true);
    expect(isModelId('app', 'app-main')).toBe(false);
    expect(appId(new Set(['app-0000']))).toMatch(/^app-[0-9a-f]{4}$/);
  });

  it('使用三字母类型前缀和两段四位十六进制随机值', () => {
    const pair = modelIdPairFromBytes(new Uint8Array([0x8f, 0x3a, 0x2d, 0x71]));

    expect(pair).toEqual({ ingressId: 'ing-8f3a', chainId: 'chn-8f3a-2d71' });
    expect(isModelId('ingress', pair.ingressId)).toBe(true);
    expect(isModelId('chain', pair.chainId)).toBe(true);
    expect(isModelId('ingress', 'i-bacemu')).toBe(false);
    expect(isModelId('chain', 'c-lumira')).toBe(false);
    expect(modelIdsArePaired(pair)).toBe(true);
    expect(modelIdsArePaired({ ingressId: 'i-bacemu', chainId: 'c-lumira' })).toBe(false);
  });

  it('链路继承接入面片段且避开当前快照中的完整 ID', () => {
    const first = modelIdPair();
    const second = modelIdPair(new Set([first.ingressId, first.chainId]));

    expect(modelIdsArePaired(second)).toBe(true);
    expect(second.ingressId).not.toBe(first.ingressId);
    expect(second.chainId).not.toBe(first.chainId);
  });
});
