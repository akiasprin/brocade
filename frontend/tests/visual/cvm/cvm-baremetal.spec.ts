import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`bare-metal adoption and instance ${width}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=baremetal-hosts&theme=${theme}`);
    await page.getByRole('button', { name: '纳管裸机', exact: true }).click();
    const picker = page.getByRole('dialog', { name: '选择裸机' });
    await expect(picker.getByRole('combobox', { name: '选择裸机' })).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`baremetal-picker-${width}-${theme}.png`, { fullPage: true });
    await picker.getByRole('button', { name: '下一步' }).click();
    const dialog = page.getByRole('dialog', { name: '纳管裸机' });
    await expect(dialog.getByRole('button', { name: '确认纳管' })).toBeDisabled();
    await dialog.getByRole('checkbox').check();
    await expect(dialog.getByRole('button', { name: '确认纳管' })).toBeEnabled();
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`baremetal-adopt-${width}-${theme}.png`, { fullPage: true });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=baremetal-detail&theme=${theme}`);
    await expect(page.getByRole('button', { name: '解除纳管', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`baremetal-detail-${width}-${theme}.png`, { fullPage: true });
    expect(errors).toEqual([]);
  });

  test(`bare-metal entry before native capability ${width}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=list&theme=${theme}`);
    await page.getByRole('button', { name: '纳管裸机', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '选择裸机' });
    await expect(dialog.getByText('尚无可纳管的裸机')).toBeVisible();
    await expect(dialog.getByText(/当前宿主以容器模式运行/)).toBeVisible();
    await expect(dialog.getByRole('button', { name: '下一步' })).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`baremetal-unavailable-${width}-${theme}.png`, { fullPage: true });
    await dialog.getByRole('button', { name: '查看宿主' }).click();
    await expect(page.getByRole('tab', { name: '宿主', exact: true })).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByRole('button', { name: '纳管裸机', exact: true })).toBeVisible();
  });
}
