import { test, expect, webkit, type Page } from '@playwright/test';
test.use({ deviceScaleFactor: 2 });
async function open(page: Page, scene: string, theme: string, width = 1440, height = 900) {
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.clock.setFixedTime(new Date('2026-11-06T12:00:00Z'));
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/user-header-preview.html?theme=${theme}&substore=${scene}&haitun=active`);
  await page.getByRole('button', { name: '订阅', exact: true }).click();
  const d = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
  await expect(d.getByRole('button', { name: /切换到 Sub-Store 后端同步/ })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await expect(d.locator('.sub-pass-stack')).toHaveClass(/substore-two-backs/);
  await expect(d).toHaveAttribute('data-motion-state', 'entered');
  const before = (await d.locator('.sub-dialog').boundingBox())!;
  await d.getByRole('button', { name: /切换到 Sub-Store 后端同步/ }).click({ position: { x: 90, y: 12 } });
  await expect(d.getByRole('region', { name: 'Sub-Store 后端同步', exact: true })).toBeVisible();
  const after = (await d.locator('.sub-dialog').boundingBox())!;
  expect(Math.abs(before.height - after.height), 'C ticket must match Clash height').toBeLessThan(1);
  expect(Math.abs(before.y - after.y), 'Switch must not move the dialog').toBeLessThan(1);
  expect(await d.locator('.sub-dialog').evaluate(e => e.scrollWidth > e.clientWidth)).toBe(false);
  await expect(d.locator('.sub-dialog-close')).toBeInViewport({ ratio: 1 });
  return { d, errors };
}
for (const theme of ['dark', 'light']) {
  for (const scene of ['ready', 'empty', 'running', 'partial', 'failed', 'inactive', 'readonly']) {
    test(`C ring ${scene} ${theme}`, async ({ page }) => {
      const { d, errors } = await open(page, scene, theme);
      await expect(d).toHaveScreenshot(`substore-c-${scene}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
  for (const [width, height] of [
    [390, 844],
    [320, 568],
    [1440, 720],
  ]) {
    test(`C ring fit ${width}x${height} ${theme}`, async ({ page }) => {
      const { d, errors } = await open(page, 'ready', theme, width, height);
      await expect(d).toHaveScreenshot(`substore-c-${width}x${height}-${theme}.png`);
      await d.getByRole('button', { name: '立即同步', exact: true }).scrollIntoViewIfNeeded();
      await expect(d.getByRole('button', { name: '立即同步', exact: true })).toBeInViewport({ ratio: 1 });
      await d.getByRole('button', { name: '设置', exact: true }).click();
      await expect(d.getByText('Sub-Store 设置', { exact: true })).toBeVisible();
      expect(await page.getByRole('dialog').count()).toBe(1);
      expect((await d.locator('.sub-dialog').boundingBox())!.width).toBeLessThanOrEqual(404);
      await expect(d).toHaveScreenshot(`substore-settings-${width}x${height}-${theme}.png`);
      await d.getByRole('button', { name: '同步记录', exact: true }).click();
      await expect(d.getByRole('button', { name: /登记同步，成功/ })).toBeVisible();
      await d.getByRole('button', { name: /登记同步，成功/ }).click();
      await expect(d.getByText('验证转换结果', { exact: true })).toBeVisible();
      await expect(d).toHaveScreenshot(`substore-run-${width}x${height}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
}
test('configuration, records, automatic maintenance and exit guard are usable by keyboard', async ({ page }) => {
  const { d } = await open(page, 'ready', 'dark');
  await d.getByRole('combobox', { name: '自动维护', exact: true }).click();
  await page.getByRole('option', { name: /^关闭/ }).click();
  await expect(d.getByRole('combobox', { name: '自动维护', exact: true })).toContainText('关闭');
  await d.getByRole('button', { name: '设置', exact: true }).click();
  await d.getByLabel('来源名称', { exact: true }).fill('Renamed source');
  await d.locator('.sub-dialog-close').click();
  await expect(page.getByText('放弃未保存的更改？', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '取消', exact: true }).click();
  await expect(d.getByLabel('来源名称', { exact: true })).toHaveValue('Renamed source');
  await d.getByRole('button', { name: '保存', exact: true }).click();
  await expect(d.getByRole('region', { name: 'Sub-Store 后端同步', exact: true })).toBeVisible();
});

for (const theme of ['dark', 'light']) {
  test(`C ring Safari ${theme}`, async () => {
    const browser = await webkit.launch();
    try {
      const context = await browser.newContext({
        baseURL: 'http://127.0.0.1:4318',
        viewport: { width: 390, height: 844 },
        deviceScaleFactor: 2,
        reducedMotion: 'reduce',
        locale: 'zh-CN',
        timezoneId: 'Asia/Taipei',
      });
      const page = await context.newPage();
      const { d, errors } = await open(page, 'ready', theme, 390, 844);
      await expect(d).toHaveScreenshot(`substore-c-webkit-${theme}.png`);
      await d.getByRole('button', { name: '设置', exact: true }).click();
      await expect(d.getByLabel('后端地址', { exact: true })).toHaveAttribute('placeholder', /已保存/);
      expect(errors).toEqual([]);
    } finally {
      await browser.close();
    }
  });
}
