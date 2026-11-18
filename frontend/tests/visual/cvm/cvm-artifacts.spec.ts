import { waitForVisualAssets } from '../helpers/visual-ready';
import { expect, test } from '@playwright/test';
for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`artifact review ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-18T00:00:00+08:00') });
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=artifacts&theme=${theme}`);
      await page.getByRole('tab', { name: '产物', exact: true }).click();
      await expect(page.getByText(/影子比对一致/)).toBeVisible();
      await expect(page.locator('.cvm-monitoring')).toHaveCount(0);
      await waitForVisualAssets(page);
      await expect(page.locator('.cvm-artifacts')).toHaveScreenshot(`artifacts-${width}-${theme}.png`);
      await page.getByRole('button', { name: /网卡附件/ }).click();
      await expect(page.getByLabel('产物差异')).toContainText('nic-business');
      await page.getByRole('button', { name: '全文', exact: true }).click();
      await expect(page.getByLabel('产物全文')).toContainText('cubevs-attachments-v2');
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }
for (const scene of ['artifacts-mismatch', 'artifacts-legacy'])
  test(`artifact state ${scene}`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=light`);
    await page.getByRole('tab', { name: '产物', exact: true }).click();
    await expect(
      page.getByText(scene === 'artifacts-mismatch' ? /影子比对不一致/ : /这一代没有编译产物/),
    ).toBeVisible();
    await expect(page.locator('.cvm-detail')).toHaveScreenshot(`${scene}.png`);
  });

test('artifact pending restart does not show the previous operation evidence', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1000 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=artifacts-pending&theme=light');
  await page.getByRole('tab', { name: '产物', exact: true }).click();
  await expect(page.getByText('尚无这一代的产物比对回执')).toBeVisible();
  await expect(page.getByText('尚无已确认的生效摘要')).toBeVisible();
  await expect(page.getByText('e'.repeat(64), { exact: true })).toHaveCount(0);
  await expect(page.getByText('执行绑定值')).toHaveCount(0);
  await waitForVisualAssets(page);
  await expect(page.locator('.cvm-artifacts')).toHaveScreenshot('artifacts-pending-390-light.png');
});
