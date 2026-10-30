import { test, expect } from '@playwright/test';

for (const width of [390, 1440])
  for (const theme of ['dark', 'light'])
    test(`move chain form ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-29T04:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(
        `/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains/chain/apac/hk-direct`,
      );
      await expect(page.getByRole('button', { name: '移动到分组', exact: true })).toBeVisible();
      if (width === 1440) {
        expect(
          await page
            .locator('.chain-detail-identity h1 button')
            .evaluate(element => element.scrollWidth <= element.clientWidth),
        ).toBe(true);
      }
      await expect(page.locator('.chain-detail-head')).toHaveScreenshot(`chain-group-action-${width}-${theme}.png`);
      await page.getByRole('button', { name: '移动到分组', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '移动到分组', exact: true });
      await dialog.getByRole('combobox', { name: '目标分组' }).selectOption('__new_group__');
      await dialog.getByRole('textbox', { name: '新分组名称' }).fill('香港专线');
      await expect(dialog).toHaveScreenshot(`move-chain-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await dialog.getByRole('button', { name: '保存到草稿', exact: true }).click();
      await expect(dialog).toBeHidden();
      await expect(page).toHaveURL(/#\/chains\/chain\/app-[0-9a-f]+\/hk-direct$/);
      await expect(page.getByRole('heading', { level: 1, name: '香港 直出' })).toBeVisible();
      await expect(page.getByText(/没有这条链/)).toHaveCount(0);
    });

test('cross-group pointer movement commits on release and Escape cancels', async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 1500 });
  await page.goto('/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=dark#/chains');
  const source = page.locator('.chain-card[data-order-id="hk-direct"]');
  const original = page.locator('[data-order-kind="app"][data-order-id="apac"]');
  const target = page.locator('[data-order-kind="app"][data-order-id="cn2"]');
  await expect(source).toBeVisible();
  const from = await source.boundingBox();
  const to = await target.locator('.chain-section-head').boundingBox();
  await page.mouse.move(from!.x + 80, from!.y + 30);
  await page.mouse.down();
  await page.mouse.move(to!.x + 160, to!.y + 20, { steps: 12 });
  await expect(target).toHaveClass(/chain-drop-target/);
  await expect(target).toHaveScreenshot('move-chain-drop-target.png');
  await page.keyboard.press('Escape');
  await page.mouse.up();
  await expect(original.locator('.chain-card[data-order-id="hk-direct"]')).toHaveCount(1);
  await expect(target).not.toHaveClass(/chain-drop-target/);
  await page.mouse.move(from!.x + 80, from!.y + 30);
  await page.mouse.down();
  await page.mouse.move(to!.x + 160, to!.y + 20, { steps: 12 });
  await page.mouse.up();
  await expect(target.locator('.chain-card[data-order-id="hk-direct"]')).toHaveCount(1);
  await expect(original.locator('.chain-card[data-order-id="hk-direct"]')).toHaveCount(0);
});

test('readonly operators have no group movement action', async ({ page }) => {
  await page.goto('/tests/fixtures/chain-unlock-preview.html?role=readonly&scene=calm#/chains/chain/apac/hk-direct');
  await expect(page.getByRole('heading', { level: 1, name: '香港 直出' })).toBeVisible();
  await expect(page.getByRole('button', { name: '移动到分组', exact: true })).toHaveCount(0);
});

test('old detail links remain usable after a move and browser back does not redirect again', async ({ page }) => {
  await page.goto('/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm#/chains');
  await page.locator('.chain-card[data-order-id="hk-direct"]').click();
  await page.getByRole('button', { name: '移动到分组', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '移动到分组', exact: true });
  await dialog.getByRole('combobox', { name: '目标分组' }).selectOption('cn2');
  await dialog.getByRole('button', { name: '保存到草稿', exact: true }).click();
  await expect(page).toHaveURL(/#\/chains\/chain\/cn2\/hk-direct$/);
  await page.goBack();
  await expect(page).toHaveURL(/#\/chains\/chain\/apac\/hk-direct$/);
  await expect(page.getByRole('heading', { level: 1, name: '香港 直出' })).toBeVisible();
  await expect(page.locator('.chain-detail-identity .nd-ident-meta')).toContainText('cn2');
});

test('a card can be dropped into an empty draft group', async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 2000 });
  await page.goto('/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm#/chains');
  await page.getByRole('button', { name: '＋ 新建分组', exact: true }).click();
  await page.getByPlaceholder('分组名称，比如：「三网优化」').fill('空分组');
  await page.getByRole('button', { name: '创建', exact: true }).click();
  const target = page
    .locator('[data-order-kind="app"]')
    .filter({ has: page.getByRole('button', { name: '空分组', exact: true }) });
  await expect(target).toBeVisible();
  const source = page.locator('.chain-card[data-order-id="hk-direct"]');
  const from = await source.boundingBox();
  const to = await target.boundingBox();
  await page.mouse.move(from!.x + 80, from!.y + 30);
  await page.mouse.down();
  await page.mouse.move(to!.x + 160, to!.y + 20, { steps: 12 });
  await page.mouse.up();
  await expect(target.locator('.chain-card[data-order-id="hk-direct"]')).toHaveCount(1);
});
