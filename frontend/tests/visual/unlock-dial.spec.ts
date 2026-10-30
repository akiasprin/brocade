import { test, expect } from '@playwright/test';

for (const [width, theme] of [[1440, 'dark'], [390, 'light']] as const)
  for (const scenario of ['no-egress', 'unreachable', 'mixed'])
    test(`actual exit ${scenario} ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&theme=${theme}&dial-error=${scenario}`);
      await page.getByRole('button', { name: '解锁', exact: true }).click();
      const panel = page.getByRole('dialog', { name: /解锁/ });
      await panel.getByRole('button', { name: 'IPv6', exact: true }).click();
      const youtube = panel.getByRole('article', { name: 'YouTube', exact: true });
      const premium = panel.getByRole('article', { name: 'YouTube Premium', exact: true });
      if (scenario === 'unreachable') {
        await expect(premium).toContainText('IPv6 出站网络不可达');
        await expect(youtube).toContainText('IPv6 出站网络不可达');
      } else {
        await expect(youtube).toContainText('已跳过 · 无 IPv6 出站');
        if (scenario === 'mixed') await expect(premium).toHaveAttribute('data-tone', 'ok');
        else await expect(premium).toContainText('已跳过 · 无 IPv6 出站');
      }
      await youtube.getByRole('button', { name: 'YouTube 分项' }).click();
      const detail = page.getByRole('dialog', { name: 'YouTube 分项', exact: true });
      await expect(detail).toContainText(scenario === 'unreachable' ? 'IPv6 出站网络不可达' : '无 IPv6 出站');
      if (scenario === 'mixed') await expect(detail).toContainText('nrt12s28');
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`dial-${scenario}-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });

for (const scenario of ['no-egress', 'unreachable'])
  test(`chain list keeps ${scenario} distinct from service restrictions`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.goto(`/tests/fixtures/chain-unlock-preview.html?premium-region-notice=1&dial-error=${scenario}#/chains`);
    const row = page.locator('.cut-row').filter({ has: page.getByRole('button', { name: /香港 直出/ }) });
    const youtube = row.locator('[data-service="youtube"]');
    await expect(youtube).toHaveAttribute('data-tone', scenario === 'no-egress' ? 'ok' : 'part');
    await expect(youtube).toHaveAttribute('title', scenario === 'no-egress' ? /无 IPv6 出站/ : /IPv6 出站网络不可达/);
    await expect(row.locator('[data-service="youtube_premium"]')).toHaveAttribute('data-tone', 'info');
    await expect(page).toHaveScreenshot(`chain-dial-${scenario}.png`);
  });
