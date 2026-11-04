import { test, expect } from '@playwright/test';
for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`stopped Xray keeps machine configuration editable ${width} ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-25T12:00:00Z'));
    await page.setViewportSize({ width, height: 960 });
    await page.goto(`/tests/fixtures/node-list-preview.html?theme=${theme}&scene=calm&xray=off#/nodes`);
    await page
      .getByRole('button', { name: /查看 .* 的机器详情/ })
      .first()
      .click();
    await page.getByRole('tab', { name: '配置', exact: true }).click();
    const panel = page.locator('.nd-tab-config');
    const headings = (await panel.getByRole('heading', { level: 4 }).allTextContents()).map(text => text.trim());
    const order = ['流量统计', 'XRAY', 'WIREGUARD', '来源 IP 封禁'].map(title => headings.indexOf(title));
    expect(order.every(index => index >= 0)).toBe(true);
    expect(order).toEqual([...order].sort((a, b) => a - b));
    await expect(panel.getByText(/已关闭，配置仍可编辑/)).toBeVisible();
    await expect(panel.getByRole('button', { name: '启用', exact: true }).first()).toBeEnabled();
    const inputs = panel.locator('input.f:not([type="checkbox"])');
    await expect(inputs.first()).toBeEnabled();
    await inputs.first().fill('关闭期间仍可编辑');
    await expect(inputs.first()).toHaveValue('关闭期间仍可编辑');
    const dns = panel.locator('.panel').filter({ has: page.getByRole('heading', { name: 'DNS', exact: true }) });
    await expect(dns.locator('select').first()).toBeEnabled();
    const connection = panel.locator('details.conn-card');
    await connection.locator('summary').click();
    await expect(connection.locator('input').first()).toBeEnabled();
    await connection.locator('input').first().fill('600');
    await expect(connection.locator('input').first()).toHaveValue('600');
    await page.evaluate(() => document.fonts.ready);
    await panel
      .locator('.panel')
      .filter({ has: page.getByRole('heading', { name: 'XRAY', exact: true }) })
      .scrollIntoViewIfNeeded();
    await expect(page).toHaveScreenshot(`xray-off-config-${width}-${theme}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
