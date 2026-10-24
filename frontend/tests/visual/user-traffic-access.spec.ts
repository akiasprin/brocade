import { test, expect } from '@playwright/test';

for (const [name, width] of [
  ['desktop', 1440],
  ['mobile', 390],
] as const) {
  for (const failed of [false, true]) {
    test(`${name}: user quota policy ${failed ? 'failure' : 'read'}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-10-23T08:00:00Z') });
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(
        `/tests/fixtures/user-header-preview.html?palette=jinzi&role=user${failed ? '&policy=error' : ''}`,
      );
      const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
      const quotas = card.getByRole('list', { name: '各线路的用量与额度' });
      await expect(quotas.locator('.qta-r').first()).toBeVisible();
      if (failed) await expect(card.getByText('策略暂不可用')).toBeVisible();
      else await expect(card.getByText('当前账号不能查看这项内容')).toHaveCount(0);
      const bounds = await card.evaluate(element => {
        const body = element.querySelector('.user-usage')!;
        const ledger = body.querySelector('.user-usage-ledger')!.getBoundingClientRect();
        const quota = body.querySelector('.user-access-quotas')!.getBoundingClientRect();
        return {
          children: body.children.length,
          ledger: { right: ledger.right, bottom: ledger.bottom },
          quota: { left: quota.left, top: quota.top },
          overflow: element.scrollWidth > element.clientWidth,
        };
      });
      expect(bounds.children).toBe(2);
      expect(bounds.overflow).toBe(false);
      if (name === 'desktop') expect(bounds.quota.left).toBeGreaterThanOrEqual(bounds.ledger.right - 1);
      else expect(bounds.quota.top).toBeGreaterThanOrEqual(bounds.ledger.bottom - 1);
      await page.evaluate(() => document.fonts.ready);
      await expect(card).toHaveScreenshot(`${name}-user-quota-${failed ? 'failure' : 'read'}.png`);
    });
  }
}
