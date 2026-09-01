import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { RegionFlag } from '../src/ui/region-flag';

afterEach(cleanup);

describe('RegionFlag', () => {
  it('center-crops the first and last sprite cells symmetrically in square mode', () => {
    const view = render(<RegionFlag code="AD" square />);
    const first = view.getByRole('img', { name: 'AD 地区旗' }) as HTMLSpanElement;
    const firstX = Number.parseFloat(first.style.backgroundPosition.split(' ')[0]);

    expect(first.classList.contains('square')).toBe(true);
    expect(first.style.backgroundSize).toBe('2400% 1700%');

    view.rerender(<RegionFlag code="AZ" square />);
    const last = view.getByRole('img', { name: 'AZ 地区旗' }) as HTMLSpanElement;
    const lastX = Number.parseFloat(last.style.backgroundPosition.split(' ')[0]);

    expect(firstX).toBeGreaterThan(0);
    expect(lastX).toBeLessThan(100);
    expect(firstX + lastX).toBeCloseTo(100, 8);
  });

  it('keeps the complete 3:2 cell for inline flags', () => {
    const view = render(<RegionFlag code="TW" />);
    const flag = view.getByRole('img', { name: 'TW 地区旗' }) as HTMLSpanElement;

    expect(flag.classList.contains('square')).toBe(false);
    expect(flag.style.backgroundSize).toBe('1600% 1700%');
  });
});
