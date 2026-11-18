import { waitForVisualAssets } from './helpers/visual-ready';
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
      await expect(card.getByRole('switch', { name: /：链路可视化$/, checked: true })).toHaveCount(2);
      for (const flow of await card.locator('.rule-flow').all()) {
        await expect(flow).toBeVisible();
        const input = flow.locator('.flow-well').getByRole('combobox', { name: '追踪域名或 IP' });
        await expect(input).toBeVisible();
        await input.fill('google.com');
        expect((await input.boundingBox())!.width).toBeGreaterThanOrEqual(24);
        await input.press('Escape');
      }
      await card.getByRole('heading', { name: '链路规则', exact: true }).click();
      const titlesFit = await card.locator('.flow-machine-head').evaluateAll(heads =>
        heads.every(head => {
          const facts = head.querySelector('.flow-machine-facts')!.getBoundingClientRect();
          return facts.right <= head.getBoundingClientRect().right - 4;
        }),
      );
      expect(titlesFit).toBe(true);
      const groups = await card.locator(':scope > .node-chain-use').evaluateAll(elements =>
        elements.map(element => {
          const separator = getComputedStyle(element, '::before');
          const style = getComputedStyle(element);
          return {
            content: separator.content,
            width: parseFloat(separator.width),
            height: separator.height,
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
      expect(separator.width).toBeCloseTo(separator.availableWidth, 1);
      expect(separator.height).toBe('1px');
      expect(separator.marginLeft).toBe(0);
      expect(separator.marginRight).toBe(0);
      expect(separator.marginTop).toBe('12px');
      expect(separator.marginBottom).toBe('0px');
      expect(await card.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
      await waitForVisualAssets(page);
      await card.locator(':scope > .node-chain-use').last().scrollIntoViewIfNeeded();
      await page.mouse.move(0, 0);
      await expect(page).toHaveScreenshot(`chain-separator-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
}
