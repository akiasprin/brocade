import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`actual first records at ${width}px in ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.setFixedTime(new Date('2026-10-25T12:58:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/site-usage-preview.html?theme=${theme}`);
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    const domains = page.getByRole('region', { name: '其他站点明细' });
    const targets = page.getByRole('region', { name: '嗅探失败明细' });
    await expect(domains).toContainText('最早记录：今天 02:00');
    await expect(targets).toContainText('最早记录：今天 02:00');
    await page.evaluate(() => document.fonts.ready);
    await expect(domains).toHaveScreenshot(`day-${width}-${theme}.png`);

    await page.getByRole('button', { name: '当周', exact: true }).click();
    await expect(domains).toContainText('最早记录：昨天 23:00');
    await expect(targets).toContainText('最早记录：昨天 23:00');
    await expect(domains).not.toContainText(/起保留|起采集/);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(domains).toHaveScreenshot(`week-${width}-${theme}.png`);
    expect(errors).toEqual([]);
  });
}
