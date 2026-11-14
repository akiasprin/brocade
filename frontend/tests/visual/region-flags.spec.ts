import { expect, test } from '@playwright/test';

for (const browserName of ['chromium', 'webkit'] as const) {
  for (const deviceScaleFactor of [1, 2]) {
    const flagTest = test.extend({ browserName, deviceScaleFactor });
    for (const theme of ['light', 'dark']) {
      flagTest(`region flags ${browserName} dpr${deviceScaleFactor} ${theme}`, async ({ page }) => {
        await page.setViewportSize({ width: 640, height: 800 });
        await page.goto(`/tests/fixtures/region-flags-preview.html?theme=${theme}`);
        await expect(page.locator('.flag-preview .geo-flag')).toHaveCount(30);
        await page.evaluate(async () => {
          await document.fonts.ready;
          const image = new Image();
          image.src = getComputedStyle(document.querySelector('.geo-flag')!).backgroundImage.match(
            /url\("([^"]+)"\)/,
          )![1];
          await image.decode();
        });

        // These flat-color edges must never contain pixels from the adjoining sprite rows.
        for (const [code, edge, color] of [
          ['CN', 'top', 'red'],
          ['TW', 'bottom', 'red'],
          ['SG', 'bottom', 'white'],
        ] as const) {
          const flag = page.locator(`[data-probe="${code}"] .geo-flag`);
          await expect(flag).toHaveCSS('width', '16px');
          await expect(flag).toHaveCSS('height', '11px');
          const bounds = (await flag.boundingBox())!;
          expect(bounds.x).toBeCloseTo(Math.round(bounds.x), 6);
          expect(bounds.y).toBeCloseTo(Math.round(bounds.y), 6);
          const shot = await flag.screenshot();
          const pixel = await page.evaluate(
            async ({ source, code, edge }) => {
              const image = new Image();
              image.src = source;
              await image.decode();
              const canvas = document.createElement('canvas');
              canvas.width = image.width;
              canvas.height = image.height;
              const context = canvas.getContext('2d')!;
              context.drawImage(image, 0, 0);
              const x = Math.floor(image.width * (code === 'TW' ? 0.5 : 0.75));
              const y = edge === 'top' ? 0 : image.height - 1;
              return [...context.getImageData(x, y, 1, 1).data];
            },
            { source: `data:image/png;base64,${shot.toString('base64')}`, code, edge },
          );
          const [red, green, blue] = pixel;
          expect(
            color === 'red' ? red > 200 && green < 80 && blue < 90 : red > 240 && green > 240 && blue > 240,
            `${code} ${edge} edge should be ${color}, got ${pixel.join(', ')}`,
          ).toBe(true);
        }

        await expect(page.locator('.flag-preview')).toHaveScreenshot(
          `${browserName}-dpr${deviceScaleFactor}-${theme}.png`,
        );
      });
    }
  }
}

for (const browserName of ['chromium', 'webkit'] as const) {
  const flagTest = test.extend({ browserName });
  flagTest(`region flags ${browserName} preview survives a delayed vector request`, async ({ page }) => {
    let release!: () => void;
    const gate = new Promise<void>(resolve => {
      release = resolve;
    });
    let blocked = false;
    await page.route('**/flags.svg*', async route => {
      if (route.request().resourceType() === 'image') {
        blocked = true;
        await gate;
      }
      await route.continue();
    });
    try {
      await page.setViewportSize({ width: 640, height: 800 });
      await page.goto('/tests/fixtures/region-flags-preview.html?theme=light', { waitUntil: 'domcontentloaded' });
      await expect(page.locator('.flag-preview .geo-flag')).toHaveCount(30);
      await expect.poll(() => blocked).toBe(true);
      await page.evaluate(() => document.fonts.ready);
      const flags = page.locator('.flag-preview');
      const preview = await flags.screenshot();
      await expect(flags).toHaveScreenshot(`${browserName}-pending-vector.png`);
      release();
      await page.evaluate(async () => {
        const image = new Image();
        image.src = getComputedStyle(document.querySelector('.geo-flag')!).backgroundImage.match(
          /url\("([^"]+)"\)/,
        )![1];
        await image.decode();
      });
      await expect(flags).toHaveScreenshot(`${browserName}-ready-vector.png`);
      expect((await flags.screenshot()).equals(preview)).toBe(false);
    } finally {
      release();
    }
  });
}
