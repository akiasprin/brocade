import { describe, expect, it } from 'vitest';
import type { Rule, SnapshotStep } from '../src/api';
import { compilerFallbackRules, effectiveChainRuleSteps } from '../src/panes/chains';

const step = (node: string, rules: Rule[] = []): SnapshotStep => ({
  chain: 'c',
  node,
  accept: null,
  hop_in: null,
  rules,
});

const forward = (to: string): Rule => ({
  m: { t: 'any' },
  a: { t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'none' } },
});

describe('machine rule draft projection', () => {
  it('projects unsaved forwarding changes and retains disconnected stored steps', () => {
    const steps = [step('entry', [forward('old-relay')]), step('old-relay')];
    const effective = effectiveChainRuleSteps('c', steps, {
      entry: [forward('new-relay')],
      'new-relay': [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
    });
    expect(effective.map(item => item.node)).toEqual(['entry', 'old-relay', 'new-relay']);
    expect(effective[0].rules).toEqual([forward('new-relay')]);
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
      compilerFallbackRules(
        [],
        [
          { dest_match: dmm.m, action: dmm.a },
          { dest_match: any.m, action: any.a },
        ],
      ),
    ).toEqual([any]);
  });

  it('returns no fallback when the written table already ends in Any', () => {
    expect(
      compilerFallbackRules(
        [any],
        [
          { dest_match: dmm.m, action: dmm.a },
          { dest_match: any.m, action: any.a },
        ],
      ),
    ).toEqual([]);
  });
});
