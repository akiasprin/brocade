import { describe, expect, it } from 'vitest';
import { forwardAction, POOL_DEFAULT } from '../src/panes/rules';

describe('new relay hop connection handling', () => {
  it('does not opt new rules into the concurrency-one Mux.cool pool', () => {
    expect(POOL_DEFAULT).toEqual({ t: 'none' });
    expect(forwardAction('relay', { t: 'overlay' })).toEqual({
      t: 'forward',
      to: 'relay',
      dial: { t: 'overlay' },
      pool: { t: 'none' },
    });
  });

  it('keeps an explicit pool choice for authored-model compatibility', () => {
    expect(forwardAction('relay', { t: 'overlay' }, { t: 'pool' })).toEqual({
      t: 'forward',
      to: 'relay',
      dial: { t: 'overlay' },
      pool: { t: 'pool' },
    });
  });
});
