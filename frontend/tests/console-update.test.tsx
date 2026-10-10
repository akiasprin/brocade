import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ConsoleUpdate } from '../src/ui/console-update';
import { useUnsavedChanges } from '../src/ui/navigation-guard';

const addVersionMarker = (version = 'current') => {
  const marker = document.createElement('meta');
  marker.name = 'brocade-ui-version';
  marker.content = version;
  document.head.append(marker);
};

function TestRoot({ children }: { children: React.ReactNode }) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
}

function DirtySurface({ exposeClear }: { exposeClear: (clear: () => void) => void }) {
  const clear = useUnsavedChanges(true, '未保存的密码');
  exposeClear(clear);
  return null;
}

afterEach(() => {
  cleanup();
  document.querySelectorAll('meta[name="brocade-ui-version"]').forEach(marker => marker.remove());
  vi.restoreAllMocks();
});

it('consumes the next click, reports the update, then reloads', async () => {
  addVersionMarker();
  const readLatestVersion = vi.fn().mockResolvedValue('next');
  const reloadPage = vi.fn();
  const oldAction = vi.fn();
  render(
    <TestRoot>
      <ConsoleUpdate readLatestVersion={readLatestVersion} reloadDelayMs={0} reloadPage={reloadPage} />
      <button onClick={oldAction}>旧页面操作</button>
    </TestRoot>,
  );

  await waitFor(() => expect(readLatestVersion).toHaveBeenCalledOnce());
  await act(async () => undefined);
  fireEvent.click(screen.getByRole('button', { name: '旧页面操作' }));

  expect(oldAction).not.toHaveBeenCalled();
  expect(screen.getByRole('status').textContent).toContain('网站版本已更新，加载新版中…');
  await waitFor(() => expect(reloadPage).toHaveBeenCalledOnce());
});

it('lets a save interaction finish when volatile edits make reloading unsafe', async () => {
  addVersionMarker();
  const readLatestVersion = vi.fn().mockResolvedValue('next');
  const reloadPage = vi.fn();
  const save = vi.fn();
  let clear = () => {};
  render(
    <TestRoot>
      <ConsoleUpdate readLatestVersion={readLatestVersion} reloadDelayMs={0} reloadPage={reloadPage} />
      <DirtySurface exposeClear={next => (clear = next)} />
      <button onClick={save}>保存</button>
    </TestRoot>,
  );

  await waitFor(() => expect(readLatestVersion).toHaveBeenCalledOnce());
  await act(async () => undefined);
  fireEvent.click(screen.getByRole('button', { name: '保存' }));

  expect(save).toHaveBeenCalledOnce();
  expect(reloadPage).not.toHaveBeenCalled();
  expect(screen.getByRole('status').textContent).toContain('新版可用，完成当前操作后加载');

  clear();
  fireEvent.click(screen.getByRole('button', { name: '保存' }));
  expect(save).toHaveBeenCalledOnce();
  await waitFor(() => expect(reloadPage).toHaveBeenCalledOnce());
});

it('recovers from a removed lazy chunk through the same reload status', async () => {
  const reloadPage = vi.fn();
  render(
    <TestRoot>
      <ConsoleUpdate reloadDelayMs={0} reloadPage={reloadPage} />
    </TestRoot>,
  );

  const preloadError = new Event('vite:preloadError', { cancelable: true });
  act(() => window.dispatchEvent(preloadError));

  expect(preloadError.defaultPrevented).toBe(true);
  expect(screen.getByRole('status').textContent).toContain('网站版本已更新，加载新版中…');
  await waitFor(() => expect(reloadPage).toHaveBeenCalledOnce());
});

it('does not compare a Vite development page with the proxied production version', async () => {
  const readLatestVersion = vi.fn().mockResolvedValue('production');
  render(
    <TestRoot>
      <ConsoleUpdate readLatestVersion={readLatestVersion} />
    </TestRoot>,
  );

  await act(async () => undefined);
  expect(readLatestVersion).not.toHaveBeenCalled();
});
