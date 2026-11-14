import flagSheet from '../src/ui/flags.svg?raw';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { FLAG_SHEET } from '../src/ui/flags';
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { RegionFlag } from '../src/ui/region-flag';

const previewSheet = readFileSync('src/ui/flags-preview.css', 'utf8');

afterEach(cleanup);

describe('RegionFlag', () => {
  it('ships an inline, compact preview generated from the current vector sheet', () => {
    const digest = createHash('sha256').update(flagSheet).digest('hex');
    expect(previewSheet).toContain(digest);
    const encoded = previewSheet.match(/data:image\/png;base64,([A-Za-z0-9+/=]+)/)?.[1];
    expect(encoded).toBeTruthy();
    const pixels = Buffer.from(encoded!, 'base64');
    expect(pixels.length).toBeLessThan(20 * 1024);
    expect(pixels.readUInt32BE(16)).toBe(16 * 6);
    expect(pixels.readUInt32BE(20)).toBe(FLAG_SHEET.length * 4);
  });
  it('ships vector cells with isolated, complete SVG references for every region', () => {
    const source = flagSheet;
    const svg = new DOMParser().parseFromString(source, 'image/svg+xml');
    expect(svg.querySelector('parsererror')).toBeNull();
    expect(svg.documentElement.children.length).toBe(FLAG_SHEET.join('').length / 2);
    expect(svg.querySelector('image,script,foreignObject')).toBeNull();
    const ids = [...svg.querySelectorAll('[id]')].map(element => element.id);
    expect(new Set(ids).size).toBe(ids.length);
    const known = new Set(ids);
    for (const element of svg.querySelectorAll('*'))
      for (const attribute of element.attributes) {
        if (attribute.localName === 'href') {
          expect(attribute.value.startsWith('#')).toBe(true);
          expect(known.has(attribute.value.slice(1))).toBe(true);
        }
        for (const match of attribute.value.matchAll(/url\(#([^)]*)\)/g)) expect(known.has(match[1])).toBe(true);
      }
  });

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
