import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useObservationUpdates } from '../src/observation-events';
import { freshFleetNicSample, useFleetRealtime, type FleetRealtimeNode } from '../src/fleet-realtime';
import { SessionProvider } from '../src/session';
import { chainSnapshot, e2eProbes, MACHINES } from './fixtures/chain-data';
import { agentState } from './fixtures/node-roster-data';

class Events {
  static sources: Events[] = [];
  handlers = new Map<string, (event: { data: string }) => void>();
  onerror: (() => void) | null = null;
  closed = false;
  constructor(readonly url: string) {
    Events.sources.push(this);
  }
  addEventListener(name: string, callback: (event: { data: string }) => void) {
    this.handlers.set(name, callback);
  }
  close() {
    this.closed = true;
  }
  send(name: string, data: unknown) {
    this.handlers.get(name)?.({ data: JSON.stringify(data) });
  }
}

beforeEach(() => {
  vi.useFakeTimers();
  Events.sources = [];
  vi.stubGlobal('EventSource', Events);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
const advance = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

it('accepts actual zero rates and Console clock skew while rejecting gaps, disconnections and expired readings', () => {
  const event = {
    node_id: 'n',
    received_at_unix_millis: 10_000,
    sample: { rx_bytes_per_sec: 0, tx_bytes_per_sec: 0, has_gap: false },
  };
  const node: FleetRealtimeNode = { connected: true, active: true, intervalSecs: 5, latest: event };
  expect(freshFleetNicSample(node, 25_000)).toBe(event);
  expect(freshFleetNicSample(node, 25_001)).toBeNull();
  expect(freshFleetNicSample(node, 6_000)).toBe(event);
  expect(freshFleetNicSample({ ...node, connected: false }, 10_000)).toBeNull();
  expect(freshFleetNicSample({ ...node, active: false }, 10_000)).toBeNull();
  expect(
    freshFleetNicSample({ ...node, latest: { ...event, sample: { ...event.sample, has_gap: true } } }, 10_000),
  ).toBeNull();
});

function Updates() {
  useObservationUpdates();
  return null;
}

it('shares one hint stream, batches reports and refreshes observations without replacing an editor snapshot', async () => {
  const reads = { nodes: vi.fn(() => 2), e2e: vi.fn(() => 3), snapshot: vi.fn(() => 'server') };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['nodes'], 1);
  client.setQueryData(['e2e-probes'], 1);
  client.setQueryData(['snapshot'], 'editing');
  function Reads() {
    const nodes = useQuery({ queryKey: ['nodes'], queryFn: reads.nodes });
    const e2e = useQuery({ queryKey: ['e2e-probes'], queryFn: reads.e2e });
    const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: reads.snapshot });
    return <output>{`${nodes.data}:${e2e.data}:${snapshot.data}`}</output>;
  }
  const page = render(
    <QueryClientProvider client={client}>
      <Updates />
      <Updates />
      <Reads />
    </QueryClientProvider>,
  );
  expect(Events.sources).toHaveLength(1);
  const source = Events.sources[0];
  expect(source.url).toBe('/realtime/observations/events');
  act(() => {
    for (let i = 0; i < 100; i++) source.send('invalidate', ['nodes', 'e2e', 'unknown', '__proto__']);
  });
  await advance(1_100);
  expect(reads.nodes).toHaveBeenCalledTimes(1);
  expect(reads.e2e).toHaveBeenCalledTimes(1);
  expect(reads.snapshot).not.toHaveBeenCalled();
  expect(screen.getByRole('status').textContent).toBe('2:3:editing');
  page.unmount();
  expect(source.closed).toBe(true);
});

it('recovers missed results on reconnect and rejects callbacks from the old stream', async () => {
  const client = new QueryClient();
  const invalidate = vi.spyOn(client, 'invalidateQueries').mockResolvedValue();
  render(
    <QueryClientProvider client={client}>
      <Updates />
    </QueryClientProvider>,
  );
  const old = Events.sources[0];
  act(() => old.send('reset', {}));
  await advance(0);
  const current = Events.sources[1];
  expect(old.closed).toBe(true);
  act(() => old.send('invalidate', ['nodes']));
  await advance(1_100);
  expect(invalidate).not.toHaveBeenCalled();
  act(() => current.send('ready', {}));
  await advance(1_100);
  expect(invalidate).toHaveBeenCalledTimes(1);
});

it('keeps an invalidation received during a read for the next batch', async () => {
  const client = new QueryClient();
  let finish!: () => void;
  const invalidate = vi
    .spyOn(client, 'invalidateQueries')
    .mockImplementationOnce(
      () =>
        new Promise<void>(resolve => {
          finish = resolve;
        }),
    )
    .mockResolvedValue();
  render(
    <QueryClientProvider client={client}>
      <Updates />
    </QueryClientProvider>,
  );
  act(() => Events.sources[0].send('invalidate', ['e2e']));
  await advance(1_000);
  act(() => Events.sources[0].send('invalidate', ['e2e']));
  await advance(2_000);
  expect(invalidate).toHaveBeenCalledTimes(1);
  await act(async () => finish());
  await advance(1_000);
  expect(invalidate).toHaveBeenCalledTimes(2);
});

