import { test, expect } from '@playwright/test';

for (const [name, width, theme] of [
  ['desktop', 1440, 'dark'],
  ['mobile', 390, 'dark'],
  ['light', 1100, 'light'],
] as const) {
  test(`${name}: source bans inside 在线设备`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-10-23T08:00:00Z') });
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/user-header-preview.html?palette=jinzi&theme=${theme}&presence=sources`);
    const card = page.locator('.user-presence-card');
    const banned = card.getByRole('list', { name: '已封禁的来源' });
    await expect(banned.getByText('203.0.113.17')).toBeVisible();
    await expect(banned.getByText('2001:db8:abcd:ffff')).toBeVisible();
    await expect(banned.getByRole('button', { name: '已生效' })).toBeVisible();
    // 已封禁的地址移出时间线，只在卡底一组里出现一次。
    await expect(card.locator('.user-presence-row', { hasText: '203.0.113.17' })).toHaveCount(1);
    await expect(page.getByRole('group', { name: '来源 IP 封禁' })).toHaveCount(0);
    await card.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    expect(await card.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(card).toHaveScreenshot(`${name}-source-access.png`);

    // 行内封禁：禁行钮与复制钮并排，确认面板贴着它弹出；Esc 关闭后焦点回到禁行钮。
    const row = card.locator('.user-presence-row', { hasText: '198.51.100.77' });
    await row.hover();
    const ban = row.getByRole('button', { name: '封禁 198.51.100.77' });
    await ban.click();
    const confirm = page.getByRole('dialog', { name: '封禁 198.51.100.77' });
    await expect(confirm.getByText('断开该来源的会话，并拒绝再次接入。')).toBeVisible();
    await expect(confirm.locator('.dialog-surface')).toHaveScreenshot(`${name}-source-access-confirm.png`);
    await page.keyboard.press('Escape');
    await expect(confirm).toBeHidden();
    await expect(ban).toBeFocused();

    // 手动添加：输入已知来源时，下方写出它的地区和在线状态。
    await card.getByRole('button', { name: '封禁其他 IP' }).click();
    const input = card.getByRole('textbox', { name: '来源 IP' });
    await expect(input).toBeFocused();
    await input.fill('203.0.113.24');
    await expect(card.getByText(/中国电信 · 在线，封禁后断开/)).toBeVisible();
    await page.mouse.move(0, 0);
    await expect(card).toHaveScreenshot(`${name}-source-access-add.png`);
    await input.press('Escape');
    await expect(input).toBeHidden();
    await expect(card.getByRole('button', { name: '封禁其他 IP' })).toBeVisible();

    // 执行情况：点开逐台列出机器，面板与状态右缘对齐、不越出视口。
    await banned.getByRole('button', { name: '已生效' }).click();
    const nodes = page.getByRole('dialog', { name: '各机器执行情况' });
    await expect(nodes.getByText('示例节点')).toBeVisible();
    const placement = await nodes.locator('.dialog-surface').evaluate(surface => {
      const box = surface.getBoundingClientRect();
      return { left: box.left, right: box.right };
    });
    const status = await banned.getByRole('button', { name: '已生效' }).boundingBox();
    expect(Math.abs(placement.right - (status!.x + status!.width))).toBeLessThanOrEqual(1);
    expect(placement.left).toBeGreaterThanOrEqual(8);
    if (name === 'desktop')
      await expect(nodes.locator('.dialog-surface')).toHaveScreenshot(`${name}-source-access-nodes.png`);
    await page.keyboard.press('Escape');
    await expect(nodes).toBeHidden();
    expect(errors).toEqual([]);
  });
}
