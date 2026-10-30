import { expect, test } from '@playwright/test';

for (const width of [1440, 390])
  test(`A list uses only the ordinary family ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto('/tests/fixtures/chain-unlock-preview.html?scene=calm&dns-fallback=1');
    await page.getByRole('group', { name: '线路视图' }).getByRole('button', { name: '解锁', exact: true }).click();
    const cell = page.locator('.cut-row [data-service="chatgpt"]').first();
    await expect(cell).toHaveAttribute('title', /常规流量仅 IPv4/);
    await expect(cell.locator('.cut-v6')).toHaveCount(0);
    await expect(page.locator('.cut-row').getByText(/仅 v[46]/)).toHaveCount(0);
    await expect(page).toHaveScreenshot(`a-dns-list-${width}.png`);
  });

for (const width of [1440, 390])
  for (const theme of ['dark', 'light']) {
    test(`A measured DNS fallback ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&dns-fallback=1&theme=${theme}`);
      const open = page.getByRole('button', { name: '解锁', exact: true });
      await open.click();
      const panel = page.getByRole('dialog', { name: /解锁/ });
      await panel.getByRole('button', { name: 'IPv6', exact: true }).click();
      const ai = panel.getByRole('article', { name: 'ChatGPT' });
      await expect(ai).toHaveAttribute('data-route', 'fallback');
      await expect(panel.locator('.unlock-dns-tag')).toHaveCount(3);
      await expect(panel.locator('.unlock-b-strip i[data-route]')).toHaveCount(3);
      await expect(panel.locator('.unlock-b-hero')).toContainText('/ 10 可用');
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`a-dns-${width}-${theme}.png`);
      const tag = ai.getByRole('button', { name: 'ChatGPT · 回退结果的解析路径' });
      await tag.click();
      const detail = page.getByRole('region', { name: 'ChatGPT · 回退结果的解析路径' });
      await expect(detail).toContainText('UseIP');
      await expect(detail).toContainText('本次实际连接的 DNS 解析与拨号');
      await expect(page).toHaveScreenshot(`a-dns-path-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(detail).toHaveCount(0);
      await expect(tag).toBeFocused();
      await panel.getByRole('button', { name: 'IPv4', exact: true }).click();
      await expect(panel.locator('.unlock-dns-tag')).toHaveCount(0);
      await expect(panel.getByRole('article')).toHaveCount(13);
    });
  }
test('A stop reason distinguishes a fixed family from an observed empty answer', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
  await page.setViewportSize({ width: 390, height: 1100 });
  await page.goto(
    '/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&dns-fallback=1&dns-stop=1&dns-fixed=1',
  );
  await page.getByRole('button', { name: '解锁', exact: true }).click();
  const panel = page.getByRole('dialog', { name: /解锁/ });
  await panel.getByRole('button', { name: 'IPv6', exact: true }).click();
  await expect(panel.getByRole('article', { name: 'ChatGPT' })).toContainText('DNS 策略只解析 IPv4');
  await expect(page).toHaveScreenshot('a-dns-stop-390.png');
});

for (const mixed of [false, true])
  test(`connection-bound YouTube paths ${mixed ? 'partial' : 'split'}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
    await page.setViewportSize({ width: mixed ? 390 : 1440, height: 1100 });
    await page.goto(
      `/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&connection-evidence=1${mixed ? '&connection-mixed=1' : ''}`,
    );
    await page.getByRole('button', { name: '解锁', exact: true }).click();
    const panel = page.getByRole('dialog', { name: /解锁/ });
    const webpage = panel.getByRole('article', { name: 'YouTube', exact: true });
    const premium = panel.getByRole('article', { name: 'YouTube Premium', exact: true });
    await expect(webpage).toHaveAttribute('data-route', 'fallback');
    if (mixed) {
      await expect(premium).not.toHaveAttribute('data-route');
      await premium.getByRole('button', { name: 'YouTube Premium · 部分回退的解析路径' }).click();
      const detail = page.getByRole('region', { name: 'YouTube Premium · 部分回退的解析路径' });
      await expect(detail).toContainText('保留可用计数');
      await expect(detail).toContainText('akile-mol1');
      await expect(detail).toContainText('exit-taiwan');
    } else {
      await expect(premium).toHaveAttribute('data-route', 'fallback');
      await webpage.getByRole('button', { name: 'YouTube 分项' }).click();
      const detail = page.getByRole('dialog', { name: 'YouTube 分项', exact: true });
      await expect(detail).toContainText('回退 · akile-mol1');
      await expect(detail).toContainText('常规解析 · exit-taiwan');
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`connection-youtube-${mixed ? 'partial-390' : 'split-1440'}.png`);
  });

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const)
  test(`unclassified DNS adds no badge or YouTube path text ${width} ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(
      `/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&dns-unclassified=1&theme=${theme}`,
    );
    await page.getByRole('button', { name: '解锁', exact: true }).click();
    const panel = page.getByRole('dialog', { name: /解锁/ });
    await panel.getByRole('button', { name: 'IPv6', exact: true }).click();
    await expect(panel.getByRole('article', { name: 'ChatGPT' })).toContainText('无 IPv6 地址解析');
    await expect(panel.locator('.unlock-dns-tag')).toHaveCount(0);
    await expect(page.getByText('证据不足', { exact: false })).toHaveCount(0);
    await panel
      .getByRole('article', { name: 'YouTube', exact: true })
      .getByRole('button', { name: 'YouTube 分项' })
      .click();
    const detail = page.getByRole('dialog', { name: 'YouTube 分项', exact: true });
    await expect(detail).toContainText('无 IPv6 地址解析');
    await expect(detail.getByText('证据不足', { exact: false })).toHaveCount(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`dns-unclassified-${width}-${theme}.png`);
  });

test('YouTube CN region restrictions use the same notice as MO', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto('/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&youtube-region=CN&theme=light');
  await page.getByRole('button', { name: '解锁', exact: true }).click();
  const panel = page.getByRole('dialog', { name: /解锁/ });
  for (const name of ['YouTube', 'YouTube Premium']) {
    const tile = panel.getByRole('article', { name, exact: true });
    await expect(tile).toHaveAttribute('data-tone', 'info');
    await expect(tile).toContainText('提示 · 地区不支持');
    await expect(tile).toContainText('CN');
  }
  await expect(page).toHaveScreenshot('youtube-cn-region-notice-1440-light.png');
});
