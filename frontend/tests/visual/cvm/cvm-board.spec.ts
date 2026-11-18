import { test, expect, type Page } from '@playwright/test';

async function open(page: Page, width: number, theme: string) {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 900 });
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=board&theme=${theme}`);
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

for (const width of [1440, 1280])
  for (const theme of ['light', 'dark']) {
    test(`composition board ${width} ${theme}`, async ({ page }) => {
      const errors = await open(page, width, theme);
      await expect(page.locator('.cz-pill')).toHaveCount(4);
      await expect(page.locator('.cz-map:not(.free):not(.taken)')).toHaveCount(3);
      await expect(page.locator('.cz-net')).toHaveCount(2);
      await expect(page.locator('.cz-wire.in')).toHaveCount(3);
      await expect(page.locator('.cz-wire.out')).toHaveCount(3);
      await expect(page.locator('.cz-layout')).toHaveScreenshot(`board-${width}-${theme}.png`);
      // Click path: pick a free shared port for the selected container, then confirm.
      await page.locator('[data-pill="cvm-alice"]').click();
      await expect(page.locator('.cz-inspector')).toContainText('访问入口');
      await page.getByRole('button', { name: '＋ 入口' }).click();
      await page.locator('.cz-map.free', { hasText: '＋ 端口' }).click();
      const proposal = page.getByRole('region', { name: '待确认的组合变更' });
      await expect(proposal).toContainText('203.0.113.20:30000 TCP → home-workspace:22');
      await expect(page.locator('.cz-layout')).toHaveScreenshot(`board-proposal-${width}-${theme}.png`);
      await proposal.getByRole('button', { name: '接入' }).click();
      await expect(proposal).toHaveCount(0);
      const mutations = await page.evaluate(
        () => (window as unknown as { __cvmMutations: Record<string, unknown>[] }).__cvmMutations,
      );
      expect(mutations.at(-1)).toMatchObject({
        method: 'POST',
        path: '/cvm/entries',
        instance_id: 'cvm-alice',
        publication_id: 'entrypub-hk',
        address: '203.0.113.20',
        mode: { kind: 'l4', protocol: 'tcp', listen_port: 30000, target_port: 22 },
      });
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
  }

for (const theme of ['light', 'dark']) {
  test(`composition board phone ${theme}`, async ({ page }) => {
    const errors = await open(page, 390, theme);
    await expect(page.locator('.cz-phone-row')).toHaveCount(4);
    await expect(page.locator('.cz-phone')).toHaveScreenshot(`board-phone-${theme}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`entry addresses and container entries ${width} ${theme}`, async ({ page }) => {
      const errors = await open(page, width, theme);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const admin = page.locator('.cvm-entry-publications');
      await expect(admin.locator('.cvm-entry-card')).toHaveCount(2);
      await expect(admin).toHaveScreenshot(`entry-publications-${width}-${theme}.png`);
      await admin.getByRole('button', { name: '＋ 发布入口地址' }).click();
      const dialog = page.getByRole('dialog', { name: '发布入口地址' });
      await expect(dialog.getByRole('combobox', { name: '入口机器' })).toContainText('GreenVPS');
      await expect(dialog).toHaveScreenshot(`entry-publication-dialog-${width}-${theme}.png`);
      await dialog.getByRole('combobox', { name: '方式', exact: true }).selectOption('dedicated');
      await expect(dialog.getByRole('combobox', { name: '方式', exact: true })).toHaveValue('dedicated');
      await expect(dialog).toHaveScreenshot(`entry-l3-dialog-${width}-${theme}.png`);
      const crossMachine = dialog.getByRole('checkbox', { name: '允许转发到其他机器' });
      await crossMachine.scrollIntoViewIfNeeded();
      await expect(dialog).toHaveScreenshot(`entry-l3-transport-${width}-${theme}.png`);
      await dialog.getByRole('combobox', { name: 'WireGuard 传输' }).selectOption('phantun');
      await expect(dialog).toContainText('封装为 FakeTCP');
      await expect(dialog).toHaveScreenshot(`entry-phantun-${width}-${theme}.png`);
      await dialog.getByRole('combobox', { name: '传输方式' }).selectOption('xray');
      await expect(dialog).toContainText('Xray 使用加密连接');
      await expect(dialog).toHaveScreenshot(`entry-xray-${width}-${theme}.png`);
      await crossMachine.uncheck();
      await expect(dialog).not.toContainText('跨机器传输');
      await expect(dialog).toContainText('仅转发到入口机器上的实例');
      await dialog.getByRole('button', { name: '取消' }).click();
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}`);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const list = page.getByRole('list', { name: '访问入口' });
      await expect(list.getByRole('listitem')).toHaveCount(3);
      await expect(page.locator('.cvm-flow-col.in')).toHaveScreenshot(`instance-entries-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
  }

for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`network configuration logs ${width} ${theme}`, async ({ page }) => {
    const errors = await open(page, width, theme);
    if (width > 820) {
      await page.locator('[data-pill="cvm-alice"]').click();
      await page.locator('.cz-inspector').getByRole('button', { name: '查看入口网络日志' }).first().click();
    } else {
      await page.getByRole('button', { name: '查看网络配置日志' }).first().click();
    }
    const dialog = page.getByRole('dialog').last();
    await expect(dialog).toContainText('Agent 将继续重试');
    await expect(dialog.getByText('Phantun server started; listening on port 44000')).toBeVisible();
    await expect(page.locator('.cvm-network-log-dialog')).toHaveScreenshot(`network-logs-${width}-${theme}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark'])
    for (const kind of ['容器', '裸机']) {
      test(`instance configuration log control ${kind} ${width} ${theme}`, async ({ page }) => {
        const errors = await open(page, width, theme);
        const scene = kind === '裸机' ? 'baremetal-detail' : 'detail';
        await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
        await page.getByRole('tab', { name: '网络', exact: true }).click();
        // 配置日志在实例小组合台的「网卡」栏头。
        const workspace = page.locator('.cvm-flow-col.mid');
        await expect(workspace).toHaveScreenshot(`instance-log-control-${scene}-${width}-${theme}.png`);
        await workspace.getByRole('button', { name: `查看${kind}网络配置日志` }).click();
        const dialog = page.getByRole('dialog', { name: `${kind}网络配置日志` });
        await expect(dialog.getByRole('list', { name: '网络执行日志' })).toBeVisible();
        expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
        expect(errors).toEqual([]);
      });
    }
