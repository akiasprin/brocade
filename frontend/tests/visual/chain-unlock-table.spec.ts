import { test, expect, type Page } from '@playwright/test';

async function preview(page: Page, width: number, theme = 'dark', query = '') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/chain-unlock-preview.html?theme=${theme}&${query}#/chains`);
  await expect(page.getByRole('table', { name: /线路服务解锁/ })).toBeVisible();
  await expect(page.locator('.cut-row')).toHaveCount(16);
  if (!query.includes('error')) await expect(page.locator('td[data-service="chatgpt"]').first()).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

async function expectRegionAlignment(page: Page) {
  const readings = await page.locator('.cut-row [data-service="gemini"]').evaluateAll(cells =>
    cells.flatMap(cell => {
      const primary = cell.querySelector('.cut-regions > span');
      const glyph = cell.querySelector('.unlock-glyph');
      if (!primary || !glyph || !/^[A-Z]{2}$/.test(primary.textContent ?? '')) return [];
      const bounds = cell.getBoundingClientRect();
      const mark = glyph.getBoundingClientRect();
      const label = primary.getBoundingClientRect();
      return [
        {
          x: mark.x - bounds.x,
          y: mark.y - bounds.y,
          labelX: label.x - bounds.x,
          labelY: label.y - bounds.y,
          secondary: cell.querySelector('.cut-v6')?.textContent,
        },
      ];
    }),
  );
  const single = readings.find(reading => !reading.secondary)!;
  const dual = readings.find(reading => reading.secondary === 'v6 US')!;
  expect(single).toBeTruthy();
  expect(dual).toBeTruthy();
  for (const property of ['x', 'y', 'labelX', 'labelY'] as const)
    expect(dual[property]).toBeCloseTo(single[property], 1);
}

for (const width of [1440, 390])
  for (const theme of ['dark', 'light']) {
    test(`production D ${width} ${theme}`, async ({ page }) => {
      const requests: string[] = [];
      page.on('request', request => {
        if (request.url().includes('unlock-probe')) requests.push(request.url());
      });
      const errors = await preview(page, width, theme);
      await expect(page.getByRole('columnheader', { name: 'YouTube Premium' })).toBeVisible();
      await expect(page.locator('.cut-row .cut-when').getByText('检测中', { exact: true })).toHaveCount(1);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(requests.length).toBe(0); // Fixture intercepts fetch; no real diagnostics or external requests.
      await expectRegionAlignment(page);
      await expect(page.locator('.cut-row').getByText(/仅 v[46]/)).toHaveCount(0);
      await expect(page).toHaveScreenshot(`d-${width}-${theme}.png`);
      const scroll = page.getByRole('region', { name: /线路解锁对照表/ });
      if (width === 390) {
        const before = await page.locator('.cut-chain').first().boundingBox();
        await scroll.evaluate(element => {
          element.scrollLeft = element.scrollWidth;
        });
        const after = await page.locator('.cut-chain').first().boundingBox();
        expect(after!.x).toBeCloseTo(before!.x, 0);
        await expect(page).toHaveScreenshot(`d-${width}-${theme}-end.png`);
      }
      expect(errors).toEqual([]);
    });
  }

test('aggregates both families, no-AAAA and regions without a family switch; detail keeps its family controls', async ({
  page,
}) => {
  const errors = await preview(page, 1440, 'dark', 'scene=calm&pages=1');
  await expect(page.getByText('读取中…', { exact: true })).toHaveCount(0);
  await expect(page.locator('td[data-service="chatgpt"]')).toHaveCount(15);
  await expect(page.getByRole('group', { name: '解锁地址族' })).toHaveCount(0);
  const tokyo = page.locator('.cut-row').filter({ has: page.getByRole('button', { name: /东京 IIJ/ }) });
  await expect(tokyo.locator('[data-service="chatgpt"]')).toHaveAttribute('data-tone', 'part');
  await expect(tokyo.locator('[data-service="gemini"]')).toContainText('JP');
  await expect(tokyo.locator('[data-service="gemini"]')).toContainText('v6 US');
  const taipei = page.locator('.cut-row').filter({ has: page.getByRole('button', { name: /台北 HiNet/ }) });
  await expect(taipei.locator('[data-service="chatgpt"]')).toHaveAttribute('data-tone', 'ok');
  await expect(taipei.locator('[data-service="chatgpt"]')).toHaveAttribute('title', /IPv6 无解析地址，采用 IPv4 结果/);
  await expect(taipei.locator('[data-service="netflix"]')).toContainText('TW');
  await expect(taipei.locator('[data-service="netflix"]')).toContainText('v6 JP');
  await expect(
    page
      .locator('.cut-row')
      .filter({ has: page.getByRole('button', { name: /圣何塞/ }) })
      .getByText('不分族'),
  ).toBeVisible();
  await expect(page).toHaveScreenshot('d-dual-stack-aggregate.png');
  await page.getByRole('group', { name: '线路视图' }).getByRole('button', { name: '卡片' }).click();
  await expect(page.locator('.chain-card')).toHaveCount(16);
  await expect(page.getByRole('table')).toHaveCount(0);
  await page.getByRole('group', { name: '线路视图' }).getByRole('button', { name: '解锁' }).click();
  const first = page.locator('.cut-chain button').first();
  await first.focus();
  await page.keyboard.press('Enter');
  await page.getByRole('button', { name: '解锁', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: /区域性服务解锁状态/ });
  await expect(dialog.getByRole('article')).toHaveCount(13);
  await expect(dialog.getByRole('group', { name: '解锁地址族' })).toBeVisible();
  await expect(page).toHaveScreenshot('d-original-detail.png');
  expect(errors).toEqual([]);
});

