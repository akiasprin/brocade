import { test, expect } from '@playwright/test';
import { openListInstance } from '../helpers/cvm-instance';
import { waitForVisualAssets } from '../helpers/visual-ready';
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
    const list = page.getByRole('table', { name: '按宿主分组的容器', exact: true });
    await expect(
      list.getByRole('button', { name: 'TCP 203.0.113.20:30022 → 容器 22 · 已生效', exact: true }),
    ).toBeVisible();
    await waitForVisualAssets(page);
    await expect(page).toHaveScreenshot(`webkit-list-${width}-${theme}.png`);
    await openListInstance(page);
    await expect(page.getByRole('group', { name: '当前容器读数' }).locator('.kpi')).toHaveCount(6);
    await expect(page.locator('.cvm-overview .cvm-runtime > header')).toContainText('持续采样');
    await expect(page).toHaveScreenshot(`webkit-detail-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const network = page.getByRole('tabpanel', { name: '网络', exact: true });
    await expect(network.getByRole('heading', { name: '网卡', exact: true })).toBeVisible();
    await expect(network.getByRole('table', { name: '目标路由', exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
