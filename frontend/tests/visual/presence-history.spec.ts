import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [1440, 'light'],
  [390, 'dark'],
  [320, 'light'],
] as const) {
  test(`returning IP history at ${width}px in ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-10-28T08:00:00Z') });
    await page.setViewportSize({ width, height: 1800 });
    await page.goto(
      `/tests/fixtures/user-header-preview.html?palette=jinzi&theme=${theme}&presence=sources&sessionHistory`,
    );
    const card = page.locator('.user-presence-card');
    const timeline = card.getByRole('list', { name: '在线与最近 30 天的来源' });
    const returning = timeline.locator('.user-presence-row', { hasText: '198.51.100.77' });
    // Nearby completed epochs share a row; the current online epoch stays separate.
    await expect(returning).toHaveCount(3);
    await expect(returning.first()).not.toHaveClass(/off/);
    await expect(card.locator('header .rt')).toContainText('在线 2 · 30 天 5 个地址');
    await expect(card.locator('details')).toHaveCount(0);
    await expect(timeline.locator('.user-presence-day')).toHaveText(['在线', '今天', '昨天', '7 天内']);
    await card.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    await expect(card).toHaveScreenshot(`presence-history-${width}-${theme}-timeline.png`);

    // The existing timeline-wide preview limit still applies to all history rows together.
    await card.getByRole('button', { name: '显示其余 2 条历史记录' }).click();
    await expect(returning).toHaveCount(4);
    await expect(timeline.locator('.user-presence-row.off')).toHaveCount(7);
    const past = timeline.locator('.user-presence-row.off', { hasText: '198.51.100.77' }).first();
    await expect(past.getByLabel('合计流量 2.70 GiB')).toBeVisible();
    await expect(past.getByText('2 条接入链 · VLESS · 3 段记录')).toBeVisible();
    const access = past.getByRole('button', { name: '展开 2 条接入链详情' });
    await access.focus();
    await page.keyboard.press('Enter');
    await expect(past.locator('.user-presence-access')).toBeVisible();
    await expect(past.getByText('50分')).toHaveCount(3);
    await expect(past.locator('.user-presence-access time')).toHaveText(['02:00', '01:40', '01:01']);
    await expect(past.locator('.user-presence-access-item')).toHaveCount(3);
    // Banning an address does not remove its completed epochs from the dated timeline.
    const bannedHistory = timeline.locator('.user-presence-row.off', { hasText: '203.0.113.17' });
    await expect(bannedHistory).toHaveCount(1);
    await expect(bannedHistory.getByRole('button', { name: '封禁 203.0.113.17' })).toHaveCount(0);
    await expect(card).toHaveScreenshot(`presence-history-${width}-${theme}-access.png`);
    expect(await card.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await card.getByRole('button', { name: '收起历史记录' }).click();
    await expect(returning).toHaveCount(3);
    expect(errors).toEqual([]);
  });
}
