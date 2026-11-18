import { waitForVisualAssets } from './helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`per-node IP change notifications at ${width}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(`/tests/fixtures/ui-regressions.html?scene=nodes&theme=${theme}`);
    const toggle = page.getByRole('group', { name: 'IP 变更通知', exact: true });
    await expect(toggle.getByRole('button', { name: '开启' })).toHaveAttribute('aria-pressed', 'true');
    const row = toggle.locator('..').locator('..');
    await expect(row.locator('xpath=following-sibling::*[1]')).toContainText('出网权限');
    const identity = row.locator('..');
    await identity.scrollIntoViewIfNeeded();
    await waitForVisualAssets(page);
    await expect(identity).toHaveScreenshot(`ip-notifications-${width}-${theme}.png`);
    await toggle.getByRole('button', { name: '关闭' }).click();
    await expect(toggle.getByRole('button', { name: '关闭' })).toHaveAttribute('aria-pressed', 'true');
    await toggle.getByRole('button', { name: '开启' }).click();
    await expect(toggle.getByRole('button', { name: '开启' })).toHaveAttribute('aria-pressed', 'true');
  });
}
