import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { confirmDiscardChanges, prepareForDocumentReload, useUnsavedChanges } from '../src/ui/navigation-guard';

function DirtySurface({
  active = true,
  label,
  scope,
  exposeClear,
  preserveOnLeave,
}: {
  active?: boolean;
  label: string;
  scope?: string;
  exposeClear?: (clear: () => void) => void;
  preserveOnLeave?: () => void;
}) {
  const clear = useUnsavedChanges(active, label, scope, preserveOnLeave);
  exposeClear?.(clear);
  return null;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it('allows navigation without prompting when local edits cannot be preserved', () => {
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  render(
    <>
      <DirtySurface label="机器身份" />
      <DirtySurface label="DNS" />
    </>,
  );

  expect(confirmDiscardChanges()).toBe(true);
  expect(confirm).not.toHaveBeenCalled();
});

it('preserves only the tab or dialog that is about to be discarded', () => {
  const preserveConfig = vi.fn();
  const preserveDetail = vi.fn();
  render(
    <>
      <DirtySurface label="配置页字段" scope="config" preserveOnLeave={preserveConfig} />
      <DirtySurface label="仍会保留的身份字段" scope="detail" preserveOnLeave={preserveDetail} />
    </>,
  );

  expect(confirmDiscardChanges('config')).toBe(true);
  expect(preserveConfig).toHaveBeenCalledOnce();
  expect(preserveDetail).not.toHaveBeenCalled();
});

it('never blocks browser unload while a local edit is dirty', () => {
  render(<DirtySurface label="Agent 发布范围" />);
  const dirtyEvent = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(dirtyEvent);
  expect(dirtyEvent.defaultPrevented).toBe(false);
});

it('can synchronously mark a successful edit saved before same-tick navigation', () => {
  let clear: () => void = () => {};
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  render(<DirtySurface label="新隧道" exposeClear={next => (clear = next)} />);

  clear();

  expect(confirmDiscardChanges()).toBe(true);
  expect(confirm).not.toHaveBeenCalled();
});

it('preserves eligible local edits instead of showing a destructive navigation prompt', () => {
  const preserve = vi.fn();
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  render(<DirtySurface label="规则表" preserveOnLeave={preserve} />);

  expect(confirmDiscardChanges()).toBe(true);
  expect(preserve).toHaveBeenCalledOnce();
  expect(confirm).not.toHaveBeenCalled();

  const unload = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(unload);
  expect(unload.defaultPrevented).toBe(false);
});

it('still allows navigation when preserving an edit fails', () => {
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  render(
    <DirtySurface
      label="规则表"
      preserveOnLeave={() => {
        throw new Error('storage unavailable');
      }}
    />,
  );

  expect(confirmDiscardChanges()).toBe(true);
  expect(confirm).not.toHaveBeenCalled();
});

it('only permits a document reload after volatile edits have been saved', () => {
  let clear: () => void = () => {};
  render(<DirtySurface label="新密码" exposeClear={next => (clear = next)} />);

  expect(prepareForDocumentReload()).toBe(false);
  clear();
  expect(prepareForDocumentReload()).toBe(true);
});

it('persists eligible edits before permitting a document reload', () => {
  const preserve = vi.fn();
  render(<DirtySurface label="规则表" preserveOnLeave={preserve} />);

  expect(prepareForDocumentReload()).toBe(true);
  expect(preserve).toHaveBeenCalledOnce();
});
