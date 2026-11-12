import { test, expect } from '@playwright/test';

for (const width of [390, 1440])
  for (const theme of ['dark', 'light'])
    test(`listener references follow the chain ID ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(
        `/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}&listener-references=1#/chains`,
      );
      const card = page.locator('.chain-card[data-order-id="jp-iij"]');
      await expect(card.getByText('2 处监听引用', { exact: true })).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      const metadata = card.locator('.chain-card-title > span');
      await expect(metadata).toHaveText('jp-iij | 2 处监听引用');
      const placement = await metadata.evaluate(element => {
        const id = document.createRange();
        id.selectNode(element.firstChild!);
        const idBounds = id.getBoundingClientRect();
        const referenceBounds = element.querySelector('span')!.getBoundingClientRect();
        return {
          sameLine: Math.abs(idBounds.y - referenceBounds.y) < 1,
          followsId: referenceBounds.x >= idBounds.right,
          fits: element.scrollWidth <= element.clientWidth,
        };
      });
      expect(placement).toEqual({ sameLine: true, followsId: true, fits: true });
      await expect(card).toHaveScreenshot(`listener-reference-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });
