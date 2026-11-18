import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`readable byte history ticks ${width} ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-28T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/observe-focus-preview.html?theme=${theme}`);
    await waitForVisualAssets(page);
    for (const [category, title] of [
      ['内存', '容量构成'],
      ['磁盘', '块设备吞吐'],
    ]) {
      await page.getByRole('button', { name: new RegExp(`展开.*${category}`) }).click();
      const card = page
        .locator('.history-chart-card')
        .filter({ has: page.getByLabel(`${title} 图例`, { exact: true }) });
      await card.scrollIntoViewIfNeeded();
      const axis = () =>
        card.locator('.observe-plot-canvas').evaluate(async host => {
          const moduleUrl = '/tests/fixtures/observe-focus-preview.tsx';
          const { getInstanceByDom } = await import(moduleUrl);
          const option = getInstanceByDom(host)?.getOption();
          const y = option?.yAxis?.[0];
          return y?.interval ? { max: y.max, interval: y.interval } : null;
        });
      await expect.poll(axis).not.toBeNull();
      const values = (await axis())!;
      if (category === '内存') {
        expect(values.max).toBe(8 * 1024 ** 3);
        expect(values.interval).toBe(2 * 1024 ** 3);
      } else expect(values.interval).toBe(0.5 * 1024 ** 2);
      await page.mouse.move(0, 0);
      await expect(card).toHaveScreenshot(`byte-history-${category}-${width}-${theme}.png`);
      expect(await card.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
