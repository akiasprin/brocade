import { test, expect } from '@playwright/test';
import { waitForVisualAssets } from './helpers/visual-ready';

for (const [name, width, height, theme] of [
  ['desktop-dark', 1440, 1000, 'dark'],
  ['desktop-light', 1440, 1000, 'light'],
  ['mobile-dark', 390, 844, 'dark'],
  ['mobile-light', 390, 844, 'light'],
] as const) {
  test(`${name}: earlier history remains reachable after merged sessions`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.clock.install({ time: new Date('2026-10-23T08:00:00Z') });
    await page.goto(`/tests/fixtures/user-header-preview.html?palette=jinzi&historyPages&theme=${theme}`);
    const card = page.locator('.user-presence-card');
    const more = card.getByRole('button', { name: '读取更早的历史记录' });
    await expect(more).toBeVisible();
    await expect(card.locator('.user-presence-row.off')).toHaveCount(5);
    await expect(card.getByText(/254 段记录/)).toBeVisible();
    await waitForVisualAssets(page);
    await expect(card).toHaveScreenshot(`${name}-first-page.png`);
    await more.click();
    await expect(card.getByText('198.51.100.88', { exact: true })).toBeVisible();
    await expect(card.locator('.user-presence-row.off')).toHaveCount(6);
    await expect(more).toHaveCount(0);
    await expect(card).toHaveScreenshot(`${name}-all-pages.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}
