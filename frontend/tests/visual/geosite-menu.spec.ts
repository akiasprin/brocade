import { expect, test } from '@playwright/test';
import { waitForVisualAssets } from './helpers/visual-ready';

for (const { width, height, theme } of [
  { width: 1440, height: 900, theme: 'light' },
  { width: 1440, height: 900, theme: 'dark' },
  { width: 390, height: 844, theme: 'light' },
  { width: 390, height: 844, theme: 'dark' },
  { width: 820, height: 420, theme: 'light' },
  { width: 2560, height: 1440, theme: 'light' },
]) {
  test(`bounded geosite menu ${width} ${height} ${theme}`, async ({ page }, testInfo) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.goto(`/tests/fixtures/dns-groups-preview.html?scene=editing&catalog=long&theme=${theme}`);
    const trigger = page
      .locator('.node-egress-rules-table tbody tr')
      .first()
      .getByRole('combobox', { name: /DNS 匹配内容/ });
    await trigger.click();
    const menu = page.getByRole('dialog', { name: 'geosite 选项', exact: true });
    const surface = menu.locator('.geo-menu');
    const search = menu.getByRole('textbox', { name: '搜索 geosite', exact: true });
    const browse = menu.getByRole('button', { name: /更多/ });
    await expect(browse).toContainText('1,564 类');
    await waitForVisualAssets(page);
    await testInfo.attach('observed-menu', {
      body: await page.screenshot({ animations: 'disabled' }),
      contentType: 'image/png',
    });

    const panel = (await surface.boundingBox())!;
    expect(panel.height).toBeLessThanOrEqual(Math.min(480, height * (width <= 820 ? 0.65 : 0.7)) + 1);
    expect(panel.y).toBeGreaterThanOrEqual(8);
    expect(panel.y + panel.height).toBeLessThanOrEqual(height - 7);
    expect(panel.x).toBeGreaterThanOrEqual(8);
    expect(panel.x + panel.width).toBeLessThanOrEqual(width - 7);

    const footer = (await browse.boundingBox())!;
    const label = (await browse.locator('.geo-browse-label').boundingBox())!;
    const summary = (await browse.locator('.geo-browse-summary').boundingBox())!;
    const icons = browse.locator(':scope > .ui-icon');
    const leading = (await icons.first().boundingBox())!;
    const trailing = (await icons.last().boundingBox())!;
    expect(leading.width).toBeLessThanOrEqual(18);
    expect(trailing.width).toBeLessThanOrEqual(14);
    expect(label.x - leading.x - leading.width).toBeGreaterThanOrEqual(7);
    expect(label.x - leading.x - leading.width).toBeLessThanOrEqual(9);
    expect(summary.x).toBeGreaterThanOrEqual(label.x + label.width + 7);
    expect(trailing.x).toBeGreaterThanOrEqual(summary.x + summary.width + 7);
    expect(Math.abs(label.y + label.height / 2 - leading.y - leading.height / 2)).toBeLessThan(2);
    expect(Math.abs(footer.y + footer.height - panel.y - panel.height)).toBeLessThan(2);
    await expect(page).toHaveScreenshot(`geosite-menu-${width}-${height}-${theme}.png`);

    const options = menu.getByRole('listbox', { name: 'geosite', exact: true });
    const headerBefore = (await search.boundingBox())!;
    await options.hover();
    await page.mouse.wheel(0, 1200);
    await expect.poll(() => options.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    expect((await search.boundingBox())!.y).toBeCloseTo(headerBefore.y, 0);
    expect((await browse.boundingBox())!.y).toBeCloseTo(footer.y, 0);
    expect(await surface.evaluate(element => element.scrollTop)).toBe(0);

    await search.fill('site-1540');
    await menu.getByRole('option', { name: 'site-1540', exact: true }).click();
    await expect(menu.getByRole('option', { name: /site-1540/ })).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await trigger.click();
    await browse.click();
    await expect(page.getByRole('dialog', { name: '规则集预览', exact: true })).toBeVisible();
    expect(errors).toEqual([]);
  });
}
