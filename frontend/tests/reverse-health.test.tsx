import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ReverseHealthCard } from '../src/reverse-health';
import { DEFAULT_REVERSE_HEALTH, reverseHealthError } from '../src/api';
class Events {
  static current: Events;
  handlers = new Map<string, (event: { data: string }) => void>();
  closed = false;
  onerror: (() => void) | null = null;
  constructor() {
    Events.current = this;
  }
  addEventListener(name: string, callback: (event: { data: string }) => void) {
    this.handlers.set(name, callback);
  }
  close() {
    this.closed = true;
  }
  send(name: string, value: unknown) {
    this.handlers.get(name)?.({ data: JSON.stringify(value) });
  }
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('EventSource', Events);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
it('keeps ACK health separate from failed business canaries and expires stale data', () => {
  const view = render(<ReverseHealthCard nodeId="node-a" />);
  expect(screen.getByText(/UNKNOWN/)).toBeTruthy();
  const now = Date.now();
  act(() =>
    Events.current.send('sample', {
      received_at_unix_millis: now,
      sample: {
        sampled_at_unix_millis: now,
        reverse_health: {
          boot_id: '18446744073709551615',
          sequence: 3,
          sampled_at_unix_ms: now,
          workers: [
            {
              worker_id: '18446744073709551614',
              pair: 'p',
              role: 'portal',
              state: 'READY',
              reason: 'validated',
              ack_age_ms: 20,
              rtt_ms: 150,
              probes: 2,
              acks: 2,
              timeouts: 0,
            },
          ],
          events: [],
          canaries: [
            {
              pair: 'p',
              state: 'FAILED',
              latency_ms: 750,
              consecutive_successes: 0,
              stable_since_unix_ms: 0,
              sampled_at_unix_ms: now,
            },
          ],
        },
      },
    }),
  );
  expect(screen.getByText('READY')).toBeTruthy();
  expect(screen.getByText(/可用 1/)).toBeTruthy();
  expect(screen.getByText(/FAILED/)).toBeTruthy();
  act(() => vi.advanceTimersByTime(16000));
  expect(screen.queryByText('READY')).toBeNull();
  expect(screen.getByText('可用数量未知')).toBeTruthy();
  const previous = Events.current;
  view.rerender(<ReverseHealthCard nodeId="node-b" />);
  expect(previous.closed).toBe(true);
  expect(screen.queryByText(/p 业务探测/)).toBeNull();
  expect(screen.getByText(/UNKNOWN/)).toBeTruthy();
  view.unmount();
  expect(Events.current.closed).toBe(true);
});
it('validates reverse policy independently from ordinary Mux second-based bounds', () => {
  expect(reverseHealthError(DEFAULT_REVERSE_HEALTH)).toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, probe_timeout_ms: 1000 })).not.toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, max_idle_ready_workers: 1 })).not.toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, health_lease_ms: 1000 })).not.toBeNull();
});
