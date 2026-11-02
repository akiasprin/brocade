import { test, expect, type Page } from '@playwright/test';

async function openCards(page: Page, width: number, theme = 'dark', role = 'system-admin') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(
    `/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}&role=${role}#/chains`,
  );
  await expect(page.locator('.chain-card')).toHaveCount(16);
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

for (const [width, columns] of [
  [390, 1],
  [760, 2],
  [820, 2],
  [821, 2],
  [1024, 2],
  [1280, 3],
  [1440, 3],
  [1536, 4],
  [1920, 5],
  [2560, 7],
])
  for (const theme of ['dark', 'light'])
    test(`adaptive chain cards ${width} ${theme}`, async ({ page }) => {
      const errors = await openCards(page, width, theme);
      // Responsive sizing uses layout widths; mount transforms change painted bounds temporarily.
      const grids = await page.locator('.chain-card-grid').evaluateAll(elements =>
        elements.map(grid => ({
          columns: getComputedStyle(grid).gridTemplateColumns.split(' ').length,
          availableWidth: Number.parseFloat(getComputedStyle(grid).width),
          widths: [...grid.querySelectorAll('.chain-card')].map(card =>
            Number.parseFloat(getComputedStyle(card).width),
          ),
        })),
      );
      expect(grids).toHaveLength(4);
      const referenceWidth = grids[0].widths[0];
      for (const grid of grids) {
        expect(grid.columns).toBe(columns);
        for (const cardWidth of grid.widths) {
          expect(cardWidth).toBeGreaterThanOrEqual(Math.min(330, grid.availableWidth) - 0.02);
          expect(cardWidth).toBeCloseTo(referenceWidth, 0);
        }
      }
      await test
        .info()
        .attach('card-geometry', { body: JSON.stringify({ width, theme, grids }), contentType: 'application/json' });
      await expect(page.locator('.chain-section-head .no, .chain-section-head .id')).toHaveCount(0);
      const counts = page.locator('.chain-section-head .count');
      if (width <= 820) await expect(counts.first()).toBeHidden();
      else await expect(counts.first()).toBeVisible();
      await expect(page.getByRole('heading', { level: 1, name: '线路', exact: true })).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      const card = page.locator('.chain-card').first();
      expect(await card.evaluate(el => getComputedStyle(el).borderTopColor)).toBe('rgba(0, 0, 0, 0)');
      expect(await card.evaluate(el => getComputedStyle(el).boxShadow)).not.toBe('none');
      await expect(page).toHaveScreenshot(`adaptive-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });

test('390px touch viewport preserves the production card width', async ({ browser, baseURL }) => {
  const context = await browser.newContext({
    baseURL,
    viewport: { width: 390, height: 1000 },
    isMobile: true,
    hasTouch: true,
    reducedMotion: 'reduce',
    locale: 'zh-CN',
    timezoneId: 'Asia/Taipei',
  });
  try {
    const page = await context.newPage();
    const errors = await openCards(page, 390, 'light');
    const card = await page.locator('.chain-card').first().boundingBox();
    await test.info().attach('touch-card-geometry', { body: JSON.stringify(card), contentType: 'application/json' });
    expect(card!.width).toBeGreaterThanOrEqual(330);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot('touch-390-light.png');
    expect(errors).toEqual([]);
  } finally {
    await context.close();
  }
});

test('page tabs support keyboard navigation, association and stored choice', async ({ page }) => {
  const errors = await openCards(page, 1440);
  const tabs = page.getByRole('tablist', { name: '线路视图' });
  const cards = tabs.getByRole('tab', { name: '卡片', exact: true });
  const unlock = tabs.getByRole('tab', { name: '解锁', exact: true });
  await cards.focus();
  for (const key of ['ArrowRight', 'End', 'ArrowLeft', 'Home']) {
    const source = key === 'ArrowLeft' || key === 'Home' ? unlock : cards;
    const selected = key === 'ArrowLeft' || key === 'Home' ? cards : unlock;
    // Restore the starting tab without crossing a navigation boundary.
    await source.click();
    await source.press(key);
    await expect(selected).toHaveAttribute('aria-selected', 'true');
    await expect(selected).toBeFocused();
    const panel = page.getByRole('tabpanel');
    await expect(selected).toHaveAttribute('aria-controls', (await panel.getAttribute('id'))!);
    await expect(panel).toHaveAttribute('aria-labelledby', (await selected.getAttribute('id'))!);
  }
  await unlock.click();
  expect(await page.evaluate(() => localStorage.getItem('brocade-console:chain-view'))).toBe('unlock');
  await expect(page.getByRole('table', { name: /线路服务解锁/ })).toBeVisible();
  await cards.click();
  await page.locator('.chain-card[data-order-id="hk-direct"]').click();
  await expect(page.getByRole('heading', { level: 1, name: '香港 直出' })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole('tab', { name: '卡片', exact: true })).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('.chain-card')).toHaveCount(16);
  expect(errors).toEqual([]);
});

test('selection and group edits lock only the other tab and recover on cancel', async ({ page }) => {
  await openCards(page, 390);
  const tabs = page.getByRole('tablist', { name: '线路视图' });
  const cards = tabs.getByRole('tab', { name: '卡片', exact: true });
  const unlock = tabs.getByRole('tab', { name: '解锁', exact: true });
  const header = page.locator('.chain-list-panel > header');
  await header.getByRole('button', { name: '多选', exact: true }).click();
  await page.getByRole('checkbox', { name: '选中 香港 直出', exact: true }).check();
  await expect(header.getByRole('button', { name: '删除链 1', exact: true })).toBeEnabled();
  await expect(cards).toBeEnabled();
  await expect(unlock).toBeDisabled();
  await cards.press('ArrowRight');
  await expect(cards).toHaveAttribute('aria-selected', 'true');
  await header.getByRole('button', { name: '取消', exact: true }).click();
  await expect(unlock).toBeEnabled();
  await header.getByRole('button', { name: '＋ 新建分组', exact: true }).click();
  await expect(page.getByPlaceholder('分组名称，比如：「三网优化」')).toBeVisible();
  await expect(unlock).toBeDisabled();
  await page.locator('.prj-newbar').getByRole('button', { name: '取消', exact: true }).click();
  await page.getByRole('button', { name: '亚太精品', exact: true }).click();
  await expect(unlock).toBeDisabled();
  await page.locator('.prj-ren').getByRole('button', { name: '取消', exact: true }).click();
  await expect(unlock).toBeEnabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

for (const role of ['readonly', 'public'])
  for (const width of [390, 1440])
    test(`page tabs remain usable for ${role} at ${width}`, async ({ page }) => {
      const errors = await openCards(page, width, 'light', role);
      const header = page.locator('.chain-list-panel > header');
      for (const name of ['多选', '＋ 新建分组']) {
        const action = header.getByRole('button', { name, exact: true });
        if (role === 'public') await expect(action).toHaveCount(0);
        else await expect(action).toBeDisabled();
      }
      await expect(page.locator('.chain-section-name')).toHaveCount(0);
      await expect(page.locator('.new-chain').first()).toBeDisabled();
      await expect(page).toHaveScreenshot(`roles-${role}-${width}.png`);
      await header.getByRole('tab', { name: '解锁', exact: true }).click();
      await expect(page.getByRole('table', { name: /线路服务解锁/ })).toBeVisible();
      expect(errors).toEqual([]);
    });

test('a two-chain group keeps the common card width after a cross-group drop', async ({ page }) => {
  await openCards(page, 1920);
  await page.setViewportSize({ width: 1920, height: 1800 });
  const target = page.locator('.chain-section[data-order-id="lab"]');
  await expect(target.locator('.chain-card')).toHaveCount(1);
  const source = page.locator('.chain-card[data-order-id="hk-direct"]');
  const from = await source.boundingBox();
  const to = await target.locator('.chain-section-head').boundingBox();
  await page.mouse.move(from!.x + 80, from!.y + 30);
  await page.mouse.down();
  await page.mouse.move(to!.x + 160, to!.y + 20, { steps: 12 });
  await page.mouse.up();
  await expect(target.locator('.chain-card')).toHaveCount(2);
  const reference = await page.locator('.chain-card-grid').first().locator('.chain-card').first().boundingBox();
  for (const card of await target.locator('.chain-card').all()) {
    const box = await card.boundingBox();
    expect(box!.width).toBeCloseTo(reference!.width, 0);
  }
});
