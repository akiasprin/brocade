import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [390, 'light'],
  [1440, 'dark'],
] as const) {
  for (const role of ['system-admin', 'public']) {
    test(`chain rule headings ${role} ${width}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(
        `/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=${theme}&role=${role}#/chains/chain/apac/hk-direct`,
      );
      const tree = page.locator('.chain-rule-tree');
      await expect(tree.locator('.rule-editor')).toHaveCount(1);
      await expect(tree.locator('.chain-rule-node-head .who > b')).toHaveText('香港 HKIX');
      await expect(tree.locator('.rule-editor-title')).toHaveCount(0);
      await expect(tree).not.toContainText('hk-direct / hk-hkix');
      await page.evaluate(() => document.fonts.ready);
      await expect(tree).toHaveScreenshot(`chain-${role}-${width}.png`);
      expect(errors).toEqual([]);
    });
  }
}

for (const width of [390, 1440]) {
  test(`chain rule folding ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(
      '/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=light&role=public#/chains/chain/apac/jp-iij',
    );
    const tree = page.locator('.chain-rule-tree');
    await expect(tree.locator('.chain-rule-node-head[role="button"]')).toHaveCount(2);
    await expect(tree.locator('.chain-rule-body:not([hidden])')).toHaveCount(1);
    await expect(tree.locator('.chain-rule-node-head[role="button"]').nth(0)).toHaveAttribute('aria-expanded', 'true');
    await expect(tree.locator('.chain-rule-node-head[role="button"]').nth(1)).toHaveAttribute('aria-expanded', 'false');
    await page.evaluate(() => document.fonts.ready);
    await expect(tree).toHaveScreenshot(`chain-folding-${width}.png`);
  });
}
