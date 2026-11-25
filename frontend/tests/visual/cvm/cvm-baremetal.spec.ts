import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`bare-metal adoption and instance ${width}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=baremetal-hosts&theme=${theme}`);
    await page.getByRole('button', { name: '纳管裸机', exact: true }).click();
    await expect(page).toHaveURL(/#\/cvm\/adopt$/);
    const form = page.getByRole('form', { name: '纳管裸机', exact: true });
    const picker = form.getByRole('region', { name: '裸机与使用者', exact: true });
    await expect(picker.getByRole('combobox', { name: '选择裸机', exact: true })).toHaveValue('hk-hkix');
    const confirmation = form.getByRole('region', { name: '整机纳管确认', exact: true });
    await expect(confirmation).toContainText('Debian GNU/Linux 13');
    const submit = form.getByRole('button', { name: '确认纳管', exact: true });
    await expect(submit).toBeDisabled();
    await confirmation
      .getByRole('checkbox', {
        name: '确认此用户可管理本物理系统；宿主业务没有容器隔离，Cube 容器保持独立运行',
        exact: true,
      })
      .check();
    await expect(submit).toBeEnabled();
    await waitForVisualAssets(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`baremetal-adopt-page-${width}-${theme}.png`, { fullPage: true });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=baremetal-detail&theme=${theme}`);
    await page.getByRole('button', { name: '实例更多操作' }).click();
    await expect(page.getByRole('menuitem', { name: /^解除纳管/ })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0);
    await waitForVisualAssets(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`baremetal-detail-${width}-${theme}.png`, { fullPage: true });
    expect(errors).toEqual([]);
  });

  test(`bare-metal entry before native capability ${width}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1100 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=list&theme=${theme}`);
    await page.getByRole('button', { name: '纳管裸机', exact: true }).click();
    await expect(page).toHaveURL(/#\/cvm\/adopt$/);
    const form = page.getByRole('form', { name: '纳管裸机', exact: true });
    await expect(form.getByText('尚无可纳管的裸机', { exact: true })).toBeVisible();
    await expect(form.getByText(/安装或升级组合执行栈/)).toBeVisible();
    await expect(form.getByRole('combobox', { name: '选择裸机', exact: true })).toHaveCount(0);
    await expect(form.getByRole('button', { name: '确认纳管', exact: true })).toHaveCount(0);
    await waitForVisualAssets(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`baremetal-adopt-unavailable-page-${width}-${theme}.png`, { fullPage: true });
    await form.getByRole('button', { name: '查看宿主', exact: true }).click();
    await expect(page).toHaveURL(/#\/cvm\/hosts$/);
    await expect(page.getByRole('tab', { name: '宿主', exact: true })).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByRole('table', { name: '宿主', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '纳管裸机', exact: true })).toBeVisible();
  });
}
