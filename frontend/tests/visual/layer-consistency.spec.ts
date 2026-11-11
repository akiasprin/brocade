import { expect, test as base, chromium, webkit, type Page } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
import { layerCases, layerMetrics, themedUrl } from './helpers/layer-cases';

const shortCases = new Set([
  'dynamic-confirmation',
  'dynamic-choice',
  'user-service-policy',
  'tunnel-editor',
  'node-inflow',
  'cvm-terminal',
]);
const landscapeCases = new Set(['dynamic-confirmation', 'dynamic-choice', 'mux-editor', 'node-inflow', 'cvm-terminal']);
const webkitDesktopCases = new Set([
  'dynamic-confirmation',
  'dynamic-choice',
  'user-password',
  'dns-import',
  'node-inflow',
  'tunnel-editor',
  'cvm-quota',
  'cvm-terminal',
]);
const shortDesktopCases = new Set(['dynamic-confirmation', 'dns-import', 'node-inflow', 'cvm-quota']);

async function ready(page: Page) {
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}

for (const engine of ['chromium', 'webkit'] as const) {
  const test = base.extend({
    browser: [
      async ({}, use) => {
        const browser = await { chromium, webkit }[engine].launch();
        await use(browser);
        await browser.close();
      },
      { scope: 'worker' },
    ],
  });

  for (const profile of [
    { name: 'phone', width: 390, height: 844, theme: 'light', touch: true },
    { name: 'small', width: 320, height: 568, theme: 'dark', touch: true },
    { name: 'landscape', width: 780, height: 390, theme: 'dark', touch: true },
    { name: 'desktop', width: 1440, height: 1000, theme: 'dark', touch: false },
    { name: 'short-desktop', width: 1440, height: 600, theme: 'light', touch: false },
  ]) {
    test.describe(`${engine} layers ${profile.name}`, () => {
      test.use({
        viewport: { width: profile.width, height: profile.height },
        isMobile: profile.touch,
        hasTouch: profile.touch,
      });
      for (const scenario of layerCases) {
        if (profile.name === 'small' && !shortCases.has(scenario.id)) continue;
        if (profile.name === 'landscape' && !landscapeCases.has(scenario.id)) continue;
        if (profile.name === 'desktop' && engine === 'webkit' && !webkitDesktopCases.has(scenario.id)) continue;
        if (profile.name === 'short-desktop' && !shortDesktopCases.has(scenario.id)) continue;
        test(scenario.id, async ({ page }, testInfo) => {
          const errors: string[] = [];
          page.on('pageerror', error => errors.push(error.message));
          await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
          await page.goto(themedUrl(scenario.url, profile.theme));
          await scenario.open(page);
          const layer = page.locator('.dialog-layer').last();
          await expect(layer).toBeVisible();
          await ready(page);
          const metrics = await layerMetrics(page);
          await writeFile(testInfo.outputPath('metrics.json'), JSON.stringify(metrics, null, 2));
          expect(metrics.layerFont.size).toBe('12.5px');
          expect(metrics.layerFont.family).toContain('sans-serif');
          expect(metrics.surfaceFont.size).toBe('12.5px');
          expect(metrics.surface.x).toBeGreaterThanOrEqual(-1);
          expect(metrics.surface.y).toBeGreaterThanOrEqual(-1);
          expect(metrics.surface.right).toBeLessThanOrEqual(profile.width + 1);
          expect(metrics.surface.bottom).toBeLessThanOrEqual(profile.height + 1);
          expect(metrics.horizontalOverflow).toBe(false);
          expect(metrics.pageOverflow).toBe(false);
          if (metrics.mode.split(' ').includes('modal')) {
            if (profile.touch && metrics.surfaceClassName.includes('sub-dialog')) {
              expect(Math.abs(metrics.surface.bottom - profile.height)).toBeLessThanOrEqual(1);
            } else {
              expect(Math.abs(metrics.surface.x + metrics.surface.width / 2 - profile.width / 2)).toBeLessThanOrEqual(
                1,
              );
              expect(Math.abs(metrics.surface.y + metrics.surface.height / 2 - profile.height / 2)).toBeLessThanOrEqual(
                1,
              );
            }
          }
          if (metrics.mode.split(' ').includes('drawer')) {
            expect(Math.abs(metrics.surface.bottom - profile.height)).toBeLessThanOrEqual(1);
            expect(Math.abs(metrics.surface.right - profile.width)).toBeLessThanOrEqual(1);
          }

          const segments = layer.locator('.segsw button');
          if (profile.touch) {
            for (const control of metrics.controls.filter(control => control.className.includes('node-usage-close')))
              expect(control.rect.height).toBe(36);
            for (const size of await segments.evaluateAll(elements =>
              elements.map(element => element.getBoundingClientRect().height),
            ))
              expect(size).toBeLessThanOrEqual(36);
            for (const size of await layer
              .locator('.choice-menu button')
              .evaluateAll(elements => elements.map(element => getComputedStyle(element).fontSize)))
              expect(size).toBe('12.5px');
          }

          const name = metrics.name!;
          const surface = layer.locator(':scope > .dialog-surface');
          if (profile.name === 'phone' || (profile.name === 'short-desktop' && scenario.id === 'node-inflow'))
            await expect(page).toHaveScreenshot(`${engine}-${profile.name}-${scenario.id}.png`);

          // Move each scrollable production body to its end; the fixed surface must remain bounded.
          const body = surface
            .locator(
              '.confirm-body, .tunnel-dialog-body, .external-outbound-body, .nid-body, .cvm-dialog-body, .sub-dialog-body, .unlock-dialog-body, .node-usage-body',
            )
            .first();
          if (await body.count()) {
            await body.evaluate(element => {
              element.scrollTop = element.scrollHeight;
            });
            const box = (await surface.boundingBox())!;
            expect(box.y).toBeGreaterThanOrEqual(-1);
            expect(box.y + box.height).toBeLessThanOrEqual(profile.height + 1);
          }
          expect(errors).toEqual([]);
          await page.keyboard.press(scenario.id === 'cvm-terminal' ? 'Control+Shift+Escape' : 'Escape');
          await expect(page.getByRole('dialog', { name, exact: true })).toHaveCount(0);
        });
      }
    });
  }

  test.describe(`${engine} ordinary pages`, () => {
    test.use({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
    for (const route of ['nodes', 'chains', 'tunnels', 'users', 'usage', 'deploy', 'settings', 'cvm']) {
      test(route, async ({ page }, testInfo) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
        await page.goto(
          route === 'cvm'
            ? '/tests/fixtures/cvm/preview.html?scene=detail&theme=light'
            : `/tests/fixtures/navigation-preview.html?theme=light#/${route}`,
        );
        await expect(page.locator('.fg-top')).toBeVisible();
        const main = page.getByRole('main', { name: /内容$/ });
        await expect(main).toBeVisible();
        if (route === 'cvm') {
          await expect(main.locator('.cvm-detail')).toBeVisible();
        }
        await expect(main.locator('.cardpage, .nd-page').first()).toBeVisible();
        await expect(main.locator('.callout.err')).toHaveCount(0);
        await ready(page);
        const panels = await page.locator('.fg-view .panel').evaluateAll(elements =>
          elements
            .filter(element => element.getBoundingClientRect().width > 0)
            .map(element => ({
              className: element.className,
              size: getComputedStyle(element).fontSize,
              family: getComputedStyle(element).fontFamily,
              overflow: element.scrollWidth > element.clientWidth + 1,
            })),
        );
        if (['users', 'tunnels', 'settings', 'cvm'].includes(route)) expect(panels.length).toBeGreaterThan(0);
        for (const panel of panels) {
          expect(panel.size).toBe('12.5px');
          expect(panel.family).toContain('sans-serif');
          expect(panel.overflow, panel.className).toBe(false);
        }
        const titles = main.locator('.panel.titled > header .panel-title');
        for (const title of await titles.all()) {
          await expect(title).toHaveCSS('font-size', '12px');
          await expect(title.locator('.ui-icon').first()).toBeVisible();
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1)).toBe(false);
        await writeFile(testInfo.outputPath('panels.json'), JSON.stringify(panels, null, 2));
        await expect(page).toHaveScreenshot(`${engine}-page-${route}.png`);
        expect(errors).toEqual([]);
      });
    }
  });
}
