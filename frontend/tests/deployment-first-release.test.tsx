import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { ArtifactChanges } = await import('../src/panes/deploy');

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('首次发布产物', () => {
  it('没有基线时默认展开所有目标机器的新建内容，且不请求不存在的旧修订', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/nodes/agent-state') return json({ nodes: [{ node_id: 'n1', name: '台北一号' }] });
      if (path === '/artifacts/index?revision=7') {
        return json({
          revision: 7,
          artifacts: [
            {
              target_kind: 'node',
              target_id: 'n1',
              artifact_kind: 'xray',
              state: 'present',
              sha256: 'a'.repeat(64),
              byte_len: 21,
            },
          ],
        });
      }
      if (path === '/artifacts/content/node/n1/xray?revision=7') {
        return json({
          revision: 7,
          target_kind: 'node',
          target_id: 'n1',
          artifact_kind: 'xray',
          state: 'present',
          sha256: 'a'.repeat(64),
          byte_len: 21,
          content: '{\n  "log": "warning"\n}',
          redacted: false,
        });
      }
      throw new Error(`未预期的请求：${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ArtifactChanges revision={7} base={null} targets={[{ node_id: 'n1' }]} />
      </QueryClientProvider>,
    );

    expect(await view.findByText('台北一号')).toBeTruthy();
    expect(view.getByText('1 份新建')).toBeTruthy();
    expect(await view.findByText(/warning/)).toBeTruthy();
    expect(view.container.querySelectorAll('tr.add')).toHaveLength(3);
    expect(fetchMock.mock.calls.filter(([path]) => String(path).startsWith('/artifacts/index'))).toHaveLength(1);
  });
});
