import { test, expect } from '@playwright/test';

const flavor = process.env.BROCADE_PRODUCT_FLAVOR === 'base' ? 'base' : 'cvm';
for (const width of [1440, 390]) {
  test(`${flavor} navigation and machine boundary ${width}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-02T10:00:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
    await page.addInitScript(() => {
      const reads: string[] = [];
      Object.defineProperty(window, '__productReads', { value: reads });
      const wrap =
        (next: typeof fetch): typeof fetch =>
        (input, init) => {
          const path = new URL(input instanceof Request ? input.url : String(input), location.origin).pathname;
          reads.push(path);
          return next(input, init);
        };
      let current = wrap(window.fetch.bind(window));
      Object.defineProperty(window, 'fetch', {
        configurable: true,
        get: () => current,
        set: next => {
          current = wrap(next);
        },
      });
    });
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/tests/fixtures/navigation-preview.html?theme=light#/nodes/node/jp-iij');
    await expect(page.locator('.nd-page-head')).toBeVisible();
    const cvm = page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '容器', exact: true });
    await expect(cvm).toHaveCount(flavor === 'cvm' ? 1 : 0);
    if (flavor === 'base') {
      await expect(page.getByText(/^CUBE(?:\[|$)/)).toHaveCount(0);
      await page.clock.runFor(31_000);
      expect(
        await page.evaluate(() =>
          (window as Window & { __productReads?: string[] }).__productReads!.filter(path => path.startsWith('/cvm')),
        ),
      ).toEqual([]);
    }
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`${flavor}-machine-${width}.png`, { clip: { x: 0, y: 0, width, height: 190 } });
    await page.evaluate(() => {
      location.hash = '#/cvm';
    });
    if (flavor === 'base') {
      await expect(page).toHaveURL(/#\/nodes$/);
      await expect(page.locator('.nd-page-head')).toBeVisible();
    } else {
      await expect(page.getByRole('tab', { name: '组合台', exact: true })).toBeVisible();
    }
  });
}
