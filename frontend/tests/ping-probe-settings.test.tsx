import { describe, expect, it } from 'vitest';
import type { PingProbeSettings } from '../src/api';
import { pingProbeAddressError, pingProbeFormError } from '../src/panes/settings';
import { pingLatencyMs, pingSampleText } from '../src/ui/ping-probe';

describe('Ping probe settings', () => {
  it.each(['tcp://1.1.1.1:443', 'tcp://example.com:80', 'tcp://[2001:db8::1]:443'])('accepts TCP target %s', address =>
    expect(pingProbeAddressError(address)).toBeNull(),
  );

  it.each(['icmp://1.1.1.1', 'icmp://example.com', 'icmp://[2001:db8::1]'])('accepts ICMP target %s', address =>
    expect(pingProbeAddressError(address)).toBeNull(),
  );

  it.each(['tcp://example.com', 'tcp://[not-ipv6]:443', 'icmp://example.com:443', 'icmp://2001:db8::1'])(
    'rejects malformed target %s',
    address => expect(pingProbeAddressError(address)).not.toBeNull(),
  );

  it('accepts TCP and ICMP targets in the same schedule', () => {
    const form: PingProbeSettings = {
      interval_secs: 5,
      timeout_ms: 420,
      targets: [
        { name: 'TCP', address: 'tcp://1.1.1.1:443' },
        { name: 'ICMP', address: 'icmp://1.1.1.1' },
      ],
    };
    expect(pingProbeFormError(form)).toBeNull();
  });

  it('rejects a schedule below five seconds', () => {
    expect(
      pingProbeFormError({
        interval_secs: 4,
        timeout_ms: 420,
        targets: [],
      }),
    ).toContain('5–86400');
  });

  it('rejects duplicate addresses across the shared target list', () => {
    const form: PingProbeSettings = {
      interval_secs: 60,
      timeout_ms: 420,
      targets: [
        { name: 'first', address: 'icmp://1.1.1.1' },
        { name: 'second', address: 'icmp://1.1.1.1' },
      ],
    };
    expect(pingProbeFormError(form)).toContain('不能重复');
  });

  it('keeps capability gaps distinct from attempted timeouts', () => {
    const at = 1_700_000_000;
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: false, latency_us: null })).toBe('未探测');
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: true, latency_us: null })).toBe('无响应');
    expect(pingLatencyMs({ latency_us: 37_250 })).toBe(37.25);
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: true, latency_us: 850 })).toBe('0.85 ms');
  });
});
