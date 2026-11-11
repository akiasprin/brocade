import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useFleetTrafficSource } from '../src/fleet-traffic-source';

class Events {
  static sources: Events[] = [];
  handlers = new Map<string, (event: { data: string }) => void>();
  onerror: (() => void) | null = null;
  closed = false;
  constructor(readonly url: string) {
    Events.sources.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void) {
    this.handlers.set(name, handler);
  }
  close() {
    this.closed = true;
  }
  send(name: string, payload: unknown) {
    this.handlers.get(name)?.({ data: JSON.stringify(payload) });
  }
}
const payload = { policy: { enabled: true }, nodes: [], expected: [] };
const advance = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
beforeEach(() => {
  vi.useFakeTimers();
  Events.sources = [];
  vi.stubGlobal('EventSource', Events);
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json(payload)),
  );
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it('reconnects after a push failure and stops fallback downloads once push resumes', async () => {
  renderHook(() => useFleetTrafficSource());
  await advance(0);
  act(() => Events.sources[0].send('snapshot', payload));
  act(() => Events.sources[0].onerror?.());
  await advance(0);
  expect(Events.sources[0].closed).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(2);
  await advance(1_999);
  expect(Events.sources).toHaveLength(1);
  await advance(1);
  expect(Events.sources).toHaveLength(2);
  act(() => Events.sources[1].send('snapshot', payload));
  await advance(20_000);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it('pauses hidden-page requests and reconnects with fresh authorization on return', async () => {
  let visibility: DocumentVisibilityState = 'visible';
  vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility);
  renderHook(() => useFleetTrafficSource());
  await advance(0);
  act(() => {
    Events.sources[0].onerror?.();
    visibility = 'hidden';
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await advance(0);
  const reads = vi.mocked(fetch).mock.calls.length;
  await advance(60_000);
  expect(fetch).toHaveBeenCalledTimes(reads);
  expect(Events.sources).toHaveLength(1);
  act(() => {
    visibility = 'visible';
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await advance(0);
  expect(Events.sources).toHaveLength(2);
  expect(fetch).toHaveBeenCalledTimes(reads + 1);
});

it('bounds polling without EventSource and cancels the timer on unmount', async () => {
  vi.stubGlobal('EventSource', undefined);
  const page = renderHook(() => useFleetTrafficSource());
  await advance(0);
  await advance(4_999);
  expect(fetch).toHaveBeenCalledTimes(1);
  await advance(1);
  expect(fetch).toHaveBeenCalledTimes(2);
  page.unmount();
  await advance(30_000);
  expect(fetch).toHaveBeenCalledTimes(2);
});
