import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [390, 'dark'],
  [1440, 'light'],
  [2560, 'light'],
] as const) {
  test(`Agent flavor controls preserve the cockpit layout at ${width}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/ui-regressions.html?scene=agent-variants&theme=${theme}`);
    await page.getByRole('tab', { name: /^Agent/ }).click();
    await page.getByRole('button', { name: '容器版 Agent', exact: true }).click();
    await page.getByRole('checkbox', { name: '允许基础版机器升级为容器版' }).check();
    await page.getByRole('button', { name: '升级 Agent', exact: true }).click();
    for (const checkbox of await page.getByRole('checkbox', { name: /^升级 / }).all()) await checkbox.check();
    await waitForVisualAssets(page);

    const cockpit = page.getByRole('tabpanel');
    const ledger = cockpit.getByRole('region', { name: '已一致' });
    const main = cockpit.locator(':scope > .cgc-main:not(.cgc-span)');
    const scope = page.getByRole('group', { name: '升级范围' });
    await expect(scope.getByText('已选 2 台')).toBeVisible();
    await expect(scope.getByRole('button', { name: '批准', exact: true })).toBeEnabled();
    const ledgerBox = (await ledger.boundingBox())!;
    const mainBox = (await main.boundingBox())!;
    if (width > 900) {
      expect(Math.abs(mainBox.y - ledgerBox.y)).toBeLessThanOrEqual(1);
      expect(mainBox.x).toBeGreaterThanOrEqual(ledgerBox.x + ledgerBox.width - 1);
      expect(mainBox.width).toBeGreaterThan(700);
    } else {
      expect(mainBox.y).toBeGreaterThanOrEqual(ledgerBox.y + ledgerBox.height - 1);
    }
    expect(await cockpit.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    expect(await main.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    const options = page.getByRole('group', { name: 'Agent 产品版本' });
    const optionsBox = (await options.boundingBox())!;
    expect(optionsBox.x).toBeGreaterThanOrEqual(mainBox.x);
    expect(optionsBox.x + optionsBox.width).toBeLessThanOrEqual(mainBox.x + mainBox.width);
    await expect(page.locator('.cgc-page')).toHaveScreenshot(`agent-flavor-${width}-${theme}.png`);
    expect(errors).toEqual([]);
  });
}
