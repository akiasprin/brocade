import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`ordinary user sees provider names without management inventory ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=list&role=user&theme=${theme}`);
    const catalog = await page.evaluate(async () => fetch('/cvm/networks').then(response => response.json()));
    expect(catalog.providers).toEqual([]);
    expect(catalog.can_manage).toBe(false);
    expect(catalog.provider_names['remote-ingress']).toBe('GreenVPS · 香港网络');
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(page.locator('.cvm-network-publications .cvm-pub-sub')).toContainText('GreenVPS · 香港网络');
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&role=user&theme=${theme}`);
    const flow = page.getByRole('region', { name: '入口、网卡与出口' });
    await expect(flow.locator('.cvm-flow-exit-sub')).toContainText('GreenVPS · 香港网络');
    await expect(flow).not.toContainText('remote-ingress');
    await page.evaluate(() => document.fonts.ready);
    await expect(flow).toHaveScreenshot(`ordinary-user-names-${width}-${theme}.png`);
  });
}

for (const [width, theme, role] of [
  [1440, 'light', 'system-admin'],
  [1440, 'dark', 'user'],
  [900, 'dark', 'public'],
  [390, 'light', 'system-admin'],
  [390, 'dark', 'readonly'],
  [320, 'light', 'system-admin'],
  [320, 'dark', 'public'],
] as const) {
  test(`instance dashboard ${width} ${theme} ${role}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}&role=${role}`);
    const header = page.locator('.cvm-page-head');
    // 运行状态是标题图标角上的状态灯，与机器详情一致。
    await expect(header.getByRole('img', { name: '运行中', exact: true })).toBeVisible();
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
    await expect(page.locator('.cvm-stat')).toHaveCount(4);
    await expect(page.getByRole('region', { name: '容器 监控' }).getByText(/持续采样/)).toBeVisible();
    await expect(header).toHaveScreenshot(`identity-${width}-${theme}-${role}.png`);
    const widgets = page.locator(
      '.cvm-dashboard > .cvm-flow, .cvm-dashboard > .cvm-monitoring-view, .cvm-overview-row > .cvm-section',
    );
    const bodyWidth = (await page.locator('.cvm-dashboard').boundingBox())!.width;
    for (const [index, widget] of (await widgets.all()).entries()) {
      await widget.scrollIntoViewIfNeeded();
      if (width <= 820) expect((await widget.boundingBox())!.width).toBeGreaterThan(bodyWidth - 2);
      await expect(widget).toHaveScreenshot(`overview-${index}-${width}-${theme}-${role}.png`);
    }
    await header.scrollIntoViewIfNeeded();
    await expect(page).toHaveScreenshot(`dashboard-${width}-${theme}-${role}.png`);
    // 小组合台的插座有意落在卡片边缘之外，只检查承载文字的块。
    for (const box of await page.locator('.cvm-section, .cvm-stat, .cvm-flow-routes').all())
      expect(await box.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    if (width <= 820) {
      for (const text of await page
        .locator('.cvm-flow-box-head > b, .cvm-flow-nic-name, .cvm-flow-nic-name > small, .cvm-flow-ip')
        .all())
        expect(await text.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(page.getByRole('heading', { name: '网卡详情', exact: true })).toBeVisible();
    await expect(page.locator('.cvm-flow-routes').first()).toBeVisible();
    for (const [index, card] of (await page.locator('.cvm-attached').all()).entries()) {
      await card.scrollIntoViewIfNeeded();
      await expect(card).toHaveScreenshot(`nic-${index}-${width}-${theme}-${role}.png`);
    }
    await page.locator('.cvm-flow').scrollIntoViewIfNeeded();
    await expect(page.locator('.cvm-flow')).toHaveScreenshot(`network-flow-${width}-${theme}-${role}.png`);
    if (role === 'public' || role === 'readonly') {
      for (const name of ['删除', '终端', '开机', '关机', '重启', '＋ 挂载网卡', '路由表', '卸载', '更换承载'])
        await expect(page.getByRole('button', { name, exact: true })).toHaveCount(0);
    }
    await page.getByRole('tab', { name: '监控与诊断', exact: true }).click();
    await expect(page.getByRole('heading', { name: '网卡详情', exact: true })).toHaveCount(0);
    await page.getByRole('tab', { name: '概览', exact: true }).click();
    await expect(page.locator('.cvm-dashboard')).toBeVisible();
  });
}

test('dashboard fits a narrow desktop workspace', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&theme=light');
  const workspace = page.locator('.cvm-page');
  await workspace.evaluate(el => {
    (el as HTMLElement).style.width = '520px';
  });
  await expect(workspace.locator('.cvm-stat')).toHaveCount(4);
  // The entries, provider catalog and readings settle independently of the workspace query.
  await expect(workspace.getByRole('list', { name: '访问入口' }).getByRole('listitem')).toHaveCount(3);
  await expect(workspace.locator('.cvm-flow-exit-sub').first()).toContainText('GreenVPS · 香港网络');
  await expect(workspace.getByRole('region', { name: '容器 监控' }).getByText(/持续采样/)).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  expect(await workspace.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  for (const text of await workspace.locator('.cvm-flow-nic-name, .cvm-flow-ip').all())
    expect(await text.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  await expect(workspace).toHaveScreenshot('narrow-desktop-dashboard.png');
});
