import { test, expect, type Page } from '@playwright/test';

async function open(page: Page, query: string, width: number) {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.install({ time: new Date('2026-10-18T13:40:00Z') });
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/inflow-details-preview.html?${query}`);
  const dialog = page.getByRole('dialog', { name: 'alice · 流入明细' });
  await expect(dialog.locator('.nid-body')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return { dialog, surface: dialog.locator('.dialog-surface'), errors };
}

for (const [name, width, theme] of [
  ['desktop', 1440, 'dark'],
  ['mobile', 390, 'dark'],
  ['light', 1100, 'light'],
] as const) {
  test(`${name}: 流入明细按来源排行`, async ({ page }) => {
    const { dialog, surface, errors } = await open(page, `theme=${theme}`, width);
    const list = dialog.getByRole('list', { name: '来源' });
    const rows = list.locator(':scope > li');
    await expect(rows).toHaveCount(13);
    // 两条链上的同一地址只排一行，条形按链分段；只折叠地址，观测没能拆开的部分排在最后。
    await expect(list.locator('li', { hasText: '203.0.113.24' }).locator('.nid-meter > i')).toHaveCount(2);
    await expect(list.getByRole('button', { name: '其余 2 个来源 · 184.3 MiB' })).toBeVisible();
    await expect(rows.last()).toContainText('未记录来源');
    await expect(list.locator('li', { hasText: '203.0.113.24' })).toContainText('中国电信');
    expect(await dialog.locator('.nid-body').evaluate(element => element.scrollWidth > element.clientWidth)).toBe(
      false,
    );
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(surface).toHaveScreenshot(`${name}-inflow-details.png`);

    // 图例只看一条链；上行改变排行，每行只算这条链上的流量，没有上行的来源不列出。
    const legend = dialog.getByRole('group', { name: '接入链（点击只看这条链的来源）' });
    await legend.getByRole('button', { name: /香港 01 直出/ }).click();
    await dialog.getByRole('button', { name: '上行' }).click();
    await expect(rows).toHaveCount(8);
    await expect(rows.first()).toContainText('2001:db8:91f0:2c07');
    await expect(rows.first()).toContainText('307.2 MiB');
    if (name === 'desktop') await expect(surface).toHaveScreenshot(`${name}-inflow-details-chain.png`);
    expect(errors).toEqual([]);
  });
}

test('desktop: 流入明细的完整性提示只写一行，账本值点开再看', async ({ page }) => {
  const { dialog, surface, errors } = await open(page, 'scene=notice', 1440);
  await expect(dialog.getByRole('status')).toContainText(
    '来源记录不完整，仅含已记录部分；所选时段有采集缺口；与用量账本不一致',
  );
  await dialog.getByRole('button', { name: '与用量账本不一致' }).click();
  await expect(dialog.getByText(/用量账本：上行 2.71 GiB、下行 57.94 GiB/)).toBeVisible();
  await expect(surface).toHaveScreenshot('desktop-inflow-details-notice.png');
  expect(errors).toEqual([]);
});

test('desktop: 脱敏地址逐行保留，不给复制，只在唯一时写地区', async ({ page }) => {
  const { dialog, surface, errors } = await open(page, 'scene=masked', 1440);
  await expect(dialog.getByRole('list', { name: '来源' }).locator(':scope > li')).toHaveCount(6);
  await expect(dialog.getByRole('button', { name: /复制来源 IP/ })).toHaveCount(0);
  await expect(dialog.getByRole('region', { name: '流入合计' })).not.toContainText('来源 IP');
  await expect(surface).toHaveScreenshot('desktop-inflow-details-masked.png');
  expect(errors).toEqual([]);
});

test('desktop: 没有记录时只写一行', async ({ page }) => {
  const { dialog, surface, errors } = await open(page, 'scene=empty', 1440);
  await expect(dialog.getByText('当天 · 10-18 00:00–21:40，没有已记录的流入。')).toBeVisible();
  await expect(dialog.getByRole('list', { name: '来源' })).toHaveCount(0);
  await expect(surface).toHaveScreenshot('desktop-inflow-details-empty.png');
  expect(errors).toEqual([]);
});
