import { expect, test, type Page, type Locator } from '@playwright/test';
async function open(page: Page, scene: string, width: number, theme = 'light', name = 'long') {
  await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
  await page.setViewportSize({ width, height: width === 1440 ? 1000 : 568 });
  await page.goto(`/tests/fixtures/followup-layout-preview.html?scene=${scene}&theme=${theme}&name=${name}`);
  await page.evaluate(() => document.fonts.ready);
}
for (const [width, name, theme] of [
  [320, 'long', 'light'],
  [320, 'ascii', 'dark'],
  [390, 'long', 'light'],
  [1440, 'long', 'dark'],
] as const) {
  test(`installation actions fit ${width}px with ${name} machine name`, async ({ page }) => {
    await open(page, 'install', width, theme, name);
    const button = page.getByRole('button', { name: /查看 .* 的机器详情/ });
    await expect(button).toHaveText('查看机器');
    await button.scrollIntoViewIfNeeded();
    for (const action of await page.locator('.pv-foot button').all()) {
      const r = await action.boundingBox();
      expect(r!.x).toBeGreaterThanOrEqual(0);
      expect(r!.x + r!.width).toBeLessThanOrEqual(width);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    expect(await page.evaluate(() => innerWidth)).toBe(width);
    await expect(page.locator('.pv-foot')).toHaveScreenshot(`install-${width}-${name}.png`);
    await button.click();
    await expect(page.getByRole('status')).toHaveText('{"p":"node","id":"node-0"}');
  });
}
test('Mux clearing and sequential typing preserve 64→12 and 1→0 through Apply', async ({ page }) => {
  await open(page, 'mux', 390, 'light');
  const configure = page.getByRole('button', { name: '配置参数', exact: true });
  await configure.click();
  const drawer = page.locator('.mux-config-drawer');
  const concurrency = drawer.getByRole('spinbutton', { name: '复用流数量', exact: true });
  const prewarm = drawer.getByRole('spinbutton', { name: '预热目标', exact: true });
  const apply = drawer.getByRole('button', { name: '应用', exact: true });
  await concurrency.press('ControlOrMeta+A');
  await concurrency.press('Backspace');
  await expect(concurrency).toHaveValue('');
  await expect(apply).toBeDisabled();
  await expect(drawer).toHaveScreenshot('mux-empty.png');
  await concurrency.pressSequentially('12');
  await expect(concurrency).toHaveValue('12');
  await prewarm.press('ControlOrMeta+A');
  await prewarm.press('Backspace');
  await expect(prewarm).toHaveValue('');
  await expect(apply).toBeDisabled();
  await prewarm.pressSequentially('0');
  await expect(prewarm).toHaveValue('0');
  await expect(apply).toBeEnabled();
  await expect(drawer).toHaveScreenshot('mux-valid-retyped.png');
  await apply.click();
  await expect(drawer).toHaveCount(0);
  await configure.click();
  await expect(concurrency).toHaveValue('12');
  await expect(prewarm).toHaveValue('0');
  await concurrency.fill('');
  await concurrency.pressSequentially('129');
  await expect(drawer).toContainText('复用流数量必须在 1–128 之间');
  await expect(apply).toBeDisabled();
  await concurrency.fill('12');
  await expect(apply).toBeEnabled();
});
for (const [width, name, theme] of [
  [320, 'short', 'light'],
  [390, 'short', 'dark'],
  [390, 'long', 'light'],
  [320, 'ascii', 'light'],
  [1440, 'short', 'light'],
] as const) {
  test(`proxy impact prose fits ${width}px with ${name} machine name`, async ({ page }) => {
    await open(page, 'proxy', width, theme, name);
    const card = page.locator('.external-outbound-summary');
    const impact = card.locator('.external-summary-impact');
    await expect(impact).toContainText('修改会重启引用它的内部节点上的 XRAY。');
    await impact.scrollIntoViewIfNeeded();
    expect(await impact.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    const copy = impact.locator('.external-summary-impact-copy');
    const rects = await copy.evaluate(el => {
      const range = document.createRange();
      range.selectNodeContents(el);
      return [...range.getClientRects()].map(r => ({ left: r.left, right: r.right, bottom: r.bottom }));
    });
    const bounds = await card.boundingBox();
    for (const r of rects) {
      expect(r.left).toBeGreaterThanOrEqual(bounds!.x);
      expect(r.right).toBeLessThanOrEqual(bounds!.x + bounds!.width);
      expect(r.bottom).toBeLessThanOrEqual(bounds!.y + bounds!.height);
    }
    await expect(card).toHaveScreenshot(`proxy-impact-${width}-${name}.png`);
  });
}
async function readableHeader(header: Locator) {
  const title = header.getByRole('heading');
  await expect(title).toBeVisible();
  expect((await title.boundingBox())!.height).toBeLessThanOrEqual(24);
  expect(await header.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  const bounds = await header.boundingBox();
  const textRects = await header.evaluate(el => {
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    const rects: { left: number; right: number }[] = [];
    let text: Node | null;
    while ((text = walker.nextNode())) {
      if (!text.textContent?.trim()) continue;
      const range = document.createRange();
      range.selectNode(text);
      rects.push(...[...range.getClientRects()].map(r => ({ left: r.left, right: r.right })));
    }
    return rects;
  });
  for (const r of textRects) {
    expect(r.left).toBeGreaterThanOrEqual(bounds!.x);
    expect(r.right).toBeLessThanOrEqual(bounds!.x + bounds!.width + 1);
  }
}
for (const width of [320, 390, 1440])
  for (const scene of ['catalog', 'artifacts', 'plan']) {
    test(`${scene} card header keeps one row and adapts statistics at ${width}px`, async ({ page }) => {
      await open(page, scene, width, width === 390 ? 'dark' : 'light');
      if (scene === 'catalog') {
        const runtime = page.locator('.vpngate-usage-section');
        await expect(runtime).toBeVisible();
        const containers = await runtime.evaluate(el => getComputedStyle(el).containerName.split(' '));
        expect(containers).toContain('cg-section');
        expect(containers).toContain('vpngate-runtime');
        await page.getByRole('tab', { name: '目录采集' }).click();
      }
      const title = scene === 'catalog' ? 'VPN Gate 目录同步' : scene === 'artifacts' ? '产物记录' : '执行计划';
      const header = page
        .locator('.cg-sec > :is(header,summary)')
        .filter({ has: page.getByRole('heading', { name: title, exact: true }) });
      await readableHeader(header);
      if (width <= 620) {
        await expect(header.locator('.cg-meta')).toBeHidden();
        expect((await header.boundingBox())!.height).toBeLessThanOrEqual(36);
      } else await expect(header.locator('.cg-meta')).toBeVisible();
      await header.evaluate(el => el.scrollIntoView({ block: 'center' }));
      if (scene === 'catalog') await expect(header).toContainText('本轮首次发现 128');
      if (scene === 'artifacts') await expect(header).toContainText('121 份文件');
      if (scene === 'plan') await expect(header).toContainText('26 步需确认');
      const main = page.locator('.cg-main');
      if (await main.count())
        expect(await main.first().evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
      await expect(header).toHaveScreenshot(`header-${scene}-${width}.png`);
      if (scene === 'artifacts') {
        await header.click();
        await readableHeader(header);
        await expect(header).toHaveScreenshot(`header-artifacts-${width}-collapsed.png`);
        await header.click();
        await expect(page.locator('.cga-machines, .cga-list')).toHaveCount(1);
      }
    });
  }
