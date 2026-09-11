import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ReverseHealthCard, parsePair } from '../src/reverse-health';
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

const worker = (extra: Record<string, unknown> = {}) => ({
  worker_id: '18446744073709551614',
  pair: 'rev:portal:app-a/jp-a>tyo-exit',
  role: 'portal',
  state: 'READY',
  reason: 'validated',
  active_sessions: 0,
  affected_sessions: 0,
  control_queue_depth: 0,
  queue_delay_ms: 0,
  scheduler_lag_ms: 0,
  ack_age_ms: 20,
  rtt_ms: 150,
  probes: 2,
  acks: 2,
  timeouts: 0,
  ...extra,
});

it('uses tunnel health only and ignores retired business canary reports', () => {
  const view = render(<ReverseHealthCard nodeId="node-a" />);
  // 没有上报过的机器不渲染这张卡：整个机队里绝大多数机器不做反向，
  // 常驻一张空卡会把每一页观测页都撑长一节。
  expect(screen.queryByLabelText('反向隧道')).toBeNull();
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
          workers: [worker({ queue_delay_ms: 7775, scheduler_lag_ms: 0 })],
          events: [],
          canaries: [
            {
              freshness_budget_ms: 15000,
              pair: 'rev:portal:app-a/jp-a>tyo-exit',
              state: 'FAILED',
              reason: 'timeout',
              latency_ms: 750,
              consecutive_successes: 0,
              stable_since_unix_ms: 0,
              first_ok_unix_ms: 0,
              last_failure_unix_ms: now,
              attempts: 1,
              failures: 1,
              sampled_at_unix_ms: now,
            },
          ],
        },
      },
    }),
  );
  expect(screen.getAllByText('READY').length).toBeGreaterThan(0);
  expect(screen.getByText('7775 ms / 0 ms')).toBeTruthy();
  expect(screen.getByLabelText('反向隧道').querySelector('.ov-one.ov-rev > summary .nums')?.textContent).toContain(
    '隧道 1 / 1',
  );
  expect(screen.getAllByText('全部就绪')).toHaveLength(2);
  expect(screen.getByText(/每秒采样 · 0 s 前 · 实时计算/)).toBeTruthy();
  expect(screen.queryByText('业务不可用')).toBeNull();
  // 标签解析出的链与对端机器，而不是原始串。
  expect(screen.getByText('jp-a')).toBeTruthy();
  expect(screen.getByText('node-a → tyo-exit')).toBeTruthy();

  act(() => vi.advanceTimersByTime(16000));
  expect(screen.queryByText('READY')).toBeNull();
  expect(screen.queryByText('数据未到达')).toBeNull();
  expect(screen.getByText(/每秒采样 · 16 s 前 · 实时计算/)).toBeTruthy();
  expect(screen.getAllByText('数据已过期').length).toBeGreaterThan(0);
  expect(screen.getByText(/不代表实时状态/)).toBeTruthy();

  const previous = Events.current;
  view.rerender(<ReverseHealthCard nodeId="node-b" />);
  expect(previous.closed).toBe(true);
  // 换一台机器：上一台的读数不能继续显示，而新机器还没有上报。
  expect(screen.queryByLabelText('反向隧道')).toBeNull();
  view.unmount();
  expect(Events.current.closed).toBe(true);
});

it('validates reverse policy independently from ordinary Mux second-based bounds', () => {
  expect(reverseHealthError(DEFAULT_REVERSE_HEALTH)).toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, probe_timeout_ms: 1000 })).not.toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, max_idle_ready_workers: 1 })).not.toBeNull();
  expect(reverseHealthError({ ...DEFAULT_REVERSE_HEALTH, health_lease_ms: 1000 })).not.toBeNull();
  expect(
    reverseHealthError({
      ...DEFAULT_REVERSE_HEALTH,
      min_healthy_workers: 100_000,
      max_idle_ready_workers: 100_000,
      max_parallel_dials_per_pair: 100_000,
      tuning: {
        ...DEFAULT_REVERSE_HEALTH.tuning!,
        spare_workers: 0,
        max_healthy_workers: 100_000,
        max_sessions_per_worker: 65_535,
      },
    }),
  ).toBeNull();
});

it('does not render a reverse card for a canary report without workers', () => {
  render(<ReverseHealthCard nodeId="node-a" />);
  const now = Date.now();
  act(() =>
    Events.current.send('sample', {
      received_at_unix_millis: now,
      sample: {
        sampled_at_unix_millis: now,
        reverse_health: {
          boot_id: '1',
          sequence: 1,
          sampled_at_unix_ms: now,
          workers: [],
          events: [],
          canaries: [
            {
              pair: 'slow-canary',
              state: 'AVAILABLE',
              latency_ms: 150,
              consecutive_successes: 3,
              stable_since_unix_ms: 0,
              sampled_at_unix_ms: now - 30000,
              freshness_budget_ms: 120750,
            },
          ],
        },
      },
    }),
  );
  expect(screen.queryByLabelText('反向隧道')).toBeNull();
});

it('marks the events that never reached any snapshot', () => {
  render(<ReverseHealthCard nodeId="node-a" />);
  const now = Date.now();
  act(() =>
    Events.current.send('sample', {
      received_at_unix_millis: now,
      sample: {
        sampled_at_unix_millis: now,
        reverse_health: {
          boot_id: '1',
          sequence: 9,
          sampled_at_unix_ms: now,
          workers: [worker({ state: 'SUSPECT', reason: '探测截止未应答' })],
          canaries: [],
          events: [
            {
              ...worker({ state: 'READY' }),
              sequence: 1212,
              at_unix_ms: now - 4000,
              from: 'SUSPECT',
            },
            {
              ...worker({ state: 'SUSPECT', reason: '探测截止未应答' }),
              sequence: 1216,
              at_unix_ms: now - 1000,
              from: 'READY',
              affected_sessions: 5,
            },
          ],
        },
      },
    }),
  );
  // 序号不连续：中间三条没有出现在任何一份快照里，不能读成「这期间没发生任何事」。
  expect(screen.getByText(/缺 3 条 · #1213–#1215/)).toBeTruthy();
  expect(screen.getByText(/受影响业务流 5/)).toBeTruthy();
});

it('reads the chain and the peer out of the reverse tag, and gives up on anything else', () => {
  expect(parsePair('rev:portal:app-a/jp-a>tyo-exit')).toEqual({
    role: 'portal',
    app: 'app-a',
    chain: 'jp-a',
    peer: 'tyo-exit',
  });
  // app 段可缺省（AppIr 没有 app_id 时，见 physical/node.rs 的 reverse_portal_tag）。
  expect(parsePair('rev:bridge:jp-a<hk-relay')).toEqual({
    role: 'bridge',
    app: null,
    chain: 'jp-a',
    peer: 'hk-relay',
  });
  expect(parsePair('p')).toBeNull();
  expect(parsePair('rev:portal:jp-a')).toBeNull();
  expect(parsePair('rev:portal:jp-a>')).toBeNull();
});
