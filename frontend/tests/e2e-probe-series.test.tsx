import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchE2eProbes } from '../src/api';

const chain = {
  app_id: 'app-main',
  chain_id: 'chain-jp',
  chain_name: '日本出口',
  node_id: 'hk-edge',
  status: 'ok',
  ttfb_ms: 98,
  exit_ip: '219.104.18.20',
  exit_loc: 'JP',
  exit_verdict: 'match',
  detail: null,
  probed_at: '2026-09-16T05:15:53Z',
};

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

afterEach(() => vi.unstubAllGlobals());

describe('end-to-end probe sample series', () => {
  it('requests and keeps the exact columnar series', async () => {
    const fetch = vi.fn(async (_input: RequestInfo | URL) =>
      response({
        chains: [
          {
            ...chain,
            samples: {
              probed_at_unix_secs: [1_800_000_000, 1_800_000_060],
              status: ['ok', 'timeout'],
              ttfb_ms: [98, null],
            },
          },
        ],
      }),
    );
    vi.stubGlobal('fetch', fetch);

    const view = await fetchE2eProbes();

    expect(fetch.mock.calls[0]?.[0]).toBe('/probes/e2e?format=columnar-v1');
    expect(view.chains[0].samples).toEqual({
      probed_at_unix_secs: [1_800_000_000, 1_800_000_060],
      status: ['ok', 'timeout'],
      ttfb_ms: [98, null],
    });
  });

  it('normalizes the legacy response during a rolling Console replacement', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        response({
          chains: [
            {
              ...chain,
              samples: [{ probed_at: '2026-09-16T05:15:53Z', status: 'ok', ttfb_ms: 98 }],
            },
          ],
        }),
      ),
    );

    const view = await fetchE2eProbes();
    expect(view.chains[0].samples).toEqual({
      probed_at_unix_secs: [Date.parse('2026-09-16T05:15:53Z') / 1_000],
      status: ['ok'],
      ttfb_ms: [98],
    });
  });

  it('rejects mismatched sample columns instead of drawing corrupt points', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        response({
          chains: [
            {
              ...chain,
              samples: {
                probed_at_unix_secs: [1_800_000_000],
                status: [],
                ttfb_ms: [98],
              },
            },
          ],
        }),
      ),
    );

    await expect(fetchE2eProbes()).rejects.toThrow('线路 chain-jp 的端到端样本列长度不一致');
  });
});
