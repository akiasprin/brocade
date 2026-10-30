import { test, expect } from '@playwright/test';

for (const width of [1440, 390]) {
  test(`path IPv6 readiness ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto('/tests/fixtures/chain-unlock-preview.html?family-unavailable=1#/chains');
    const first = page.locator('.cut-chain button').first();
    await first.click();
    await page.getByRole('button', { name: '解锁', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: /区域性服务解锁状态/ });
    const v6 = dialog.getByRole('button', { name: 'IPv6', exact: true });
    await expect(v6).toBeEnabled();
    await v6.click();
    await expect(dialog.getByText('检测未就绪，稍后重试', { exact: true })).toBeVisible();
    await expect(dialog.getByText('机器无 IPv6 地址', { exact: true })).toHaveCount(0);
    await expect(dialog.getByText('需要更新线路配置', { exact: true })).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`path-readiness-${width}.png`);
  });
}