for (const role of ['readonly', 'public'])
  test(`readable ${role} and errors remain explicit`, async ({ page }) => {
    const errors = await preview(page, 390, 'light', `role=${role}&error=1`);
    await expect(page.getByText('摘要暂不可用')).toBeVisible();
    await expect(page.getByRole('button', { name: '重试' })).toBeVisible();
    await expect(page.getByRole('button', { name: '多选', exact: true })).toBeDisabled();
    await expect(page.getByRole('button', { name: '＋ 新建分组', exact: true })).toBeDisabled();
    await expect(page).toHaveScreenshot(`d-error-${role}.png`);
    expect(errors).toEqual([]);
  });

for (const width of [1440, 390])
  test(`card view stays usable ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto('/tests/fixtures/chain-unlock-preview.html?view=cards#/chains');
    await expect(page.locator('.chain-card')).toHaveCount(16);
    await expect(page.getByRole('table')).toHaveCount(0);
    await expect(page.getByRole('group', { name: '线路视图' }).getByRole('button', { name: '卡片' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`d-cards-${width}.png`);
  });

test('WebKit mobile table keeps fixed names and complete headings', async ({ playwright, baseURL }) => {
  const browser = await playwright.webkit.launch();
  try {
    const context = await browser.newContext({
      baseURL,
      reducedMotion: 'reduce',
      locale: 'zh-CN',
      timezoneId: 'Asia/Taipei',
    });
    const page = await context.newPage();
    const errors = await preview(page, 390, 'light');
    const scroll = page.getByRole('region', { name: /线路解锁对照表/ });
    const before = await page.locator('.cut-chain').first().boundingBox();
    await scroll.evaluate(element => {
      element.scrollLeft = element.scrollWidth;
    });
    const after = await page.locator('.cut-chain').first().boundingBox();
    expect(after!.x).toBeCloseTo(before!.x, 0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot('d-webkit-mobile-end.png');
    expect(errors).toEqual([]);
  } finally {
    await browser.close();
  }
});

for (const [width, theme] of [[1440, 'dark'], [390, 'light']] as const)
  test(`CN and MO Premium keep the same notice when IPv6 fails ${width} ${theme}`, async ({ page }) => {
    const errors = await preview(page, width, theme, 'premium-region-notice=1');
    const rows = [
      page.locator('.cut-row').filter({ has: page.getByRole('button', { name: /香港 直出/ }) }),
      page.locator('.cut-row').filter({ has: page.getByRole('button', { name: /东京 IIJ/ }) }),
    ];
    const scroll = page.getByRole('region', { name: /线路解锁对照表/ });
    await scroll.evaluate(element => { element.scrollLeft = 250; });
    for (const [index, row] of rows.entries()) {
      const premium = row.locator('[data-service="youtube_premium"]');
      await expect(premium).toHaveAttribute('data-tone', 'info');
      await expect(premium).toContainText(index === 0 ? 'CN' : 'MO');
      await expect(premium).toHaveAttribute('title', /IPv4：提示 · 地区不支持/);
      await expect(premium).toHaveAttribute('title', /IPv6：检测失败.*网络连接失败/);
      await expect(row.locator('[data-service="youtube"]')).toHaveAttribute('data-tone', 'part');
    }
    await expect(page).toHaveScreenshot(`premium-region-notice-${width}-${theme}.png`);
    expect(errors).toEqual([]);
  });
