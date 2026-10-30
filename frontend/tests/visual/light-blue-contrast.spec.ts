import { test, expect } from '@playwright/test';

for (const [name, width, height] of [
  ['desktop', 1440, 980],
  ['mobile', 390, 844],
] as const) {
  test(`light blue ${name}: secondary copy remains readable`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-10-23T08:00:00Z') });
    await page.setViewportSize({ width, height });
    await page.goto('/tests/fixtures/user-header-preview.html?palette=dailan&theme=light&presence=sources&apps=two');
    await expect(page.locator('.user-usage-ledger')).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`light-blue-${name}.png`, { threshold: 0.05 });

    const measurements = await page.evaluate(() => {
      // Resolve CSS color-mix and alpha against the actual painted ancestor surfaces.
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = 1;
      const context = canvas.getContext('2d')!;
      const rgba = (color: string) => {
        context.clearRect(0, 0, 1, 1);
        context.fillStyle = color;
        context.fillRect(0, 0, 1, 1);
        const [r, g, b, a] = context.getImageData(0, 0, 1, 1).data;
        return [r, g, b, a / 255];
      };
      const over = (fg: number[], bg: number[]) => fg.slice(0, 3).map((c, i) => c * fg[3] + bg[i] * (1 - fg[3]));
      const luminance = (rgb: number[]) => {
        const [r, g, b] = rgb.map(c => c / 255).map(c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
      };
      const samples = [
        ['.user-usage-reset', ''],
        ['.user-cycle-reset-action', ''],
        ['.qta-sub', ''],
        ['.qta-cap', ''],
        ['.qta-left', ''],
        ['.user-uuid-copy', ''],
        ['.user-row-presence', ''],
        ['.user-row-usage small', ''],
        [".user-roster-filter button[aria-pressed='false'] i", ''],
        ['.user-list-search input', '::placeholder'],
      ];
      return samples.flatMap(([selector, pseudo]) => {
        const elements = [...document.querySelectorAll<HTMLElement>(selector)].filter(
          e => e.getBoundingClientRect().width,
        );
        return elements.map(element => {
          const ancestors: HTMLElement[] = [];
          for (let parent: HTMLElement | null = element; parent; parent = parent.parentElement)
            ancestors.unshift(parent);
          let background = [255, 255, 255];
          for (const ancestor of ancestors)
            background = over(rgba(getComputedStyle(ancestor).backgroundColor), background);
          const style = getComputedStyle(element, pseudo || null);
          const foreground = rgba(style.color);
          foreground[3] *= Number(style.opacity);
          const a = luminance(over(foreground, background));
          const b = luminance(background);
          return { selector, ratio: (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05) };
        });
      });
    });
    expect(new Set(measurements.map(({ selector }) => selector)).size).toBeGreaterThanOrEqual(
      name === 'desktop' ? 9 : 5,
    );
    for (const { selector, ratio } of measurements) expect(ratio, selector).toBeGreaterThanOrEqual(4.5);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
