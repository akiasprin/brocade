import { test, expect, type Page } from '@playwright/test';

async function expectHeaderLayout(page: Page, selecting = false) {
  const header = page.locator('.chain-list-panel > header');
  const views = header.getByRole('tablist', { name: '线路视图' });
  const secondary = header.getByRole('button', { name: selecting ? '取消' : '多选', exact: true });
  const primary = header.getByRole('button', { name: selecting ? '删除链' : '＋ 新建分组', exact: true });
  const boxes = await Promise.all([views.boundingBox(), secondary.boundingBox(), primary.boundingBox()]);
  const [tabs, first, last] = boxes;
  expect(tabs).not.toBeNull();
  expect(first).not.toBeNull();
  expect(last).not.toBeNull();
  expect(first!.x + first!.width).toBeLessThanOrEqual(last!.x);
  expect(first!.y + first!.height / 2).toBeCloseTo(last!.y + last!.height / 2, 0);
  const bounds = await header.boundingBox();
  if (bounds!.width <= 700) expect(tabs!.y).toBeGreaterThanOrEqual(last!.y + last!.height);
  else {
    expect(tabs!.x + tabs!.width).toBeLessThanOrEqual(first!.x);
    expect(first!.y + first!.height / 2).toBeCloseTo(tabs!.y + tabs!.height / 2, 0);
  }
  expect(tabs!.x).toBeGreaterThanOrEqual(bounds!.x);
  expect(last!.x + last!.width).toBeLessThanOrEqual(bounds!.x + bounds!.width);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}

for (const width of [320, 390, 482, 760, 1440])
  for (const theme of ['dark', 'light'])
    test(`chain list page tabs and actions ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains`);
      await expect(page.locator('.chain-card')).toHaveCount(16);
      await page.evaluate(() => document.fonts.ready);
      await expectHeaderLayout(page);
      const header = page.locator('.chain-list-panel > header');
      await expect(header).toHaveScreenshot(`chain-header-${width}-${theme}.png`);

      const views = header.getByRole('tablist', { name: '线路视图' });
      await views.getByRole('tab', { name: '解锁', exact: true }).click();
      await expect(page.getByRole('table', { name: /线路服务解锁/ })).toBeVisible();
      await expectHeaderLayout(page);
      await expect(header.getByRole('button', { name: '多选', exact: true })).toBeDisabled();
      await views.getByRole('tab', { name: '卡片', exact: true }).click();
      await header.getByRole('button', { name: '多选', exact: true }).click();
      await expectHeaderLayout(page, true);
      await expect(views.getByRole('tab', { name: '卡片', exact: true })).toBeEnabled();
      await expect(views.getByRole('tab', { name: '解锁', exact: true })).toBeDisabled();
      if (width === 320 || width === 390)
        await expect(header).toHaveScreenshot(`chain-header-selecting-${width}-${theme}.png`);
    });
