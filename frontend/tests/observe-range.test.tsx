import { describe, expect, it } from 'vitest';
import {
  canonicalLoadRangeQuery,
  customLoadRange,
  DEFAULT_LOAD_RANGE,
  LOAD_RANGES,
  loadRangeFromQuery,
  loadRangeQuery,
  loadRangeTriggerText,
  localDayStart,
  observeSelection,
  observeSnapStep,
  observeZoomOut,
} from '../src/ui/observe-range';

/** Local wall-clock seconds, so every expectation holds in any time zone. */
const at = (day: number, hour: number, minute: number, second = 0) =>
  Math.floor(new Date(2026, 9, day, hour, minute, second).getTime() / 1000);

describe('observation range model', () => {
  it('snaps a drag to the finest step one pixel can still resolve', () => {
    expect(observeSnapStep(0.5)).toBe(1);
    // An hour across ~540 px is ~6.7 s per pixel; a day is ~160 s per pixel.
    expect(observeSnapStep(6.7)).toBe(10);
    expect(observeSnapStep(160)).toBe(300);
    expect(observeSnapStep(5_000)).toBe(600);
  });

  it('rounds the dragged span, widens anything under a minute and never ends in the future', () => {
    const now = at(5, 14, 32);
    const expected = { startUnixSecs: at(5, 13, 41), endUnixSecs: at(5, 13, 51, 10), step: 10, widened: false };
    expect(observeSelection(at(5, 13, 41, 3), at(5, 13, 51, 8), 10, now)).toEqual(expected);
    expect(observeSelection(at(5, 13, 51, 8), at(5, 13, 41, 3), 10, now)).toEqual(expected);
    expect(observeSelection(at(5, 13, 45, 40), at(5, 13, 46), 10, now)).toEqual({
      startUnixSecs: at(5, 13, 45, 20),
      endUnixSecs: at(5, 13, 46, 20),
      step: 10,
      widened: true,
    });
    expect(observeSelection(now - 20, now, 10, now)).toEqual({
      startUnixSecs: now - 60,
      endUnixSecs: now,
      step: 10,
      widened: true,
    });
  });

  it('zooms out around the centre, keeps the end in the past and stops at 24 hours', () => {
    const now = at(5, 14, 32);
    expect(observeZoomOut(at(5, 13, 41), at(5, 13, 51, 10), now)).toEqual({
      startUnixSecs: at(5, 13, 35, 55),
      endUnixSecs: at(5, 13, 56, 15),
    });
    expect(observeZoomOut(now - 3_600, now, now)).toEqual({ startUnixSecs: now - 7_200, endUnixSecs: now });
    expect(observeZoomOut(now - 50_000, now, now)).toEqual({ startUnixSecs: now - 86_400, endUnixSecs: now });
    expect(observeZoomOut(now - 86_400, now, now)).toBeNull();
  });

  it('labels fixed ranges with seconds only when needed and dates only away from today', () => {
    const range = customLoadRange(at(5, 13, 41), at(5, 13, 51, 10));
    expect(range).toMatchObject({ seconds: 610, label: 'custom', heading: 'CUSTOM RANGE' });
    expect(range.menuLabel).toBe('10/05 13:41:00 → 13:51:10');
    expect(loadRangeTriggerText(range, localDayStart(at(5, 14, 32)))).toBe('13:41:00 → 13:51:10');
    expect(loadRangeTriggerText(customLoadRange(at(5, 13, 40), at(5, 13, 50)), localDayStart(at(5, 9, 0)))).toBe(
      '13:40 → 13:50',
    );
    expect(loadRangeTriggerText(range, localDayStart(at(6, 9, 0)))).toBe('10/05 13:41:00 → 13:51:10');
    expect(customLoadRange(at(4, 23, 50), at(5, 0, 20)).menuLabel).toBe('10/04 23:50 → 10/05 00:20');
    expect(loadRangeTriggerText(LOAD_RANGES[4], localDayStart(at(5, 9, 0)))).toBe('近 24 小时');
  });

  it('writes the default range as nothing, presets by key and fixed ranges by their bounds', () => {
    expect(loadRangeQuery(DEFAULT_LOAD_RANGE)).toEqual({});
    expect(loadRangeQuery(LOAD_RANGES[2])).toEqual({ range: '6h' });
    expect(loadRangeQuery(customLoadRange(1_700_000_000, 1_700_000_600))).toEqual({
      from: 1_700_000_000,
      to: 1_700_000_600,
    });
    expect(loadRangeFromQuery({ range: '6h' })).toBe(LOAD_RANGES[2]);
    expect(loadRangeFromQuery({})).toBe(DEFAULT_LOAD_RANGE);
    expect(loadRangeFromQuery({ from: 1_700_000_000, to: 1_700_000_600 })).toMatchObject({
      seconds: 600,
      startUnixSecs: 1_700_000_000,
      endUnixSecs: 1_700_000_600,
    });
    expect(loadRangeFromQuery({ from: 1_700_000_000, to: 1_700_000_030 })).toBe(DEFAULT_LOAD_RANGE);
    expect(canonicalLoadRangeQuery({ p: 'node', id: 'n1', range: '24h', from: 1, to: 2 })).toEqual({
      p: 'node',
      id: 'n1',
    });
  });
});
