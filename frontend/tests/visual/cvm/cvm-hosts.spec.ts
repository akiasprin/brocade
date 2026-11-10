import { test, expect } from '@playwright/test';

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light'])
    for (const scene of ['hosts', 'empty'])
      test(`host tab ${scene} ${width} ${theme}`, async ({ page }) => {
        await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
        await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
        await page.setViewportSize({ width, height: 1000 });
        await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
        const board = page.getByRole('tab', { name: '组合台', exact: true });
        const hosts = page.getByRole('tab', { name: '宿主', exact: true });
        await expect(board).toHaveAttribute('aria-selected', 'true');
        await hosts.click();
        await expect(page.getByRole('tabpanel', { name: '宿主', exact: true })).toBeVisible();
        await expect(page.getByText('能力报告新鲜', { exact: true })).toHaveCount(0);
        await expect(page.getByText('等待新的能力报告', { exact: true })).toHaveCount(0);
        await expect(page.getByRole('button', { name: /查看.*实例/ })).toHaveCount(0);
        await page.evaluate(() => document.fonts.ready);
        if (scene === 'hosts') {
          await expect(page.locator('.cvm-host-card')).toHaveCount(4);
          const header = await page.locator('.cvm-page-head').boundingBox();
          const first = await page.locator('.cvm-host-card').first().boundingBox();
          expect(first!.y).toBeGreaterThanOrEqual(header!.y + header!.height);
          await expect(page.getByRole('status').filter({ hasText: '能力已过期' })).toHaveCount(1);
          await expect(page.getByRole('status').filter({ hasText: '执行能力不可用' })).toHaveCount(1);
          for (const card of await page.locator('.cvm-host-card').all())
            expect(await card.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        } else await expect(page.getByText('尚无宿主', { exact: true })).toBeVisible();
        expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
        await expect(page).toHaveScreenshot(`hosts-${scene}-${width}-${theme}.png`, { fullPage: true });
        await hosts.focus();
        await page.keyboard.press('Home');
        await expect(board).toBeFocused();
        await expect(board).toHaveAttribute('aria-selected', 'true');
        await page.keyboard.press('End');
        const quotas = page.getByRole('tab', { name: '额度', exact: true });
        await expect(quotas).toBeFocused();
        await expect(quotas).toHaveAttribute('aria-selected', 'true');
      });
