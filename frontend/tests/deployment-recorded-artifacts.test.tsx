import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { DeploymentTargetDetail } from '../src/api';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addListener() {},
  removeListener() {},
  addEventListener() {},
  removeEventListener() {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;
const { RecordedArtifactChanges } = await import('../src/panes/deploy');

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function show(before: unknown, after: unknown) {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    if (String(input) !== '/nodes/agent-state') throw new Error(`Unexpected revision lookup: ${input}`);
    return new Response(JSON.stringify({ nodes: [{ node_id: 'n1', name: '测试节点' }] }), {
      headers: { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  const target: DeploymentTargetDetail = {
    node_id: 'n1',
    status: 'succeeded',
    error: null,
    wave: 0,
    disruptive: true,
    desired_structure: { xray: after },
    observed_before: before === undefined ? null : { xray: before },
    observed_after: null,
    verdict: null,
    dispatched_at: null,
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <RecordedArtifactChanges targets={[target]} />
    </QueryClientProvider>,
  );
}

it('直接显示保存的配置差异，不按同一修订重新生成两份相同产物', async () => {
  const view = show(
    { state: 'present', sha256: 'before', content: '{"serverNames":["old.example","new.example"]}' },
    { state: 'present', sha256: 'after', content: '{"serverNames":["new.example","old.example"]}' },
  );
  await view.findByText('测试节点');
  fireEvent.click(view.getByText('xray.json'));
  expect(view.container.querySelector('tr.del')?.textContent).toContain('["old.example","new.example"]');
  expect(view.container.querySelector('tr.add')?.textContent).toContain('["new.example","old.example"]');
});

it('旧原文缺失时显示实际哈希，不把未知基线当成没有变化', async () => {
  const view = show(
    { state: 'present', sha256: 'before-hash' },
    { state: 'present', sha256: 'after-hash', content: '{}' },
  );
  await view.findByText('测试节点');
  fireEvent.click(view.getByText('xray.json'));
  expect(view.getByText('before-hash')).toBeTruthy();
  expect(view.getByText('after-hash')).toBeTruthy();
  expect(view.container.querySelectorAll('tr.add')).toHaveLength(0);
});
