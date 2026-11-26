import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';
import { openListInstance } from '../helpers/cvm-instance';

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
    await expect(page.getByRole('button', { name: '创建容器', exact: true })).toHaveCount(0);
    await expect(page.getByRole('tab', { name: '宿主', exact: true })).toHaveCount(0);
    await expect(page.getByRole('tab', { name: '网络', exact: true })).toHaveCount(0);
    const list = page.getByRole('table', { name: '按宿主分组的容器', exact: true });
    await expect(list.locator('.cvm-list-row')).toHaveCount(1);
    await expect(list.getByRole('checkbox')).toHaveCount(0);
    await expect(list).not.toContainText('203.0.113.20');
    await waitForVisualAssets(page);
    await expect(page).toHaveScreenshot(`guest-readonly-list-${width}-${theme}.png`);
    await openListInstance(page);
    await expect(page.getByRole('button', { name: '实例更多操作', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: '终端', exact: true })).toHaveCount(0);
    await expect(page.getByText('连接地址已脱敏。')).toBeVisible();
    // 流量曲线分块加载，画出后再截图，基准不取决于加载时机。
    await expect(page.getByRole('region', { name: '流量与事件' }).locator('canvas')).toBeVisible();
    await expect(page).toHaveScreenshot(`guest-detail-${width}-${theme}.png`);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const network = page.getByRole('tabpanel', { name: '网络', exact: true });
    await expect(network.getByText('访问地址已脱敏。', { exact: true })).toBeVisible();
    await expect(network.getByRole('button', { name: '添加出站方式', exact: true })).toHaveCount(0);
    await page.getByRole('tab', { name: '监控与诊断', exact: true }).click();
    await expect(page.getByRole('region', { name: '容器 监控', exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual([]);
    expect(errors).toEqual([]);
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=new&role=public&theme=${theme}`);
    await expect(page).toHaveURL(/#\/cvm$/);
    await expect(page.getByRole('textbox', { name: '容器名称', exact: true })).toHaveCount(0);
  });
}
