import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`retained hours stay visible without a routine gap warning at ${width}px in ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.setFixedTime(new Date('2026-10-28T19:16:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/site-usage-preview.html?theme=${theme}&resumed`);
    const plot = page.locator('.site-usage-plot');
    await expect(plot).not.toContainText('站点采集可能有缺口');
    await expect(plot).not.toContainText('观测起点');
    await expect(plot.locator('.site-usage-meta')).toHaveText(/下行.*上行/);
    await expect(plot.locator('.site-usage-meta .warn')).toHaveCount(0);
    await expect(plot.getByRole('img', { name: '站点流量随时间分布' }).locator('canvas')).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    await expect(plot).toHaveScreenshot(`retained-${width}-${theme}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}
