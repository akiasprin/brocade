import { test, expect } from '@playwright/test';

for (const [name, width, height, theme, safe, longName] of [
  ['phone', 390, 844, 'dark', [47, 0, 34, 0], false],
  ['small-phone', 320, 600, 'light', [20, 0, 34, 0], false],
  ['landscape', 844, 390, 'dark', [0, 47, 21, 47], false],
  ['long-name', 390, 844, 'light', [47, 0, 34, 0], true],
  ['no-safe-area', 390, 844, 'light', [0, 0, 0, 0], false],
  ['desktop', 1440, 1000, 'dark', [0, 0, 0, 0], false],
] as const) {
  test.describe(name, () => {
    test.use({ viewport: { width, height }, isMobile: width < 1000, hasTouch: width < 1000 });
    test('Mux actions stay visible while editing and scrolling', async ({ page }) => {
      await page.goto(`/tests/fixtures/mux-editor-preview.html?theme=${theme}${longName ? '&longName' : ''}`);
      await page.evaluate(insets => {
        ['top', 'right', 'bottom', 'left'].forEach((side, i) =>
          document.documentElement.style.setProperty(`--safe-${side}`, `${insets[i]}px`),
        );
      }, safe);
      const configure = page.getByRole('button', { name: '配置参数', exact: true });
      await configure.click();
      const drawer = page.locator('.mux-config-drawer');
      const apply = drawer.getByRole('button', { name: '应用', exact: true });
      await page.evaluate(() => document.fonts.ready);
      await expect(page.getByRole('dialog')).toHaveScreenshot(`${name}.png`);
      await expect(apply).toBeInViewport({ ratio: 1 });
      const bounds = await drawer.evaluate(element => {
        const rect = element.getBoundingClientRect();
        return { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left };
      });
      expect(bounds.top).toBe(safe[0]);
      expect(bounds.right).toBeLessThanOrEqual(width - safe[1]);
      expect(bounds.bottom).toBe(height - safe[2]);
      expect(bounds.left).toBeGreaterThanOrEqual(safe[3]);
      const body = drawer.locator('.external-outbound-body');
      const scrollable = await body.evaluate(element => element.scrollHeight > element.clientHeight);
      if (width < 1000) expect(scrollable).toBe(true);
      if (scrollable) {
        await body.evaluate(element => {
          element.scrollTop = element.scrollHeight;
        });
        expect(await body.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
      }
      await expect(apply).toBeInViewport({ ratio: 1 });
      await expect(page.getByRole('dialog')).toHaveScreenshot(`${name}-bottom.png`);
      const concurrency = drawer.getByRole('spinbutton', { name: '复用流数量', exact: true });
      await concurrency.fill('7');
      await expect(apply).toBeEnabled();
      await expect(apply).toBeInViewport({ ratio: 1 });
      await apply.click();
      await expect(drawer).toHaveCount(0);
      await configure.click();
      await expect(page.getByRole('spinbutton', { name: '复用流数量', exact: true })).toHaveValue('7');
      await page.locator('.mux-config-drawer').getByRole('button', { name: '取消', exact: true }).click();
      await expect(drawer).toHaveCount(0);
      const save = page.getByRole('button', { name: '保存到草稿', exact: true });
      await expect(save).toBeEnabled();
      await save.scrollIntoViewIfNeeded();
      await expect(save).toBeInViewport({ ratio: 1 });
    });
  });
}
