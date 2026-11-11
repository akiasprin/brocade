import { expect, test, type Page } from '@playwright/test';

// 香港 AKILE II 第一条规则的目标是这组截图一直使用的入口。
const akileHead = (page: Page) =>
  page.locator('.chain-rule-node-head').filter({ has: page.getByText('香港 AKILE II', { exact: true }) });
const akileTarget = (page: Page) =>
  akileHead(page)
    .locator('xpath=following-sibling::div[contains(concat(" ", @class, " "), " chain-rule-body ")]')
    .locator('.external-target-trigger')
    .first();

for (const [width, theme] of [
  [390, 'dark'],
  [1440, 'light'],
] as const) {
  for (const surface of ['rules', 'wizard'] as const) {
    test(`custom forwarding ${surface} ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      if (surface === 'rules') {
        await page.goto(`/tests/fixtures/rule-layout-preview.html?theme=${theme}`);
        // 链路规则默认只展开入口机器。
        if ((await akileHead(page).getAttribute('aria-expanded')) !== 'true') await akileHead(page).click();
        await akileTarget(page).click();
      } else {
        await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
        await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/nodes/chain/jp-iij`);
        await expect(page.getByRole('heading', { name: '新建链', exact: true })).toBeVisible();
        await page.getByRole('button', { name: '＋ 加一条' }).click();
        await page.getByRole('combobox', { name: /第 1 条动作/ }).selectOption('forward');
        // 与链路规则一致：改成转发后目标菜单直接打开。
        await expect(page.getByRole('button', { name: /第 1 条转发目标/ })).toHaveAttribute('aria-expanded', 'true');
      }
      await page.evaluate(() => document.fonts.ready);
      const menu = page.locator('.external-target-menu');
      const custom = menu.getByRole('button', { name: /自定义.*手动设置 IP、端口和协议/ });
      await expect(custom).toBeVisible();
      await expect(menu.getByRole('button', { name: /复用其他链的监听/ })).toBeVisible();
      if (surface === 'wizard') {
        await menu.getByRole('button', { name: /复用其他链的监听/ }).scrollIntoViewIfNeeded();
        await expect(menu).toHaveScreenshot(`wizard-targets-${width}-${theme}.png`);
      }
      const target = surface === 'rules' ? akileTarget(page) : page.getByRole('button', { name: /第 1 条转发目标/ });
      await custom.click();
      await expect(menu).toHaveCount(0);
      const dialog = page.getByRole('dialog', { name: '配置代理出站' });
      await expect(dialog.getByRole('button', { name: '手动填写', exact: true })).toHaveAttribute(
        'aria-pressed',
        'true',
      );
      await expect(dialog.getByRole('textbox', { name: '服务器地址' })).toBeVisible();
      await expect(dialog.getByRole('spinbutton', { name: '服务器端口' })).toBeVisible();
      await expect(dialog).toHaveScreenshot(`custom-${surface}-${width}-${theme}.png`);
      await dialog.getByRole('button', { name: '取消', exact: true }).click();
      await expect(dialog).toHaveCount(0);
      await expect(target).toBeFocused();
      await target.click();
      await custom.click();
      await dialog.getByRole('button', { name: 'SOCKS5', exact: true }).click();
      await dialog.getByRole('textbox', { name: '服务器地址' }).fill('edge.example.com');
      await dialog.getByRole('spinbutton', { name: '服务器端口' }).fill('18080');
      await dialog.getByRole('button', { name: '创建并选中' }).click();
      await expect(dialog).toHaveCount(0);
      await expect(target).toContainText('新代理出站');
      await expect(target).toBeFocused();
    });
  }
}
