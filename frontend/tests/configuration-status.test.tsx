import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ArtifactIndexEntry } from '../src/api';
import { changedConfigurationNodes } from '../src/forge/artifacts';
import { useConfigurationStatus } from '../src/forge/configuration-status';

const clients: QueryClient[] = [];
const entry = (node: string, sha = 'a', kind = 'xray'): ArtifactIndexEntry => ({
  target_kind: 'node',
  target_id: node,
  artifact_kind: kind,
  state: 'present',
  sha256: sha.repeat(64),
  byte_len: 100,
});
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

function mount(
  before: ArtifactIndexEntry[],
  after: ArtifactIndexEntry[],
  { released = 1 as number | null | undefined, revision = 3 as number | undefined } = {},
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  if (released != null) client.setQueryData(['artifact-index', released], { revision: released, artifacts: before });
  if (revision != null) client.setQueryData(['artifact-index', revision], { revision, artifacts: after });
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

it('does not advertise an initialized new machine while the business baseline remains unchanged', async () => {
  let resolve!: (value: Response) => void;
  const fetch = vi.fn(
    () =>
      new Promise<Response>(done => {
        resolve = done;
      }),
  );
  vi.stubGlobal('fetch', fetch);
  expect([...changedConfigurationNodes([entry('old'), entry('new')], [entry('old')])]).toEqual(['new']);
  const { client } = mount([entry('old')], [entry('old'), entry('new')]);
  expect(screen.getByRole('status').textContent).toBe('unknown');
  expect(fetch).toHaveBeenCalledOnce();
  expect(fetch.mock.calls[0]).toMatchObject([
    '/deployments/verify',
    { method: 'POST', body: JSON.stringify({ revision_id: 3 }) },
  ]);
  resolve(verified(0));
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  expect(client.getQueryData(['artifact-index', 1])).toEqual({ revision: 1, artifacts: [entry('old')] });
});

it('also recognizes successful initialization before the first business release', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => verified(0)),
  );
  mount([], [entry('new')], { released: null });
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
});

it('preserves real configuration work reported for a new machine', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => verified(1)),
  );
  mount([entry('old')], [entry('old'), entry('new', 'b')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
});

it('clears the hint after initialization completes without creating a new model revision', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  const fetch = vi
    .fn()
    .mockImplementationOnce(async () => verified(1))
    .mockImplementation(async () => verified(0));
  vi.stubGlobal('fetch', fetch);
  mount([entry('old')], [entry('old'), entry('new')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  await act(() => vi.advanceTimersByTimeAsync(5_000));
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  expect(fetch).toHaveBeenCalledTimes(2);
});

it('checks subsequent edits against live state even when initialization was previously consistent', async () => {
  const fetch = vi
    .fn()
    .mockImplementationOnce(async () => verified(0))
    .mockImplementation(async () => verified(1));
  vi.stubGlobal('fetch', fetch);
  const { client, rerender } = mount([entry('old')], [entry('old'), entry('new')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  client.setQueryData(['artifact-index', 4], { revision: 4, artifacts: [entry('old'), entry('new', 'b')] });
  rerender(
    <QueryClientProvider client={client}>
      <Status revision={4} released={1} />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  expect(fetch).toHaveBeenLastCalledWith('/deployments/verify', expect.objectContaining({ body: '{"revision_id":4}' }));
});

it.each([
  ['ordinary edits', [entry('old')], [entry('old', 'b')]],
  ['old and new machine edits', [entry('old')], [entry('old', 'b'), entry('new')]],
  ['enabling an existing disabled machine', [{ ...entry('old'), state: 'disabled', sha256: null }], [entry('old')]],
] as const)('keeps %s on the lightweight index path', (_name, before, after) => {
  const fetch = vi.fn();
  vi.stubGlobal('fetch', fetch);
  mount([...before], [...after]);
  expect(screen.getByRole('status').textContent).toBe('pending');
  expect(fetch).not.toHaveBeenCalled();
});

it('does not mistake grant-only and subscription changes for configuration work', async () => {
  const fetch = vi.fn(async () => verified(0));
  vi.stubGlobal('fetch', fetch);
  mount(
    [entry('old'), entry('old', 'a', 'grants')],
    [entry('old'), entry('old', 'b', 'grants'), { ...entry('user', 'c', 'uri'), target_kind: 'user' }],
  );
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  expect(fetch).not.toHaveBeenCalled();
});

it('does not verify when grant and subscription artifacts are removed', async () => {
  const fetch = vi.fn(async () => verified(0));
  vi.stubGlobal('fetch', fetch);
  mount(
    [entry('old'), entry('old', 'a', 'grants'), { ...entry('user', 'c', 'uri'), target_kind: 'user' }],
    [entry('old')],
  );
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('consistent'));
  expect(fetch).not.toHaveBeenCalled();
});

it('advertises a pure container change even when ordinary configuration artifacts are identical', async () => {
  const fetch = vi.fn(async () => verified(1));
  vi.stubGlobal('fetch', fetch);
  mount([entry('old')], [entry('old')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('pending'));
  expect(fetch).toHaveBeenCalledWith('/deployments/verify', expect.objectContaining({ body: '{"revision_id":3}' }));
});

it('keeps pure container status unknown when live verification fails', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: 'verification unavailable' }, { status: 503 })),
  );
  mount([entry('old')], [entry('old')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('error'));
});

it('reports a failed live check instead of claiming that initialization is pending or complete', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => Response.json({ error: 'verification unavailable' }, { status: 503 })),
  );
  mount([entry('old')], [entry('old'), entry('new')]);
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('error'));
});

it('does not verify when the caller disables configuration status for a draft or visitor', () => {
  const fetch = vi.fn();
  vi.stubGlobal('fetch', fetch);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  render(
    <QueryClientProvider client={client}>
      <Status released={1} />
    </QueryClientProvider>,
  );
  expect(screen.getByRole('status').textContent).toBe('unknown');
  expect(fetch).not.toHaveBeenCalled();
});
