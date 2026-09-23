import { act, cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cancelVisualTransition, motionOriginFor, runVisualTransition } from '../src/ui/motion';
import { usePresence } from '../src/ui/presence';

function Surface({ open, duration = 180 }: { open: boolean; duration?: number }) {
  const presence = usePresence(open, duration);
  return presence.present ? <div data-testid="surface" data-phase={presence.phase} /> : null;
}

const flushPresence = () => act(async () => Promise.resolve());

const nativeViewTransition = Object.getOwnPropertyDescriptor(document, 'startViewTransition');

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => true,
    })),
  );
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  if (nativeViewTransition) Object.defineProperty(document, 'startViewTransition', nativeViewTransition);
  else Reflect.deleteProperty(document, 'startViewTransition');
  delete document.documentElement.dataset.motionTransition;
  document.documentElement.style.removeProperty('--motion-origin-x');
  document.documentElement.style.removeProperty('--motion-origin-y');
  document.documentElement.style.removeProperty('--motion-reveal-radius');
});

it('anchors keyboard-triggered appearance motion to the control center', () => {
  const target = document.createElement('button');
  vi.spyOn(target, 'getBoundingClientRect').mockReturnValue({
    left: 20,
    top: 30,
    width: 80,
    height: 40,
  } as DOMRect);

  expect(motionOriginFor(target, 0, 0)).toEqual({ x: 60, y: 50 });
  expect(motionOriginFor(target, 24, 36)).toEqual({ x: 24, y: 36 });
});

it('sets up and cleans a supported appearance view transition', async () => {
  let finish!: () => void;
  const finished = new Promise<void>(resolve => {
    finish = resolve;
  });
  const startViewTransition = vi.fn((update: () => void) => {
    update();
    return { finished, skipTransition: vi.fn() } as unknown as ViewTransition;
  });
  Object.defineProperty(document, 'startViewTransition', { configurable: true, value: startViewTransition });
  const update = vi.fn();

  runVisualTransition(update, 'appearance', { x: 24, y: 36 });

  expect(update).toHaveBeenCalledOnce();
  expect(startViewTransition).toHaveBeenCalledOnce();
  expect(document.documentElement.dataset.motionTransition).toBe('appearance');
  expect(document.documentElement.style.getPropertyValue('--motion-origin-x')).toBe('24px');
  expect(document.documentElement.style.getPropertyValue('--motion-origin-y')).toBe('36px');
  expect(document.documentElement.style.getPropertyValue('--motion-reveal-radius')).toMatch(/px$/);

  finish();
  await finished;
  await Promise.resolve();

  expect(document.documentElement.dataset.motionTransition).toBeUndefined();
  expect(document.documentElement.style.getPropertyValue('--motion-origin-x')).toBe('');
});

it('labels directional theme transitions without requiring a pointer origin', async () => {
  let finish!: () => void;
  const finished = new Promise<void>(resolve => {
    finish = resolve;
  });
  const startViewTransition = vi.fn((update: () => void) => {
    update();
    return { finished, skipTransition: vi.fn() } as unknown as ViewTransition;
  });
  Object.defineProperty(document, 'startViewTransition', { configurable: true, value: startViewTransition });

  runVisualTransition(vi.fn(), 'theme-light');

  expect(document.documentElement.dataset.motionTransition).toBe('theme-light');
  expect(document.documentElement.style.getPropertyValue('--motion-origin-x')).toBe('');

  finish();
  await finished;
  await Promise.resolve();

  expect(document.documentElement.dataset.motionTransition).toBeUndefined();
});

it('cancels an appearance snapshot before unrelated navigation replaces the page', async () => {
  let finish!: () => void;
  const finished = new Promise<void>(resolve => {
    finish = resolve;
  });
  const skipTransition = vi.fn();
  Object.defineProperty(document, 'startViewTransition', {
    configurable: true,
    value: (update: () => void) => {
      update();
      return { finished, skipTransition } as unknown as ViewTransition;
    },
  });

  runVisualTransition(vi.fn(), 'theme-dark');
  expect(document.documentElement.dataset.motionTransition).toBe('theme-dark');

  cancelVisualTransition();
  expect(skipTransition).toHaveBeenCalledOnce();
  expect(document.documentElement.dataset.motionTransition).toBeUndefined();

  finish();
  await finished;
});

it('keeps a closing surface mounted until its exit motion completes', async () => {
  const view = render(<Surface open={false} />);
  expect(view.queryByTestId('surface')).toBeNull();
  await flushPresence();

  view.rerender(<Surface open />);
  expect(view.getByTestId('surface').dataset.phase).toBe('entering');
  await flushPresence();
  act(() => vi.advanceTimersByTime(180));
  expect(view.getByTestId('surface').dataset.phase).toBe('entered');

  view.rerender(<Surface open={false} />);
  expect(view.getByTestId('surface').dataset.phase).toBe('exiting');
  await flushPresence();
  act(() => vi.advanceTimersByTime(179));
  expect(view.getByTestId('surface')).toBeTruthy();
  act(() => vi.advanceTimersByTime(1));
  expect(view.queryByTestId('surface')).toBeNull();
});

it('cancels an exit when the surface is reopened', async () => {
  const view = render(<Surface open duration={200} />);
  await flushPresence();
  act(() => vi.advanceTimersByTime(200));

  view.rerender(<Surface open={false} duration={200} />);
  await flushPresence();
  act(() => vi.advanceTimersByTime(80));
  view.rerender(<Surface open duration={200} />);
  expect(view.getByTestId('surface').dataset.phase).toBe('entering');

  await flushPresence();
  act(() => vi.advanceTimersByTime(200));
  expect(view.getByTestId('surface').dataset.phase).toBe('entered');
});

it('does not retain or animate surfaces when reduced motion is requested', () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: query === '(prefers-reduced-motion: reduce)',
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => true,
    })),
  );

  const view = render(<Surface open />);
  expect(view.getByTestId('surface').dataset.phase).toBe('entered');
  view.rerender(<Surface open={false} />);
  expect(view.queryByTestId('surface')).toBeNull();
});
