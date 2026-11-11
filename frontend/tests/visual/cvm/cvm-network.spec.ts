import { expect, test, type Page } from '@playwright/test';
async function open(page: Page, width: number, theme: string, detail = true) {
  page.on('pageerror', error => console.error('preview page error:', error.message));
  page.on('console', message => {
    if (message.type() === 'error') console.error('preview console:', message.text());
  });
  page.on('requestfailed', request => console.error('preview request:', request.url(), request.failure()?.errorText));
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${detail ? 'detail' : 'list'}&theme=${theme}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  if (detail) {
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(page.getByText('两端已连接', { exact: true })).toBeVisible();
    // The entry list above the NICs loads separately; wait so the layout is final.
    await expect(page.getByRole('list', { name: '访问入口' }).getByRole('listitem')).toHaveCount(3);
    await page.evaluate(() => document.fonts.ready);
  }
}
async function fits(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}
for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`NICs and independent routes ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      await expect(page.getByText('两端已连接')).toBeVisible();
      const network = page
        .locator('.cvm-section')
        .filter({ has: page.getByRole('heading', { name: '网卡与路由', exact: true }) });
      await expect(network).toHaveScreenshot(`allocated-nics-${width}-${theme}.png`);
      await page.getByRole('button', { name: '路由表', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '容器路由表' });
      await expect(dialog).toHaveScreenshot(`route-table-${width}-${theme}.png`);
      await fits(page);
      await dialog.getByRole('button', { name: '移除此卡的路由引用' }).click();
      await expect(dialog.getByRole('textbox', { name: '目标 IP／网段' })).toHaveCount(0);
      await dialog.getByRole('button', { name: '保存', exact: true }).click();
      await expect(dialog).toBeHidden();
      // NICs carry egress only: once no route references the NIC it can be detached.
      await expect(page.getByRole('button', { name: '卸载', exact: true })).toBeEnabled();
    });
    test(`publication and revisions ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, false);
      await page.getByRole('tab', { name: '网络' }).click();
      await expect(page.locator('.cvm-network-publications').getByText('网络', { exact: true })).toBeVisible();
      await expect(page.locator('.cvm-network-publications')).toHaveScreenshot(`publications-${width}-${theme}.png`);
      await page.getByRole('button', { name: '＋ 创建网络' }).click();
      const dialog = page.getByRole('dialog', { name: '创建网络' });
      await expect(dialog.getByRole('combobox', { name: '提供机器' })).toContainText('GreenVPS');
      await expect(dialog).toHaveScreenshot(`publish-nic-${width}-${theme}.png`);
      await fits(page);
    });
    for (const [button, title, file] of [
      ['更换承载', '更换网卡承载', 'nic-transport'],
      ['应用新规格', '应用网卡新规格', 'nic-version'],
    ] as const) {
      test(`${file} ${width} ${theme}`, async ({ page }) => {
        await open(page, width, theme);
        const action = page.getByRole('button', { name: button, exact: true });
        await action.evaluate(element => element.scrollIntoView({ block: 'center', behavior: 'instant' }));
        await action.click();
        const dialog = page.getByRole('dialog', { name: title, exact: true });
        await expect(dialog).toBeVisible();
        await expect(dialog).toHaveScreenshot(`${file}-${width}-${theme}.png`);
        await fits(page);
      });
    }
  }

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`publication dependencies and bulk update ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, false);
      await page.getByRole('tab', { name: '网络' }).click();
      await page.getByRole('button', { name: '删除网络', exact: true }).click();
      const remove = page.getByRole('dialog', { name: '删除网络', exact: true });
      await expect(remove.getByText('home-workspace')).toBeVisible();
      await expect(remove.getByRole('button', { name: '删除网络', exact: true })).toBeDisabled();
      await expect(remove).toHaveScreenshot(`publication-dependencies-${width}-${theme}.png`);
      await remove.getByRole('button', { name: '取消', exact: true }).click();
      await page.getByRole('button', { name: '批量应用新规格', exact: true }).click();
      const bulk = page.getByRole('dialog', { name: '批量应用网卡规格' });
      await bulk.getByRole('checkbox').check();
      await expect(bulk.getByRole('button', { name: '应用所选兼容规格' })).toBeEnabled();
      await expect(bulk).toHaveScreenshot(`bulk-nic-version-${width}-${theme}.png`);
      await fits(page);
    });
  }
