import { test, expect } from '@playwright/test';

for (const width of [320, 390, 820, 1440]) {
  for (const theme of ['dark', 'light']) {
    test(`forwarding loading retains content insets at ${width}px in ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
      await page.setViewportSize({ width, height: 844 });
      await page.goto(`/tests/fixtures/user-header-preview.html?palette=jinzi&forwarding=pending&theme=${theme}`);
      await page.getByRole('tab', { name: '转发', exact: true }).click();
      const rules = page.getByRole('status', { name: '读取转发规则' });
      const authorization = page.getByRole('status', { name: '读取转发授权' });
      await expect(rules).toBeVisible();
      await expect(authorization).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      const card = page.locator('.fwr-panel');
      const layouts = await page.locator('.user-forwarding-usage,.fwr-panel').evaluateAll(cards =>
        cards.map(card => {
          const bounds = card.getBoundingClientRect();
          const header = card.querySelector(':scope > header')!.getBoundingClientRect();
          const loader = card.querySelector('.loading-mark-field')!.getBoundingClientRect();
          const style = getComputedStyle(card);
          const ring = card.querySelector('.loading-mark > i')!.getBoundingClientRect();
          return {
            left: loader.left - bounds.left - Number.parseFloat(style.borderLeftWidth),
            top: loader.top - header.bottom,
            bottom: bounds.bottom - Number.parseFloat(style.borderBottomWidth) - loader.bottom,
            height: bounds.height,
            ring: [ring.width, ring.height],
          };
        }),
      );
      expect(layouts).toHaveLength(2);
      expect(layouts[0]).toEqual(layouts[1]);
      expect(layouts[0]).toMatchObject({ left: 13, top: 19, bottom: 19, height: 92, ring: [16, 16] });
      expect(await card.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
      await expect(card).toHaveScreenshot(`rules-${width}-${theme}.png`);
      await expect(page.locator('.user-forwarding-usage')).toHaveScreenshot(`authorization-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
}

for (const width of [390, 1440]) {
  for (const theme of ['dark', 'light']) {
    test(`loaded forwarding preserves its layout at ${width}px in ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
      await page.setViewportSize({ width, height: 844 });
      await page.goto(`/tests/fixtures/forwarding-actions-mockup.html?v=current&scene=fleet&nav=0&theme=${theme}`);
      const card = page.locator('.fwr-panel');
      await expect(card.locator('.fwr-item').first()).toBeVisible();
      expect(await card.locator('.fwr-item').count()).toBeGreaterThanOrEqual(5);
      await page.evaluate(() => document.fonts.ready);
      const layout = await card.evaluate(card => {
        const bounds = card.getBoundingClientRect();
        const header = card.querySelector(':scope > header')!.getBoundingClientRect();
        const tools = card.querySelector('.fwr-tools')!.getBoundingClientRect();
        return {
          top: tools.top - header.bottom,
          left: tools.left - bounds.left - Number.parseFloat(getComputedStyle(card).borderLeftWidth),
          overflow: card.scrollWidth > card.clientWidth,
        };
      });
      expect(layout).toEqual({ top: 14, left: width === 390 ? 14 : 16, overflow: false });
      await expect(card).toHaveScreenshot(`loaded-${width}-${theme}.png`);
    });
  }
}

for (const [width, theme] of [
  [390, 'dark'],
  [1440, 'light'],
] as const) {
  test(`forwarding read errors retain matching spacing at ${width}px in ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
    await page.setViewportSize({ width, height: 844 });
    await page.goto(`/tests/fixtures/user-header-preview.html?palette=jinzi&forwarding=error&theme=${theme}`);
    await page.getByRole('tab', { name: '转发', exact: true }).click();
    const cards = page.locator('.user-forwarding-usage,.fwr-panel');
    await expect(cards.locator('.callout.err')).toHaveCount(2);
    const positions = await cards.evaluateAll(cards =>
      cards.map(card => {
        const header = card.querySelector(':scope > header')!.getBoundingClientRect();
        const error = card.querySelector('.callout.err')!.getBoundingClientRect();
        const bounds = card.getBoundingClientRect();
        return {
          top: error.top - header.bottom,
          bottom: bounds.bottom - Number.parseFloat(getComputedStyle(card).borderBottomWidth) - error.bottom,
        };
      }),
    );
    await expect(page.locator('.fwr-panel')).toHaveScreenshot(`error-${width}-${theme}.png`);
    expect(positions[0]).toEqual(positions[1]);
  });
}
