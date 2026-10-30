import { test, expect, type Page } from '@playwright/test';

async function expectActionRow(page: Page, selecting = false) {
  const header = page.locator('.chain-list-panel > header');
  const views = header.getByRole('group', { name: '线路视图' });
  const secondary = header.getByRole('button', { name: selecting ? '取消' : '多选', exact: true });
  const primary = header.getByRole('button', { name: selecting ? '删除链' : '＋ 新建分组', exact: true });
  const boxes = await Promise.all([views.boundingBox(), secondary.boundingBox(), primary.boundingBox()]);
  const [tabs, first, last] = boxes;
  expect(tabs).not.toBeNull();
  expect(first).not.toBeNull();
  expect(last).not.toBeNull();
  expect(tabs!.x + tabs!.width).toBeLessThanOrEqual(first!.x);
  expect(first!.x + first!.width).toBeLessThanOrEqual(last!.x);
  for (const button of [first!, last!]) {
    expect(button.y + button.height / 2).toBeCloseTo(tabs!.y + tabs!.height / 2, 0);
  }
  const bounds = await header.boundingBox();
  expect(tabs!.x).toBeGreaterThanOrEqual(bounds!.x);
  expect(last!.x + last!.width).toBeLessThanOrEqual(bounds!.x + bounds!.width);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}

for (const width of [320, 390, 482, 760, 1440])
  for (const theme of ['dark', 'light'])
    test(`chain list actions share a row ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains`);
      await expect(page.locator('.chain-card')).toHaveCount(16);
      await page.evaluate(() => document.fonts.ready);
      await expectActionRow(page);
      const header = page.locator('.chain-list-panel > header');
      await expect(header).toHaveScreenshot(`chain-header-${width}-${theme}.png`);

      const views = header.getByRole('group', { name: '线路视图' });
      await views.getByRole('button', { name: '解锁', exact: true }).click();
      await expect(page.getByRole('table', { name: /线路服务解锁/ })).toBeVisible();
      await expectActionRow(page);
      await expect(header.getByRole('button', { name: '多选', exact: true })).toBeDisabled();
      await views.getByRole('button', { name: '卡片', exact: true }).click();
      await header.getByRole('button', { name: '多选', exact: true }).click();
      await expectActionRow(page, true);
      await expect(views.getByRole('button', { name: '卡片', exact: true })).toBeDisabled();
      if (width === 320 || width === 390)
        await expect(header).toHaveScreenshot(`chain-header-selecting-${width}-${theme}.png`);
    });
