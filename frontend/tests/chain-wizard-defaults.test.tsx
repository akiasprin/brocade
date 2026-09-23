import { describe, expect, it } from 'vitest';
import {
  NEW_CHAIN_PROTOCOL_DEFAULTS,
  customHopHostError,
  defaultListenerHopWire,
  entryTcpPortCollision,
  newChainWires,
} from '../src/panes/chain-wizard';
import { defaultHopWire, defaultHopWireForListener, seedHopIn, seedHops, type ForwardPeer } from '../src/panes/rules';
import type { HopDial, Rule } from '../src/api';

describe('建链向导协议默认值', () => {
  it('默认开启所有接入协议并填入 AnyTLS 连接复用参数', () => {
    expect(NEW_CHAIN_PROTOCOL_DEFAULTS).toEqual({
      vless: true,
      vlessEncryption: false,
      anytls: true,
      hysteria2: true,
    });

    const wires = newChainWires({
      ...NEW_CHAIN_PROTOCOL_DEFAULTS,
      anytlsPort: 14443,
      hy2Start: 30000,
      hy2End: 30099,
    });
    expect(wires.vless).toEqual({ kind: 'vless-reality' });
    expect(wires.anytls).toMatchObject({
      port: 14443,
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
      anytlsPort: 14443,
      hy2Start: 30000,
      hy2End: 30099,
    });
    expect(wires).toEqual({ vless: { kind: 'vless-reality' }, anytls: null, hysteria2: null });
  });

  it('展开卡片编辑的 Padding 和 Hysteria 带宽进入对应协议，而不影响其他协议', () => {
    const wires = newChainWires({
      vless: true,
      anytls: true,
      hysteria2: true,
      anytlsPort: 14443,
      anytlsPaddingScheme: ['stop=4', '0=20-30'],
      hy2Start: 30000,
      hy2End: 30099,
      hy2Up: '200 mbps',
      hy2Down: '500 mbps',
    });
    expect(wires.anytls?.padding_scheme).toEqual(['stop=4', '0=20-30']);
    expect(wires.hysteria2?.bandwidth).toEqual({ up: '200 mbps', down: '500 mbps' });
    expect(wires.vless).toEqual({ kind: 'vless-reality' });
  });
});

describe('建链向导提交前校验', () => {
  it('拒绝同一入口上共用 TCP 端口的接入协议', () => {
    expect(
      entryTcpPortCollision([
        { label: 'VLESS · REALITY', port: 14443 },
        { label: 'AnyTLS', port: 14443 },
      ]),
    ).toBe('VLESS · REALITY 与 AnyTLS 不能共用 TCP 14443');
    expect(
      entryTcpPortCollision([
        { label: 'VLESS · REALITY', port: 13443 },
        { label: 'AnyTLS', port: 14443 },
      ]),
    ).toBeNull();
  });

  it('自定义一跳只接收主机，不把端口或路径重复拼进地址', () => {
    expect(customHopHostError('')).toBe('填写自定义主机地址');
    expect(customHopHostError('edge.internal')).toBeNull();
    expect(customHopHostError('2001:db8::9')).toBeNull();
    expect(customHopHostError('[2001:db8::9]')).toBeNull();
    expect(customHopHostError('edge.internal:443')).toContain('IPv6 地址无效');
    expect(customHopHostError('edge.internal/path')).toContain('不要带端口、路径或空格');
  });
});

it('可单独启用 VLESS Encryption，默认 13800 或使用配置的分配端口', () => {
  const options = {
    vless: false,
    anytls: false,
    hysteria2: false,
    vlessEncryption: true,
    anytlsPort: 14443,
    hy2Start: 30000,
    hy2End: 30099,
  };
  expect(newChainWires(options)).toEqual({
    vless: null,
    anytls: null,
    hysteria2: null,
    vless_encryption: { port: 13800 },
  });
  expect(newChainWires({ ...options, vlessEncryptionPort: 49002 }).vless_encryption).toEqual({ port: 49002 });
  expect(newChainWires({ ...options, vlessEncryptionProfile: 'native' }).vless_encryption).toMatchObject({
    port: 13800,
    options: { appearance: 'native', ticket_lifetime: '600s' },
  });
});

describe('链路承载协议默认值', () => {
  const rule = (dial: HopDial): Rule => ({
    m: { t: 'any' },
    a: { t: 'forward', to: 'relay', dial, pool: { t: 'none' } },
  });
  const relay: ForwardPeer = {
    id: 'relay',
    name: 'Relay',
    public_ipv4: '192.0.2.2',
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    step: null,
    where: 'next',
    blocked: null,
  };

  it.each([
    [{ t: 'addr', v: '192.0.2.2:20000' } as HopDial, 'encryption'],
    [{ t: 'reverse', v: 'v4' } as HopDial, 'encryption'],
    [{ t: 'overlay' } as HopDial, 'none'],
  ])('按连接方式为 %o 选择 %s', (dial, expected) => {
    expect(defaultHopWire(dial)).toBe(expected);
    expect(seedHopIn(null, 'relay', new Map(), dial)?.security?.t).toBe(expected);
  });

  it('规则编辑器只让纯 WireGuard 新监听默认 VLESS-NONE，并保留已有配置', () => {
    const direct = seedHops([relay], undefined, 20000, { sourceNode: 'entry', rules: [rule({ t: 'addr', v: '' })] });
    const overlay = seedHops([relay], undefined, 20000, { sourceNode: 'entry', rules: [rule({ t: 'overlay' })] });
    expect(direct.relay).toMatchObject({ kind: 'encryption', wireAutomatic: true });
    expect(overlay.relay).toMatchObject({ kind: 'none', wireAutomatic: true });

    const saved = seedHops(
      [
        {
          ...relay,
          step: {
            chain: 'chain',
            node: 'relay',
            accept: null,
            hop_in: { port: 21000, security: { t: 'none' } },
            rules: [],
          },
        },
      ],
      undefined,
      20000,
      { sourceNode: 'entry', rules: [rule({ t: 'addr', v: '' })] },
    );
    expect(saved.relay).toMatchObject({ kind: 'none', wireAutomatic: false });
  });

  it('同一监听只要承载一条非 WireGuard 边就默认 VLESS-ENCRY', () => {
    const mixedRules: Rule[] = [
      rule({ t: 'overlay' }),
      {
        m: { t: 'domain_suffix', v: ['example.com'] },
        a: { t: 'forward', to: 'tail', dial: { t: 'reverse', v: 'v4' }, pool: { t: 'none' } },
      },
    ];
    expect(defaultHopWireForListener('entry', 'relay', mixedRules)).toBe('none');
    expect(defaultHopWireForListener('entry', 'entry', mixedRules)).toBe('encryption');

    const spine = ['entry', 'relay', 'tail'];
    expect(
      defaultListenerHopWire(spine, 'relay', index => (index === 1 ? { t: 'overlay' } : { t: 'reverse', v: 'v4' })),
    ).toBe('encryption');
    expect(defaultListenerHopWire(['entry', 'relay'], 'relay', () => ({ t: 'overlay' }))).toBe('none');
  });
});
