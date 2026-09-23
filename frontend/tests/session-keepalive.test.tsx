import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { SESSION_KEEPALIVE_INTERVAL_MS, SessionProvider } from '../src/session';

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it('keeps an open authenticated console session active and stops after unmount', async () => {
  vi.useFakeTimers();
  const fetchMock = vi.fn(
    async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(
        JSON.stringify({
          operator_id: 'root',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
  );
  vi.stubGlobal('fetch', fetchMock);

  const view = render(
    <SessionProvider
      value={{
        who: {
          operator_id: 'root',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 0, chain_group_count: [] },
      }}
    >
      <div>console</div>
    </SessionProvider>,
  );

  expect(fetchMock).not.toHaveBeenCalled();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(SESSION_KEEPALIVE_INTERVAL_MS);
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(fetchMock.mock.calls[0]?.[0]).toBe('/whoami');
  expect(fetchMock.mock.calls[0]?.[1]?.cache).toBe('no-store');

  view.unmount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(SESSION_KEEPALIVE_INTERVAL_MS);
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
});
