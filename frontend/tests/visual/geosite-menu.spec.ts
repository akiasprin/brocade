import { expect, test, type Page } from '@playwright/test';
import { waitForVisualAssets } from './helpers/visual-ready';
import { geoCatalog } from '../fixtures/geodata-data';

for (const width of [390, 820, 1440]) {
  for (const theme of ['light', 'dark']) {
    test(`geosite catalogue loading keeps rule layout stable ${width} ${theme}`, async ({ page }) => {
      let finish!: () => void;
      const ready = new Promise<void>(resolve => {
        finish = resolve;
      });
      await page.route('**/routing/geodata/catalog?*', async route => {
        await ready;
        await route.fulfill({ json: geoCatalog('geosite') });
      });
      await page.setViewportSize({ width, height: 900 });
      try {
        await page.goto(`/tests/fixtures/dns-groups-preview.html?scene=editing&catalog=delayed&theme=${theme}`);
        const table = page.locator('.node-egress-rules-table');
        const fields = table.locator('.geo-select');
        await expect(fields).toHaveCount(3);
        await expect(fields.first()).toHaveAttribute('aria-busy', 'true');
        await expect(table.locator('.geo-message')).toHaveCount(0);
        await waitForVisualAssets(page);
        const bounds = () =>
          table.locator('tbody > tr').evaluateAll(rows =>
            rows.map(row => {
              const { x, y, width, height } = row.getBoundingClientRect();
              return { x, y, width, height };
            }),
          );
        const screenshot = `geosite-loading-stable-${width}-${theme}.png`;
        // 截图会结束页面入场动画，之后再取基准，避免把动画位移误判为加载造成的跳变。
        await expect(page).toHaveScreenshot(screenshot);
        const pending = await bounds();
        finish();
        await expect(fields.first()).toHaveAttribute('aria-busy', 'false');
        await expect(table.locator('.geo-message')).toHaveCount(0);
        expect(await bounds()).toEqual(pending);
        await expect(page).toHaveScreenshot(screenshot);
      } finally {
        finish();
      }
    });
  }
}

for (const { kind, invalid } of [
  { kind: 'geosite', invalid: true },
  { kind: 'geoip', invalid: false },
]) {
  test(`${kind} menu stays close to its input${invalid ? ' with a validation message' : ''}`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(
      `/tests/fixtures/dns-groups-preview.html?scene=editing&kind=${kind}&theme=dark${invalid ? '&invalid-category' : ''}`,
    );
    const row = page.locator('.node-egress-rules-table tbody tr').first();
    const field = kind === 'geoip' ? page.locator('.geo-field').first() : row.locator('.geo-field');
    await expect(field.locator('.geo-select')).toHaveAttribute('aria-busy', 'false');
    if (invalid) await expect(field.locator('.geo-message')).toHaveText(/missing-category 不存在/);
    await field.getByRole('combobox').click();
    const menu = page.getByRole('dialog', { name: `${kind} 选项`, exact: true });
    await expect(menu.getByRole('listbox', { name: kind, exact: true })).toBeVisible();
    await waitForVisualAssets(page);
    await expect(page).toHaveScreenshot(`${kind}-menu-anchor${invalid ? '-invalid' : ''}.png`);
    const anchor = (await field.locator('.geo-select').boundingBox())!;
    const panel = (await menu.locator('.geo-menu').boundingBox())!;
    expect(panel.y - anchor.y - anchor.height).toBeCloseTo(2, 0);
    expect(panel.x).toBeCloseTo(anchor.x, 0);
  });
}

