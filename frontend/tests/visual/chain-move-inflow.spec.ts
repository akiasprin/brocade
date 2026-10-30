import { test, expect } from '@playwright/test';

for (const [name, width, theme] of [
  ['desktop', 1440, 'dark'],
  ['mobile', 390, 'dark'],
  ['light', 1100, 'light'],
] as const) {
  test(`${name}: chain migration preserves one source and its name`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-10-29T10:20:00Z') });
    await page.setViewportSize({ width, height: 950 });
    await page.goto(`/tests/fixtures/chain-move-inflow-preview.html?theme=${theme}`);
    const card = page.getByRole('region', { name: '流入统计' });
    await expect(card.locator('.site-usage-label')).toContainText('3 个来源');
    await expect(card).toContainText('香港 01 @ FxT');
    await expect(card).toContainText('接入链 deleted');
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await page.evaluate(() => document.fonts.ready);
    await expect(card).toHaveScreenshot(`${name}-moved-chain.png`);
    await card.getByRole('button', { name: /alice ·/ }).click();
    const dialog = page.getByRole('dialog', { name: 'alice · 流入明细' });
    const legend = dialog.getByRole('group', { name: '接入链（点击只看这条链的来源）' });
    await expect(legend.getByRole('button')).toHaveCount(2);
    await expect(dialog.getByRole('list', { name: '来源' }).locator(':scope > li')).toHaveCount(1);
    if (name === 'desktop')
      await expect(dialog.locator('.dialog-surface')).toHaveScreenshot('desktop-moved-chain-detail.png');
    expect(errors).toEqual([]);
  });
}
