import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { MuxObservationCard, parseMuxPair } from '../src/mux-observation';
import { NodeRealtimeProvider } from '../src/node-realtime';
import { ReverseHealthCard } from '../src/reverse-health';

class Events {
  static current: Events;
  static opened = 0;
  handlers = new Map<string, (event: { data: string }) => void>();
  closed = false;
  onerror: (() => void) | null = null;
  constructor() {
    Events.current = this;
    Events.opened += 1;
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
  Events.opened = 0;
  vi.useFakeTimers();
  vi.stubGlobal('EventSource', Events);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const pool = (extra: Record<string, unknown> = {}) => ({
  pool_id: '101',
  pair: 'out:app-a/jp-a>tyo-exit',
  role: 'dialer',
  kind: 'tcp',
  used: true,
  draining: false,
  config: {
    concurrency: 8,
    prewarm_workers: 1,
    reuse_threshold: 2,
    max_probing_workers: 1,
    probe_interval_ms: 5000,
    probe_timeout_ms: 1000,
    idle_ttl_ms: 60000,
    max_sessions_per_worker: 100,
    health_lease_ms: 15000,
    confirm_timeout_ms: 4000,
    recovery_successes: 2,
    session_end_timeout_ms: 10000,
  },
  active_sessions: 2,
  available_slots: 6,
  ready_workers: 1,
  total_workers: 1,
  dispatches: 20,
  active_reuses: 10,
  idle_reuses: 5,
  demand_dials: 5,
  rejected_dispatches: 0,
  probes: 2,
  acks: 2,
  timeouts: 0,
  workers_created_demand: 5,
  workers_created_warm: 1,
  workers_warm_ready: 1,
  workers_warm_failed: 0,
  workers_closed_idle_ttl: 0,
  workers_closed_probe: 0,
  workers_closed_capacity: 0,
  workers_closed_requests: 0,
  workers_closed_transport: 0,
  health_suspects: 0,
  health_recoveries: 0,
  health_draining: 0,
  health_queue_failures: 0,
  health_dial_throttled: 0,
  ...extra,
});

const worker = {
  pool_id: '101',
  worker_id: '18446744073709551614',
  pair: 'out:app-a/jp-a>tyo-exit',
  role: 'dialer',
  kind: 'tcp',
  state: 'READY',
  reason: 'serving',
  phase: 'active',
  active_sessions: 2,
  affected_sessions: 0,
  available_slots: 6,
  lifetime_sessions: 30,
  ack_age_ms: 20,
  rtt_ms: 150,
  probes: 2,
  acks: 2,
  timeouts: 0,
  lease_remaining_ms: 15000,
  control_queue_depth: 0,
  queue_delay_ms: 0,
};

function sendMux(pools: unknown[], workers: unknown[] = [worker], events: unknown[] = []) {
  const now = Date.now();
  act(() =>
    Events.current.send('sample', {
      node_id: 'node-a',
      received_at_unix_millis: now,
      sample: {
        sampled_at_unix_millis: now,
        mux: {
          boot_id: '9',
          sequence: 3,
          sampled_at_unix_ms: now,
          pools,
          workers,
          events,
        },
      },
    }),
  );
}

it('shows current capacity and derives reuse counters only from this browser observation window', () => {
  render(<MuxObservationCard nodeId="node-a" nodeName={id => ({ 'node-a': 'JP', 'tyo-exit': 'TYO' })[id] ?? id} />);
  expect(screen.queryByLabelText('Mux 连接复用')).toBeNull();

  sendMux([pool()]);
  expect(screen.getByLabelText('Mux 连接复用')).toBeTruthy();
  expect(screen.getByText('JP → TYO · TCP')).toBeTruthy();
  expect(screen.getByLabelText('Mux 连接复用').querySelector('.ov-one.ov-mux > summary .nums')?.textContent).toContain(
    'Worker 1 / 1',
  );
  expect(screen.getByText(/可复用槽位/).textContent).toContain('6');
  expect(screen.getByText(/预热目标/).textContent).toContain('1');
  expect(screen.getByText(/复用阈值/).textContent).toContain('2');
  expect(screen.getByText(/每秒采样 · 0 s 前 · 实时计算/)).toBeTruthy();
  // The first frame is the in-browser baseline; Xray lifetime totals are not mislabelled as
  // activity during this observation window.
  expect(screen.getByText('调度统计').parentElement?.textContent).toContain('活跃复用 0');

  sendMux([
    pool({
      dispatches: 24,
      active_reuses: 12,
      idle_reuses: 6,
      demand_dials: 6,
    }),
  ]);
  const dispatchStats = screen.getByText('调度统计').parentElement?.textContent;
  expect(dispatchStats).toContain('活跃复用 2');
  expect(dispatchStats).toContain('空闲命中 1');
  expect(dispatchStats).toContain('按需拨号 1');
  expect(dispatchStats).toContain('复用率 75%');
  expect(screen.getByText(/发生 1 次按需拨号/)).toBeTruthy();
  expect(screen.getByText(/未达到复用阈值则创建新 Worker/)).toBeTruthy();
});

it('shows active health policy, current quarantine and window deltas without needing old events', () => {
  render(<MuxObservationCard nodeId="node-a" />);
  const config = {
    ...pool().config,
    health_lease_ms: 15000,
    confirm_timeout_ms: 4000,
    recovery_successes: 2,
    session_end_timeout_ms: 10000,
  };
  const suspect = {
    ...worker,
    state: 'SUSPECT',
    reason: 'health_lease_expired',
    available_slots: 0,
    lease_remaining_ms: 0,
    control_queue_depth: 2,
    queue_delay_ms: 130,
  };
  sendMux([pool({ config, health_suspects: 8, ready_workers: 0, available_slots: 0 })], [suspect]);
  expect(screen.getAllByText('SUSPECT').every(element => element.className.includes('st-warn'))).toBe(true);
  expect(screen.getByText(/当前 1 条 Worker 已暂停接收新业务流；既有业务流继续保留/)).toBeTruthy();
  const runtime = screen.getByText('运行参数').parentElement?.textContent;
  expect(runtime).toContain('健康租约 15.00 s');
  expect(runtime).toContain('确认窗口 4.00 s');
  expect(runtime).toContain('活跃恢复 2 次 ACK');
  expect(runtime).toContain('收尾宽限 10.00 s');
  expect(screen.getByText('2 / 130 ms')).toBeTruthy();
  expect(screen.getByText(/疑似失活/).textContent).toBe('疑似失活 0');
  sendMux(
    [pool({ config, health_suspects: 9, health_recoveries: 1, health_draining: 1, health_dial_throttled: 3 })],
    [{ ...suspect, state: 'DRAINING', phase: 'draining', reason: 'confirmation_timeout' }],
  );
  expect(screen.getByText(/疑似失活/).textContent).toBe('疑似失活 1');
  expect(screen.getByText(/健康排空/).textContent).toBe('健康排空 1');
  expect(screen.getByText(/替换退避拒绝/).textContent).toBe('替换退避拒绝 3');
  expect(screen.getAllByText(/确认超时，保留旧流排空/).length).toBeGreaterThan(0);
});

it('distinguishes ending quarantine, ending timeout and base capacity reclaim', () => {
  render(<MuxObservationCard nodeId="node-a" />);
  sendMux(
    [pool({ active_sessions: 0, available_slots: 0 })],
    [{ ...worker, active_sessions: 0, available_slots: 0, phase: 'ending', reason: 'session_ending' }],
    [
      {
        ...worker,
        sequence: 1,
        at_unix_ms: Date.now() - 1000,
        from: 'READY',
        state: 'CLOSED',
        reason: 'capacity_reclaim',
      },
      { ...worker, sequence: 2, at_unix_ms: Date.now(), from: 'READY', state: 'DEAD', reason: 'session_end_timeout' },
    ],
  );
  expect(screen.getByText(/可复用槽位/).textContent).toContain('0');
  expect(screen.getByText('收尾中 · 等待收尾写入 · session_ending')).toBeTruthy();
  expect(screen.getByText('基础容量回收 · capacity_reclaim')).toBeTruthy();
  expect(screen.getByText('收尾写入超时 · session_end_timeout')).toBeTruthy();
});

it('keeps the aligned Mux summary concise and filters the bounded detail table', () => {
  render(<MuxObservationCard nodeId="node-a" />);
  const workers = [
    { ...worker, worker_id: 'worker-ready', active_sessions: 1 },
    { ...worker, worker_id: 'worker-slow', active_sessions: 0, queue_delay_ms: 90 },
    { ...worker, worker_id: 'worker-suspect', state: 'SUSPECT', reason: 'probe_timeout', rtt_ms: 0 },
  ];
  sendMux([pool({ ready_workers: 2, total_workers: 3 })], workers);

  const card = screen.getByLabelText('Mux 连接复用');
  const summary = card.querySelector('.ov-one.ov-mux > summary')!;
  expect(summary.querySelector('.tail')).toBeNull();
  expect(summary.textContent).not.toContain('运行正常');
  expect(summary.textContent).not.toContain('状态异常');
  expect(summary.textContent).not.toContain('性能异常');
  expect(screen.getByText('3 条结果')).toBeTruthy();

  fireEvent.click(screen.getByRole('button', { name: '非就绪' }));
  expect(screen.getByText('1 条结果')).toBeTruthy();
  expect(card.querySelectorAll('.ov-scroll tbody tr')).toHaveLength(1);

  fireEvent.click(screen.getByRole('button', { name: '性能异常' }));
  expect(screen.getByText('1 条结果')).toBeTruthy();
  expect(card.querySelector('.ov-scroll tbody')?.textContent).toContain('ker-slow');
});

it('shares one node realtime connection with the reverse-health card', () => {
  render(
    <NodeRealtimeProvider nodeId="node-a">
      <MuxObservationCard nodeId="node-a" />
      <ReverseHealthCard nodeId="node-a" />
    </NodeRealtimeProvider>,
  );
  expect(Events.opened).toBe(1);
  sendMux([pool()]);
  expect(screen.getByLabelText('Mux 连接复用')).toBeTruthy();
});

it('falls back to bounded snapshot polling when the tunnel cannot carry SSE', async () => {
  const now = Date.now();
  const request = vi.fn().mockResolvedValue({
    ok: true,
    status: 200,
    json: async () => ({
      nodes: [
        {
          node_id: 'node-a',
          connected: true,
          samples: [
            {
              node_id: 'node-a',
              received_at_unix_millis: now,
              sample: {
                sampled_at_unix_millis: now,
                mux: {
                  boot_id: '9',
                  sequence: 3,
                  sampled_at_unix_ms: now,
                  pools: [pool()],
                  workers: [worker],
                  events: [],
                },
              },
            },
          ],
        },
      ],
    }),
  });
  vi.stubGlobal('fetch', request);

  render(<MuxObservationCard nodeId="node-a" />);
  await act(async () => {
    Events.current.onerror?.();
    await Promise.resolve();
    await Promise.resolve();
  });

  expect(Events.current.closed).toBe(true);
  expect(request).toHaveBeenCalledWith(
    '/realtime/nodes/node-a/snapshot',
    expect.objectContaining({ credentials: 'include' }),
  );
  expect(screen.getByLabelText('Mux 连接复用')).toBeTruthy();
});

it('parses ordinary outbound tags without guessing malformed values', () => {
  expect(parseMuxPair('out:app-a/jp-a>tyo-exit')).toEqual({ app: 'app-a', chain: 'jp-a', peer: 'tyo-exit' });
  expect(parseMuxPair('out:jp-a>tyo-exit')).toEqual({ app: null, chain: 'jp-a', peer: 'tyo-exit' });
  expect(parseMuxPair('rev:portal:jp-a>tyo-exit')).toBeNull();
  expect(parseMuxPair('out:jp-a')).toBeNull();
});
