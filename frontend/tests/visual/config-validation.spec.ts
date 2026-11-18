import { expect, test, type Page } from '@playwright/test';
async function open(page: Page, width: number, theme: string, extra = '') {
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/rule-layout-preview.html?theme=${theme}${extra}`);
  await page.getByRole('switch', { checked: true }).click();
  const collapsed = page.locator('.chain-rule-node-head[aria-expanded="false"]');
  while (await collapsed.count()) await collapsed.first().click();
  await expect(page.locator('.geo-select').first()).toBeVisible();
  await expect(
    page.locator('[data-rule-location$="/hk-akile"]').getByRole('button', { name: '＋ 加 AI 组合' }),
  ).toBeEnabled();
  await page.evaluate(() => document.fonts.ready);
}
for (const width of [390, 1440])
  for (const theme of ['light', 'dark']) {
    test(`A selector and preview ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', e => errors.push(e.message));
      await open(page, width, theme);
      await page.locator('[data-rule-location$="/hk-akile"]').getByRole('button', { name: '＋ 加 AI 组合' }).click();
      await expect(page.getByRole('button', { name: '预览 huggingface' })).toBeVisible();
      await expect(page.locator('.chain-detail-section')).toHaveScreenshot(`a-rules-${width}-${theme}.png`, {
        animations: 'disabled',
      });
      await page
        .locator('[data-rule-location$="/hk-akile"]')
        .getByRole('combobox', { name: '匹配内容（规则 2）' })
        .click();
      const menu = page.getByRole('dialog', { name: 'geosite 选项' });
      await expect(menu).toBeVisible();
      await expect(menu.getByRole('button', { name: /更多/ })).toBeVisible();
      await expect(menu.locator('.geo-menu')).toHaveScreenshot(`a-menu-${width}-${theme}.png`, {
        animations: 'disabled',
      });
      await menu.getByRole('button', { name: /更多/ }).click();
      const browser = page.getByRole('dialog', { name: '规则集预览' });
      await expect(browser).toBeVisible();
      await browser.getByRole('button', { name: /google 102/ }).click();
      await expect(browser.getByText('example-0.com', { exact: true })).toBeVisible();
      await expect(browser.locator('.geo-browser')).toHaveScreenshot(`a-preview-${width}-${theme}.png`, {
        animations: 'disabled',
      });
      const fits = await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
      expect(fits).toBe(true);
      expect(errors).toEqual([]);
    });
  }
test('includes prevent saving, raw separators survive typing, and unknown catalogues stay explicit', async ({
  page,
}) => {
  await open(page, 1440, 'light', '&invalid=1');
  await expect(page.getByText(/openai 规则 1 已包含/).first()).toBeVisible();
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeDisabled();
  await expect(page.locator('.chain-detail-section')).toHaveScreenshot('a-invalid-1440-light.png', {
    animations: 'disabled',
  });
  await page.getByRole('button', { name: '＋ 加一条' }).first().click();
  const raw = page.locator('[data-rule-location$="/hk-fxt"] input.f[aria-label^="匹配内容"]');
  await raw.fill('one.example, ');
  await raw.pressSequentially('two.example');
  await expect(raw).toHaveValue('one.example, two.example');
});
