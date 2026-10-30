import { expect, test, type Page } from '@playwright/test';

async function expectDnsOrder(page: Page, values: string[]) {
  const inputs = page.locator('.node-egress-rules-table .rule-new-dns-value');
  await expect(inputs).toHaveCount(values.length);
  for (let index = 0; index < values.length; index++) await expect(inputs.nth(index)).toHaveValue(values[index]);
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`DNS group input ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/dns-groups-preview.html?theme=${theme}`);
      const address = page.getByRole('textbox', { name: /DNS 地址/ });
      await expect(address).toHaveValue('192.0.2.53, 198.51.100.53, 2001:db8::53');
      await page.evaluate(() => document.fonts.ready);
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
      await expect(page).toHaveScreenshot(`dns-group-${width}-${theme}.png`);
      await address.fill('192.0.2.1, 192.0.2.2, 192.0.2.3, 192.0.2.4');
      await expect(page.getByRole('alert')).toHaveCount(0);
      await page.getByRole('button', { name: '保存到草稿' }).click();
      await expect(page.getByRole('alert')).toHaveText('每组最多 3 个 DNS 地址');
      await expect(page.getByRole('alert')).toHaveCSS('white-space', 'nowrap');
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
      await expect(page).toHaveScreenshot(`dns-group-invalid-${width}-${theme}.png`);
      await address.fill('192.0.2.53, 2001:db8::53');
      await expect(page.getByRole('alert')).toHaveCount(0);
      await expect(page.getByRole('button', { name: '保存到草稿' })).toBeEnabled();
      await page.getByRole('button', { name: 'ChatGPT · 回退待确认的解析路径', exact: true }).click();
      const path = page.getByRole('region', { name: 'ChatGPT · 回退待确认的解析路径', exact: true });
      await expect(path).toContainText('采用 · 198.51.100.53');
      await expect(path).toContainText('5 次 · 4000ms');
      await expect(path).toHaveScreenshot(`dns-group-path-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(path).toHaveCount(0);
    });
  }

for (const theme of ['light', 'dark']) {
  test(`new DNS policy ${theme}`, async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 900 });
    await page.goto(`/tests/fixtures/dns-groups-preview.html?theme=${theme}&scene=create`);
    await page.getByRole('button', { name: '添加新策略' }).click();
    const row = page.locator('.node-egress-dns-new-row');
    await expect(row.getByRole('combobox', { name: /DNS 匹配类型/ })).toHaveValue('geosite');
    await expect(page.getByRole('alert')).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`dns-new-${theme}.png`);
    await row.getByRole('textbox', { name: /DNS 匹配内容/ }).fill('netflix');
    await page.getByRole('button', { name: '保存到草稿' }).click();
    const error = row.getByRole('alert');
    await expect(error).toHaveText('请填写完整的 DNS 地址，用逗号分隔');
    await expect(error).toHaveCSS('white-space', 'nowrap');
    const errorBox = await error.boundingBox();
    const fallbackBox = await row.getByRole('combobox', { name: /失败处理/ }).boundingBox();
    expect(errorBox!.x).toBeGreaterThanOrEqual(fallbackBox!.x + fallbackBox!.width);
    expect(Math.abs(errorBox!.y + errorBox!.height / 2 - fallbackBox!.y - fallbackBox!.height / 2)).toBeLessThan(2);
    await expect(page).toHaveScreenshot(`dns-new-invalid-${theme}.png`);
  });
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`DNS editing and import ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/dns-groups-preview.html?theme=${theme}&scene=editing`);
      await expect(page.getByRole('button', { name: /复制 DNS 策略/ })).toHaveCount(3);
      await expect(page.getByRole('button', { name: /拖动排序 DNS 策略/ })).toHaveCount(3);
      await page.evaluate(() => document.fonts.ready);
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
      await expect(page).toHaveScreenshot(`dns-editing-${width}-${theme}.png`, { fullPage: true });

      await page.getByRole('button', { name: '从节点导入', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '从节点导入 DNS 策略' });
      await dialog.getByLabel('来源节点').selectOption('preview-source');
      await expect(dialog).toContainText('将导入 2 条策略，按原顺序追加到当前列表末尾。');
      await expect(dialog).toHaveScreenshot(`dns-import-${width}-${theme}.png`);
      await dialog.getByRole('button', { name: '追加导入', exact: true }).click();
      await expect(dialog).toHaveCount(0);
      await expect(page.getByRole('alert')).toHaveCount(0);
      await expectDnsOrder(page, ['category-ai-!cn', 'disney', 'google', 'netflix', 'category-ai-!cn']);
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    });
  }

test('copies local edits, drags the copy to D1 and preserves all resolver fields', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/tests/fixtures/dns-groups-preview.html?theme=dark&scene=editing');
  await page
    .getByRole('textbox', { name: 'DNS 地址（geosite disney）', exact: true })
    .fill('203.0.113.53, 2001:db8::53');
  await page.getByRole('button', { name: '复制 DNS 策略（geosite disney）', exact: true }).click();
  const copy = page.locator('.node-egress-dns-new-row');
  await copy.getByRole('textbox', { name: /DNS 匹配内容/ }).fill('dazn');
  await expect(copy.getByRole('textbox', { name: /DNS 地址/ })).toHaveValue('203.0.113.53, 2001:db8::53');
  await expect(copy.getByRole('combobox', { name: /传输/ })).toHaveValue('udp');
  await expect(copy.getByRole('combobox', { name: /地址策略/ })).toHaveValue('use_ip');
  await expect(copy.getByRole('combobox', { name: /失败处理/ })).toHaveValue('machine');

  const grip = await copy.getByRole('button', { name: /拖动排序/ }).boundingBox();
  const target = await page.locator('.node-egress-rules-table tbody tr').first().boundingBox();
  await page.mouse.move(grip!.x + grip!.width / 2, grip!.y + grip!.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip!.x + grip!.width / 2, target!.y + target!.height / 2, { steps: 8 });
  await expect(page.locator('.dns-order-before')).toHaveCount(1);
  await expectDnsOrder(page, ['category-ai-!cn', 'disney', 'dazn', 'google']);
  await expect(page).toHaveScreenshot('dns-drag-target.png');
  await page.mouse.up();
  await expectDnsOrder(page, ['dazn', 'category-ai-!cn', 'disney', 'google']);
  await expect(page.getByRole('textbox', { name: 'DNS 地址（geosite disney）', exact: true })).toHaveValue(
    '203.0.113.53, 2001:db8::53',
  );
  await expect(page.getByRole('combobox', { name: '地址策略（geosite google）', exact: true })).toHaveValue('use_ipv6');
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeEnabled();
});

test('cancels a pointer reorder without losing an edited DNS value', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/tests/fixtures/dns-groups-preview.html?scene=editing');
  await page.getByRole('textbox', { name: 'DNS 地址（geosite disney）', exact: true }).fill('203.0.113.53');
  const grip = await page
    .getByRole('button', { name: '拖动排序 DNS 策略（geosite google）', exact: true })
    .boundingBox();
  const target = await page.locator('.node-egress-rules-table tbody tr').first().boundingBox();
  await page.mouse.move(grip!.x + grip!.width / 2, grip!.y + grip!.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip!.x + grip!.width / 2, target!.y + target!.height / 2, { steps: 8 });
  await page.keyboard.press('Escape');
  await page.mouse.up();
  await expect(page.locator('.dns-order-dragging')).toHaveCount(0);
  await expectDnsOrder(page, ['category-ai-!cn', 'disney', 'google']);
  await expect(page.getByRole('textbox', { name: 'DNS 地址（geosite disney）', exact: true })).toHaveValue(
    '203.0.113.53',
  );
});

test('supports real touch dragging from the dedicated grip', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Chromium provides the touch event injection used by this gesture test');
  await page.setViewportSize({ width: 390, height: 1100 });
  await page.goto('/tests/fixtures/dns-groups-preview.html?scene=editing');
  const handle = page.getByRole('button', { name: '拖动排序 DNS 策略（geosite disney）', exact: true });
  await expect(handle).toHaveCSS('touch-action', 'none');
  const grip = await handle.boundingBox();
  const target = await page
    .getByRole('button', { name: '拖动排序 DNS 策略（geosite category-ai-!cn）', exact: true })
    .boundingBox();
  const touch = await page.context().newCDPSession(page);
  const x = Math.round(grip!.x + grip!.width / 2);
  const fromY = Math.round(grip!.y + grip!.height / 2);
  const toY = Math.round(target!.y + target!.height / 2);
  await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: fromY }] });
  for (let step = 1; step <= 8; step++)
    await touch.send('Input.dispatchTouchEvent', {
      type: 'touchMove',
      touchPoints: [{ x, y: Math.round(fromY + ((toY - fromY) * step) / 8) }],
    });
  await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await expectDnsOrder(page, ['disney', 'category-ai-!cn', 'google']);
  await expect(page.getByRole('textbox', { name: 'DNS 地址（geosite disney）', exact: true })).toHaveValue(
    '192.0.2.53, 198.51.100.53, 2001:db8::53',
  );
  await touch.detach();
});

test('scrolls the rules container while dragging to a row beyond the viewport', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await page.goto('/tests/fixtures/dns-groups-preview.html?scene=editing');
  const handle = await page
    .getByRole('button', { name: '拖动排序 DNS 策略（geosite category-ai-!cn）', exact: true })
    .boundingBox();
  await page.mouse.move(handle!.x + handle!.width / 2, handle!.y + handle!.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle!.x + handle!.width / 2, 790, { steps: 8 });
  await expect
    .poll(() => page.locator('[data-dns-preview-scroll]').evaluate(element => element.scrollTop))
    .toBeGreaterThan(400);
  await page.mouse.up();
  await expectDnsOrder(page, ['disney', 'google', 'category-ai-!cn']);
});
