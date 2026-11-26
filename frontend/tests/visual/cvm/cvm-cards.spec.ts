import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`cards and network ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=fleet&theme=${theme}`);
      // The same grouped workbench uses rows on desktop and cards on phones.
      const list = page.getByRole('region', { name: '容器清单' });
      await expect(list.locator('.cvm-list-host-row')).toHaveCount(2);
      await expect(list.locator('.cvm-list-row')).toHaveCount(7);
      await expect(page).toHaveURL(/#\/cvm$/);
      await waitForVisualAssets(page);
      if (width === 1440) {
        const entry = list.getByRole('button', { name: /^TCP 203\.0\.113\.20:/ }).first();
        await expect(entry).toBeVisible();
        expect(await entry.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
      }
      await expect(page).toHaveScreenshot(`fleet-${width}-${theme}.png`);
      await list.getByRole('button', { name: /^需处理/ }).click();
      await expect(list.locator('.cvm-list-row')).toHaveCount(2);
      await list.getByRole('button', { name: /^全部/ }).click();
      await list.getByRole('searchbox', { name: '搜索容器' }).fill('build-runner');
      await expect(list.locator('.cvm-list-row')).toHaveCount(1);
      await list.getByRole('button', { name: 'build-runner', exact: true }).click();
      await expect(page.getByRole('heading', { name: 'build-runner', exact: true })).toBeVisible();
      await page.goBack();
      await expect(page).toHaveURL(/#\/cvm$/);
      await expect(list.getByRole('searchbox', { name: '搜索容器' })).toHaveValue('build-runner');
      await expect(list.locator('.cvm-list-row')).toHaveCount(1);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await expect(page).toHaveURL(/#\/cvm\/networks$/);
      const resources = page.getByRole('tabpanel', { name: '网络', exact: true });
      const egress = resources.getByRole('region', { name: '出站方式', exact: true });
      const table = egress.getByRole('table', { name: '出站方式', exact: true });
      const row = table
        .getByRole('row')
        .filter({ has: page.getByRole('button', { name: '香港 · 公网出口', exact: true }) });
      await expect(row.getByRole('cell', { name: 'WireGuard · Xray', exact: true })).toBeVisible();
      await expect(row.getByRole('switch', { name: '香港 · 公网出口 接受新增', exact: true })).toBeChecked();
      await expect(egress).toHaveScreenshot(`egress-network-table-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
  }
