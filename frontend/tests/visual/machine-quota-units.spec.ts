import { expect, test } from '@playwright/test';

for (const { width, theme } of [
  { width: 1280, theme: 'dark' },
  { width: 390, theme: 'light' },
]) {
  test(`machine quota GiB-${width}-${theme}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.clock.setFixedTime(new Date('2026-10-22T09:00:00+08:00'));
    await page.goto(`/tests/fixtures/traffic-limit-preview.html?scene=heartbeat&theme=${theme}`);
    const card = page.locator('.node-traffic-accounting');
    await expect(card.getByLabel('机器流量额度单位')).toHaveValue('TiB');
    await card.getByLabel('机器流量额度单位').selectOption('GiB');
    await card.getByLabel('机器流量额度', { exact: true }).fill('2000.5');
    await expect(card.locator('.node-traffic-used')).toContainText('/ 2000.5 GiB');
    await page.evaluate(() => document.fonts.ready);
    await expect(card).toHaveScreenshot(`machine-gib-${width}-${theme}.png`, { animations: 'disabled' });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

  test(`forecast quota GiB-${width}-${theme}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/capacity-coverage-preview.html?theme=${theme}&edit`);
    await expect(page.locator('.cap-d-foot')).toBeVisible();
    await page.getByRole('button', { name: '修改', exact: true }).click();
    await page.getByLabel('跨区域高流量机器 机器配额单位').selectOption('GiB');
    await page.getByLabel('跨区域高流量机器 机器配额', { exact: true }).fill('250.5');
    await page.evaluate(() => document.fonts.ready);
    await expect(page.locator('.cap-d-foot')).toHaveScreenshot(`forecast-gib-${width}-${theme}.png`, {
      animations: 'disabled',
    });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });
}
