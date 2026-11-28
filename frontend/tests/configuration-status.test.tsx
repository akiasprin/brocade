import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useConfigurationStatus } from '../src/forge/configuration-status';

const clients: QueryClient[] = [];
const verified = (changed: number) => Response.json({ summary: { changed_targets: changed } });

function Status({ revision, released }: { revision?: number; released: number | null | undefined }) {
  const status = useConfigurationStatus(revision, released);
  return (
    <output>
      {status.error
        ? 'error'
        : status.pendingChanges === undefined
          ? 'unknown'
          : status.pendingChanges
            ? 'pending'
            : 'consistent'}
    </output>
  );
}

function mount(options: { released?: number | null; revision?: number } = {}) {
  const released = Object.hasOwn(options, 'released') ? options.released : 1;
  const revision = Object.hasOwn(options, 'revision') ? options.revision : 3;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  const view = render(
    <QueryClientProvider client={client}>
      <Status revision={revision} released={released} />
    </QueryClientProvider>,
  );
  return { client, ...view };
}

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it('uses live convergence after a retired-node deployment is canceled without advancing the successful baseline', async () => {
  let resolve!: (value: Response) => void;
  const fetch = vi.fn(
    () =>
      new Promise<Response>(done => {
        resolve = done;
      }),
  );
  vi.stubGlobal('fetch', fetch);
  mount({ released: 3288, revision: 3304 });
  expect(screen.getByRole('status').textContent).toBe('unknown');
  expect(fetch).toHaveBeenCalledWith(
    '/deployments/verify',
    expect.objectContaining({ method: 'POST', body: '{"revision_id":3304}' }),
  );
  resolve(verified(0));
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
});

it('uses live verification for ordinary configuration edits', async () => {
  const fetch = vi.fn(async () => verified(1));
  vi.stubGlobal('fetch', fetch);
  mount();
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  expect(fetch).toHaveBeenCalledWith('/deployments/verify', expect.objectContaining({ body: '{"revision_id":3}' }));
});

it('also recognizes convergence before the first successful release', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => verified(0)),
  );
  mount({ released: null });
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
});

it('clears the hint after machines converge without creating a new model revision', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  const fetch = vi
    .fn()
    .mockImplementationOnce(async () => verified(1))
    .mockImplementation(async () => verified(0));
  vi.stubGlobal('fetch', fetch);
  mount();
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  await act(() => vi.advanceTimersByTimeAsync(5_000));
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  expect(fetch).toHaveBeenCalledTimes(2);
});

it('checks a subsequent revision against live state instead of reusing an earlier result', async () => {
  const fetch = vi
    .fn()
    .mockImplementationOnce(async () => verified(0))
    .mockImplementation(async () => verified(1));
  vi.stubGlobal('fetch', fetch);
  const { client, rerender } = mount();
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  rerender(
    <QueryClientProvider client={client}>
      <Status revision={4} released={1} />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  expect(fetch).toHaveBeenLastCalledWith('/deployments/verify', expect.objectContaining({ body: '{"revision_id":4}' }));
});

it('reports a failed live check instead of inferring configuration state from revision history', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: 'verification unavailable' }, { status: 503 })),
  );
  mount();
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('error'));
});

it('does not verify when configuration status is disabled or already covers the current revision', () => {
  const fetch = vi.fn();
  vi.stubGlobal('fetch', fetch);
  const disabled = mount({ revision: undefined });
  expect(screen.getByRole('status').textContent).toBe('unknown');
  disabled.unmount();
  mount({ revision: 3, released: 3 });
  expect(screen.getByRole('status').textContent).toBe('consistent');
  expect(fetch).not.toHaveBeenCalled();
});
