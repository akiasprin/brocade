import { test, expect, type Page } from '@playwright/test';

async function load(page: Page, width: number, theme: string, scene: string, role = 'system-admin') {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}&role=${role}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}
async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  expect(await page.locator('.cvm-page').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
}
for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    for (const scene of ['list', 'new', 'detail', 'empty', 'unknown'])
      test(`${scene} ${width} ${theme}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await load(page, width, theme, scene);
        await noOverflow(page);
        await expect(page).toHaveScreenshot(`cvm-${scene}-${width}-${theme}.png`);
        if (scene === 'new') {
          const interfaces = page
            .locator('.cvm-section')
            .filter({ has: page.getByRole('heading', { name: '网络', exact: true }) });
          await expect(interfaces).toHaveScreenshot(`cvm-create-interfaces-${width}-${theme}.png`);
        }
        expect(errors).toEqual([]);
      });
    test(`quota dialog ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'list');
      await page.getByRole('tab', { name: '额度', exact: true }).click();
      await page.getByRole('button', { name: '编辑额度', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '容器 用户额度' });
      await expect(dialog).toHaveScreenshot(`cvm-quota-${width}-${theme}.png`);
    });
    test(`attach dialog ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await expect(page.getByText('两端已连接', { exact: true })).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      const mount = page.getByRole('button', { name: '＋ 挂载网卡', exact: true });
      // The slot sits below the NICs on phones; pin the backdrop scroll after all card data is ready.
      await mount.evaluate(button => button.scrollIntoView({ block: 'center', behavior: 'instant' }));
      await mount.click();
      const dialog = page.getByRole('dialog', { name: '挂载网卡' });
      await expect(dialog).toBeVisible();
      await expect(dialog).toHaveScreenshot(`cvm-attach-${width}-${theme}.png`);
      await dialog.getByRole('button', { name: '挂载并分配地址' }).click();
      await expect(dialog).toBeHidden();
      await expect(page.locator('.cvm-attached')).toHaveCount(3);
      await expect(page.getByRole('status').filter({ hasText: '待执行' })).toBeVisible();
    });
  }

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light'])
    for (const action of ['restart', 'reinstall', 'configure'])
      test(`lifecycle ${action} ${width} ${theme}`, async ({ page }) => {
        await load(page, width, theme, action === 'configure' ? 'stopped' : 'detail');
        if (action === 'restart') await page.getByRole('button', { name: '重启', exact: true }).click();
        else {
          await page.getByRole('button', { name: '实例更多操作' }).click();
          await page.getByRole('menuitem', { name: action === 'configure' ? /修改配置/ : /重装系统/ }).click();
        }
        const dialog = page.getByRole('dialog', {
          name: action === 'restart' ? '重启实例' : action === 'configure' ? '修改配置' : '重装系统',
          exact: true,
        });
        await expect(dialog).toBeVisible();
        await noOverflow(page);
        const surface = dialog.locator('.dialog-surface');
        expect(await surface.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        await expect(dialog).toHaveScreenshot(`cvm-${action}-${width}-${theme}.png`);
        await dialog.getByRole('button', { name: '取消', exact: true }).click();
        await expect(dialog).toBeHidden();
      });
test('create keeps images separate and initializes only the independent management NIC', async ({ page }) => {
  await load(page, 1440, 'light', 'new');
  await page.getByLabel('实例名称', { exact: true }).fill('three-interface-workspace');
  await page
    .getByLabel('SSH 公钥', { exact: true })
    .fill('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEiBcpQEzqzjADp8VXvOigHnqLh5L5xP76cwoCD8YMK5 fixture');
  await expect(
    page.getByText(
      '创建时生成主网卡，通过宿主机 NAT 访问公网，优先级最低。创建后可挂载已授权的业务网卡，并单独配置优先路由。',
    ),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: /Debian 13/ })).not.toContainText('MiB');
  await page.getByRole('button', { name: '创建实例', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm\/instance\/cvm-created/);
  await expect(page.getByRole('status').filter({ hasText: '待执行' })).toBeVisible();
  await page.getByRole('tab', { name: '网络', exact: true }).click();
  await expect(page.locator('.cvm-attached')).toHaveCount(1);
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light'])
    test(`delete removes the entire instance directly ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.setViewportSize({ width, height: width === 1440 ? 600 : width === 390 ? 500 : 480 });
      await page.getByRole('button', { name: '删除', exact: true }).click();
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await expect(page.getByRole('status').filter({ hasText: '待执行' })).toBeVisible();
      await expect(page.getByRole('button', { name: '删除', exact: true })).toBeDisabled();
      await expect(page.getByRole('button', { name: '终端', exact: true })).toBeDisabled();
      const changes = await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations);
      expect(changes).toHaveLength(1);
      expect(changes[0]).toMatchObject({ action: 'delete', delete_data: true });
      await noOverflow(page);
      await expect(page).toHaveScreenshot(`cvm-delete-${width}-${theme}.png`);
    });