// 1,564 类是线上 geosite.dat 的规模；夹具用合成分类补足数量。
async function openMenu(page: Page, width: number, height: number, theme: string) {
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/dns-groups-preview.html?scene=editing&catalog=long&theme=${theme}`);
  const trigger = page
    .locator('.node-egress-rules-table tbody tr')
    .first()
    .getByRole('combobox', { name: /DNS 匹配内容/ });
  await trigger.click();
  const menu = page.getByRole('dialog', { name: 'geosite 选项', exact: true });
  await expect(menu.getByRole('listbox', { name: 'geosite', exact: true })).toBeVisible();
  if (width > 820) await expect(menu.locator('.geo-menu-foot')).toContainText('1,564 类');
  await waitForVisualAssets(page);
  return { trigger, menu, surface: menu.locator('.geo-menu') };
}

for (const { width, height, theme } of [
  { width: 1440, height: 900, theme: 'light' },
  { width: 1440, height: 900, theme: 'dark' },
  { width: 390, height: 844, theme: 'light' },
  { width: 390, height: 844, theme: 'dark' },
  { width: 820, height: 420, theme: 'light' },
  { width: 2560, height: 1440, theme: 'light' },
]) {
  test(`grouped geosite menu ${width} ${height} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    const { trigger, menu, surface } = await openMenu(page, width, height, theme);
    const phone = width <= 820;
    await expect(menu.getByRole('button', { name: /^常用/, pressed: true })).toBeVisible();

    // 截图结束入场动画后再量锚点，避免把过渡中的字段高度当成最终布局。
    await expect(page).toHaveScreenshot(`geosite-menu-${width}-${height}-${theme}.png`);
    const panel = (await surface.boundingBox())!;
    if (phone) {
      const anchor = (await trigger.locator('xpath=ancestor::div[contains(@class,"geo-select")]').boundingBox())!;
      expect(panel.x).toBeGreaterThanOrEqual(8);
      expect(panel.width).toBeCloseTo(Math.min(440, width - 16), 0);
      expect(panel.height).toBeLessThanOrEqual(321);
      expect(panel.y).toBeGreaterThanOrEqual(8);
      expect(panel.y + panel.height).toBeLessThanOrEqual(height - 7);
      const gap =
        panel.y >= anchor.y + anchor.height ? panel.y - anchor.y - anchor.height : anchor.y - panel.y - panel.height;
      expect(gap).toBeCloseTo(2, 0);
      await expect(menu.getByRole('combobox')).toHaveCount(0);
      await expect(menu.locator('.dialog-scrim')).toHaveCount(0);
    } else {
      expect(panel.height).toBeLessThanOrEqual(Math.min(520, height - 16) + 1);
      expect(panel.y).toBeGreaterThanOrEqual(8);
      expect(panel.y + panel.height).toBeLessThanOrEqual(height - 7);
      expect(panel.x).toBeGreaterThanOrEqual(8);
      expect(panel.x + panel.width).toBeLessThanOrEqual(width - 7);
      const anchor = (await trigger.locator('xpath=ancestor::div[contains(@class,"geo-select")]').boundingBox())!;
      expect(Math.abs(panel.x - anchor.x)).toBeLessThan(2);
      expect(panel.y - anchor.y - anchor.height).toBeCloseTo(2, 0);
    }
    const footer = phone ? null : (await menu.locator('.geo-menu-foot').boundingBox())!;
    if (footer) expect(Math.abs(footer.y + footer.height - panel.y - panel.height)).toBeLessThan(2);

    // 仅结果区滚动，字段和分组保持原位。
    const list = menu.getByRole('listbox', { name: 'geosite', exact: true });
    await menu.getByRole('button', { name: /^A–Z/ }).click();
    await menu.getByRole('button', { name: '首字母 S' }).click();
    const search = phone ? trigger : menu.getByRole('combobox', { name: '搜索 geosite', exact: true });
    const headerBefore = (await search.boundingBox())!;
    await list.hover();
    await page.mouse.wheel(0, 1200);
    await expect.poll(() => list.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    expect((await search.boundingBox())!.y).toBeCloseTo(headerBefore.y, 0);
    if (footer) expect((await menu.locator('.geo-menu-foot').boundingBox())!.y).toBeCloseTo(footer.y, 0);
    expect(await surface.evaluate(element => element.scrollTop)).toBe(0);

    await search.fill('site-1540');
    await menu.getByRole('option', { name: 'site-1540', exact: true }).click();
    await expect(
      (phone ? page.locator('.geo-select.editing') : menu).getByRole('button', { name: '移除 site-1540' }),
    ).toBeVisible();
    await expect(search).toHaveValue('');
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await trigger.click();
    await menu.getByRole('button', { name: '查看条目…' }).click();
    await expect(page.getByRole('dialog', { name: '规则集预览', exact: true })).toBeVisible();
    const fits = await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
    expect(fits).toBe(true);
    expect(errors).toEqual([]);
  });
}

