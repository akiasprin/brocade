import { describe, expect, it } from 'vitest';
import { NEW_CHAIN_PROTOCOL_DEFAULTS, newChainWires } from '../src/panes/chain-wizard';

describe('建链向导协议默认值', () => {
  it('默认开启所有接入协议并填入 AnyTLS 连接复用参数', () => {
    expect(NEW_CHAIN_PROTOCOL_DEFAULTS).toEqual({ vless: true, anytls: true, hysteria2: true });

    const wires = newChainWires({
      ...NEW_CHAIN_PROTOCOL_DEFAULTS,
      anytlsPort: 18443,
      hy2Start: 30000,
      hy2End: 30099,
    });
    expect(wires.vless).toEqual({ kind: 'vless-reality' });
    expect(wires.anytls).toMatchObject({
      port: 18443,
      idle_session_check_interval_secs: 30,
      idle_session_timeout_secs: 30,
      min_idle_session: 1,
    });
    expect(wires.hysteria2).toMatchObject({
      port: 30000,
      hop: { start: 30000, end: 30099 },
    });
  });

  it('关闭的协议不会偷偷写入创建请求', () => {
    const wires = newChainWires({
      vless: true,
      anytls: false,
      hysteria2: false,
      anytlsPort: 18443,
      hy2Start: 30000,
      hy2End: 30099,
    });
    expect(wires).toEqual({ vless: { kind: 'vless-reality' }, anytls: null, hysteria2: null });
  });
});
