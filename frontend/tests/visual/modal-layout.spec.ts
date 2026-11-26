import { waitForVisualAssets } from './helpers/visual-ready';
import { test, expect, type Locator, type Page } from '@playwright/test';

async function bounded(page: Page, surface: Locator) {
  await expect(surface).toBeVisible();
  const rect = (await surface.boundingBox())!;
  const viewport = page.viewportSize()!;
  const padding = await surface.evaluate(el => {
    const css = getComputedStyle(el.parentElement!);
    return {
      top: parseFloat(css.paddingTop),
      right: parseFloat(css.paddingRight),
      bottom: parseFloat(css.paddingBottom),
      left: parseFloat(css.paddingLeft),
    };
  });
  expect(rect.x).toBeGreaterThanOrEqual(padding.left);
  expect(rect.y).toBeGreaterThanOrEqual(padding.top);
  expect(rect.x + rect.width).toBeLessThanOrEqual(viewport.width - padding.right);
  expect(rect.y + rect.height).toBeLessThanOrEqual(viewport.height - padding.bottom);
  expect(await surface.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
}

for (const [width, height] of [
  [1440, 600],
  [390, 500],
  [320, 480],
] as const)
  for (const theme of ['light', 'dark']) {
    test(`CVM operation stays inside ${width}x${height} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.setViewportSize({ width, height });
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}`);
      const trigger = page.getByRole('button', { name: '关机', exact: true });
      await trigger.click();
      const dialog = page.getByRole('dialog', { name: '关机', exact: true });
      const surface = dialog.locator('.dialog-surface');
      await bounded(page, surface);
      await expect(dialog.getByRole('button', { name: '保存变更' })).toBeInViewport();
      const body = dialog.locator('.tunnel-dialog-body');
      await body.evaluate(el => {
        el.scrollTop = el.scrollHeight;
      });
      await expect(dialog.getByRole('button', { name: '保存变更' })).toBeInViewport();
      await waitForVisualAssets(page);
      await expect(dialog).toHaveScreenshot(`operation-short-${width}-${theme}.png`);
      await dialog.locator('header').getByRole('button', { name: '关闭', exact: true }).click();
      await expect(dialog).toBeHidden();
      await expect(trigger).toBeFocused();
    });
    test(`shared deployment confirmation fits ${width}x${height} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
      await page.setViewportSize({ width, height });
      await page.goto(`/tests/fixtures/ui-regressions.html?scene=deploy&theme=${theme}`);
      await page.getByLabel('更多操作', { exact: true }).click();
      await page.getByRole('menuitem', { name: /取消并回滚/ }).click();
      const surface = page.locator('.dialog-layer.modal > .confirm-card');
      await bounded(page, surface);
      await expect(surface.locator('.confirm-actions button').last()).toBeInViewport();
      await waitForVisualAssets(page);
      await expect(surface).toHaveScreenshot(`deployment-short-${width}-${theme}.png`);
    });
  }
