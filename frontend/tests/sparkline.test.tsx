import { describe, expect, it } from 'vitest';
import { sparklinePaths } from '../src/ui/sparkline';

/** All y coordinates of a path: its numbers alternate x, y. */
const ysOf = (path: string) =>
  [...path.matchAll(/-?\d+(?:\.\d+)?/g)].map(match => Number(match[0])).filter((_, index) => index % 2 === 1);

describe('compact trend paths', () => {
  it('breaks at missing windows and draws the baseline only where windows were collected', () => {
    const paths = sparklinePaths([0, 0, null, 0, 10, 0], 10, 30);
    expect(paths.bases).toEqual(['M0 29.5H1', 'M3 29.5H5']);
    expect(paths.areas).toHaveLength(2);
    // The quiet first segment has no stroke; the second rises from the baseline and returns to it.
    expect(paths.lines).toHaveLength(1);
    expect(paths.lines[0].startsWith('M3 29.50')).toBe(true);
    expect(paths.lines[0].endsWith(' 5 29.50')).toBe(true);
  });

  it('exposes complete curves for zero and tiny traffic without closing them to the baseline', () => {
    const paths = sparklinePaths([0, 0, null, 0, 0.01, 0], 100, 46);
    expect(paths.curves).toHaveLength(2);
    expect(paths.curves[0]).toMatch(/^M0 45.50C/);
    expect(paths.curves.every(path => !/[LZ]/.test(path))).toBe(true);
    expect(paths.lines).toHaveLength(0);
  });

  it('strokes a rise from the baseline even where its foot is below one pixel', () => {
    // 0.3 is under a pixel at this scale; stroking only the visible point and its neighbours
    // started the stroke in mid-air and the foot of the rise looked cut off.
    const { lines } = sparklinePaths([0, 0.3, 0.2, 5, 0.3, 0, 0], 10, 30);
    expect(lines).toHaveLength(1);
    expect(lines[0].startsWith('M0 29.50')).toBe(true);
    expect(lines[0].endsWith(' 5 29.50')).toBe(true);
  });

  it('draws no stroke for traffic below one pixel, so a quiet hour is not a dashed line', () => {
    const paths = sparklinePaths([0, 0.01, 0, 0.02, 0], 100, 30);
    expect(paths.lines).toEqual([]);
    expect(paths.bases).toEqual(['M0 29.5H4']);
  });

  it('keeps the curve between the baseline and the top, including its control points', () => {
    const { areas, lines } = sparklinePaths([0, 1, 100, 2, 0, 50, 50, 0, 300], 100, 30);
    for (const path of [...areas, ...lines]) {
      for (const y of ysOf(path)) {
        expect(y).toBeGreaterThanOrEqual(2.5);
        expect(y).toBeLessThanOrEqual(29.5);
      }
    }
  });
});
