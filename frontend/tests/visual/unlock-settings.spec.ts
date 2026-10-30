import { test, expect } from '@playwright/test';
for (const width of [1440, 390])
  for (const theme of ['dark', 'light']) {
    test(`unlock settings ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/unlock-settings-preview.html?theme=${theme}`);
      const panel = page.locator('#set-unlock');
      await expect(panel.getByRole('button', { name: '定时循环', exact: true })).toHaveAttribute(
        'aria-pressed',
        'true',
      );
      await page.evaluate(() => document.fonts.ready);
      await expect(page).toHaveScreenshot(`unlock-scheduled-${width}-${theme}.png`);
      await panel.getByRole('button', { name: '手动', exact: true }).click();
      await expect(panel.getByRole('button', { name: '手动', exact: true })).toHaveAttribute('aria-pressed', 'true');
      await panel.getByRole('button', { name: '立即全量检测' }).click();
      await expect(panel.getByRole('status')).toContainText('已安排全量检测');
      await expect(page).toHaveScreenshot(`unlock-manual-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    });
  }
