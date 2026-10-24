import { test, expect } from '@playwright/test';
for (const width of [390, 1440])
  test(`high overage keeps quota and crossing labels separate at ${width}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/capacity-coverage-preview.html?theme=${width === 390 ? 'light' : 'dark'}`);
    const chart = page.locator('.cap-chart');
    await expect(chart.locator('svg')).toBeVisible();
    await expect(chart).toHaveScreenshot(`high-overage-${width}.png`);
    const quota = await chart.locator('text.q').boundingBox(),
      crossing = await chart.locator('text.bad').boundingBox();
    expect(quota).not.toBeNull();
    expect(crossing).not.toBeNull();
    test.fail(true, 'UI-016: 配额与越过日期仅相差3px，没有标签避让，早期超额时相互重叠');
    const overlap =
      quota &&
      crossing &&
      Math.min(quota.x + quota.width, crossing.x + crossing.width) > Math.max(quota.x, crossing.x) &&
      Math.min(quota.y + quota.height, crossing.y + crossing.height) > Math.max(quota.y, crossing.y);
    expect(overlap).toBe(false);
  });
