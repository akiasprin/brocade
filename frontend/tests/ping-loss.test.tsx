import { describe, expect, it } from 'vitest';
import type { PingProbePoint } from '../src/api';
import {
  pingLossStats,
  pingLossText,
  pingLossTone,
  pingSkipReason,
  pingSkipReasonShort,
  worstPingLossTone,
} from '../src/ui/ping-probe';

const sample = (attempted: boolean, latency_us: number | null): PingProbePoint => ({
  probed_at_unix_secs: 100,
  attempted,
  latency_us,
});

describe('Ping loss readings', () => {
  it('counts only attempted probes and treats zero latency as a response', () => {
    expect(pingLossStats([sample(true, 0), sample(true, null), sample(false, null)])).toEqual({
      attempted: 2,
      lost: 1,
      percent: 50,
    });
  });

  it('does not infer lost probes from missing timestamps', () => {
    expect(pingLossStats([sample(true, 12_000), { ...sample(true, 15_000), probed_at_unix_secs: 10_000 }])).toEqual({
      attempted: 2,
      lost: 0,
      percent: 0,
    });
  });

  it.each([{ samples: [] }, { samples: [sample(false, null)] }])(
    'leaves an unmeasured range unknown',
    ({ samples }) => {
      const stats = pingLossStats(samples);
      expect(stats).toEqual({ attempted: 0, lost: 0, percent: null });
      expect(pingLossText(stats.percent)).toBe('—');
    },
  );

  it('tells partial loss from a target that answered none of the attempted probes', () => {
    expect(pingLossTone(pingLossStats([sample(true, 1_000), sample(true, null)]))).toBe('partial');
    expect(pingLossTone(pingLossStats([sample(true, null), sample(false, null)]))).toBe('down');
    expect(pingLossTone(pingLossStats([sample(true, 1_000), sample(false, null)]))).toBeNull();
    expect(pingLossTone(pingLossStats([sample(false, null)]))).toBeNull();
    expect(worstPingLossTone([null, 'partial', null])).toBe('partial');
    expect(worstPingLossTone(['partial', 'down'])).toBe('down');
    expect(worstPingLossTone([])).toBeNull();
  });

  it('names a skip reason only when every sample of the range shares it', () => {
    const skipped = (skip_reason: PingProbePoint['skip_reason']): PingProbePoint => ({
      ...sample(false, null),
      skip_reason,
    });
    expect(pingSkipReason([skipped('no_route'), skipped('no_route')])).toBe('no_route');
    expect(pingSkipReason([skipped('no_route'), skipped('resolve_failed')])).toBeNull();
    expect(pingSkipReason([skipped('no_address'), sample(true, 1_000)])).toBeNull();
    // Agents older than the dual-stack protocol report no reason.
    expect(pingSkipReason([sample(false, null)])).toBeNull();
    expect(pingSkipReason([])).toBeNull();
    expect(pingSkipReasonShort('no_address', 'ipv4')).toBe('无 A');
    expect(pingSkipReasonShort('no_address', 'ipv6')).toBe('无 AAAA');
  });

  it.each([
    [0, '0%'],
    [100, '100%'],
    [100 / 3, '33.33%'],
    [12.5, '12.5%'],
    [0.001, '<0.01%'],
    [99.999, '>99.99%'],
  ])('formats %s without hiding partial loss', (percent, text) => {
    expect(pingLossText(percent)).toBe(text);
  });
});
