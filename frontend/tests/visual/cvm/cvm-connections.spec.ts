import { expect, test } from '@playwright/test';
for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`install opt-in ${width} ${theme}`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/cvm/install-preview.html?theme=${theme}`);
      const options = page.getByRole('group', { name: '可选安装项' });
      const command = page.getByLabel('安装命令');
      await options.getByRole('checkbox', { name: '网络提供' }).check();
      await expect(command).toContainText('--agent-flavor cvm');
      await expect(command).not.toContainText('--enable-cvm');
      await options.getByRole('checkbox', { name: 'Cube 与宿主系统管理' }).check();
      await options.getByLabel('虚拟化方式').selectOption('kvm');
      await expect(command).toContainText('--enable-cvm');
      await expect(command).toContainText('--cvm-mode kvm');
      const bounds = await page.locator('.pv-install').boundingBox();
      expect(bounds!.x).toBeGreaterThanOrEqual(0);
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
      await expect(page).toHaveScreenshot(`install-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }

test('installed state comes from the machine report', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.goto('/tests/fixtures/cvm/install-preview.html?online=1');
  await expect(page.getByText('容器运行', { exact: true })).toBeVisible();
  await expect(page.getByRole('group', { name: '可选安装项' })).toHaveCount(0);
});

test('existing host setup retains the enrolled credential', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.setViewportSize({ width: 390, height: 850 });
  await page.goto('/tests/fixtures/cvm/install-preview.html?configure=1');
  const dialog = page.getByRole('dialog', { name: '配置机器能力' });
  await dialog.getByLabel('操作').selectOption('--enable-cvm');
  await dialog.getByLabel('虚拟化方式').selectOption('pvm');
  await expect(dialog.getByLabel('机器配置命令')).toContainText('--reuse-node-token --enable-cvm --cvm-mode pvm');
  await expect(dialog).toHaveScreenshot('configure-existing-host-390.png');
});
