import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { CvmArtifacts } from '../../src/panes/cvm/artifacts';
import { artifactHistory, artifactReceipt } from '../fixtures/cvm/artifact-data';
import { cvmWorkspace } from '../fixtures/cvm/data';
import type { CvmArtifactHistory, CvmInstance } from '../../src/api';
let client: QueryClient;
afterEach(() => {
  cleanup();
  client?.clear();
  vi.unstubAllGlobals();
});
function show(
  history = artifactHistory(),
  mismatch = false,
  mode: 'shadow' | 'enforce' = 'shadow',
  prepare?: (instance: CvmInstance, history: CvmArtifactHistory) => void,
) {
  const instance = cvmWorkspace('detail', 'system-admin').instances[0];
  instance.desired.generation = history.generation;
  artifactReceipt(instance, mismatch);
  instance.report!.artifacts!.mode = mode;
  prepare?.(instance, history);
  const calls: string[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async input => {
      calls.push(String(input));
      return Response.json(history);
    }),
  );
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const content = () => (
    <QueryClientProvider client={client}>
      <CvmArtifacts instance={instance} />
    </QueryClientProvider>
  );
  const view = render(content());
  return { instance, calls, refresh: () => view.rerender(content()) };
}
it('fetches one authorized generation lazily and compares canonical artifact content', async () => {
  const { instance, calls } = show();
  expect(await screen.findByText(/影子比对一致/)).toBeTruthy();
  expect(calls).toEqual([`/cvm/${instance.desired.id}/artifacts?generation=${instance.desired.generation}`]);
  fireEvent.click(screen.getByRole('button', { name: /网卡附件/ }));
  expect(screen.getByLabelText('产物差异').textContent).toContain('+');
  expect(screen.getByRole('button', { name: /网卡附件/ }).textContent).toContain('内容变化');
  fireEvent.click(screen.getByRole('button', { name: '全文' }));
  expect(screen.getByLabelText('产物全文').textContent).toBe(artifactHistory().current!.files[2].content);
  expect(screen.getByText('尚无已确认的生效摘要')).toBeTruthy();
});
it('shows shadow mismatches without claiming an enforced application', async () => {
  show(artifactHistory(), true);
  expect(await screen.findByText(/影子比对不一致/)).toBeTruthy();
  expect(screen.queryByText('Console 产物执行')).toBeNull();
});
it('does not fabricate compiled history for a legacy generation', async () => {
  show({ generation: 1, current: null, previous: null });
  expect(await screen.findByText(/这一代没有编译产物/)).toBeTruthy();
  expect(screen.queryByRole('button', { name: '复制产物' })).toBeNull();
});

it('enforced mismatch explains refusal instead of claiming a legacy fallback', async () => {
  show(artifactHistory(), true, 'enforce');
  expect(await screen.findByText(/产物校验不一致.*本轮已拒绝执行/)).toBeTruthy();
  expect(screen.queryByText(/仍由原规格路径执行/)).toBeNull();
});

it.each(['stop', 'restart'] as const)(
  'does not reuse a preceding receipt for a pending %s with identical products',
  async action => {
    show(artifactHistory(), false, 'enforce', (instance, history) => {
      instance.report!.artifacts!.files[0].effective_sha256 = 'e'.repeat(64);
      instance.report!.artifacts!.files[0].bindings = { runtime_id: 'previous-runtime' };
      instance.desired.generation += 1;
      instance.desired.operation_id = `pending-${action}`;
      instance.desired.power = action === 'stop' ? 'stopped' : 'running';
      if (action === 'restart') instance.desired.restart_epoch += 1;
      instance.status = 'pending';
      history.generation = instance.desired.generation;
    });
    expect(await screen.findByText('尚无这一代的产物比对回执')).toBeTruthy();
    expect(screen.queryByText('Console 产物执行')).toBeNull();
    expect(screen.queryByText('e'.repeat(64))).toBeNull();
    expect(screen.queryByText('执行绑定值')).toBeNull();
    expect(screen.getByText('尚无已确认的生效摘要')).toBeTruthy();
  },
);

it.each(['generation', 'operation_id', 'input_sha256'] as const)(
  'requires matching %s even when the other receipt identities match',
  async field => {
    show(artifactHistory(), true, 'enforce', instance => {
      if (field === 'generation') instance.report!.generation -= 1;
      else if (field === 'operation_id') instance.report!.operation_id = 'another-operation';
      else instance.report!.artifacts!.input_sha256 = 'f'.repeat(64);
    });
    expect(await screen.findByText('尚无这一代的产物比对回执')).toBeTruthy();
    expect(screen.queryByText(/本轮已拒绝执行/)).toBeNull();
  },
);

it('shows the current operation receipt when it arrives after a stale report', async () => {
  const { instance, refresh } = show(artifactHistory(), false, 'enforce', (instance, history) => {
    instance.desired.generation += 1;
    instance.desired.operation_id = 'current-restart';
    instance.status = 'pending';
    history.generation = instance.desired.generation;
  });
  expect(await screen.findByText('尚无这一代的产物比对回执')).toBeTruthy();
  instance.report!.generation = instance.desired.generation;
  instance.report!.operation_id = instance.desired.operation_id;
  instance.report!.artifacts!.files[0].effective_sha256 = 'e'.repeat(64);
  instance.status = 'applied';
  refresh();
  expect(await screen.findByText('Console 产物执行')).toBeTruthy();
  expect(screen.getByText('e'.repeat(64))).toBeTruthy();
  expect(screen.getByText('执行绑定值')).toBeTruthy();
});
