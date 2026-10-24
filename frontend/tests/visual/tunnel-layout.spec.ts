import { test, expect, type Page } from '@playwright/test';

async function preview(page: Page, width: number, height: number, theme = 'dark') {
  page.on('dialog', dialog => dialog.accept());
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/tunnel-layout-preview.html?theme=${theme}`);
  await expect(page.locator('.tunnel-row.manual-row')).toHaveCount(60);
  await page.evaluate(() => document.fonts.ready);
}

async function openEditor(page: Page, protocol: string) {
  await page.getByRole('button', { name: '＋ 新建隧道', exact: true }).click();
  await page.getByRole('button', { name: /导入或自定义配置/ }).click();
  await expect(page.getByRole('dialog', { name: '创建隧道', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '手动填写', exact: true }).click();
  await page.locator('.external-protocols').getByRole('button', { name: protocol, exact: true }).click();
}

for (const width of [320, 390, 620, 621, 820, 1440]) {
  for (const theme of ['dark', 'light']) {
    test(`60 custom tunnels at ${width}px in ${theme}`, async ({ page }) => {
      await preview(page, width, 844, theme);
      await expect(page).toHaveScreenshot(`list-${width}-${theme}.png`);
      const rows = await page.locator('.tunnel-row.manual-row').evaluateAll(elements =>
        elements.map(element => {
          const main = element.querySelector('.tunnel-row-main')!;
          const name = main.querySelector('b')!;
          const row = element.getBoundingClientRect();
          const arrow = element.querySelector('.tunnel-chevron')!.getBoundingClientRect();
          return {
            nameWidth: name.getBoundingClientRect().width,
            mainWidth: main.getBoundingClientRect().width,
            arrowRight: arrow.right,
            rowRight: row.right,
            overflow: element.scrollWidth > element.clientWidth,
          };
        }),
      );
      for (const row of rows) {
        expect(row.mainWidth).toBeGreaterThanOrEqual(90);
        expect(row.nameWidth).toBeGreaterThanOrEqual(60);
        expect(row.arrowRight).toBeLessThanOrEqual(row.rowRight);
        expect(row.overflow).toBe(false);
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }
}

for (const [width, height, theme, protocol] of [
  [1440, 1000, 'dark', 'VLESS'],
  [1440, 1000, 'light', 'AnyTLS'],
  [390, 844, 'dark', 'VLESS'],
  [390, 844, 'light', 'AnyTLS'],
  [320, 600, 'dark', 'HTTP CONNECT'],
  [820, 500, 'light', 'VLESS'],
] as const) {
  test(`${protocol} drawer stays usable at ${width}x${height} in ${theme}`, async ({ page }) => {
    await preview(page, width, height, theme);
    await openEditor(page, protocol);
    const drawer = page.locator('.external-outbound-drawer');
    const body = drawer.locator('.external-outbound-body');
    const footer = drawer.locator('footer');
    await expect(page).toHaveScreenshot(`drawer-${protocol.replace(' ', '-')}-${width}-${theme}.png`);
    await expect(footer).toBeInViewport({ ratio: 1 });
    expect(await drawer.evaluate(element => element.getBoundingClientRect().height)).toBe(height);
    expect(await drawer.evaluate(element => element.getBoundingClientRect().right)).toBe(width);
    expect(await body.evaluate(element => element.scrollHeight > element.clientHeight)).toBe(true);
    await body.hover();
    await page.mouse.wheel(0, 10000);
    await expect.poll(() => body.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    await expect(footer).toBeInViewport({ ratio: 1 });
    await expect(page).toHaveScreenshot(`drawer-bottom-${protocol.replace(' ', '-')}-${width}-${theme}.png`);
    // Keyboard focus must reach the actions without scrolling the entire fixed surface offscreen.
    const cancel = footer.getByRole('button', { name: '取消', exact: true });
    await cancel.focus();
    await expect(cancel).toBeFocused();
    await expect(cancel).toBeInViewport({ ratio: 1 });
    expect(await page.evaluate(() => document.body.style.overflow)).toBe('hidden');
    expect(await page.evaluate(() => window.scrollY)).toBe(0);
    await cancel.click();
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.getByRole('button', { name: '＋ 新建隧道', exact: true })).toBeFocused();
    expect(await page.evaluate(() => document.body.style.overflow)).toBe('');
  });
}

test('an open long form follows viewport height changes', async ({ page }) => {
  await preview(page, 390, 844);
  await openEditor(page, 'VLESS');
  for (const height of [500, 700, 844]) {
    await page.setViewportSize({ width: 390, height });
    await expect(page.locator('.external-outbound-drawer > footer')).toBeInViewport({ ratio: 1 });
    await expect
      .poll(() => page.locator('.external-outbound-drawer').evaluate(element => element.clientHeight))
      .toBe(height);
  }
});
