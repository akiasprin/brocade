import { Suspense, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { lazy } from '../src/ui/lazy';
import { ConsoleUpdate } from '../src/ui/console-update';
import { useUnsavedChanges } from '../src/ui/navigation-guard';
import { useNodeTokenReceipt } from '../src/ui/node-token-receipt';
afterEach(() => {
  cleanup();
  sessionStorage.clear();
  vi.restoreAllMocks();
});
it('contains a lazy rejection, retains the surrounding edit and leaves recovery mounted', async () => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  const Failed = lazy(() => Promise.reject(new Error('network unavailable')));
  const reload = vi.fn();
  function Editor() {
    const [value, setValue] = useState('unsaved');
    const clear = useUnsavedChanges(!!value, '编辑中');
    return (
      <>
        <input aria-label="编辑" value={value} onChange={e => setValue(e.target.value)} />
        <button
          onClick={() => {
            clear();
            setValue('');
          }}
        >
          保存
        </button>
      </>
    );
  }
  render(
    <QueryClientProvider client={new QueryClient()}>
      <ConsoleUpdate reloadDelayMs={0} reloadPage={reload} />
      <Editor />
      <Suspense fallback={null}>
        <Failed />
      </Suspense>
    </QueryClientProvider>,
  );
  expect(await screen.findByRole('alert')).toBeTruthy();
  expect((screen.getByLabelText('编辑') as HTMLInputElement).value).toBe('unsaved');
  expect(screen.getByRole('status').textContent).toContain('完成当前操作');
  expect(reload).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: '保存' }));
  fireEvent.click(screen.getByRole('button', { name: '重新加载界面' }));
  await waitFor(() => expect(reload).toHaveBeenCalledOnce());
});
it('keeps a late one-time reply under its issuing machine and recovers it on return', async () => {
  const client = new QueryClient();
  let receive!: ReturnType<typeof useNodeTokenReceipt>['receive'];
  function Card({ node }: { node: string }) {
    const state = useNodeTokenReceipt(node);
    receive = state.receive;
    return (
      <p>
        {node}:{state.receipt?.install_command ?? 'empty'}
      </p>
    );
  }
  const view = render(
    <QueryClientProvider client={client}>
      <Card node="A" />
    </QueryClientProvider>,
  );
  const originalReceive = receive;
  view.rerender(
    <QueryClientProvider client={client}>
      <Card node="B" />
    </QueryClientProvider>,
  );
  act(() => {
    originalReceive({ node_id: 'A', token: 'TEST_ONLY', token_prefix: 'test', install_command: 'TEST_ONLY_A' });
  });
  expect(screen.getByText('B:empty')).toBeTruthy();
  view.rerender(
    <QueryClientProvider client={client}>
      <Card node="A" />
    </QueryClientProvider>,
  );
  expect(await screen.findByText('A:TEST_ONLY_A')).toBeTruthy();
  expect(localStorage.getItem('node-token-receipt')).toBeNull();
  client.clear();
});
