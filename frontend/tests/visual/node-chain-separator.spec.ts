import { expect, test } from '@playwright/test';

for (const theme of ['dark', 'light']) {
  for (const width of [320, 390, 1440]) {
    test(`machine chain separator ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-11-10T08:00:00Z'));
      await page.setViewportSize({ width, height: 1200 });
      await page.goto(
        `/tests/fixtures/node-unlock-preview.html?tab=chains&theme=${theme}${width <= 820 ? '&readonly' : ''}`,
      );
      await expect(page.getByRole('tab', { name: /^规则/ })).toHaveAttribute('aria-selected', 'true');
      const card = page.locator('.node-chain-sheet');
      await expect(card.locator(':scope > .node-chain-use')).toHaveCount(2);
      await expect(card.locator('.chain-rule-tree')).toHaveCount(2);
      for (const tree of await card.locator('.chain-rule-tree').all()) {
        await tree.locator('.chain-rule-node-head[role="button"]').first().click();
      }
      const groups = await card.locator(':scope > .node-chain-use').evaluateAll(elements =>
        elements.map(element => {
          const separator = getComputedStyle(element, '::before');
          const style = getComputedStyle(element);
          return {
            content: separator.content,
            width: parseFloat(separator.width),
            availableWidth: element.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
            marginLeft: parseFloat(separator.marginLeft),
            marginRight: parseFloat(separator.marginRight),
            marginTop: separator.marginTop,
            marginBottom: separator.marginBottom,
          };
        }),
      );
      expect(groups[0].content).toBe('none');
      const separator = groups[1];
      expect(separator.content).toBe('""');
      expect(separator.width).toBeCloseTo(Math.min(160, separator.availableWidth * 0.45), 1);
      expect(separator.marginLeft).toBeCloseTo(separator.marginRight, 1);
      expect(separator.marginTop).toBe('12px');
      expect(separator.marginBottom).toBe('12px');
      expect(await card.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
      await page.evaluate(() => document.fonts.ready);
      await page.mouse.move(0, 0);
      await expect(card).toHaveScreenshot(`chain-separator-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
}
