import { test, expect } from '@playwright/test';

for (const width of [1440, 820, 760, 600, 390]) {
  for (const theme of ['dark', 'light']) {
    test(`usage panels ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`/tests/fixtures/user-header-preview.html?forwarding=quota&forwardingUsage=1&theme=${theme}`);
      const access = page.locator('.user-usage-card:not(.user-forwarding-usage)');
      await expect(access.locator('.qta-policy-flag')).toHaveText('用满后限速 5 Mbps');
      await page.evaluate(() => document.fonts.ready);
      const reset = await access.locator('.user-usage-reset').innerText();
      const layout = async (selector: string) =>
        page.locator(selector).evaluate(card => {
          const ledger = card.querySelector('.user-usage-ledger')!;
          const hero = ledger.querySelector('.user-usage-hero')!.getBoundingClientRect();
          const compose = ledger.querySelector('.user-usage-compose')!.getBoundingClientRect();
          const row = card.querySelector('.qta-r')!;
          const name = row.querySelector('.qta-name')!.getBoundingClientRect();
          const amount = row.querySelector('.qta-figs')!.getBoundingClientRect();
          return {
            directions: [...ledger.querySelectorAll('.usage-io dt')].map(el => el.textContent),
            compositionBesideTotal: compose.left > hero.left,
            amountBelowName: amount.top >= name.bottom,
            overflow: card.scrollWidth > card.clientWidth,
          };
        });
      const accessLayout = await layout('.user-usage-card:not(.user-forwarding-usage)');
      await expect(access).toHaveScreenshot(`access-${width}-${theme}.png`);
      await page.getByRole('tab', { name: '转发', exact: true }).click();
      const forwarding = page.locator('.user-forwarding-usage');
      await expect(forwarding.locator('.qta-pct')).toHaveText('10%');
      await expect(forwarding.locator('.qta-left')).toHaveText('剩余45.00 GiB');
      await expect(forwarding.locator('.fwd-rule-count')).toHaveText('规则 0 / 100');
      await expect(forwarding.locator('.qta-cap')).toHaveText('额度 50 GiB');
      await expect(forwarding.locator('.user-usage-reset')).toHaveText(reset);
      expect(await layout('.user-forwarding-usage')).toEqual(accessLayout);
      expect(accessLayout.overflow).toBe(false);
      expect(accessLayout.directions).toEqual(['上行', '下行']);
      await expect(forwarding).toHaveScreenshot(`forwarding-${width}-${theme}.png`);
      if (width === 760 || width === 600) {
        await forwarding.getByRole('button', { name: '编辑 东京入口 的转发授权' }).click();
        await expect(forwarding.getByRole('form', { name: '编辑 东京入口 的转发授权' })).toBeVisible();
        expect(await forwarding.evaluate(card => card.scrollWidth > card.clientWidth)).toBe(false);
        await expect(forwarding).toHaveScreenshot(`forwarding-editor-${width}-${theme}.png`);
      }
      expect(errors).toEqual([]);
    });
  }
}
