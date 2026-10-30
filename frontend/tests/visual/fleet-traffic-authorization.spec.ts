import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [390, 'light'],
  [1280, 'dark'],
] as const) {
  test(`matrix leaves unconfirmed cells blank and updates in place at ${width}px`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-10-26T12:00:00Z') });
    await page.clock.pauseAt(new Date('2026-10-26T12:00:01Z'));
    await page.setViewportSize({ width, height: 900 });
    await page.goto(
      `/tests/fixtures/fleet-traffic-preview.html?scene=quiet&theme=${theme}&nav=0&run=0&auth_delay=10000`,
    );
    await expect(page.getByRole('heading', { name: '用量', exact: true })).toBeVisible();
    await page.clock.runFor(100);
    const ledger = page.getByRole('region', { name: '机队当前速率' });
    await expect(ledger).toBeVisible();
    const board = page.getByRole('region', { name: '用户与机器的当前速率', includeHidden: true });
    const table = page.getByRole('table', { includeHidden: true });
    await expect(board).toHaveAttribute('aria-busy', 'true');
    await expect(table).toBeVisible();
    await expect(table.locator('tbody tr')).toHaveCount(14);
    await expect(table.locator('tbody .flt-cell.pending')).toHaveCount(14 * 24);
    await expect(page.locator('.loading-skeleton')).toHaveCount(0);
    await expect(ledger.locator('.rut-hero-sub')).toHaveText('0 位活跃 · — 位已授权');
    await expect(page.getByText('用户授权信息尚未取得', { exact: true })).toHaveCount(0);
    await expect(page.getByText('尚无用户授权到这些机器')).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`authorization-unknown-${width}.png`);

    await page.clock.runFor(10_000);
    await expect(ledger.locator('.rut-hero-sub')).toHaveText('0 位活跃 · 14 位已授权');
    await expect(board).not.toHaveAttribute('aria-busy');
    await expect(table.locator('tbody .flt-cell.pending')).toHaveCount(0);
    await expect(table.locator('tbody .flt-cell.na')).not.toHaveCount(0);
    await expect(page.getByRole('table', { includeHidden: true })).toBeAttached();
    await expect(page).toHaveScreenshot(`authorization-ready-${width}.png`);
  });
}

test('confirmed empty grants have a separate empty state', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto('/tests/fixtures/fleet-traffic-preview.html?scene=empty&theme=light&nav=0&run=0');
  await expect(page.getByText('尚无用户授权到这些机器', { exact: true })).toBeVisible();
  await expect(page.locator('.rut-hero-sub')).toHaveText('0 位活跃 · 0 位已授权');
  await page.evaluate(() => document.fonts.ready);
  await expect(page).toHaveScreenshot('authorization-empty.png');
});
