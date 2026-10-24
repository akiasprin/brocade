import { test, expect, type Page } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const evidence = fileURLToPath(new URL('../../../.tools/frontend-visual/screenshots/', import.meta.url));

async function preview(page: Page, query = '') {
  await page.clock.install({ time: new Date('2026-10-23T08:00:00Z') });
  await page.goto(`/tests/fixtures/user-header-preview.html?palette=jinzi${query}`);
  await expect(page.locator('.user-account-state')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

for (const [name, width, height, query] of [
  ['desktop', 1440, 900, ''],
  ['mobile', 390, 844, ''],
  ['long-name', 900, 900, '&user=a-very-long-user-name-for-layout'],
  ['light', 1440, 900, '&theme=light'],
] as const) {
  test(`${name}: header layout and screenshot`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await preview(page, query);
    const layout = await page.locator('.user-dhead').evaluate(header => {
      const bounds = (selector: string) => {
        const element = header.querySelector(selector);
        if (!element) throw new Error(`Missing header element: ${selector}`);
        const box = element.getBoundingClientRect();
        return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width };
      };
      const box = header.getBoundingClientRect();
      return {
        header: { left: box.left, right: box.right, width: box.width },
        identity: bounds('.user-dhead-identity'),
        name: bounds('.dname b'),
        tabs: bounds('.user-detail-tabs'),
        tools: bounds('.user-dhead-tools'),
        overflow: document.documentElement.scrollWidth > innerWidth,
      };
    });
    expect(layout.overflow).toBe(false);
    expect(layout.name.width).toBeGreaterThan(0);
    expect(layout.name.right).toBeLessThanOrEqual(layout.identity.right);
    expect(layout.identity.right).toBeLessThanOrEqual(layout.tools.left);
    expect(layout.tools.right).toBeLessThanOrEqual(layout.header.right);
    if (layout.header.width <= 760) {
      expect(layout.tabs.top).toBeGreaterThanOrEqual(layout.identity.bottom);
      expect(layout.tabs.top).toBeGreaterThanOrEqual(layout.tools.bottom);
    } else {
      expect(layout.identity.right).toBeLessThanOrEqual(layout.tabs.left);
      expect(layout.tabs.right).toBeLessThanOrEqual(layout.tools.left);
    }
    await expect(page.locator('.user-dhead')).toHaveScreenshot(`${name}.png`);
    await mkdir(evidence, { recursive: true });
    await page.screenshot({ path: `${evidence}${name}.png` });
    expect(errors).toEqual([]);
  });
}

test('UUID copies its full value and tabs retain keyboard navigation', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.setViewportSize({ width: 390, height: 844 });
  await preview(page);
  const copy = page.getByRole('button', { name: '复制完整 UUID' });
  await copy.click();
  await expect(page.getByRole('button', { name: 'UUID 已复制' })).toHaveAttribute('data-copy-state', 'done');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('00000000-0000-4000-8000-000000000000');
  const access = page.getByRole('tab', { name: '接入', exact: true });
  await access.focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('tab', { name: '转发', exact: true })).toBeFocused();
  await expect(page.getByRole('tab', { name: '转发', exact: true })).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Home');
  await expect(access).toBeFocused();
  await expect(access).toHaveAttribute('aria-selected', 'true');
});

test('service availability stays explicit on a small screen', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 740 });
  await preview(page, '&user=expired');
  const state = page.locator('.user-account-state');
  await expect(state).toHaveText('已到期');
  await expect(state).toHaveAttribute('data-tone', 'bad');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});
