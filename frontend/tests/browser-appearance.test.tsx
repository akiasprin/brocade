import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { observeBrowserAppearance } from '../src/forge/browser-appearance';

let stop: (() => void) | undefined;
let style: HTMLStyleElement;
const color = () => document.querySelector<HTMLMetaElement>('meta[name="theme-color"]')!.content;
const scheme = () => document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]')!.content;

beforeEach(() => {
  vi.useFakeTimers();
  document.head.insertAdjacentHTML('beforeend', '<meta name="color-scheme" content="dark"><meta name="theme-color">');
  style = document.createElement('style');
  // JSDOM does not resolve CSS custom properties; real CSS is exercised by test:browser-appearance.
  style.textContent = `
    html { --surface: #0c0c0c; background-color: rgb(12, 12, 12); }
    html[data-theme='light'] { --surface: #e8ebf1; }
    html[data-theme='light'][data-palette='jinzi'] { --surface: #ebe9ef; }
  `;
  document.head.append(style);
  document.documentElement.dataset.theme = 'light';
  document.documentElement.dataset.palette = 'dailan';
});

afterEach(() => {
  stop?.();
  vi.useRealTimers();
  stop = undefined;
  style.remove();
  document.querySelectorAll('meta[name="theme-color"], meta[name="color-scheme"]').forEach(meta => meta.remove());
  delete document.documentElement.dataset.theme;
  delete document.documentElement.dataset.palette;
});

it('initializes browser chrome from the restored document appearance', () => {
  stop = observeBrowserAppearance();
  expect(color()).toBe('#e8ebf1');
  expect(scheme()).toBe('light');
});

it('updates both theme metadata values and follows palette changes without duplicating tags', async () => {
  stop = observeBrowserAppearance();
  document.documentElement.dataset.theme = 'dark';
  await Promise.resolve();
  expect(scheme()).toBe('dark');
  expect(color()).toBe('#0c0c0c');
  document.documentElement.dataset.theme = 'light';
  document.documentElement.dataset.palette = 'jinzi';
  await Promise.resolve();
  expect(scheme()).toBe('light');
  expect(color()).toBe('#ebe9ef');
  expect(document.querySelectorAll('meta[name="theme-color"]')).toHaveLength(1);
});

it('reads the destination surface even while the painted background is still an old color', async () => {
  stop = observeBrowserAppearance();
  document.documentElement.dataset.theme = 'dark';
  await Promise.resolve();
  document.documentElement.dataset.theme = 'light';
  document.documentElement.dataset.palette = 'jinzi';
  await Promise.resolve();
  expect(getComputedStyle(document.documentElement).backgroundColor).toBe('rgb(12, 12, 12)');
  expect(color()).toBe('#ebe9ef');
  expect(scheme()).toBe('light');
  expect(vi.getTimerCount()).toBe(0);
});

it('disconnects the observer on cleanup', async () => {
  stop = observeBrowserAppearance();
  document.documentElement.dataset.theme = 'dark';
  stop();
  await Promise.resolve();
  expect(color()).toBe('#e8ebf1');
  expect(scheme()).toBe('light');
});
