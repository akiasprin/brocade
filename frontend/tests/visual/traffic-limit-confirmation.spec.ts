import { expect, test } from '@playwright/test';

for (const { width, theme } of [
  { width: 1280, theme: 'dark' },
  { width: 390, theme: 'light' },
]) {
  for (const scene of ['handoff', 'heartbeat', 'partial'] as const) {
    test(`${scene}-${width}-${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.clock.setFixedTime(new Date('2026-10-22T09:00:00+08:00'));
      await page.goto(
        `/tests/fixtures/node-traffic-limit-mockup.html?scene=${scene}&option=a&view=compare&nav=0&theme=${theme}`,
      );
      const card = page.locator('.node-traffic-accounting:not(.ntl-card)');
      await expect(card).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      if (scene === 'handoff') {
        await expect(card.getByRole('status')).toHaveText('等待应用确认');
        await expect(card.getByLabel('限速执行结果')).toContainText('等待旧代排空后再确认应用层上限');
        await expect(card.locator('.node-traffic-limit-chip.err')).toHaveCount(0);
        await expect(card.locator('.node-traffic-flag.on')).toHaveCount(0);
        const diagnostic = card.getByLabel('限速执行结果').locator('em');
        expect(await diagnostic.evaluate(element => getComputedStyle(element).color)).toBe(
          await diagnostic.evaluate(element => getComputedStyle(element.parentElement!).color),
        );
      } else if (scene === 'heartbeat') {
        await expect(card.getByRole('status')).toHaveText('已限速 100 Mbps');
        await expect(card.locator('.node-traffic-flag.on')).toHaveCount(1);
      } else {
        await expect(card.getByRole('status')).toHaveText('应用失败');
        const diagnostic = card.getByLabel('限速执行结果').locator('.err em');
        expect(await diagnostic.evaluate(element => getComputedStyle(element).color)).toBe(
          await card.locator('.node-traffic-limit-state.err').evaluate(element => getComputedStyle(element).color),
        );
        await expect(card.locator('.node-traffic-flag.on')).toHaveCount(0);
      }
      await expect(card).toHaveScreenshot(`${scene}-${width}-${theme}.png`, { animations: 'disabled' });
    });
  }
}
