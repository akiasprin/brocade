import { describe, expect, it } from 'vitest';
import type { HopDial, Rule } from '../src/api';
import {
  reachableWizardTables,
  wizardDefaultListenerWire,
  wizardEgressRule,
  wizardForwardEdges,
  wizardMembers,
  wizardRuleIssue,
  wizardRulesWithListenerPorts,
  wizardSpine,
} from '../src/panes/chain-wizard-graph';
import { forwardAction } from '../src/panes/rules';

const forward = (to: string, dial: HopDial): Rule => ({
  m: { t: 'any' },
  a: forwardAction(to, dial),
});

describe('新建链规则图', () => {
  it('主干只沿任意规则，例外指向的机器仍属于链', () => {
    const tables = {
      hk: [
        { m: { t: 'geosite', v: ['openai'] }, a: forwardAction('sg', { t: 'overlay' }) },
        forward('tw', { t: 'addr', v: '192.0.2.3:20000' }),
      ],
      tw: [wizardEgressRule()],
      sg: [wizardEgressRule()],
    } satisfies Record<string, Rule[]>;
    expect(wizardSpine('hk', tables)).toEqual(['hk', 'tw']);
    expect(wizardMembers('hk', tables)).toEqual(['hk', 'sg', 'tw']);
    expect(wizardRuleIssue('hk', tables)).toBeNull();
  });

  it('删除例外只清理失去引用的支路', () => {
    const tables = {
      hk: [forward('tw', { t: 'overlay' })],
      tw: [wizardEgressRule()],
      sg: [wizardEgressRule()],
    } satisfies Record<string, Rule[]>;
    expect(Object.keys(reachableWizardTables('hk', tables))).toEqual(['hk', 'tw']);
  });

  it('反向边在源节点监听；同一监听有公网边时默认加密', () => {
    const tables = {
      hk: [forward('tw', { t: 'reverse', v: 'v4' })],
      tw: [forward('sg', { t: 'overlay' })],
      sg: [wizardEgressRule()],
    } satisfies Record<string, Rule[]>;
    const edges = wizardForwardEdges('hk', tables);
    expect(edges.map(edge => edge.listener)).toEqual(['hk', 'sg']);
    expect(wizardDefaultListenerWire(edges, 'hk')).toBe('encryption');
    expect(wizardDefaultListenerWire(edges, 'sg')).toBe('none');
  });

  it('改目标监听端口会同步所有地址拨号，且保留 IPv6 主机', () => {
    const tables = {
      hk: [
        {
          m: { t: 'domain_suffix', v: ['example.com'] },
          a: forwardAction('sg', { t: 'addr', v: '[2001:db8::1]:20000' }),
        },
        forward('sg', { t: 'addr', v: '198.51.100.20:20000' }),
      ],
      sg: [wizardEgressRule()],
    } satisfies Record<string, Rule[]>;
    const staged = wizardRulesWithListenerPorts('hk', tables, () => 21000);
    expect(staged.hk.map(rule => (rule.a.t === 'forward' ? rule.a.dial : null))).toEqual([
      { t: 'addr', v: '[2001:db8::1]:21000' },
      { t: 'addr', v: '198.51.100.20:21000' },
    ]);
    expect(tables.hk[0].a).toMatchObject({ dial: { v: '[2001:db8::1]:20000' } });
  });

  it('阻止无兜底、空匹配与循环', () => {
    expect(wizardRuleIssue('hk', { hk: [] })).toContain('任意');
    expect(
      wizardRuleIssue('hk', {
        hk: [{ m: { t: 'geosite', v: [] }, a: { t: 'block' } }, wizardEgressRule()],
      }),
    ).toContain('不能为空');
    expect(
      wizardRuleIssue('hk', {
        hk: [forward('tw', { t: 'overlay' })],
        tw: [forward('hk', { t: 'overlay' })],
      }),
    ).toContain('成环');
  });
});
