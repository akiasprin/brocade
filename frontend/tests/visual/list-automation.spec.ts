import { waitForVisualAssets } from './helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const theme of ['light', 'dark']) {
  for (const width of [1440, 390, 320]) {
    test(`user avatars have no SubStore badges ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-14T00:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/users`);
      const roster = page.getByRole('listbox', { name: '用户列表' });
      await expect(roster).toBeVisible();
      await expect(roster.getByRole('img', { name: '已连接 SubStore' })).toHaveCount(0);
      await expect(roster.getByText('未连接', { exact: true })).toHaveCount(0);
      await expect(roster).not.toContainText('SubStore');
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await waitForVisualAssets(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`user-list-${width}-${theme}.png`);
    });

    test(`machine flags have no maintenance badges ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-14T00:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/node-list-preview.html?scene=few&theme=${theme}#/nodes`);
      const list = page.getByRole('list', { name: '机器列表' });
      await expect(list.locator('.node-card')).toHaveCount(3);
      await expect(list.locator('.node-card-flag')).toHaveCount(3);
      await expect(list.getByRole('img', { name: /自动养护/ })).toHaveCount(0);
      await expect(page.getByRole('tablist', { name: /详情页签/ })).toHaveCount(0);
      await waitForVisualAssets(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`machine-list-${width}-${theme}.png`);
    });
  }
}
