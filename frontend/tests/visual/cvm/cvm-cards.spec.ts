import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`cards and quota ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=fleet&theme=${theme}`);
      // The container home is the composition board; phones get the grouped list.
      await expect(page.locator(width > 820 ? '.cz-pill' : '.cz-phone-row')).toHaveCount(7);
      await waitForVisualAssets(page);
      await expect(page).toHaveScreenshot(`fleet-${width}-${theme}.png`);
      await page.getByRole('tab', { name: '额度', exact: true }).click();
      await expect(page.getByRole('table', { name: '用户额度' })).toBeVisible();
      await expect(page.locator('.cvm-quota-roster')).toHaveScreenshot(`quota-table-${width}-${theme}.png`);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await expect(page.locator('.cvm-publication-card')).toHaveScreenshot(`network-card-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
  }
