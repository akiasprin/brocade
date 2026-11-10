import { expect, test } from '@playwright/test';

for (const [width, theme, role] of [
  [1440, 'light', 'system-admin'], [1440, 'dark', 'user'],
  [900, 'dark', 'public'], [390, 'light', 'system-admin'],
  [390, 'dark', 'readonly'], [320, 'light', 'system-admin'], [320, 'dark', 'public'],
] as const) {
  test(`instance dashboard ${width} ${theme} ${role}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}&role=${role}`);
    const header = page.locator('.cvm-page-head');
    await expect(header.getByRole('status')).toHaveText('运行中');
    await page.evaluate(() => document.fonts.ready);
    const title = (await header.getByRole('heading', { level: 1 }).boundingBox())!;
    expect(title.width).toBeGreaterThanOrEqual(40);
    const tabs = (await header.getByRole('tablist').boundingBox())!;
    const actions = header.getByRole('group', { name: '页面操作', exact: true });
    if (await actions.count()) {
      const tools = (await actions.boundingBox())!;
      const overlap = Math.min(tabs.x + tabs.width, tools.x + tools.width) - Math.max(tabs.x, tools.x);
      const vertical = Math.min(tabs.y + tabs.height, tools.y + tools.height) - Math.max(tabs.y, tools.y);
      expect(overlap <= 0 || vertical <= 0).toBe(true);
    }
    await expect(page.getByRole('button', { name: '返回列表', exact: true })).toHaveCount(0);
    await expect(page.locator('.cvm-gauge')).toHaveCount(3);
    await expect(page.locator('.cvm-monitoring').getByText('持续采样')).toBeVisible();
    await expect(header).toHaveScreenshot(`identity-${width}-${theme}-${role}.png`);
    const widgets = page.locator('.cvm-dashboard > .cvm-section, .cvm-dashboard > .cvm-monitoring-view');
    const bodyWidth = (await page.locator('.cvm-dashboard').boundingBox())!.width;
    for (const [index, widget] of (await widgets.all()).entries()) {
      await widget.scrollIntoViewIfNeeded();
      if (width <= 900) expect((await widget.boundingBox())!.width).toBeGreaterThan(bodyWidth - 2);
      await expect(widget).toHaveScreenshot(`widget-${index}-${width}-${theme}-${role}.png`);
    }
    await header.scrollIntoViewIfNeeded();
    await expect(page).toHaveScreenshot(`dashboard-${width}-${theme}-${role}.png`);
    for (const box of await page.locator('.cvm-section, .cvm-monitoring').all())
      expect(await box.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(page.getByRole('heading', { name: '网卡与路由', exact: true })).toBeVisible();
    await expect(page.locator('.cvm-route-table')).toBeVisible();
    for (const [index, card] of (await page.locator('.cvm-attached').all()).entries()) {
      await card.scrollIntoViewIfNeeded();
      await expect(card).toHaveScreenshot(`nic-${index}-${width}-${theme}-${role}.png`);
    }
    await page.locator('.cvm-route-table').scrollIntoViewIfNeeded();
    await expect(page.locator('.cvm-route-summary')).toHaveScreenshot(`routes-${width}-${theme}-${role}.png`);
    if (role === 'public' || role === 'readonly') {
      for (const name of ['删除', '终端', '开机', '关机', '重启', '＋ 挂载网卡', '路由表', '卸载', '更换承载'])
        await expect(page.getByRole('button', { name, exact: true })).toHaveCount(0);
    }
    await page.getByRole('tab', { name: '监控与诊断', exact: true }).click();
    await expect(page.getByRole('heading', { name: '网卡与路由', exact: true })).toHaveCount(0);
    await page.getByRole('tab', { name: '概览', exact: true }).click();
    await expect(page.locator('.cvm-dashboard')).toBeVisible();
  });
}

test('dashboard fits a narrow desktop workspace', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&theme=light');
  const workspace = page.locator('.cvm-page');
  await workspace.evaluate(el => { (el as HTMLElement).style.width = '520px'; });
  await expect(workspace.locator('.cvm-gauge')).toHaveCount(3);
  expect(await workspace.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  await expect(workspace).toHaveScreenshot('narrow-desktop-dashboard.png');
});
