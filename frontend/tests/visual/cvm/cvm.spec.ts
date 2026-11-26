import { fitViewportTo, waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect, type Page } from '@playwright/test';

async function load(page: Page, width: number, theme: string, scene: string, role = 'system-admin') {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}&role=${role}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await waitForVisualAssets(page);
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
          const interfaces = page.getByRole('region', { name: '网络与访问', exact: true });
          await expect(interfaces).toHaveScreenshot(`cvm-create-interfaces-${width}-${theme}.png`);
        }
        expect(errors).toEqual([]);
      });
    test(`access dialog ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.getByRole('button', { name: '使用授权', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '容器使用授权' });
      const surface = dialog.locator('.cvm-access-dialog');
      await expect(dialog).toBeInViewport();
      await expect(surface).toBeInViewport();
      expect(await surface.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
      // Capture the surface that this case owns against a stable backdrop. Its translucent material
      // otherwise includes unrelated live page pixels even in an element screenshot.
      await page.locator('.forge').evaluate(element => {
        element.style.visibility = 'hidden';
      });
      await expect(surface).toHaveScreenshot(`cvm-access-${width}-${theme}.png`);
    });
    test(`inline outbound method stages before saving ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const nics = page.getByRole('region', { name: '出站方式', exact: true });
      // 出站方式列表里还有一行「还可添加」提示，只数真实配置行。
      const rows = nics.locator('.cvm-nrows > .cvm-nrow:not(.cvm-nrow-hint, [data-draft])');
      // 连通状态写在状态徽标的提示里，可见文字只说配置是否已应用。
      await expect(nics.getByTitle('两端已连接', { exact: true })).toHaveText('已应用');
      const contact = rows.filter({ hasText: '默认出站' });
      await expect(contact).toHaveCount(1);
      // 默认出站不能移除，所以没有出站方式操作菜单。
      await expect(contact.getByRole('button', { name: /出站方式操作$/ })).toHaveCount(0);
      await expect(rows.filter({ hasNotText: '默认出站' }).getByRole('button', { name: /出站方式操作$/ })).toHaveCount(
        1,
      );
      await expect(rows).toHaveCount(2);
      await nics.getByRole('button', { name: '添加出站方式', exact: true }).click();
      const form = nics.getByRole('form', { name: '添加出站方式' });
      const add = form.getByRole('button', { name: '加入变更', exact: true });
      await expect(add).toBeDisabled();
      await form.getByRole('radiogroup', { name: '网络' }).locator('input[value="network-hk"]').check();
      await expect(add).toBeEnabled();
      await expect(form).toHaveScreenshot(`cvm-inline-mount-${width}-${theme}.png`);
      await add.click();
      await expect(form).toHaveCount(0);
      await expect(nics.locator('.cvm-nrow[data-draft]')).toHaveCount(1);
      expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual(
        [],
      );
      const draft = page.getByLabel('容器草稿与待发布变更');
      await draft.getByRole('button', { name: '保存网络变更', exact: true }).click();
      await expect(nics.locator('.cvm-nrow[data-draft]')).toHaveCount(0);
      await expect(rows).toHaveCount(3);
      await expect(draft.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
      await noOverflow(page);
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
  await page.getByLabel('容器名称', { exact: true }).fill('three-interface-workspace');
  const network = page.getByRole('region', { name: '网络与访问', exact: true });
  await expect(network.locator('.cvm-chip')).toContainText('默认出站宿主 NAT');
  await expect(network.getByText(/^创建时生成，优先级最低/)).toBeVisible();
  // 未选使用者时没有可预填的网络，只提示稍后配置；核对预填网络的说明只在选了网络后出现。
  await expect(network.getByText(/^选择获授权的使用者后，可以选配入站方式和出站方式/)).toBeVisible();
  await expect(network.getByText(/到新容器的「网络」中核对并保存/)).toHaveCount(0);
  await expect(page.getByRole('region', { name: '创建后的宿主分配' })).toBeVisible();
  await expect(page.getByRole('button', { name: /Debian 13/ })).not.toContainText('MiB');
  await page.getByRole('button', { name: '保存创建变更', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm\/instance\/cvm-created$/);
  await expect(page.locator('.fg-draft').getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
  const create = await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations[0]);
  expect(create).toMatchObject({
    interfaces: [{ source_id: 'public', purpose: 'contact', default_ipv4: false, default_ipv6: false }],
  });
});

test('create from copy puts fresh network choices in the new container draft', async ({ page }) => {
  await load(page, 1440, 'light', 'new-copy');
  await page.getByLabel('复制现有容器').selectOption({ label: 'home-workspace' });
  await expect(page.getByLabel('使用者', { exact: true })).toHaveValue('');
  await page.getByLabel('使用者', { exact: true }).selectOption({ label: 'alice' });
  await page.getByLabel('入站方式').selectOption({ label: '香港共享入口 · 203.0.113.20 · 端口 30000-30999' });
  await page.getByLabel('入口端口').fill('30054');
  await expect(page.getByText(/到新容器的「网络」中核对并保存/)).toBeVisible();
  await expect(
    page.locator('.cvm-create-nic-choices').getByText('香港 · 公网出口 · WireGuard', { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole('region', { name: '创建后的宿主分配' }).getByText('4 核 / 8 核')).toBeVisible();
  await noOverflow(page);
  await expect(page).toHaveScreenshot('cvm-new-copy-1440-light.png');
  await page.getByRole('button', { name: '保存创建变更', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm\/instance\/cvm-created\?tab=network$/);
  const draft = page.locator('.fg-draft');
  await expect(draft.getByRole('button', { name: '2 处未保存' })).toBeVisible();
  await expect(draft.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
  await draft.getByRole('button', { name: '2 处未保存' }).click();
  await expect(draft.getByText(/添加入站方式.*30054/)).toBeVisible();
  await expect(draft.getByText(/添加出站方式.*network-hk/)).toBeVisible();
  await draft.getByRole('button', { name: '保存网络变更', exact: true }).click();
  await expect(draft.getByRole('button', { name: '保存网络变更', exact: true })).toHaveCount(0);
  await expect(draft.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light'])
    test(`typed deletion can be cancelled and withdrawn in the pending bar ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.setViewportSize({ width, height: width === 1440 ? 600 : width === 390 ? 500 : 480 });
      const more = page.getByRole('button', { name: '实例更多操作' });
      const mutations = () => page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations);
      const confirmation = page.getByRole('dialog', { name: '删除容器？' });
      const name = confirmation.getByRole('textbox', { name: '输入完整容器名称以确认删除' });
      const save = confirmation.getByRole('button', { name: '保存删除变更', exact: true });
      await more.click();
      await page.getByRole('menuitem', { name: /^删除/ }).click();
      await expect(confirmation).toBeVisible();
      await name.fill('home-work');
      await expect(save).toBeDisabled();
      await name.fill('home-workspace');
      await expect(save).toBeEnabled();
      expect(await mutations()).toEqual([]);
      await expect(confirmation).toHaveScreenshot(`cvm-delete-confirm-name-${width}-${theme}.png`);
      await confirmation.getByRole('button', { name: '取消', exact: true }).click();
      expect(await mutations()).toEqual([]);
      await more.click();
      await page.getByRole('menuitem', { name: /^删除/ }).click();
      await expect(name).toBeEmpty();
      await name.fill('home-workspace');
      await save.click();
      await expect(confirmation).toHaveCount(0);
      const draft = page.getByLabel('容器草稿与待发布变更');
      await draft.getByRole('button', { name: '1 个容器待发布' }).click();
      await expect(page.locator('.cvm-page-head').getByRole('status')).toHaveText('待发布');
      await expect(more).toHaveCount(0);
      await expect(page.getByRole('button', { name: '终端', exact: true })).toHaveCount(0);
      expect(await mutations()).toMatchObject([{ action: 'delete', delete_data: true }]);
      await noOverflow(page);
      await expect(draft).toHaveScreenshot(`cvm-delete-pending-bar-${width}-${theme}.png`);
      await draft.getByRole('button', { name: '撤回', exact: true }).click();
      const undo = page.getByRole('dialog', { name: '撤回待发布变更', exact: true });
      await expect(undo).toBeVisible();
      expect(await mutations()).toHaveLength(1);
      await undo.getByRole('button', { name: '确认撤回', exact: true }).click();
      await expect(undo).toHaveCount(0);
      await expect(page.getByRole('button', { name: '终端', exact: true })).toBeEnabled();
      expect(await mutations()).toMatchObject([{ action: 'delete' }, { action: 'undo_pending' }]);
    });
test('read-only detail conceals access and route details and offers no network mutations', async ({ page }) => {
  await load(page, 390, 'dark', 'detail', 'readonly');
  await page.getByRole('tab', { name: '网络', exact: true }).click();
  const panel = page.getByRole('tabpanel', { name: '网络', exact: true });
  const nics = panel.getByRole('region', { name: '出站方式', exact: true });
  await expect(nics.getByRole('button', { name: '添加出站方式', exact: true })).toHaveCount(0);
  await expect(nics.getByRole('button', { name: /出站方式操作$/ })).toHaveCount(0);
  const rows = nics.getByRole('list', { name: '出站方式列表' }).getByRole('listitem');
  await expect(rows).toHaveCount(2);
  for (const row of await rows.all()) await expect(row).toContainText('已脱敏');
  const entries = panel.getByRole('region', { name: '入站方式', exact: true });
  await expect(entries).toContainText('访问地址已脱敏。');
  await expect(entries.getByRole('button', { name: '添加入站方式', exact: true })).toHaveCount(0);
  const routes = panel.getByRole('region', { name: '路由' });
  await expect(routes).toContainText('当前权限不显示目标网段、出口和 DNS 等路由详情。');
  await expect(routes.getByRole('table', { name: '目标路由' })).toHaveCount(0);
  await expect(panel).not.toContainText(/192\.0\.2\.21|10\.60\.0\.2|203\.0\.113\.20/);
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`long names remain inside the list identity cell ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'long');
      const name = page.getByRole('button', { name: '这是一个用于验证长名称边界的 CVM 实例', exact: true });
      const row = page.getByRole('row').filter({ has: name });
      const cell = row.locator('.cvm-list-identity');
      await expect(name).toBeVisible();
      const cellBox = (await cell.boundingBox())!;
      const nameBox = (await name.boundingBox())!;
      expect(nameBox.x).toBeGreaterThanOrEqual(cellBox.x);
      expect(nameBox.x + nameBox.width).toBeLessThanOrEqual(cellBox.x + cellBox.width + 1);
      await expect(name).toHaveCSS('text-overflow', 'ellipsis');
      await noOverflow(page);
      await expect(row).toHaveScreenshot(`cvm-list-long-name-${width}-${theme}.png`);
    });
  }

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`monitoring real layout ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail');
      await page.setViewportSize({ width, height: 1400 });
      const panel = page.getByRole('region', { name: '容器 监控' });
      await page.getByRole('tab', { name: '监控与诊断' }).click();
      await expect(panel.locator('.cvm-monitor-sampling')).toContainText('持续采样');
      await expect(panel.locator('.cvm-diagnostic-card')).toHaveCount(4);
      for (const chart of await panel.locator('.cvm-diagnostic-card > .cvm-chart-flush > .history-chart-card').all()) {
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
  await page.getByRole('tab', { name: '监控与诊断' }).click();
  const panel = page.getByRole('region', { name: '容器 监控' });
  await expect(panel.locator('.cvm-monitor-sampling')).toContainText('读数已过期');
  // 读数过期时历史曲线仍然保留，只把状态标成过期。
  await expect(panel.locator('.cvm-diagnostic-card')).toHaveCount(4);
  await fitViewportTo(page, panel);
  await expect(panel).toHaveScreenshot('cvm-monitoring-stale-390-light.png');
});

test('diagnostic details keep real scope and unsupported readings on mobile', async ({ page }) => {
  await load(page, 390, 'dark', 'detail');
  await page.getByRole('tab', { name: '监控与诊断' }).click();
  const panel = page.getByRole('region', { name: '容器 监控' });
  for (const name of ['CPU', '内存', '系统盘', '网络']) await panel.getByText(`${name} 诊断`, { exact: true }).click();
  await expect(panel.getByText('TCP 已建立连接', { exact: true })).toBeVisible();
  await expect(panel.getByText('CPU 0', { exact: true })).toBeVisible();
  await noOverflow(page);
  await page.setViewportSize({ width: 390, height: 1600 });
  for (let i = 0; i < 4; i++)
    await expect(panel.locator('.cvm-diagnostic-card').nth(i)).toHaveScreenshot(
      `cvm-diagnostics-expanded-${i}-390-dark.png`,
    );
});

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`managed password ${width} ${theme}`, async ({ page }) => {
      await load(page, width, theme, 'detail', 'user');
      const reveal = page.getByRole('button', { name: '查看登录密码' });
      await reveal.scrollIntoViewIfNeeded();
      await reveal.click();
      await expect(page.getByRole('textbox', { name: '容器登录密码' })).toHaveValue('F'.repeat(32));
      await expect(page.locator('.cvm-password-row')).toHaveScreenshot(`cvm-password-${width}-${theme}.png`);
      await noOverflow(page);
      await page.getByRole('button', { name: '隐藏密码' }).click();
      await expect(page.getByRole('textbox', { name: '容器登录密码' })).toHaveCount(0);
      await expect(page.getByRole('button', { name: '使用授权' })).toHaveCount(0);
      await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0);
    });
  }