it('bounds full-fleet downloads during continuous reports without delaying faster probe hints', async () => {
  const reads = { nodes: vi.fn(() => 1), load: vi.fn(() => 1), e2e: vi.fn(() => 1) };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  for (const key of ['nodes', 'node-load-list', 'e2e-probes']) client.setQueryData([key], 0);
  function Reads() {
    useQuery({ queryKey: ['nodes'], queryFn: reads.nodes });
    useQuery({ queryKey: ['node-load-list'], queryFn: reads.load });
    useQuery({ queryKey: ['e2e-probes'], queryFn: reads.e2e });
    return null;
  }
  render(
    <QueryClientProvider client={client}>
      <Updates />
      <Reads />
    </QueryClientProvider>,
  );
  for (let i = 0; i < 31; i++) {
    act(() => Events.sources[0].send('invalidate', ['nodes', 'load', 'e2e']));
    await advance(1_000);
  }
  expect(reads.nodes).toHaveBeenCalledTimes(4);
  expect(reads.load).toHaveBeenCalledTimes(2);
  expect(reads.e2e).toHaveBeenCalledTimes(31);
});

it('closes hidden-page subscriptions and refreshes missed changes after visibility returns', async () => {
  const client = new QueryClient();
  let visibility = 'visible';
  vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility as DocumentVisibilityState);
  const invalidate = vi.spyOn(client, 'invalidateQueries').mockResolvedValue();
  render(
    <QueryClientProvider client={client}>
      <Updates />
    </QueryClientProvider>,
  );
  act(() => {
    visibility = 'hidden';
    document.dispatchEvent(new Event('visibilitychange'));
  });
  expect(Events.sources[0].closed).toBe(true);
  act(() => {
    visibility = 'visible';
    document.dispatchEvent(new Event('visibilitychange'));
  });
  expect(Events.sources).toHaveLength(2);
  act(() => Events.sources[1].send('ready', {}));
  await advance(1_000);
  expect(invalidate).toHaveBeenCalledTimes(1);
  vi.restoreAllMocks();
});

function Nic({ name }: { name: string }) {
  const view = useFleetRealtime();
  const node = view.nodes.get('n');
  return <output aria-label={name}>{`${node?.connected}:${node?.latest?.sample.rx_bytes_per_sec}`}</output>;
}

it('shares the NIC lease across readouts and cards and handles disconnects immediately', async () => {
  const page = render(
    <>
      <Nic name="header" />
      <Nic name="card" />
    </>,
  );
  expect(Events.sources).toHaveLength(1);
  const source = Events.sources[0];
  expect(source.url).toBe('/realtime/nodes/events');
  const sample = (rate: number, time: number) => ({
    node_id: 'n',
    received_at_unix_millis: time,
    sample: { rx_bytes_per_sec: rate, tx_bytes_per_sec: 0, has_gap: false },
  });
  const now = Date.now();
  act(() => source.send('sample', sample(10, now)));
  expect(screen.getByLabelText('header').textContent).toBe('true:10');
  expect(screen.getByLabelText('card').textContent).toBe('true:10');
  act(() => source.send('sample', sample(999, now - 100)));
  await advance(1_000);
  expect(screen.getByLabelText('card').textContent).toBe('true:10');
  act(() => source.send('status', { node_id: 'n', connected: false, active: false }));
  expect(screen.getByLabelText('card').textContent).toBe('false:10');
  page.rerender(<Nic name="header" />);
  expect(source.closed).toBe(false);
  page.unmount();
  expect(source.closed).toBe(true);
});

it('works without EventSource so ordinary polling remains available', () => {
  vi.stubGlobal('EventSource', undefined);
  render(
    <QueryClientProvider client={new QueryClient()}>
      <Updates />
      <Nic name="card" />
    </QueryClientProvider>,
  );
  expect(Events.sources).toHaveLength(0);
  expect(screen.getByLabelText('card').textContent).toBe('undefined:undefined');
});

it('updates a production chain card after a probe hint without waiting for its minute poll', async () => {
  const { ChainsPane } = await import('../src/panes/chains');
  const now = Math.floor(Date.now() / 1000);
  const model = chainSnapshot(false);
  const probes = e2eProbes(now);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['snapshot'], model);
  client.setQueryData(['nodes'], { nodes: MACHINES.map(machine => agentState(machine, now)) });
  client.setQueryData(['e2e-probes'], probes);
  let current = probes;
  const fetcher = vi.fn().mockImplementation(() => Promise.resolve(Response.json(current)));
  vi.stubGlobal('fetch', fetcher);
  localStorage.setItem('brocade-console:chain-view', 'cards');
  render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'reader',
            role: 'readonly',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: true,
          },
          initial: { node_count: MACHINES.length, chain_group_count: [] },
        }}
      >
        <ChainsPane
          win={{
            id: 1,
            key: 'tab:chains',
            title: '线路',
            x: 0,
            y: 0,
            w: 1000,
            h: 800,
            z: 1,
            min: false,
            home: 'desk',
            data: { drill: { p: 'list' } },
          }}
        />
      </SessionProvider>
    </QueryClientProvider>,
  );
  const latency = () => document.querySelector('.chain-card .chain-card-latency')!.textContent;
  expect(latency()).toContain('ms');
  current = {
    chains: probes.chains.map((probe, index) => (index === 0 ? { ...probe, status: 'timeout', ttfb_ms: null } : probe)),
  };
  act(() => Events.sources[0].send('invalidate', ['e2e']));
  await advance(1_100);
  expect(latency()).toContain('不通');
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(fetcher.mock.calls[0][0]).toBe('/probes/e2e?format=columnar-v1');
  expect(client.getQueryData(['snapshot'])).toEqual(model);
});
