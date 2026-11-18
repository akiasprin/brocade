import { test, expect } from '@playwright/test';
for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`oversubscribed host and quota ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', e => errors.push(e.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=oversold&theme=${theme}`);
      await page.getByRole('tab', { name: '宿主', exact: true }).click();
      await expect(page.getByText('3 / 1', { exact: false })).toBeVisible();
      await expect(page.getByText('可创建', { exact: true })).toBeVisible();
      await expect(page.locator('.cvm-host-card .cvm-meter.bad')).toHaveCount(0);
      await page.evaluate(() => document.fonts.ready);
      await expect(page).toHaveScreenshot(`oversold-host-${width}-${theme}.png`, { fullPage: true });
      await page.getByRole('tab', { name: '额度', exact: true }).click();
      await expect(page.getByText(/额度可高于宿主资源基准/)).toBeVisible();
      await expect(page).toHaveScreenshot(`oversold-quota-${width}-${theme}.png`, { fullPage: true });
      await page.getByRole('button', { name: '编辑额度', exact: true }).click();
      await expect(page.getByRole('dialog')).toHaveScreenshot(`oversold-quota-editor-${width}-${theme}.png`);
      await page.locator('.dialog-surface').getByRole('button', { name: '关闭', exact: true }).click();
      await page.getByRole('tab', { name: '组合台', exact: true }).click();
      await page.getByRole('button', { name: '＋ 创建容器', exact: true }).click();
      await expect(page.getByRole('spinbutton', { name: 'CPU（核）' })).toBeVisible();
      await expect(page.getByText(/宿主基准 1 核/)).toBeVisible();
      expect(
        await page.locator('.cvm-host-pick .cz-cap b').evaluateAll(values =>
          values.every(value => {
            const box = value.getBoundingClientRect();
            const parent = value.parentElement!.getBoundingClientRect();
            return box.left >= parent.left && box.right <= parent.right && value.scrollWidth <= value.clientWidth;
          }),
        ),
      ).toBe(true);
      await expect(page).toHaveScreenshot(`oversold-create-${width}-${theme}.png`, { fullPage: true });
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
  }
