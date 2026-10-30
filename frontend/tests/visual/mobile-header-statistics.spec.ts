import { test, expect } from '@playwright/test';

const scenes = [
  {
    key: 'configuration',
    route: 'node-source-access-preview.html?scene=mixed',
    card: '.source-access-panel',
    header: 'summary',
    statistic: '.config-disclosure-summary',
    title: '来源 IP 封禁',
  },
  {
    key: 'users',
    route: 'user-header-preview.html?presence=sources',
    card: '.user-presence-card',
    header: 'header',
    statistic: '.rt',
    title: '在线设备',
  },
  {
    key: 'rules',
    route: 'node-source-access-preview.html',
    card: '.node-egress-rule-sheet',
    header: 'header',
    statistic: '.header-stat',
    title: 'DNS 解析策略',
  },
  {
    key: 'groups',
    route: 'chain-unlock-preview.html?view=cards&scene=calm',
    card: '.chain-section',
    header: '.chain-section-head',
    statistic: '.count',
    title: '',
  },
  {
    key: 'capacity',
    route: 'capacity-coverage-preview.html',
    card: '.cap-panel',
    header: 'header',
    statistic: '.cap-rd',
    title: '流量预测',
  },
] as const;

for (const [browserName, width] of [
  ['chromium', 390],
  ['chromium', 820],
  ['chromium', 1440],
  ['webkit', 390],
] as const) {
  test(`${browserName} header statistics at ${width}px`, async ({ playwright, baseURL }) => {
    const browser = await playwright[browserName].launch();
    try {
      const context = await browser.newContext({
        baseURL,
        viewport: { width, height: 1000 },
        isMobile: width <= 820,
        hasTouch: width <= 820,
        locale: 'zh-CN',
        timezoneId: 'Asia/Taipei',
        reducedMotion: 'reduce',
      });
      const page = await context.newPage();
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.install({ time: new Date('2026-10-29T08:00:00Z') });
      for (const scene of scenes) {
        await page.goto(`/tests/fixtures/${scene.route}`);
        if (scene.key === 'rules') await page.getByRole('tab', { name: '规则' }).click();
        const card = page.locator(scene.card).first();
        const header = card.locator(`:scope > ${scene.header}`).first();
        await expect(header).toBeVisible();
        const statistic = header.locator(scene.statistic);
        await expect(statistic).toHaveCount(1);
        if (width <= 820) await expect(statistic).toBeHidden();
        else await expect(statistic).toBeVisible();
        if (scene.title) await expect(header.getByRole('heading', { name: scene.title, exact: true })).toBeVisible();
        if (scene.key === 'configuration') {
          await header.click();
          await expect(card.getByRole('heading', { name: '用户级封禁 · 3 条' })).toBeVisible();
          await expect(header.getByText('收起', { exact: true })).toBeVisible();
        } else if (scene.key === 'rules') {
          await expect(header.getByRole('button', { name: '从节点导入', exact: true })).toBeVisible();
          await expect(header.getByRole('button', { name: /添加/ })).toBeVisible();
          const bounds = await header.boundingBox();
          for (const button of await header.getByRole('button').all()) {
            const box = await button.boundingBox();
            expect(box!.x).toBeGreaterThanOrEqual(bounds!.x - 0.5);
            expect(box!.x + box!.width).toBeLessThanOrEqual(bounds!.x + bounds!.width + 0.5);
          }
        } else if (scene.key === 'capacity') {
          await expect(card.locator('.cap-row')).toHaveCount(1);
          await expect(card.locator('.cap-row')).toContainText('跨区域高流量机器');
        }
        await page.evaluate(() => document.fonts.ready);
        await header.scrollIntoViewIfNeeded();
        await page.mouse.move(0, 0);
        expect(await header.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        await expect(header).toHaveScreenshot(`${browserName}-${scene.key}-${width}.png`);
      }
      expect(errors).toEqual([]);
    } finally {
      await browser.close();
    }
  });
}
