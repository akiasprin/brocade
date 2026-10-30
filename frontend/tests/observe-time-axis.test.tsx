import { describe, expect, it } from 'vitest';
import { observeTimeAxes, observeTimeInterval, observeTimeTooltip } from '../src/ui/observe-chart';

const DAY_MS = 86_400_000;
const at = (day: number, hour = 0, minute = 0, second = 0) => new Date(2026, 9, day, hour, minute, second).getTime();
const axis = (start: number, end: number) => {
  const primary = observeTimeAxes(start, end, '#888', '#333')[0];
  return { ...primary, axisLabel: { ...primary.axisLabel, formatter: primary.axisLabel.formatter! } };
};

describe('observation time axis', () => {
  it('keeps short ranges on wall-clock minutes and seconds', () => {
    const hour = axis(at(28, 10), at(28, 11));
    expect(hour.axisLabel.formatter(at(28, 10, 20))).toBe('10:20');
    const minute = axis(at(28, 10, 0, 5), at(28, 10, 1, 35));
    expect(minute.axisLabel.formatter(at(28, 10, 0, 30))).toBe('10:00:30');
    expect(minute.axisTick.customValues).toEqual([at(28, 10, 0, 30), at(28, 10, 1), at(28, 10, 1, 30)]);
  });

  it.each([7, 18, 27, 31])('names the dates and bounds tick density across %i days', days => {
    const start = at(1);
    const end = start + days * DAY_MS;
    const value = axis(start, end);
    const ticks = value.axisTick.customValues;
    const labels = ticks.map(value.axisLabel.formatter);
    expect(ticks.length).toBeGreaterThanOrEqual(3);
    expect(ticks.length).toBeLessThanOrEqual(8);
    expect(ticks.every(tick => tick >= start && tick <= end)).toBe(true);
    expect(labels.every(label => /^\d{2}-\d{2}$/.test(label))).toBe(true);
    expect(new Set(labels).size).toBe(labels.length);
    expect(observeTimeInterval(end - start)).toBeGreaterThan(DAY_MS);
  });

  it('keeps sub-day ticks distinct in a multi-day custom range', () => {
    const value = axis(at(27, 6), at(28, 18));
    const labels = value.axisTick.customValues.map(value.axisLabel.formatter);
    expect(value.axisLabel.formatter(at(28, 12))).toBe('10-28\n12:00');
    expect(new Set(labels).size).toBe(labels.length);
  });

  it('includes the year when a range crosses New Year', () => {
    const start = new Date(2026, 11, 28).getTime();
    const end = new Date(2027, 0, 5).getTime();
    const value = axis(start, end);
    expect(value.axisLabel.formatter(start)).toBe('2026-12-28');
    expect(value.axisLabel.formatter(end)).toBe('2027-01-05');
  });

  it('adds the actual date to tooltips for multi-day and midnight-crossing ranges', () => {
    expect(observeTimeTooltip(at(28, 10, 20, 30), at(28, 10), at(28, 11))).toBe('10:20:30');
    expect(observeTimeTooltip(at(28, 10, 20, 30), at(1), at(28, 11))).toBe('10-28 10:20:30');
    expect(observeTimeTooltip(at(28, 0, 0, 30), at(27, 23, 30), at(28, 0, 30))).toBe('10-28 00:00:30');
    const start = new Date(2026, 11, 28).getTime();
    const end = new Date(2027, 0, 5).getTime();
    expect(observeTimeTooltip(start, start, end)).toBe('2026-12-28 00:00:00');
    expect(observeTimeTooltip(end, start, end)).toBe('2027-01-05 00:00:00');
  });
});
