import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test } from '@playwright/test';
for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`mixed release ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/mixed-release-preview.html?theme=${theme}`);
      await expect(page.getByText('家中 DELL · 开发宿主')).toHaveCount(1);
      await expect(page.getByText('xray.json')).toBeVisible();
      await expect(page.getByText('容器 Ubuntu 开发环境 · cube/egress.json')).toBeVisible();
      await waitForVisualAssets(page);
      await expect(page.locator('main')).toHaveScreenshot(`mixed-${width}-${theme}.png`);
      await page.getByText('容器 Ubuntu 开发环境 · cube/egress.json').click();
      await expect(page.getByText(/deny-by-default/)).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }
