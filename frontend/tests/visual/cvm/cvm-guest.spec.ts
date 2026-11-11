import { test, expect } from '@playwright/test';
import { openBoardInstance } from '../helpers/cvm-board';

for (const [width, theme] of [
  [1440, 'dark'],
  [320, 'light'],
] as const) {
  test(`guest container browsing ${width} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?role=public&theme=${theme}`);
    const navigation = page.getByRole('navigation', { name: '主导航' });
    await expect(navigation.getByRole('button', { name: '容器', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '＋ 创建容器', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'WebShell 管理', exact: true })).toHaveCount(0);
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`guest-list-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '宿主', exact: true }).click();
    await expect(page.locator('.cvm-host-card')).toHaveCount(1);
    await expect(page).toHaveScreenshot(`guest-hosts-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '组合台', exact: true }).click();
    await openBoardInstance(page, width);
    await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: '＋ 挂载网卡', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: '终端', exact: true })).toHaveCount(0);
    await expect(page.getByText('连接地址已脱敏。')).toBeVisible();
    await expect(page).toHaveScreenshot(`guest-detail-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '监控与诊断', exact: true }).click();
    await expect(page.getByRole('region', { name: '容器 监控', exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual([]);
    expect(errors).toEqual([]);
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=new&role=public&theme=${theme}`);
    await expect(page).toHaveURL(/#\/cvm$/);
    await expect(page.getByRole('textbox', { name: '实例名称', exact: true })).toHaveCount(0);
  });
}