test('read only and expired capability cannot mutate, final contact cannot detach', async ({ page }) => {
  await load(page, 390, 'dark', 'detail', 'readonly');
  await page.getByRole('tab', { name: '网络', exact: true }).click();
  await expect(page.getByRole('button', { name: '＋ 挂载网卡', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0);
  await expect(page.locator('.cvm-attached').first().getByText('已脱敏', { exact: true })).toHaveCount(1);
  await expect(page.locator('.cvm-attached').nth(1).getByText('已脱敏', { exact: true })).toHaveCount(2);
  await expect(page.locator('.cvm-route-table code')).toHaveText(['已脱敏', '已脱敏']);
  await expect(page.locator('.cvm-detail')).not.toContainText(/192\.0\.2\.21|10\.60\.0\.2|203\.0\.113\.20/);
  await expect(page.locator('.cvm-attached').first().getByRole('button', { name: '卸载' })).toHaveCount(0);
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`long names remain inside the composition board ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'long');
      const card = page.locator(width <= 820 ? '.cz-phone-row' : '.cz-pill');
      const name = card.locator(width <= 820 ? '.cz-phone-name > b' : ':scope > b');
      // Desktop connection handles extend outside the pill; its name must stay inside it.
      const cardBox = (await card.boundingBox())!;
      const nameBox = (await name.boundingBox())!;
      expect(nameBox.x).toBeGreaterThanOrEqual(cardBox.x);
      expect(nameBox.x + nameBox.width).toBeLessThanOrEqual(cardBox.x + cardBox.width);
      if (width <= 820) expect(await card.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await expect(name).toHaveCSS('text-overflow', 'ellipsis');
      await noOverflow(page);
      await expect(card).toHaveScreenshot(`cvm-long-${width}-${theme}.png`);
    });
  }

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`monitoring real layout ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.setViewportSize({ width, height: 1400 });
      const panel = page.getByRole('region', { name: '容器 监控' });
      await expect(panel.getByText('持续采样', { exact: true })).toBeVisible();
      await page.getByRole('tab', { name: '监控与诊断' }).click();
      await expect(panel.locator('.cvm-diagnostic-card')).toHaveCount(4);
      for (const chart of await panel.locator('.cvm-diagnostic-card > .cvm-section-body > .history-chart-card').all()) {
        await chart.scrollIntoViewIfNeeded();
        await expect(chart.locator('canvas')).toHaveCount(1);
      }
      await expect(panel.locator('canvas:visible')).toHaveCount(4);
      await noOverflow(page);
      await page.getByRole('tab', { name: '监控与诊断' }).scrollIntoViewIfNeeded();
      await expect(page).toHaveScreenshot(`cvm-monitoring-${width}-${theme}.png`);
      const cards = panel.locator('.cvm-diagnostic-card');
      for (let i = 0; i < 4; i++)
        await expect(cards.nth(i)).toHaveScreenshot(`cvm-monitor-card-${i}-${width}-${theme}.png`);
      await panel.getByLabel('监控网卡').selectOption('nic-business');
      await expect(page).toHaveScreenshot(`cvm-monitoring-nic-${width}-${theme}.png`);
    });
  }
test('monitoring keeps stale values explicitly historical', async ({ page }) => {
  await load(page, 390, 'light', 'stale');
  await page.setViewportSize({ width: 390, height: 1400 });
  const panel = page.getByRole('region', { name: '容器 监控' });
  await expect(panel.getByText('读数已过期', { exact: true })).toBeVisible();
  await expect(panel.getByLabel('最后实例读数')).toBeVisible();
  await expect(panel).toHaveScreenshot('cvm-monitoring-stale-390-light.png');
});

test('diagnostic details keep real scope and unsupported readings on mobile', async ({ page }) => {
  await load(page, 390, 'dark', 'detail');
  await page.getByRole('tab', { name: '监控与诊断' }).click();
  const panel = page.getByRole('region', { name: '容器 监控' });
  for (const name of ['CPU', '内存', '系统盘', '网络'])
    await panel.getByText(`展开${name}诊断`, { exact: true }).click();
  await expect(panel.getByText('TCP 已建立连接', { exact: true })).toBeVisible();
  await expect(panel.getByText('CPU 0', { exact: true })).toBeVisible();
  await noOverflow(page);
  await page.setViewportSize({ width: 390, height: 1600 });
  for (let i = 0; i < 4; i++)
    await expect(panel.locator('.cvm-diagnostic-card').nth(i)).toHaveScreenshot(
      `cvm-diagnostics-expanded-${i}-390-dark.png`,
    );
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`network quota simple and detailed ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'quota-network');
      await page.getByRole('tab', { name: '额度', exact: true }).click();
      await page.getByRole('button', { name: '编辑额度', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '容器 用户额度' });
      const editor = dialog.getByRole('region', { name: '香港 HKIX 网络额度' });
      await expect(editor.getByText('最多可设 106,976 条')).toBeVisible();
      await expect(editor.getByText('计算：宿主可分配 126,976 − 其他用户已分配 20,000 = 106,976 条')).toBeVisible();
      await expect(dialog.getByRole('button', { name: '保存额度', exact: true })).toBeEnabled();
      await editor.scrollIntoViewIfNeeded();
      await expect(editor).toHaveScreenshot(`cvm-network-quota-${width}-${theme}.png`);
      await editor.getByText('高级设置', { exact: true }).click();
      await expect(editor.locator('details').first()).toHaveScreenshot(
        `cvm-network-quota-advanced-${width}-${theme}.png`,
      );
      await editor.getByText('高级设置', { exact: true }).click();
      await editor.getByText('计算详情', { exact: true }).click();
      await expect(editor.getByText(/当前状态占用 2,500/)).toBeVisible();
      await expect(editor.locator('details').nth(1)).toHaveScreenshot(
        `cvm-network-quota-details-${width}-${theme}.png`,
      );
      await noOverflow(page);
      await editor.getByLabel('香港 HKIX 网络连接额度').fill('120000');
      await page.clock.runFor(300);
      await expect(editor.getByText('超出可分配额度 13,024 条。')).toBeVisible();
      await expect(dialog.getByRole('button', { name: '保存额度', exact: true })).toBeDisabled();
      await editor.getByLabel('香港 HKIX 网络连接额度').fill('100000');
      await page.clock.runFor(300);
      await expect(dialog.getByRole('button', { name: '保存额度', exact: true })).toBeEnabled();
      await dialog.getByRole('button', { name: '保存额度', exact: true }).click();
      const changes = await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations);
      expect(changes.at(-1)).toMatchObject({
        host_network_quotas: [
          {
            node_id: 'hk-hkix',
            expected_revision: 2,
            budget_revision: 7,
            limits: { max_states: 100000, new_per_second: 1000, burst: 2000 },
          },
        ],
      });
    });
  }
