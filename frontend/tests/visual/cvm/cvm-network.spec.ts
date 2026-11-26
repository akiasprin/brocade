import { fitViewportTo, waitForVisualAssets } from '../helpers/visual-ready';
import { expect, test, type Page } from '@playwright/test';
async function open(page: Page, width: number, theme: string, detail = true) {
  page.on('pageerror', error => console.error('preview page error:', error.message));
  page.on('console', message => {
    if (message.type() === 'error') console.error('preview console:', message.text());
  });
  page.on('requestfailed', request => console.error('preview request:', request.url(), request.failure()?.errorText));
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${detail ? 'detail' : 'list'}&theme=${theme}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await waitForVisualAssets(page);
  if (detail) {
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const editor = page.locator('.cvm-network-edit');
    await expect(editor.getByTitle('两端已连接', { exact: true })).toBeVisible();
    // Entry and route data load separately from the NIC state.
    await expect(editor.getByRole('list', { name: '入站方式列表', exact: true }).getByRole('listitem')).toHaveCount(3);
    await expect(editor.getByRole('table', { name: '目标路由', exact: true })).toBeVisible();
    await waitForVisualAssets(page);
  }
}
async function fits(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}
async function mutations(page: Page): Promise<unknown[]> {
  return page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations);
}
for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`outbound methods and independent routes ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      const editor = page.locator('.cvm-network-edit');
      const nics = editor.getByRole('region', { name: '出站方式', exact: true });
      const routes = editor.getByRole('region', { name: '路由', exact: true });
      const routeTable = routes.getByRole('table', { name: '目标路由', exact: true });
      const destinations = routeTable.getByRole('button', { name: /^编辑路由 / });
      const nicMenu = nics.getByRole('button', { name: '香港 · 公网出口 出站方式操作', exact: true });
      const detach = page.getByRole('menuitem', { name: /^移除出站方式/ });
      await expect(destinations).toHaveCount(2);
      // 仍被路由引用的出站方式不能移除，菜单里写明原因。
      await nicMenu.click();
      await expect(detach).toBeDisabled();
      await expect(detach).toContainText('请先在路由中移除此出站方式的引用');
      await page.keyboard.press('Escape');
      await expect(detach).toHaveCount(0);
      if (width === 1440) {
        // 宽屏下入站与出站两块面板并排，路由面板占满整行。
        const entries = editor.getByRole('region', { name: '入站方式', exact: true });
        const entryPanel = (await entries.boundingBox())!;
        const nicPanel = (await nics.boundingBox())!;
        const grid = (await editor.boundingBox())!;
        const routeArea = (await routes.boundingBox())!;
        expect(entryPanel.y).toBe(nicPanel.y);
        expect(routeArea.width).toBe(grid.width);
      }
      await fitViewportTo(page, editor);
      await expect(editor).toHaveScreenshot(`network-flow-${width}-${theme}.png`);
      await expect(nics).toHaveScreenshot(`allocated-nics-${width}-${theme}.png`);
      await expect(routes).toHaveScreenshot(`route-table-${width}-${theme}.png`);
      await fits(page);

      const priorities = routes.getByRole('list', { name: '出口优先级', exact: true });
      await priorities.getByRole('button', { name: /香港 · 公网出口 · IPv4/ }).click();
      await routes.getByRole('button', { name: '移除此出口的路由与优先级', exact: true }).click();
      await expect(destinations).toHaveCount(0);
      await expect(priorities.getByRole('listitem')).toHaveCount(0);
      // 路由和优先级里的引用都清掉后，同一份本地草稿里即可移除。
      await nicMenu.click();
      await expect(detach).toBeEnabled();
      await page.keyboard.press('Escape');
      const draft = page.getByLabel('容器草稿与待发布变更', { exact: true });
      await draft.getByRole('button', { name: '查看配置差异', exact: true }).click();
      await expect(draft.getByRole('status')).toHaveText('服务端配置检查通过。保存后进入待发布修订。');
      expect(await mutations(page)).toEqual([]);
      await draft.getByRole('button', { name: '保存网络变更', exact: true }).click();
      await expect(draft.getByRole('button', { name: '1 个容器待发布', exact: true })).toBeVisible();
      await expect(draft.getByRole('button', { name: '1 处未保存', exact: true })).toHaveCount(0);
      await expect(nicMenu).toBeVisible();
      const writes = await mutations(page);
      expect(writes).toMatchObject([
        {
          method: 'PUT',
          path: '/cvm/cvm-alice/network-edit',
          expected_revision: 1,
          routes: { routes: [], priorities: [], dns: [] },
        },
      ]);
      expect(writes[0]).not.toHaveProperty('detach_interface_ids');
    });
    test(`publication and revisions ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, false);
      await page.getByRole('tab', { name: '网络' }).click();
      await expect(
        page.locator('.cvm-network-publications').getByRole('heading', { name: '出站方式', exact: true }),
      ).toBeVisible();
      await expect(page.locator('.cvm-network-publications')).toHaveScreenshot(`publications-${width}-${theme}.png`);
      await page.getByRole('button', { name: '添加出站方式' }).click();
      await expect(page.getByRole('heading', { level: 1, name: '添加出站方式' })).toBeVisible();
      await expect(page.getByRole('combobox', { name: '提供机器' })).toContainText('GreenVPS');
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await fitViewportTo(page, page.locator('.cvm-resource-editor'));
      await expect(page.locator('.cvm-resource-editor')).toHaveScreenshot(`publish-nic-${width}-${theme}.png`);
      await fits(page);
    });
    for (const [button, title, file] of [
      ['更换连接方式', '更换出站连接方式', 'nic-transport'],
      ['应用新规格', '应用出站方式新规格', 'nic-version'],
    ] as const) {
      test(`${file} ${width} ${theme}`, async ({ page }) => {
        await open(page, width, theme);
        const nics = page.getByRole('region', { name: '出站方式', exact: true });
        await nics.getByRole('button', { name: '香港 · 公网出口 出站方式操作', exact: true }).click();
        await page.getByRole('menuitem', { name: new RegExp(`^${button}`) }).click();
        const dialog = page.getByRole('dialog', { name: title, exact: true });
        await expect(dialog).toBeVisible();
        await expect(dialog).toHaveScreenshot(`${file}-${width}-${theme}.png`);
        await fits(page);
      });
    }
  }

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`publication dependencies and bulk update ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, false);
      await page.getByRole('tab', { name: '网络' }).click();
      const outbound = page.getByRole('table', { name: '出站方式', exact: true });
      // 删除收在出站方式行的 ⋯ 菜单里。
      await outbound.getByRole('button', { name: '香港 · 公网出口 更多操作', exact: true }).click();
      await page.getByRole('menuitem', { name: /^删除网络/ }).click();
      const remove = page.getByRole('dialog', { name: '删除网络', exact: true });
      await expect(remove.getByText('home-workspace')).toBeVisible();
      await expect(remove.getByRole('button', { name: '删除网络', exact: true })).toBeDisabled();
      await expect(remove).toHaveScreenshot(`publication-dependencies-${width}-${theme}.png`);
      await remove.getByRole('button', { name: '取消', exact: true }).click();
      await outbound.getByRole('button', { name: '香港 · 公网出口', exact: true }).click();
      await expect(page.getByRole('heading', { level: 1, name: '香港 · 公网出口', exact: true })).toBeVisible();
      const derived = page.getByRole('region', { name: '已分配出站方式', exact: true });
      const nics = derived.getByRole('table', { name: '已分配出站方式', exact: true });
      await expect(nics.getByText('v1', { exact: true })).toBeVisible();
      await expect(nics.getByText('旧规格', { exact: true })).toBeVisible();
      const grants = page.getByRole('group', { name: '授权使用者', exact: true });
      await expect(grants.getByText('1 个出站方式', { exact: true })).toBeVisible();
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await fitViewportTo(page, page.locator('.cvm-resource-editor'));
      await expect(page.locator('.cvm-resource-editor')).toHaveScreenshot(`edit-network-${width}-${theme}.png`);
      await derived.getByRole('button', { name: '批量应用 v2 · 1 个', exact: true }).click();
      const bulk = page.getByRole('dialog', { name: '批量应用出站方式规格', exact: true });
      const selection = bulk.getByRole('checkbox');
      await expect(selection).toHaveCount(1);
      await expect(selection).toBeChecked();
      await expect(bulk.getByRole('button', { name: '保存变更', exact: true })).toBeEnabled();
      await expect(bulk).toHaveScreenshot(`bulk-nic-version-${width}-${theme}.png`);
      await fits(page);
    });
  }
