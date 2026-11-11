import { test, expect } from '@playwright/test';
import { openBoardInstance } from '../helpers/cvm-board';
test.use({ browserName: 'webkit' });
for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`browse ${width} ${theme}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?theme=${theme}`);
    await expect(page.locator(width <= 820 ? '.cz-phone-row' : '.cz-pill')).toBeVisible();
    await expect(page).toHaveScreenshot(`webkit-list-${width}-${theme}.png`);
    await openBoardInstance(page, width);
    await expect(page.locator('.cvm-gauge')).toHaveCount(3);
    await expect(page).toHaveScreenshot(`webkit-detail-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(page.locator('.cvm-route-table')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
