import { describe, expect, it } from 'vitest';

import { observeAreaFill } from '../src/ui/observe-chart';

const stops = (fill: ReturnType<typeof observeAreaFill>) =>
  typeof fill === 'string' ? [fill] : fill.colorStops.map(stop => stop.color);

describe('observation area fill paper', () => {
  it('tints unstacked fills against the card color the caller resolves', () => {
    // 堇紫暗色的卡片是 #1c1b1f：12% 系列色 + 88% 卡片色。
    expect(stops(observeAreaFill('#9e98f1', 'dark', { paper: '#1c1b1f' }))).toEqual([
      'rgba(44,42,56,0.55)',
      'rgba(44,42,56,0.3025)',
    ]);
  });

  it('falls back to the theme constant when the card color cannot be read', () => {
    const fallback = stops(observeAreaFill('#3e8fb0', 'dark'));
    expect(stops(observeAreaFill('#3e8fb0', 'dark', { paper: '' }))).toEqual(fallback);
    expect(stops(observeAreaFill('#3e8fb0', 'dark', { paper: 'color-mix(in srgb, red, blue)' }))).toEqual(fallback);
  });

  it('keeps stacked bands as flat series tints regardless of the paper', () => {
    expect(observeAreaFill('#9e98f1', 'dark', { stacked: true, paper: '#1c1b1f' })).toBe('rgba(158,152,241,0.34)');
  });
});
