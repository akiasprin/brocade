import { test, expect } from '@playwright/test';

for (const width of [390, 1440])
  for (const theme of ['dark', 'light'])
    test(`high latency chain keeps normal colors ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-29T04:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains`);
      await expect(page.locator('.chain-card')).toHaveCount(16);
      await page.evaluate(() => document.fonts.ready);

      const card = page.locator('.chain-card[data-order-id="us-cmin2"]');
      await expect(card.locator('.chain-card-latency b')).toHaveText(/^\d+$/);
      expect(Number(await card.locator('.chain-card-latency b').textContent())).toBeGreaterThan(300);
      await expect(card).toHaveClass(/tone-ok/);
      await expect(card.locator('.chain-card-status')).toHaveClass(/\bok\b/);
      await expect(card).toHaveScreenshot(`high-latency-card-${width}-${theme}.png`);
      await expect(page.locator('.chain-card[data-order-id="gb-lon"]')).toHaveClass(/tone-warn/);
      await expect(page.locator('.chain-card[data-order-id="jp-sb"]')).toHaveClass(/tone-bad/);

      if (width === 1440) {
        await card.click();
        const banner = page.locator('.blk').filter({ has: page.locator('.probe-face') });
        await expect(banner).toHaveClass(/tone-ok/);
        await expect(banner).toHaveScreenshot(`high-latency-banner-${theme}.png`);
      }
    });
