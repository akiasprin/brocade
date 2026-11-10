import { expect, test } from '@playwright/test';

for (const theme of ['dark', 'light']) {
  for (const width of [1440, 390]) {
    test(`account menu border ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-25T12:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/node-list-preview.html?theme=${theme}&scene=calm#/nodes`);
      await expect(page.locator('.node-card').first()).toBeVisible();
      await expect(page.locator('.node-card').first().locator('.node-card-resources')).toHaveAttribute(
        'data-state',
        'ready',
      );
      await page.evaluate(() => document.fonts.ready);
      await page.getByRole('button', { name: /账户与更多功能/ }).click();
      const menu = page.getByRole('menu', { name: '账户与更多功能' });
      await expect(menu).toBeVisible();
      for (const side of ['top', 'right', 'bottom', 'left']) {
        await expect(menu).toHaveCSS(`border-${side}-width`, '1px');
      }
      await expect(menu).not.toHaveCSS('box-shadow', /inset/);
      await menu.evaluate(async element => {
        await Promise.all(
          element
            .getAnimations()
            .filter(animation => animation.effect?.getComputedTiming().iterations !== Infinity)
            .map(animation => animation.finished.catch(() => undefined)),
        );
      });
      const bounds = (await menu.boundingBox())!;
      await expect(page).toHaveScreenshot(`account-menu-${width}-${theme}.png`, {
        threshold: 0,
        clip: { x: bounds.x - 4, y: bounds.y - 4, width: bounds.width + 8, height: bounds.height + 8 },
      });
    });
  }
}
