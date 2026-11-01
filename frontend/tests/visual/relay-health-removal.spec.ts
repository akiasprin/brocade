import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  for (const [nodeId, badge] of [
    ['hk-hkix', false],
    ['jp-sb', true],
  ] as const) {
    test(`node observation at ${width}px keeps runtime attention ${badge}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-31T06:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(
        `/tests/fixtures/node-list-mockup.html?option=current&scene=typical&nav=0&theme=${theme}#/nodes/node/${nodeId}`,
      );
      const tab = page.getByRole('tab', { name: /^观测/ });
      await expect(tab).toBeVisible();
      await expect(tab.locator('.nd-tab-badge')).toHaveCount(badge ? 1 : 0);
      await expect(page.getByRole('heading', { name: '运行状态', exact: true })).toBeAttached();
      await page.evaluate(() => document.fonts.ready);
      await expect(page.locator('.nd-tabs')).toHaveScreenshot(`observation-tabs-${width}-${theme}-${badge}.png`);
      const runtime = page.locator('.nd-runtime');
      await runtime.scrollIntoViewIfNeeded();
      await expect(runtime.locator(':scope > header')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
      await expect(runtime.locator(':scope > header')).toHaveCSS('border-bottom-width', '0px');
      if (badge) await expect(runtime.getByText(/握手超过 180 秒/)).toBeVisible();
      await expect(runtime).toHaveScreenshot(`runtime-${width}-${theme}-${badge}.png`, { threshold: 0.01 });
      expect(errors).toEqual([]);
    });
  }
}
