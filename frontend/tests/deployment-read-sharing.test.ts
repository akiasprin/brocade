import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invalidateDeploymentReads, verifyDeployment } from '../src/api';
import { draft } from '../src/draft';

function pending() {
  let resolve!: (response: Response) => void;
  const promise = new Promise<Response>(done => {
    resolve = done;
  });
  return { promise, resolve };
}
const verification = (changed: number) => ({
  revision_id: 7,
  converged: changed === 0,
  summary: { changed_targets: changed },
  targets: [],
});

beforeEach(() => {
  draft.init('read-sharing-a');
  draft.clear();
  invalidateDeploymentReads();
});
afterEach(() => {
  invalidateDeploymentReads();
  vi.unstubAllGlobals();
});

describe('deployment reads', () => {
  it('shares only outstanding identical reads and fetches fresh runtime state after completion', async () => {
    const first = pending();
    const fetch = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(Response.json(verification(0)));
    vi.stubGlobal('fetch', fetch);
    const left = verifyDeployment({ revision_id: 7 });
    const right = verifyDeployment({ revision_id: 7 });
    expect(left).toBe(right);
    expect(fetch).toHaveBeenCalledTimes(1);
    first.resolve(Response.json(verification(2)));
    expect((await left).summary.changed_targets).toBe(2);
    expect((await verifyDeployment({ revision_id: 7 })).summary.changed_targets).toBe(0);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('starts a fresh lane after an observation change, without letting the old read erase it', async () => {
    const old = pending();
    const next = pending();
    const fetch = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
    vi.stubGlobal('fetch', fetch);
    const left = verifyDeployment({ revision_id: 7 });
    invalidateDeploymentReads();
    const right = verifyDeployment({ revision_id: 7 });
    expect(right).not.toBe(left);
    old.resolve(Response.json(verification(2)));
    await left;
    expect(verifyDeployment({ revision_id: 7 })).toBe(right);
    next.resolve(Response.json(verification(0)));
    await right;
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('does not share reads across operators or retain failed requests', async () => {
    const old = pending();
    const fetch = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce(new Response('{}', { status: 503 }))
      .mockResolvedValueOnce(Response.json(verification(0)));
    vi.stubGlobal('fetch', fetch);
    const left = verifyDeployment({ revision_id: 7 });
    draft.init('read-sharing-b');
    await expect(verifyDeployment({ revision_id: 7 })).rejects.toThrow();
    expect((await verifyDeployment({ revision_id: 7 })).summary.changed_targets).toBe(0);
    old.resolve(Response.json(verification(2)));
    await left;
    expect(fetch).toHaveBeenCalledTimes(3);
  });
});
