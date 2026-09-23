import { describe, expect, it } from 'vitest';
import {
  appId,
  appIdFromBytes,
  isModelId,
  modelIdPair,
  modelIdPairFromBytes,
  modelIdsArePaired,
  tunnelId,
  tunnelIdFromBytes,
  vpngateTunnelId,
  vpngateTunnelIdFromBytes,
  warpTunnelId,
  warpTunnelIdFromBytes,
} from '../src/model-id';

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

  it('自定义隧道使用 custom 加两段四位十六进制随机值', () => {
    expect(tunnelIdFromBytes(new Uint8Array([0x8f, 0x3a, 0x2d, 0x71]))).toBe('custom-8f3a-2d71');
    expect(isModelId('tunnel', 'custom-8f3a-2d71')).toBe(true);
    expect(isModelId('tunnel', 'tunnel-8f3a-2d71')).toBe(false);
    expect(isModelId('tunnel', 'external-1')).toBe(false);
    expect(tunnelId(new Set(['custom-0000-0000']))).toMatch(/^custom-[0-9a-f]{4}-[0-9a-f]{4}$/);
  });

  it('WARP 使用独立前缀和两段四位十六进制随机值', () => {
    expect(warpTunnelIdFromBytes(new Uint8Array([0x8f, 0x3a, 0x2d, 0x71]))).toBe('warp-8f3a-2d71');
    expect(isModelId('tunnel', 'warp-8f3a-2d71')).toBe(true);
    expect(isModelId('tunnel', 'warp')).toBe(false);
    expect(isModelId('tunnel', 'warp.platform')).toBe(false);
    expect(warpTunnelId(new Set(['warp-0000-0000']))).toMatch(/^warp-[0-9a-f]{4}-[0-9a-f]{4}$/);
  });

  it('VPN Gate 使用 vpngate 前缀和两段四位十六进制随机值', () => {
    expect(vpngateTunnelIdFromBytes(new Uint8Array([0x8f, 0x3a, 0x2d, 0x71]))).toBe('vpngate-8f3a-2d71');
    expect(isModelId('tunnel', 'vpngate-8f3a-2d71')).toBe(true);
    expect(isModelId('tunnel', 'vpngate-jp')).toBe(false);
    expect(vpngateTunnelId(new Set(['vpngate-0000-0000']))).toMatch(/^vpngate-[0-9a-f]{4}-[0-9a-f]{4}$/);
  });
});
