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

describe('发布预览中的用户订阅变更', () => {
  it('显示具体用户，并在只有一人时直接展开订阅内容差异', async () => {
    const before = 'vless://old.example#旧入口';
    const after = 'vless://new.example#新入口';
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/nodes/agent-state') return json({ nodes: [{ node_id: 'n1', name: '台北一号' }] });
      if (path === '/artifacts/index?revision=1144') {
        return json({
          revision: 1144,
          artifacts: [
            {
              target_kind: 'node',
              target_id: 'n1',
              artifact_kind: 'xray',
              state: 'present',
              sha256: 'node-same',
              byte_len: 2,
            },
            {
              target_kind: 'user',
              target_id: 'platform.acme:alice',
              artifact_kind: 'uri',
              state: 'present',
              sha256: 'subscription-after',
              byte_len: after.length,
            },
          ],
        });
      }
      if (path === '/artifacts/index?revision=1143') {
        return json({
          revision: 1143,
          artifacts: [
            {
              target_kind: 'node',
              target_id: 'n1',
              artifact_kind: 'xray',
              state: 'present',
              sha256: 'node-same',
              byte_len: 2,
            },
            {
              target_kind: 'user',
              target_id: 'platform.acme:alice',
              artifact_kind: 'uri',
              state: 'present',
              sha256: 'subscription-before',
              byte_len: before.length,
            },
          ],
        });
      }
      if (path === '/artifacts/content/user/platform.acme%3Aalice/uri?revision=1144') {
        return json({ content: after, redacted: false });
      }
      if (path === '/artifacts/content/user/platform.acme%3Aalice/uri?revision=1143') {
        return json({ content: before, redacted: false });
      }
      throw new Error(`未预期的请求：${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ArtifactChanges revision={1144} base={1143} targets={[{ node_id: 'n1' }]} />
      </QueryClientProvider>,
    );

    expect(await view.findByText('用户 alice')).toBeTruthy();
    expect(view.getByText('用户订阅变更 · 1 人')).toBeTruthy();
    expect(view.getByText('与修订 1143 相比，上方 1 台机器的产物没有变化。')).toBeTruthy();
    await view.findByText((_, element) => element?.tagName === 'TD' && element.textContent === after);
    expect(view.container.querySelector('tr.add')?.textContent).toContain('new.example');
    expect(view.container.querySelector('tr.del')?.textContent).toContain('old.example');
  });
});