for (const { width, height, theme } of [
  { width: 1440, height: 900, theme: 'light' },
  { width: 1440, height: 900, theme: 'dark' },
  { width: 390, height: 844, theme: 'light' },
]) {
  test(`geosite menu search, topics and domain lookup ${width} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    const { menu, trigger } = await openMenu(page, width, height, theme);
    const search = width <= 820 ? trigger : menu.getByRole('combobox', { name: '搜索 geosite', exact: true });

    await search.fill('cn');
    await expect(menu.getByRole('option').first()).toHaveAttribute('aria-label', 'cn');
    await expect(menu.getByRole('button', { name: /^全部结果/, pressed: true })).toBeVisible();
    await expect(page).toHaveScreenshot(`geosite-search-${width}-${theme}.png`);

    await search.fill('');
    await menu.getByRole('button', { name: /^AI/ }).click();
    await expect(menu.getByText('category-ai', { exact: true })).toBeVisible();
    await expect(page).toHaveScreenshot(`geosite-topic-${width}-${theme}.png`);

    await search.fill('chat.openai.com');
    await expect(menu.getByText('包含 chat.openai.com 的分类')).toBeVisible();
    await expect(menu.getByText('最精确')).toBeVisible();
    await expect(page).toHaveScreenshot(`geosite-lookup-${width}-${theme}.png`);
    expect(errors).toEqual([]);
  });
}

test('geosite service brands and unmapped document glyph', async ({ page }) => {
  const { menu } = await openMenu(page, 1440, 900, 'light');
  const search = menu.getByRole('combobox', { name: '搜索 geosite', exact: true });
  for (const code of ['cloudflare', 'amazon', 'verisign']) {
    await search.fill(code);
    const option = menu.getByRole('option', { name: code, exact: true });
    await expect(option).toBeVisible();
    const row = option.locator('xpath=..');
    if (code === 'verisign') {
      await expect(row.locator('[data-geo-glyph="document"]')).toBeVisible();
    } else {
      await expect(row.locator(`[data-site-icon="${code}"] > svg`)).toBeVisible();
    }
    await expect(row).toHaveScreenshot(`geosite-service-${code}.png`);
  }
});

for (const width of [320, 430]) {
  for (const theme of ['light', 'dark']) {
    test(`compact geosite field ${width} ${theme}`, async ({ page }) => {
      const { trigger, menu, surface } = await openMenu(page, width, 568, theme);
      await trigger.fill('google');
      await expect(trigger).toBeFocused();
      await menu.getByRole('button', { name: /google 的 .* 个属性/ }).click();
      await expect(trigger).toBeFocused();
      await menu.getByRole('option', { name: 'google@ads', exact: true }).click();
      await expect(trigger).toHaveValue('google');
      await expect(trigger).toBeFocused();
      await expect(page).toHaveScreenshot(`geosite-compact-attributes-${width}-${theme}.png`);
      // 模拟键盘压缩后的可见区域，浮卡仍贴附字段并完整落在其中。
      await page.evaluate(() => {
        const viewport = window.visualViewport!;
        Object.defineProperty(viewport, 'height', { configurable: true, get: () => 340 });
        viewport.dispatchEvent(new Event('resize'));
      });
      await expect
        .poll(async () => {
          const box = (await surface.boundingBox())!;
          return box.y + box.height;
        })
        .toBeLessThanOrEqual(333);
      await page
        .locator('.geo-select.editing')
        .getByRole('button', { name: /查看全部/ })
        .click();
      await expect(menu.getByRole('group', { name: '本条已选', exact: true })).toBeVisible();
      await page.getByRole('button', { name: '完成 geosite 选择' }).click();
      await expect(menu).toHaveCount(0);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    });
  }
}

test('WebKit compact geosite keeps the original input usable when selecting and closing', async ({
  playwright,
  baseURL,
}) => {
  const browser = await playwright.webkit.launch();
  try {
    const page = await browser.newPage({ baseURL, isMobile: true, hasTouch: true, reducedMotion: 'reduce' });
    const { trigger, menu } = await openMenu(page, 390, 664, 'light');
    await trigger.fill('netflix');
    await expect(trigger).toBeFocused();
    await menu.getByRole('option', { name: 'netflix', exact: true }).click();
    await expect(trigger).toHaveValue('');
    await expect(trigger).toBeFocused();
    await expect(page).toHaveScreenshot('geosite-compact-webkit.png');
    await page.getByRole('button', { name: '完成 geosite 选择' }).click();
    await expect(menu).toHaveCount(0);
  } finally {
    await browser.close();
  }
});

// 规则集预览：从 D3 的芯片打开 google；头部一行、属性与类型带条目数、条目按类型分节多列。
for (const { width, height, theme } of [
  { width: 1440, height: 900, theme: 'light' },
  { width: 1440, height: 900, theme: 'dark' },
  { width: 390, height: 844, theme: 'light' },
]) {
  test(`geosite browser ${width} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.goto(`/tests/fixtures/dns-groups-preview.html?scene=editing&catalog=long&theme=${theme}`);
    await page.getByRole('button', { name: '预览 google', exact: true }).click();
    const browser = page.getByRole('dialog', { name: '规则集预览', exact: true });
    const surface = browser.locator('.geo-browser');
    await expect(browser.getByRole('heading', { name: 'google', exact: true })).toBeVisible();
    await expect(browser.getByText('已在 D3', { exact: true })).toBeVisible();
    await expect(browser.getByText('example-0.com', { exact: true })).toBeVisible();
    await expect(browser.getByRole('button', { name: '完整 20 条' })).toBeVisible();
    // 点芯片后指针停在弹窗内，移开以免悬停底色进入截图。
    await page.mouse.move(0, 0);
    await waitForVisualAssets(page);
    const box = (await surface.boundingBox())!;
    if (width <= 820) {
      // 与其他模态框一样四周留 10px（无安全区时）。
      expect(box.width).toBeCloseTo(width - 20, 0);
      expect(box.height).toBeCloseTo(height - 20, 0);
    } else {
      expect(box.height).toBeLessThanOrEqual(660 + 1);
      // 头部只有一行，条目区在 200px 内开始。
      expect((await browser.locator('.geo-browser-bar').boundingBox())!.height).toBeLessThanOrEqual(41);
      const list = (await browser.locator('.geo-browser-list').boundingBox())!;
      expect(list.y - box.y).toBeLessThan(200);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await expect(page).toHaveScreenshot(`geosite-browser-${width}-${theme}.png`);

    // 条目在列表内滚动，头部不动。
    const list = browser.locator('.geo-browser-list');
    const head = (await browser.locator('.geo-browser-title').boundingBox())!;
    await list.hover();
    await page.mouse.wheel(0, 600);
    await expect.poll(() => list.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    expect((await browser.locator('.geo-browser-title').boundingBox())!.y).toBeCloseTo(head.y, 0);

    const find = browser.getByRole('textbox', { name: '筛选条目或检查命中' });
    await find.fill('img.example-3.com');
    await expect(browser.getByRole('status')).toContainText('由 后缀example-3.com 匹配');
    await expect(browser.locator('.geo-browser-entry.hit')).toHaveText('example-3.com');
    if (theme === 'light') await expect(page).toHaveScreenshot(`geosite-browser-hit-${width}.png`);
    await find.fill('chat.openai.com');
    await expect(browser.getByRole('status')).toContainText('未命中');
    await expect(browser.getByRole('status').getByRole('button', { name: 'openai 6 条' })).toBeVisible();
    if (theme === 'light') await expect(page).toHaveScreenshot(`geosite-browser-miss-${width}.png`);

    if (width <= 820) {
      // 手机两级：返回分类列表，第一组是来源行已选的分类。
      await browser.getByRole('button', { name: '全部分类' }).click();
      await expect(browser.getByText('D3 已选', { exact: true })).toBeVisible();
      await expect(page).toHaveScreenshot(`geosite-browser-list-${width}-${theme}.png`);
      // 已用的分类在「其他规则已用」和「全部 A–Z」各出现一次，点前一个。
      await browser
        .getByRole('navigation', { name: '分类' })
        .getByRole('button', { name: 'category-ai-!cn 1,250 条' })
        .first()
        .click();
    } else {
      await browser.getByRole('textbox', { name: '搜索分类' }).fill('category-ai');
      await browser
        .getByRole('navigation', { name: '分类' })
        .getByRole('button', { name: 'category-ai-!cn 1,250 条' })
        .click();
    }
    await browser.getByRole('button', { name: '3 个分类' }).click();
    await expect(browser.getByRole('button', { name: 'huggingface 3 条' })).toBeVisible();
    if (theme === 'light') await expect(page).toHaveScreenshot(`geosite-browser-narrower-${width}.png`);
    expect(errors).toEqual([]);
  });
}
