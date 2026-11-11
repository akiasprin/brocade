import { test, expect, chromium, webkit } from '@playwright/test';

const route = '/tests/fixtures/user-app-rate-preview.html';

for (const width of [1440, 390]) {
  for (const scene of ['mixed', 'paused', 'few', 'failed']) {
    test(`quota tiles ${scene} ${width}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-11-11T12:00:00Z'));
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`${route}?controls=0&theme=dark&quotaScene=${scene}`);
      const card = page.locator('.user-usage-card');
      await expect(card.locator('.qta-r')).toHaveCount(scene === 'few' ? 2 : 7);
      if (scene === 'mixed') {
        await expect(card).toContainText('限速中 · 5 Mbps');
        await expect(card).toContainText('已停用');
        await expect(card).toContainText('同步中');
        await expect(card).toContainText('<1%');
      }
      if (scene === 'paused') {
        await expect(card.locator('.qta-r.paused')).toHaveCount(7);
        await expect(card.locator('.qta')).not.toContainText('服务暂停');
      }
      if (scene === 'failed') {
        await expect(card.locator('.user-usage-hero-value')).toHaveText('暂不可用');
        await expect(card.locator('.qta-ring')).toHaveCount(0);
        await expect(card.locator('.qta-left')).toHaveCount(0);
        await expect(card.locator('.qta-figs')).toHaveText(Array(7).fill('—'));
      }
      await page.evaluate(() => document.fonts.ready);
      expect(await card.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await expect(card).toHaveScreenshot(`${scene}-${width}.png`);
      expect(errors).toEqual([]);
    });
  }
}

test('the editor spans the list below its selected row and restores keyboard focus', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1100 });
  await page.goto(`${route}?controls=0&theme=light`);
  const list = page.getByRole('list', { name: '各线路的用量与额度' });
  await expect(list.locator('.qta-r')).toHaveCount(7);
  const before = await list.locator('.qta-r').evaluateAll(tiles =>
    tiles.map(tile => {
      const r = tile.getBoundingClientRect();
      return { x: r.x, y: r.y, bottom: r.bottom };
    }),
  );
  expect(before.filter(tile => tile.y === before[0].y)).toHaveLength(4);
  const pencil = list.getByRole('button', { name: 'APAC Premium · 额度与限速', exact: true });
  await pencil.click();
  const sheet = list.locator('.fwe-sheet');
  await expect(sheet).toBeVisible();
  const positions = await list.evaluate(el => {
    const form = el.querySelector('.fwe-sheet')!.getBoundingClientRect();
    const tiles = [...el.querySelectorAll('.qta-r')].map(tile => tile.getBoundingClientRect());
    return {
      form: { left: form.left, right: form.right, top: form.top },
      firstRow: tiles.slice(0, 4).map(r => ({ x: r.x, y: r.y, bottom: r.bottom })),
    };
  });
  expect(positions.firstRow).toEqual(before.slice(0, 4));
  expect(positions.form.top).toBeGreaterThanOrEqual(before[0].bottom);
  await expect(list).toHaveScreenshot('editor-wide-light.png');
  await sheet.getByRole('button', { name: '取消', exact: true }).click();
  await expect(pencil).toBeFocused();
});

test('self service reads the same cards without quota editing controls', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1100 });
  await page.goto(`${route}?controls=0&theme=light&role=user`);
  const list = page.getByRole('list', { name: '各线路的用量与额度' });
  await expect(list.locator('.qta-r')).toHaveCount(7);
  await expect(list.locator('.qta-pencil')).toHaveCount(0);
  await expect(list).toContainText('常态限速 50 Mbps');
  await expect(list).toHaveScreenshot('self-mobile-light.png');
});

for (const browserName of ['chromium', 'webkit'] as const) {
  const touchTest = test.extend({
    browser: [
      async ({}, use) => {
        const browser = await { chromium, webkit }[browserName].launch();
        await use(browser);
        await browser.close();
      },
      { scope: 'worker' },
    ],
  });
  touchTest.describe(`${browserName} touch quota cards`, () => {
    touchTest.use({ viewport: { width: 390, height: 1100 }, hasTouch: true, isMobile: true });
    touchTest('pencils remain visible and the editor fits the phone', async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-11T12:00:00Z'));
      await page.goto(`${route}?controls=0&quotaScene=mixed&theme=${browserName === 'webkit' ? 'light' : 'dark'}`);
      const card = page.locator('.user-usage-card');
      const pencil = card.getByRole('button', { name: 'APAC Premium · 额度与限速', exact: true });
      await expect(pencil).toHaveCSS('opacity', '0.8');
      await pencil.tap();
      const form = page.getByRole('form', { name: 'APAC Premium · 额度与限速', exact: true });
      await expect(form).toBeVisible();
      await expect(form.getByLabel('本期流量额度（GiB）')).toHaveValue('500');
      expect(await card.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      for (const button of await card.locator('.qta-pencil').all()) await expect(button).toHaveCSS('opacity', '0.8');
      await expect(page).toHaveScreenshot(`${browserName}-touch-editor.png`);
    });
  });
}
