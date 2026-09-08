import { describe, expect, it } from 'vitest';
import {
  forwardAction,
  muxConcurrency,
  POOL_DEFAULT,
  POOL_LABEL,
  POOL_ORDER,
  poolChoice,
  poolFromMuxConcurrency,
} from '../src/panes/rules';

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

  it('presents one Mux choice, reads legacy shapes, and writes only the current full override shape', () => {
    expect(POOL_ORDER).toEqual(['none', 'mux']);
    expect(POOL_LABEL).toEqual({ none: '每次新建', mux: 'Mux 复用' });
    expect(poolChoice({ t: 'pool' })).toBe('mux');
    expect(poolChoice({ t: 'merge', v: 8 })).toBe('mux');
    expect(muxConcurrency({ t: 'pool' })).toBe(1);
    expect(muxConcurrency({ t: 'merge', v: 8 })).toBe(8);
    expect(poolFromMuxConcurrency(1)).toEqual({
      t: 'mux',
      v: {
        concurrency: 1,
        min_idle_workers: 0,
        max_idle_workers: 2,
        max_probing_workers: 1,
        probe_interval_secs: 5,
        probe_timeout_ms: 2000,
        idle_ttl_secs: 24,
        max_requests_per_worker: 128,
      },
    });
    expect(poolFromMuxConcurrency(8)).toMatchObject({ t: 'mux', v: { concurrency: 8 } });
  });
});
