import { expect, test } from '@playwright/test';
import { readFileSync } from 'node:fs';

const preserved = JSON.parse(
  readFileSync(new URL('../fixtures/palette-preserved-colors.json', import.meta.url), 'utf8'),
);
const rgb = (hex: string) => `rgb(${[1, 3, 5].map(index => parseInt(hex.slice(index, index + 2), 16)).join(', ')})`;

const palettes = ['dailan', 'jinzi', 'songlv', 'oufen', 'xuanmo'];

for (const palette of palettes) {
  for (const theme of ['light', 'dark']) {
    test(`${palette} ${theme} surfaces and controls`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
      await page.addInitScript(key => localStorage.setItem('brocade-console:palette', key), palette);
      await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/nodes`);
      await expect(page.locator('html')).toHaveAttribute('data-palette', palette);
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page.locator('.node-card').first()).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      // Node geometry and curves have their own browser matrix. Here the palette contract
      // is the exact data color; repeating the full roster adds unrelated SVG rasterization.
      await expect(page.locator('.node-card-comet:not(.empty)').first()).toHaveCSS(
        'color',
        rgb(preserved[palette][theme]['--data']),
      );

      await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/nodes/node/jp-iij`);
      await page.getByRole('tab', { name: '配置', exact: true }).click();
      await expect(page.locator('.nd-tab-config')).toBeVisible();
      await expect(page.locator('.module-load-error')).toHaveCount(0);
      const dns = page
        .locator('.panel.config-panel')
        .filter({ has: page.getByRole('heading', { name: 'DNS', exact: true }) });
      const titleBackground = await page.locator('html').evaluate(root => {
        const style = getComputedStyle(root);
        return style.getPropertyValue('--panel-heading-bg').trim();
      });
      if (theme === 'light') expect(titleBackground).toBe(preserved[palette][theme]['--panel-heading-bg']);
      await expect(dns.locator('header')).toHaveCSS('background-color', rgb(titleBackground));
      await expect(dns.getByRole('heading')).toHaveCSS('color', rgb(preserved[palette][theme]['--panel-heading-ink']));
      await expect(dns).toHaveCSS('border-top-color', rgb(preserved[palette][theme]['--line']));
      await expect(dns).toHaveCSS('border-top-width', '1px');
      await expect(dns).toHaveCSS('border-radius', '10px');
      await dns.locator('select').first().focus();
      await expect(dns.locator('select').first()).toBeFocused();
      await dns.locator('select').first().blur();
      await expect(page).toHaveScreenshot(`${palette}-${theme}-configuration.png`, { threshold: 0.01 });
      await page.screenshot({ path: testInfo.outputPath('configuration.png') });
    });
  }
}
