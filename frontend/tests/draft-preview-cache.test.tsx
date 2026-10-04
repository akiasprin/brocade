import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { draft } = await import('../src/draft');
const { draftPreview, fetchSnapshot, upsertGrant } = await import('../src/api');

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => {
    resolve = done;
  });
  return { promise, resolve };
}

beforeEach(() => {
  draft.init(`preview-cache-${crypto.randomUUID()}`);
  draft.clear();
});

afterEach(() => {
  draft.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it('reuses an identical in-flight preview even after the settled TTL duration', async () => {
  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'first' } });
  const response = deferred<Response>();
  const fetch = vi.fn(() => response.promise);
  vi.stubGlobal('fetch', fetch);
  let now = 1_000;
  vi.spyOn(Date, 'now').mockImplementation(() => now);

  const first = draftPreview();
  now += 10_000;
  const second = draftPreview();

  expect(second).toBe(first);
  expect(fetch).toHaveBeenCalledTimes(1);
  response.resolve(Response.json({ snapshot: {}, compile: {}, artifacts: {} }));
  await expect(first).resolves.toMatchObject({ snapshot: {} });
});

it('invalidates the cached draft projection after an immediate grant write without discarding edits', async () => {
  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'draft name' } });
  let revision = 1;
  const fetch = vi.fn(async (path: RequestInfo | URL) => {
    if (String(path) === '/grants') return Response.json({ revision_id: ++revision });
    expect(String(path)).toBe('/model/preview');
    return Response.json({
      snapshot: { snapshot: { revision }, node_egress_dns: [], redacted: false },
      compile: {},
      artifacts: {},
    });
  });
  vi.stubGlobal('fetch', fetch);
  expect((await fetchSnapshot()).snapshot.revision).toBe(1);
  await upsertGrant({
    app_id: 'app-main',
    tenant_id: 'platform.acme',
    user_id: 'alice',
    ingress_id: 'in-a',
    enabled: false,
  });
  expect((await fetchSnapshot()).snapshot.revision).toBe(2);
  expect(fetch.mock.calls.filter(([path]) => String(path) === '/model/preview')).toHaveLength(2);
  expect(draft.ops()).toEqual([{ op: 'update_node', node_id: 'A', node: { name: 'draft name' } }]);
});

it('cancels a stale preview on undo but not on submission-only notifications', async () => {
  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'first' } });
  const signals: AbortSignal[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn((_path: RequestInfo | URL, init?: RequestInit) => {
      const signal = init?.signal;
      if (signal) signals.push(signal);
      return new Promise<Response>((_resolve, reject) => {
        signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true });
      });
    }),
  );

  const pending = draftPreview();
  void pending.catch(() => {});
  const batch = draft.beginSubmission();
  expect(signals[0]?.aborted).toBe(false);
  draft.finishSubmission(batch, false);
  expect(signals[0]?.aborted).toBe(false);

  draft.drop('node:A');
  expect(signals[0]?.aborted).toBe(true);
  await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
});

it('cancels stale previews when all edits are discarded or the operator changes', async () => {
  const signals: AbortSignal[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn((_path: RequestInfo | URL, init?: RequestInit) => {
      const signal = init?.signal;
      if (signal) signals.push(signal);
      return new Promise<Response>((_resolve, reject) => {
        signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true });
      });
    }),
  );

  draft.push({ op: 'update_node', node_id: 'A', node: { name: 'first' } });
  const discarded = draftPreview();
  void discarded.catch(() => {});
  draft.clear();
  expect(signals[0]?.aborted).toBe(true);
  await expect(discarded).rejects.toMatchObject({ name: 'AbortError' });

  const previousOperator = draftPreview();
  void previousOperator.catch(() => {});
  draft.init(`preview-cache-other-${crypto.randomUUID()}`);
  expect(signals[1]?.aborted).toBe(true);
  await expect(previousOperator).rejects.toMatchObject({ name: 'AbortError' });
});
