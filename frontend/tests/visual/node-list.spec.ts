import { test, expect, type Page } from '@playwright/test';

async function preview(page: Page, width: number, height: number, query = '') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  // Poll ages and NIC windows are computed from Date.now(); timers keep running so queries still settle.
  await page.clock.setFixedTime(new Date('2026-10-25T12:00:00Z'));
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/node-list-preview.html?${query}`);
  await expect(page.locator('.node-list-panel > header')).toBeVisible();
  if (!query.includes('scene=empty')) {
    // Cards, NIC curves, month totals and TCP readings all come from separate queries.
    await expect(page.locator('.ncard .node-nic-plot:not(.empty)').first()).toBeVisible();
    await expect(page.locator('.ncard .nc-foot .lst-sum:not(.void)').first()).toBeVisible();
    await expect(page.locator('.ncard .nc-foot em', { hasText: 'ms' }).first()).toBeVisible();
  }
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

async function headLayout(page: Page) {
  return page.locator('.node-list-panel > header').evaluate(header => {
    const bounds = (element: Element) => {
      const box = element.getBoundingClientRect();
      return { left: box.left, right: box.right, top: box.top, bottom: box.bottom };
    };
    return {
      header: bounds(header),
      title: bounds(header.querySelector('h4')!),
      buttons: [...header.querySelectorAll('button')].map(bounds),
      overflow: document.documentElement.scrollWidth > innerWidth,
    };
  });
}

/** styles.css locks html/body; the fixture lets body scroll, standing in for the shell's work area. */
async function scrollTo(page: Page, top: number | 'end') {
  await page.evaluate(to => document.body.scrollTo(0, to === 'end' ? document.body.scrollHeight : to), top);
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(resolve)));
}

for (const [name, width, height, query] of [
  ['list-1440-dark', 1440, 900, ''],
  ['list-1440-light', 1440, 900, 'theme=light'],
  ['list-900-dark', 900, 900, ''],
  ['list-390-dark', 390, 844, ''],
  ['list-390-light', 390, 844, 'theme=light'],
] as const) {
  test(`${name}: the title bar matches the other list pages`, async ({ page }) => {
    const errors = await preview(page, width, height, query);
    const layout = await headLayout(page);
    expect(layout.overflow).toBe(false);
    expect(layout.buttons).toHaveLength(2);
    for (const button of layout.buttons) {
      // 多选与纳管靠右，和标题同在一行，不换到第二行。
      expect(button.left).toBeGreaterThanOrEqual(layout.title.right);
      expect(button.right).toBeLessThanOrEqual(layout.header.right);
      expect(button.top).toBeLessThan(layout.title.bottom);
      expect(button.bottom).toBeGreaterThan(layout.title.top);
    }
    await expect(page).toHaveScreenshot(`${name}.png`);
    expect(errors).toEqual([]);
  });
}

test('retired machines close the same grid instead of a second card', async ({ page }) => {
  const errors = await preview(page, 1440, 900);
  await expect(page.locator('.node-cardpage > .panel')).toHaveCount(1);
  await expect(page.locator('.node-list-panel .ncard')).toHaveCount(27);
  await expect(page.locator('.node-list-panel .ncard.off .nc-head > b')).toHaveText([
    '洛杉矶 旧 GIA',
    '首尔 旧机',
    '香港 旧 BGP',
  ]);
  await scrollTo(page, 'end');
  await expect.poll(() => page.evaluate(() => document.body.scrollTop)).toBeGreaterThan(0);
  await expect(page).toHaveScreenshot('retired-end-1440-dark.png');
  expect(errors).toEqual([]);
});

test('one selection retires live machines and removes stopped retired ones', async ({ page }) => {
  const errors = await preview(page, 1440, 900);
  await page.getByRole('button', { name: '多选' }).click();
  await page.getByRole('checkbox', { name: '选中 澳门 CTM', exact: true }).check();
  await page.getByRole('checkbox', { name: '选中 澳门 CTM II' }).check();
  await page.getByRole('checkbox', { name: '选中 首尔 旧机' }).check();
  await expect(page.getByRole('checkbox', { name: '选中 香港 旧 BGP' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '退役下线 2' })).toBeEnabled();
  await expect(page.getByRole('button', { name: '移除选中 1' })).toBeEnabled();
  await expect(page.getByRole('button', { name: '＋ 纳管机器' })).toHaveCount(0);
  await page.mouse.move(0, 0);
  await scrollTo(page, 0);
  await expect(page).toHaveScreenshot('select-1440-dark.png');
  await page.getByRole('button', { name: '取消' }).click();
  await expect(page.getByRole('checkbox')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('the selection tools stay inside the title bar on a phone', async ({ page }) => {
  const errors = await preview(page, 390, 844);
  await page.getByRole('button', { name: '多选' }).click();
  await page.getByRole('checkbox', { name: '选中 澳门 CTM', exact: true }).check();
  await scrollTo(page, 0);
  const layout = await headLayout(page);
  expect(layout.overflow).toBe(false);
  expect(layout.buttons).toHaveLength(3);
  for (const button of layout.buttons) {
    expect(button.left).toBeGreaterThanOrEqual(layout.header.left);
    expect(button.right).toBeLessThanOrEqual(layout.header.right);
  }
  await expect(page.locator('.node-list-panel > header')).toHaveScreenshot('select-390-head.png');
  expect(errors).toEqual([]);
});

test('visitors see the list without write tools', async ({ page }) => {
  const errors = await preview(page, 1440, 900, 'role=public');
  await expect(page.getByRole('button', { name: '多选' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '＋ 纳管机器' })).toHaveCount(0);
  await expect(page.locator('.node-list-panel > header')).toHaveScreenshot('visitor-head.png');
  expect(errors).toEqual([]);
});

test('an empty fleet keeps ＋ 纳管机器 without 多选', async ({ page }) => {
  const errors = await preview(page, 1440, 700, 'scene=empty');
  await expect(page.getByText('还没有机器')).toBeVisible();
  await expect(page.getByRole('button', { name: '多选' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '＋ 纳管机器' })).toBeVisible();
  await expect(page).toHaveScreenshot('empty-1440-dark.png');
  expect(errors).toEqual([]);
});
