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

  it('keeps an explicit mux override', () => {
    const pool = poolFromMuxConcurrency(8);
    expect(forwardAction('relay', { t: 'overlay' }, pool)).toEqual({
      t: 'forward',
      to: 'relay',
      dial: { t: 'overlay' },
      pool,
    });
  });

  it('presents one Mux choice and writes a full override shape', () => {
    expect(POOL_ORDER).toEqual(['none', 'mux']);
    expect(POOL_LABEL).toEqual({ none: '每次新建', mux: 'Mux 复用' });
    expect(poolChoice({ t: 'mux' })).toBe('mux');
    expect(muxConcurrency({ t: 'mux' })).toBe(1);
    expect(poolFromMuxConcurrency(1)).toEqual({
      t: 'mux',
      v: {
        concurrency: 1,
        prewarm_workers: 0,
        reuse_threshold: 2,
        max_probing_workers: 1,
        probe_interval_ms: 5000,
        probe_timeout_ms: 2000,
        idle_ttl_ms: 24000,
        max_requests_per_worker: 128,
      },
    });
    const eight = poolFromMuxConcurrency(8);
    expect(eight).toMatchObject({ t: 'mux', v: { concurrency: 8 } });
    expect(muxConcurrency(eight)).toBe(8);
  });
});
