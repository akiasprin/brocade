import { test, expect } from '@playwright/test';

for (const [name, width, height, theme, strategy] of [
  ['desktop-default', 1440, 1000, 'dark', 'as_is'],
  ['desktop-ipv4', 1440, 1000, 'dark', 'use_ipv4v6'],
  ['mobile-ipv4', 390, 844, 'dark', 'use_ipv4v6'],
  ['light-ipv4', 1440, 1000, 'light', 'use_ipv4v6'],
] as const) {
  test(`${name}: target DNS strategy in the real tunnel editor`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.goto(`/tests/fixtures/tunnel-editor-preview.html?theme=${theme}`);
    const strategyInput = page.getByRole('combobox', { name: '目标域名解析' });
    await expect(strategyInput).toHaveValue('as_is');
    await strategyInput.selectOption(strategy);
    await strategyInput.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    const dialog = page.getByRole('dialog', { name: '编辑隧道' });
    expect(await dialog.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
    await expect(dialog).toHaveScreenshot(`${name}.png`);
    expect(errors).toEqual([]);
  });
}
