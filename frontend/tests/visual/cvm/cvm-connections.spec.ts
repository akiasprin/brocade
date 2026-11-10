import { expect, test } from '@playwright/test';
for (const width of [1440,390,320])
 for (const theme of ['dark','light']) {
    test(`install opt-in ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/cvm/install-preview.html?theme=${theme}`);
      const cvm = page.getByRole('group', { name: '是否安装 CVM 扩展' });
      await cvm.getByRole('button', { name: '安装', exact: true }).click();
      await expect(page.getByLabel('安装命令')).toContainText('--enable-cvm');
      const bounds = await page.locator('.pv-install').boundingBox();
      expect(bounds!.x).toBeGreaterThanOrEqual(0);
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
      await expect(page).toHaveScreenshot(`install-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }

test('installed state comes from the machine report', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.goto('/tests/fixtures/cvm/install-preview.html?online=1');
  await expect(page.getByText('已启用', { exact: true })).toBeVisible();
  await expect(page.getByRole('group', { name: '是否安装 CVM 扩展' })).toHaveCount(0);
});
