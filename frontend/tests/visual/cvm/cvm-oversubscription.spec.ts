import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';
for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`logical host allocation ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', e => errors.push(e.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=oversold&theme=${theme}`);
      await page.getByRole('tab', { name: '宿主', exact: true }).click();
      await expect(page.getByText('3 / 1', { exact: false })).toBeVisible();
      await expect(page.getByText('可创建', { exact: true })).toBeVisible();
      const hosts = page.getByRole('table', { name: '宿主' });
      await expect(hosts.locator('.cvm-meter')).toHaveCount(3);
      await expect(hosts.locator('.cvm-meter.bad')).toHaveCount(0);
      await waitForVisualAssets(page);
      await expect(page).toHaveScreenshot(`oversold-host-${width}-${theme}.png`, { fullPage: true });
      await page.getByRole('tab', { name: '容器', exact: true }).click();
      await page.getByRole('button', { name: '创建容器', exact: true }).click();
      await expect(page.getByRole('spinbutton', { name: 'CPU（核）' })).toBeVisible();
      // 宿主卡写「已分配 / 宿主容量」；分配可以超过物理容量，预览如实写出总量，不提示内部策略。
      await expect(page.getByRole('region', { name: '创建后的宿主分配' })).toContainText(/\/ 1 核/);
      await expect(page.locator('body')).not.toContainText('超售');
      const capacities = page.locator('.cvm-new-host[aria-pressed="true"] .cvm-new-host-cap > span');
      await expect(capacities).toHaveCount(2);
      expect(
        await capacities.evaluateAll(values =>
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
