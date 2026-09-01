import { describe, expect, it } from 'vitest';
import type { Rule, SnapshotStep } from '../src/api';
import { compilerFallbackRules, defaultChainRuleOccurrence } from '../src/panes/chains';

const step = (node: string, rules: Rule[] = []): SnapshotStep => ({
  chain: 'c',
  node,
  accept: null,
  hop_in: null,
  rules,
});

const forward = (to: string): Rule => ({
  m: { t: 'any' },
  a: { t: 'forward', to },
});

describe('machine-detail rule default expansion', () => {
  it('locates the current machine by its full path from the chain entry', () => {
    const steps = [step('entry', [forward('relay')]), step('relay', [forward('egress')]), step('egress')];

    expect(defaultChainRuleOccurrence(['entry', 'relay', 'egress'], steps, 'egress')).toBe(
      'entry>relay>egress',
    );
  });

  it('also locates a current machine whose rule step is disconnected from the entry', () => {
    const steps = [step('entry'), step('orphan')];

    expect(defaultChainRuleOccurrence(['entry'], steps, 'orphan')).toBe('orphan');
  });
});

describe('compiler fallback extraction', () => {
  const dmm: Rule = {
    m: { t: 'geosite', v: ['dmm'] },
    a: { t: 'egress', send_through: null },
  };
  const any: Rule = {
    m: { t: 'any' },
    a: { t: 'egress', send_through: null },
  };

  it('returns only the terminal fallback when machine DNS was inserted before it', () => {
    expect(
      compilerFallbackRules([], [
        { dest_match: dmm.m, action: dmm.a },
        { dest_match: any.m, action: any.a },
      ]),
    ).toEqual([any]);
  });

  it('returns no fallback when the written table already ends in Any', () => {
    expect(
      compilerFallbackRules([any], [
        { dest_match: dmm.m, action: dmm.a },
        { dest_match: any.m, action: any.a },
      ]),
    ).toEqual([]);
  });
});
