import { expect, test } from '@playwright/test';

const scenes = [
  { name: 'machine', route: 'navigation-preview.html', hash: '#/nodes/node/jp-iij', header: '.nd-page-head' },
  { name: 'machines', route: 'navigation-preview.html', hash: '#/nodes', header: '.nd-page-head' },
  { name: 'containers', route: 'cvm/preview.html?scene=list', header: '.cvm-page-head' },
  { name: 'container', route: 'cvm/preview.html?scene=detail', header: '.cvm-page-head' },
  { name: 'user', route: 'user-header-preview.html?palette=dailan', header: '.user-dhead' },
  {
    name: 'chains',
    route: 'chain-unlock-preview.html?view=cards&scene=calm',
    hash: '#/chains',
    header: '.chain-list-panel > header',
  },
  { name: 'tunnel', route: 'navigation-preview.html', hash: '#/tunnels', header: '.tnd-head' },
  { name: 'deployment', route: 'ui-regressions.html?scene=deploy', header: '.cg-head' },
  { name: 'usage', route: 'fleet-traffic-preview.html?scene=few&nav=0&run=0', header: '.nd-page-head' },
] as const;

for (const engine of ['chromium', 'webkit'] as const)
  for (const width of [1440, 320])
    for (const theme of ['light', 'dark'])
      for (const scene of scenes) {
        test(`${engine} ${scene.name} ${width} ${theme}: grouped title actions`, async ({ playwright, baseURL }) => {
          const browser = await playwright[engine].launch();
          try {
            const page = await browser.newPage({
              baseURL,
              viewport: { width, height: 1000 },
              reducedMotion: 'reduce',
              locale: 'zh-CN',
              timezoneId: 'Asia/Taipei',
            });
            await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
            const route = scene.route + (scene.route.includes('?') ? '&' : '?') + `theme=${theme}&w=${width}`;
            await page.goto(`/tests/fixtures/${route}${'hash' in scene ? scene.hash : ''}`);
            if (scene.name === 'tunnel')
              await page.locator('.tunnel-row.manual-row').filter({ hasText: '新加坡家宽 Socks' }).click();
            if (scene.name === 'usage') await page.getByRole('tab', { name: '汇总', exact: true }).click();
            const header = page.locator(scene.header).first();
            await expect(header).toBeVisible();
            if (scene.name !== 'user') {
              const title = (await header.getByRole('heading').first().boundingBox())!;
              expect(title.width, 'the page identity keeps readable space').toBeGreaterThanOrEqual(24);
            }
            const actions = header.locator('.title-actions');
            await expect(actions).toHaveCount(1);
            if (scene.name === 'containers') {
              await expect(actions.locator('.title-action-group')).toHaveCount(0);
              await expect(actions.locator('.title-action-primary')).toBeVisible();
            } else {
              await expect(actions.locator('.title-action-group')).toBeVisible();
              await expect(actions.locator('.title-action-group .btn.primary')).toHaveCount(0);
              await expect(actions.locator('.title-action-group')).toHaveCSS('outline-style', 'solid');
            }
            const height = width <= 820 ? 36 : 30;
            for (const button of await actions
              .locator(
                'button.btn:visible, summary.btn:visible, button.rule-machine-more:visible, .segsw > button:visible',
              )
              .all()) {
              const bounds = (await button.boundingBox())!;
              expect(bounds.height).toBe(height);
              expect(bounds.x).toBeGreaterThanOrEqual(0);
              expect(bounds.x + bounds.width).toBeLessThanOrEqual(width);
            }
            expect(await header.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
            expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
            await page.evaluate(() => document.fonts.ready);
            await expect(header).toHaveScreenshot(`${engine}-${scene.name}-${width}-${theme}.png`);

            if (scene.name === 'machine') {
              const range = header.getByRole('button', { name: /观测时间范围/ });
              await range.click();
              await expect(page.getByRole('dialog', { name: '观测时间范围' })).toBeInViewport();
              await page.keyboard.press('Escape');
              const more = header.getByRole('button', { name: '更多操作', exact: true });
              await more.click();
              await expect(page.locator('[data-node-action-menu]')).toBeInViewport();
            }
            if (scene.name === 'containers') {
              await expect(header.getByRole('button', { name: '容器 更多操作', exact: true })).toHaveCount(0);
              await header.getByRole('tab', { name: '额度', exact: true }).click();
              await expect(page.getByRole('list', { name: '用户额度' })).toBeVisible();
            }
            if (scene.name === 'user') {
              await header.getByRole('button', { name: '更多操作', exact: true }).click();
              await expect(page.getByRole('menu', { name: /更多操作/ })).toBeInViewport();
            }
          } finally {
            await browser.close();
          }
        });
      }

for (const engine of ['chromium', 'webkit'] as const)
  for (const width of [1440, 320]) {
    test(`${engine} unlock primary remains separate ${width}`, async ({ playwright, baseURL }) => {
      const browser = await playwright[engine].launch();
      try {
        const page = await browser.newPage({ baseURL, viewport: { width, height: 1000 }, reducedMotion: 'reduce' });
        await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
        await page.goto('/tests/fixtures/navigation-preview.html?theme=light#/nodes/node/jp-iij');
        await page.getByRole('tab', { name: '解锁', exact: true }).click();
        const header = page.locator('.nd-page-head');
        await expect(header.locator('.title-action-primary .unlock-run')).toBeVisible();
        await expect(header.locator('.title-action-group .btn.primary')).toHaveCount(0);
        await expect(header).toHaveScreenshot(`${engine}-unlock-${width}.png`);
      } finally {
        await browser.close();
      }
    });
  }
