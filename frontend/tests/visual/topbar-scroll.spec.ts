import { expect, test } from '@playwright/test';

for (const engine of ['chromium', 'webkit'] as const) {
  test.describe(`${engine} top bar`, () => {
    for (const width of [1440, 390]) {
      for (const theme of ['light', 'dark']) {
        test(`desktop navigation scrolls normally and stops at the boundary ${width} ${theme}`, async ({
          playwright,
          baseURL,
        }) => {
          const browser = await playwright[engine].launch();
          const page = await browser.newPage({
            baseURL,
            viewport: { width, height: 900 },
            reducedMotion: 'reduce',
            locale: 'zh-CN',
            timezoneId: 'Asia/Singapore',
          });
          try {
            await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
            await page.setViewportSize({ width, height: 900 });
            await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/nodes/node/jp-iij`);
            const header = page.locator('.fg-top');
            const desk = page.locator('.fg-desk');
            await expect(header).toBeVisible();
            await expect(header).toHaveCSS('position', 'relative');
            expect(await header.evaluate(el => el.closest('.fg-desk') !== null)).toBe(true);
            await expect(header).toHaveCSS(
              'background-color',
              await page.locator('html').evaluate(el => getComputedStyle(el).backgroundColor),
            );
            await expect(desk).toHaveCSS('overscroll-behavior-y', 'none');
            await expect(page.locator('html')).toHaveCSS('overscroll-behavior-y', 'none');
            const top = (await header.boundingBox())!;
            await expect.poll(() => desk.evaluate(el => el.scrollHeight - el.clientHeight)).toBeGreaterThan(top.height);
            await desk.evaluate(el => {
              el.scrollTop = el.scrollHeight;
            });
            await expect.poll(() => desk.evaluate(el => el.scrollTop)).toBeGreaterThan(top.height);
            expect((await header.boundingBox())!.y).toBeLessThanOrEqual(-top.height);
            if (width > 820) {
              const title = (await page.locator('.nd-page-head').boundingBox())!;
              expect(title.y).toBeGreaterThanOrEqual(0);
            }
            await page.evaluate(() => document.fonts.ready);
            await expect(page).toHaveScreenshot(`${engine}-${width}-${theme}-scrolled.png`, {
              clip: { x: 0, y: 0, width, height: 210 },
            });

            // Returning to the top restores the navigation; further wheel input must not move the shell.
            await page.mouse.move(5, top.height + 30);
            await page.mouse.wheel(0, -5000);
            await expect.poll(() => desk.evaluate(el => el.scrollTop)).toBe(0);
            await page.mouse.wheel(0, -500);
            await page.mouse.move(5, 5);
            await page.mouse.wheel(0, -500);
            expect((await header.boundingBox())!.y).toBe(top.y);
            expect(await page.evaluate(() => scrollY)).toBe(0);
            await header.getByRole('button', { name: /账户与更多功能/ }).click();
            await expect(page.getByRole('menu', { name: '账户与更多功能' })).toBeInViewport();
          } finally {
            await browser.close();
          }
        });
      }
    }

    test('touch navigation retains document scrolling', async ({ playwright, baseURL }) => {
      const browser = await playwright[engine].launch();
      const page = await browser.newPage({
        baseURL,
        viewport: { width: 390, height: 844 },
        hasTouch: true,
        isMobile: true,
      });
      try {
        await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
        await page.goto('/tests/fixtures/navigation-preview.html?theme=light#/nodes/node/jp-iij');
        const header = page.locator('.fg-top');
        await expect(header).toHaveCSS('position', 'relative');
        await expect(header).toHaveCSS(
          'background-color',
          await page.locator('html').evaluate(el => getComputedStyle(el).backgroundColor),
        );
        await expect
          .poll(() => page.evaluate(() => document.documentElement.scrollHeight - innerHeight))
          .toBeGreaterThan(120);
        await page.evaluate(() => scrollTo(0, 120));
        await expect.poll(() => page.evaluate(() => scrollY)).toBe(120);
        expect((await header.boundingBox())!.y).toBeLessThan(0);
      } finally {
        await browser.close();
      }
    });
  });
}
