import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [390, 'dark'],
  [1440, 'light'],
  [2560, 'light'],
] as const) {
  test(`Agent follows the Console flavor without variant controls at ${width}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
    await page.setViewportSize({ width, height: 1000 });
    // 容器版 Console：同时内嵌基础版与容器版 Agent；机队都是基础版。
    await page.goto(`/tests/fixtures/ui-regressions.html?scene=agent-variants&theme=${theme}`);
    await page.getByRole('tab', { name: /^Agent/ }).click();
    await expect(page.getByRole('group', { name: 'Agent 产品版本' })).toHaveCount(0);
    await expect(page.getByText('允许基础版机器升级为容器版')).toHaveCount(0);
    await page.getByRole('button', { name: '升级 Agent', exact: true }).click();
    for (const checkbox of await page.getByRole('checkbox', { name: /^升级 / }).all()) await checkbox.check();
    await waitForVisualAssets(page);

    const cockpit = page.getByRole('tabpanel');
    const ledger = cockpit.getByRole('region', { name: '已一致' });
    await expect(ledger.getByText('容器版 · 跟随 Console')).toBeVisible();
    await expect(ledger.getByText('可发 v0.3.8', { exact: true })).toBeVisible();
    const main = cockpit.locator(':scope > .cgc-main:not(.cgc-span)');
    const scope = page.getByRole('group', { name: '升级范围' });
    await expect(scope.getByText('已选 2 台')).toBeVisible();
    await expect(scope.getByText(/其中 2 台由基础版换为容器版。/)).toBeVisible();
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
    await expect(page.locator('.cgc-page')).toHaveScreenshot(`agent-console-flavor-${width}-${theme}.png`);
    expect(errors).toEqual([]);
  });
}
