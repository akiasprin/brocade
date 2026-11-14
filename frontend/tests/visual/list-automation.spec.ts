import { test, expect } from '@playwright/test';

for (const theme of ['light', 'dark']) {
  for (const width of [1440, 390, 320]) {
    test(`Sub-Store flags are visible in the user roster ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-14T00:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/users`);
      const roster = page.getByRole('listbox', { name: '用户列表' });
      await expect(roster).toBeVisible();
      const connected = roster.locator('.user-row-observation > .user-row-substore');
      await expect(connected).toHaveCount(2);
      for (const status of await connected.all()) {
        await expect(status).toHaveText('· 已连接 Sub-Store');
        await expect(status).toBeVisible();
        expect(await status.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
      }
      await expect(roster.getByRole('option').filter({ hasText: 'bob' }).locator('.user-row-substore')).toHaveCount(0);
      await expect(roster.getByText('未连接', { exact: true })).toHaveCount(0);
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await page.evaluate(() => document.fonts.ready);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`user-list-${width}-${theme}.png`);
    });

    test(`automatic maintenance flags are visible on machine cards ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-14T00:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/node-list-preview.html?scene=few&theme=${theme}#/nodes`);
      const list = page.getByRole('list', { name: '机器列表' });
      await expect(list.locator('.node-card')).toHaveCount(3);
      for (const text of ['开启', '关闭', '未设置']) {
        const icon = list.getByRole('img', { name: `自动养护：${text}` });
        await expect(icon).toBeVisible();
        const positions = await icon.evaluate(element => {
          const bounds = element.getBoundingClientRect();
          const flag = element.parentElement!.querySelector('.node-card-flag')!.getBoundingClientRect();
          const comet = element.closest('.node-card')!.querySelector('.node-card-comet')!.getBoundingClientRect();
          return {
            center: bounds.x + bounds.width / 2,
            top: bounds.top,
            bottom: bounds.bottom,
            flagCenter: flag.x + flag.width / 2,
            flagBottom: flag.bottom,
            cometTop: comet.top,
          };
        });
        expect(positions.center).toBeCloseTo(positions.flagCenter, 1);
        expect(positions.top).toBeGreaterThan(positions.flagBottom);
        expect(positions.bottom).toBeLessThanOrEqual(positions.cometTop);
      }
      await expect(page.getByRole('tablist', { name: /详情页签/ })).toHaveCount(0);
      await page.evaluate(() => document.fonts.ready);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`machine-list-${width}-${theme}.png`);
    });
  }
}
