import { expect, test, type Locator, type Page } from '@playwright/test';

const scenes = [
  { scene: 'catalog', title: 'VPN Gate 目录同步', statistic: '本轮首次发现 128' },
  { scene: 'artifacts', title: '产物记录', statistic: '121 份文件' },
  { scene: 'plan', title: '执行计划', statistic: '26 步需确认' },
  { scene: 'progress', title: '执行进度', statistic: '台完成' },
  { scene: 'regions', title: '地区', statistic: '1' },
  { scene: 'country', title: '日本出口池', statistic: 'JP' },
  { scene: 'probes', title: '拨测机器', statistic: '参与' },
] as const;

async function openCard(page: Page, scene: (typeof scenes)[number], theme: string) {
  const fixture = scene.scene === 'progress' ? 'ui-regressions' : 'followup-layout-preview';
  const route =
    scene.scene === 'progress'
      ? 'deploy'
      : ['regions', 'country', 'probes'].includes(scene.scene)
        ? 'catalog'
        : scene.scene;
  await page.goto(`/tests/fixtures/${fixture}.html?scene=${route}&theme=${theme}`);
  if (scene.scene === 'catalog' || scene.scene === 'probes') await page.getByRole('tab', { name: '目录采集' }).click();
  const header = page
    .locator('.cg-sec > :is(header, summary)')
    .filter({ has: page.getByRole('heading', { name: scene.title, exact: true }) });
  await expect(header).toContainText(scene.statistic);
  await page.evaluate(() => document.fonts.ready);
  return header;
}

async function expectReadable(header: Locator) {
  const title = header.getByRole('heading');
  await expect(title).toBeVisible();
  const metrics = await header.evaluate(element => {
    const bounds = element.getBoundingClientRect();
    const heading = element.querySelector('h4')!;
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    const rects: DOMRect[] = [];
    let node: Node | null;
    while ((node = walker.nextNode())) {
      if (!node.textContent?.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      rects.push(...range.getClientRects());
    }
    return {
      titleHeight: heading.getBoundingClientRect().height,
      titleLineHeight: Number.parseFloat(getComputedStyle(heading).lineHeight),
      overflow: element.scrollWidth - element.clientWidth,
      withinViewport: bounds.left >= -1 && bounds.right <= document.documentElement.clientWidth + 1,
      textWithinHeader: rects.every(
        rect =>
          rect.left >= bounds.left - 1 &&
          rect.right <= bounds.right + 1 &&
          rect.top >= bounds.top - 1 &&
          rect.bottom <= bounds.bottom + 1,
      ),
    };
  });
  expect(metrics.titleHeight).toBeLessThanOrEqual(metrics.titleLineHeight + 1);
  expect(metrics.overflow).toBeLessThanOrEqual(1);
  expect(metrics.withinViewport).toBe(true);
  expect(metrics.textWithinHeader).toBe(true);
}

for (const browserName of ['chromium', 'webkit'] as const) {
  test.describe(`${browserName} mobile shared card headers`, () => {
    for (const width of [320, 390]) {
      test(`titles stay on one row and statistics are hidden at ${width}px`, async ({ playwright, baseURL }) => {
        const browser = await playwright[browserName].launch();
        try {
          const context = await browser.newContext({
            baseURL,
            viewport: { width, height: 844 },
            isMobile: true,
            hasTouch: true,
            locale: 'zh-CN',
            timezoneId: 'Asia/Taipei',
            reducedMotion: 'reduce',
          });
          const page = await context.newPage();
          const errors: string[] = [];
          page.on('pageerror', error => errors.push(error.message));
          await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
          await page.setViewportSize({ width, height: 844 });
          for (const scene of scenes) {
            const header = await openCard(page, scene, width === 320 ? 'light' : 'dark');
            await expectReadable(header);
            if (scene.scene === 'country') await expect(header.locator('.cg-meta-identity')).toBeVisible();
            else await expect(header.locator('.cg-meta')).toBeHidden();
            expect((await header.boundingBox())!.height).toBeLessThanOrEqual(36);
            await expect(header).toHaveScreenshot(`${browserName}-${scene.scene}-${width}.png`);
            if (scene.scene === 'artifacts') {
              const disclosure = page.locator('.cg-artifact-review');
              await header.press('Enter');
              await expect(disclosure).toHaveJSProperty('open', false);
              await expectReadable(header);
              await expect(header).toHaveScreenshot(`${browserName}-artifacts-${width}-collapsed.png`);
              await header.press('Enter');
              await expect(disclosure).toHaveJSProperty('open', true);
              await expect(page.locator('.cga-machines, .cga-list')).toHaveCount(1);
            }
          }
          expect(errors).toEqual([]);
        } finally {
          await browser.close();
        }
      });
    }
  });
}

test('a narrow card uses the same layout inside a desktop viewport', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
  await page.setViewportSize({ width: 1440, height: 1000 });
  for (const scene of scenes) {
    const header = await openCard(page, scene, 'light');
    const card = header.locator('..');
    // Model a narrow window on a wide desktop without changing the product DOM or its data.
    await card.evaluate(element => {
      (element as HTMLElement).style.width = '280px';
    });
    await expectReadable(header);
    if (scene.scene === 'country') await expect(header.locator('.cg-meta-identity')).toBeVisible();
    else await expect(header.locator('.cg-meta')).toBeHidden();
    expect((await header.boundingBox())!.height).toBeLessThanOrEqual(36);
    await expect(header).toHaveScreenshot(`desktop-narrow-${scene.scene}.png`);
  }
});
