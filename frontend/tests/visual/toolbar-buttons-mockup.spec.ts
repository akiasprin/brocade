import { expect, test, type Page } from '@playwright/test';

async function openMockup(page: Page, variant = 'soft', theme = 'dark', width = 1440) {
  await page.setViewportSize({ width, height: 1080 });
  await page.goto(`/tests/fixtures/toolbar-buttons-mockup.html?variant=${variant}&theme=${theme}`);
  await expect(page.getByRole('heading', { name: '工具栏按钮，一套语言。' })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

for (const [variant, theme, width] of [
  ['soft', 'dark', 1440],
  ['outline', 'dark', 1440],
  ['grouped', 'dark', 1440],
  ['soft', 'light', 1440],
  ['soft', 'dark', 390],
  ['soft', 'light', 320],
] as const) {
  test(`${variant} ${theme} ${width}: shared toolbar appearance`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await openMockup(page, variant, theme, width);
    const controls = await page.locator('.stage .ctl').evaluateAll(buttons =>
      buttons.map(button => {
        const style = getComputedStyle(button);
        return {
          background: style.backgroundColor,
          border: style.borderColor,
          radius: style.borderRadius,
          height: button.getBoundingClientRect().height,
          color: style.color,
          font: style.fontSize,
        };
      }),
    );
    expect(controls).toHaveLength(5);
    for (const control of controls) expect(control).toEqual(controls[0]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    await expect(page).toHaveScreenshot(`${variant}-${theme}-${width}.png`, { fullPage: true });
    expect(errors).toEqual([]);
  });
}

test('range menu supports keyboard selection, dismissal and focus return', async ({ page }) => {
  await openMockup(page);
  const range = page.locator('.stage [data-popup="range"]');
  await range.focus();
  await range.press('ArrowDown');
  const menu = page.getByRole('menu', { name: '选择时间范围' });
  await expect(menu.getByRole('menuitemradio', { name: '近 1 小时', exact: true })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(range).toHaveText('近 6 小时');
  await expect(range).toHaveAttribute('aria-expanded', 'false');
  await expect(range).toBeFocused();
  await expect(page.locator('#axis-start')).toHaveText('16:00');
  await range.click();
  await expect(menu.getByRole('menuitemradio', { name: '近 6 小时', exact: true })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  await page.mouse.move(20, 20);
  await expect(page.locator('.stage')).toHaveScreenshot('range-menu-open.png');
  await page.keyboard.press('Escape');
  await expect(menu).toBeHidden();
  await expect(range).toBeFocused();
});

test('both quick actions and more menus are usable; direction and theme apply to both contexts', async ({ page }) => {
  await openMockup(page);
  await page.getByRole('button', { name: '节点', exact: true }).first().click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('香港 · 01');
  await page.getByRole('button', { name: '关闭弹窗' }).click();
  await page.getByRole('button', { name: '订阅', exact: true }).first().click();
  await expect(dialog).toContainText('https://console.example/sub/lin');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await page.getByRole('button', { name: '用户更多操作', exact: true }).click();
  await page.getByRole('menuitem', { name: '最近活动', exact: true }).click();
  await expect(dialog).toContainText('节点连接');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '图表更多操作', exact: true }).click();
  await page.getByRole('menuitem', { name: '刷新数据', exact: true }).click();
  await expect(page.getByRole('status')).toHaveText('已刷新示例数据');
  await page.getByRole('button', { name: 'B 细描边', exact: true }).click();
  await expect(page.locator('.stage')).toHaveAttribute('data-variant', 'outline');
  await expect(page.locator('.states')).toHaveAttribute('data-variant', 'outline');
  await page.getByRole('button', { name: '切换到浅色主题' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-variant', 'outline');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
});

test('narrow screen keeps range and more menus within the viewport', async ({ page }) => {
  await openMockup(page, 'soft', 'dark', 320);
  for (const name of ['range', 'traffic', 'user']) {
    const trigger = page.locator(`.stage [data-popup="${name}"]`);
    await trigger.click();
    const menu = page.getByRole('menu');
    const bounds = (await menu.boundingBox())!;
    expect(bounds.x).toBeGreaterThanOrEqual(0);
    expect(bounds.x + bounds.width).toBeLessThanOrEqual(320);
    expect(bounds.y).toBeGreaterThanOrEqual(0);
    expect(bounds.y + bounds.height).toBeLessThanOrEqual(1080);
    await page.keyboard.press('Escape');
    await expect(trigger).toBeFocused();
  }
});
