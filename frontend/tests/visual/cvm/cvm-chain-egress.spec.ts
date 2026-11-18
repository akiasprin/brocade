import { expect, test, type Page } from '@playwright/test';
async function open(page: Page, width: number, theme: string, scene = 'chain-board') {
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  // Keep the complete proposal inside the viewport: Chromium's oversized element capture
  // temporarily resizes to 1px, crossing the real phone breakpoint and remounting the board.
  await page.setViewportSize({ width, height: scene === 'chain-board' && width > 820 ? 2000 : 1050 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
  await expect(page.locator('.cvm-page')).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() => document.fonts.ready);
}
for (const theme of ['light', 'dark']) {
  test(`chain board B, owner proposal and unauthorized drag ${theme}`, async ({ page }) => {
    await open(page, 1440, theme);
    await page.locator('[data-pill="cvm-alice"]').click();
    await expect(page.locator('[data-exit="chain:chain-us"]')).toHaveAttribute('title', 'alice 已获授权');
    await expect(page.locator('[data-exit="chain:chain-us"]')).toContainText('洛杉矶');
    await expect(page.locator('.cz-layout')).toHaveScreenshot(`chain-board-${theme}.png`);
    await page.getByRole('button', { name: '＋ 出口' }).click();
    await page.locator('[data-exit="chain:chain-us"]').click();
    const proposal = page.getByRole('region', { name: '待确认的组合变更' });
    await expect(proposal).toContainText('alice');
    await expect(proposal).toContainText('不占业务网卡槽位');
    await expect(proposal).toContainText('不回落到宿主 NAT');
    await expect(page.locator('.cz-layout')).toHaveScreenshot(`chain-proposal-${theme}.png`);
    await proposal.getByRole('button', { name: '预览产物差异', exact: true }).click();
    await expect(proposal.locator('.cvm-artifact-content').first()).toBeVisible();
    expect(
      await proposal.evaluate(
        n => n.getBoundingClientRect().right <= n.parentElement!.getBoundingClientRect().right + 1,
      ),
    ).toBe(true);
    expect(await proposal.evaluate(n => n.scrollWidth <= n.clientWidth + 1)).toBe(true);
    const notice = page.locator('.cvm-release-notice');
    expect(await notice.locator('span').evaluate(n => n.getBoundingClientRect().width)).toBeGreaterThan(200);
    await expect(page.locator('.cz-layout')).toHaveScreenshot(`chain-compiled-proposal-${theme}.png`);
    await proposal.getByRole('button', { name: '取消' }).click();
    const bob = page.locator('[data-pill="cvm-board-1"]');
    await bob.click();
    await expect(page.locator('[data-exit="chain:chain-jp"]')).toHaveAttribute('title', 'bob 未获授权');
    const from = await bob.locator('[data-anchor="out:cvm-board-1"]').boundingBox();
    const to = await page.locator('[data-exit="chain:chain-jp"]').boundingBox();
    if (!from || !to) throw new Error('drag anchors missing');
    await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 12 });
    await expect(page.locator('[data-exit="chain:chain-jp"]')).toContainText('bob 未获授权');
    await expect(page.locator('.cz-layout')).toHaveScreenshot(`chain-unauthorized-${theme}.png`);
    await page.mouse.up();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
  for (const width of [1440, 390, 320])
    test(`chain detail and grouped routes ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, 'chain-detail');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await expect(page.getByRole('list', { name: '经由线路 洛杉矶 · 三跳线路' })).toBeVisible();
      await expect(page.locator('.cvm-flow')).toHaveScreenshot(`chain-detail-${width}-${theme}.png`);
      await page.getByRole('button', { name: '路由表', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '容器路由表' });
      await expect(dialog.getByRole('group', { name: '出口优先级' })).toBeVisible();
      const selects = dialog.getByRole('combobox', { name: '经由' });
      await expect(selects.last().locator('optgroup')).toHaveCount(2);
      await expect(dialog).toHaveScreenshot(`chain-routes-${width}-${theme}.png`);
      await dialog.getByRole('button', { name: '预览产物差异', exact: true }).click();
      await expect(dialog.locator('.cvm-artifact-content').first()).toBeVisible();
      expect(await dialog.locator('.cvm-dialog-body').evaluate(n => n.scrollWidth <= n.clientWidth + 1)).toBe(true);
      await expect(dialog).toHaveScreenshot(`chain-routes-preview-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  test(`chain phone chips ${theme}`, async ({ page }) => {
    await open(page, 390, theme);
    await expect(page.locator('.cz-phone')).toContainText('洛杉矶 · 三跳线路');
    await expect(page.locator('.cz-phone')).toHaveScreenshot(`chain-phone-${theme}.png`);
  });
}
