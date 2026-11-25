import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect, type Page } from '@playwright/test';

async function open(page: Page, width: number, theme: string, scene: string) {
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await waitForVisualAssets(page);
  return errors;
}

async function mutations(page: Page) {
  return page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations);
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  expect(await page.locator('.cvm-page').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
}

for (const width of [1440, 1280, 390])
  for (const theme of ['light', 'dark']) {
    test(`instance list links to the entry workspace and saves its draft ${width} ${theme}`, async ({ page }) => {
      const errors = await open(page, width, theme, 'fleet');
      const list = page.getByRole('region', { name: '容器清单' });
      await expect(list.locator('.cvm-list-row')).toHaveCount(7);
      const row = list
        .getByRole('row')
        .filter({ has: page.getByRole('button', { name: 'home-workspace', exact: true }) });
      const ssh = row.getByRole('button', { name: 'TCP 203.0.113.20:30022 → 容器 22 · 已生效', exact: true });
      await expect(ssh).toBeVisible();
      await expect(list).toHaveScreenshot(`instance-list-network-${width}-${theme}.png`);
      await ssh.click();
      await expect(page.getByRole('tab', { name: '网络', exact: true, selected: true })).toBeVisible();
      const entries = page.getByRole('region', { name: '访问入口', exact: true });
      await entries.getByRole('button', { name: '添加入口', exact: true }).click();
      const form = entries.getByRole('form', { name: '添加访问入口' });
      await form
        .getByRole('radiogroup', { name: '入口地址' })
        .locator('input[value="entrypub-hk:203.0.113.20"]')
        .check();
      await form.getByRole('textbox', { name: 'TCP 入口端口' }).fill('30000');
      await form.getByRole('textbox', { name: 'TCP 端口映射（可选）' }).fill('30000 → 22');
      // 预览行写出入口地址和换算后的 SSH 端口。
      const preview = form.locator('.cvm-composer-preview');
      await expect(preview).toContainText('203.0.113.20 · TCP 30000');
      await expect(preview.getByText('· SSH 端口 30000', { exact: true })).toBeVisible();
      await form.getByRole('button', { name: '加入变更', exact: true }).click();
      const draft = page.getByLabel('容器草稿与待发布变更');
      await draft.getByRole('button', { name: '1 处未保存' }).click();
      await expect(draft).toContainText('添加入口 · 203.0.113.20 · TCP 30000；映射 30000 → 22');
      expect(await mutations(page)).toEqual([]);
      await expect(draft).toHaveScreenshot(`entry-draft-${width}-${theme}.png`);
      await draft.getByRole('button', { name: '保存网络变更', exact: true }).click();
      await expect(draft.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
      await expect(draft.getByRole('button', { name: '1 处未保存' })).toHaveCount(0);
      expect(await mutations(page)).toMatchObject([
        {
          method: 'PUT',
          path: '/cvm/cvm-fleet-0/network-edit',
          entry_additions: [
            {
              publication_id: 'entrypub-hk',
              address: '203.0.113.20',
              mode: {
                kind: 'protocols',
                tcp: {
                  kind: 'forward',
                  ports: { kind: 'ranges', ranges: [{ start: 30000, end: 30000 }] },
                  mappings: [{ listen: { start: 30000, end: 30000 }, target: { start: 22, end: 22 } }],
                },
                udp: { kind: 'disabled' },
                icmp: false,
                other_ip: false,
              },
            },
          ],
        },
      ]);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });
  }

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`network resource table and instance entries ${width} ${theme}`, async ({ page }) => {
      const errors = await open(page, width, theme, 'list');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const admin = page.getByRole('region', { name: '访问入口地址', exact: true });
      const table = admin.getByRole('table', { name: '访问入口地址', exact: true });
      await expect(table.getByRole('row')).toHaveCount(3);
      await expect(table.getByRole('row').filter({ hasText: '香港共享入口' })).toContainText('共享地址');
      await expect(table.getByRole('row').filter({ hasText: '香港独立地址' })).toContainText('独占地址');
      await expect(admin).toHaveScreenshot(`entry-publications-table-${width}-${theme}.png`);
      await admin.getByRole('button', { name: '添加入口地址', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '添加入口地址' });
      await expect(dialog.getByRole('combobox', { name: '入口机器' })).toContainText('GreenVPS');
      await expect(dialog).toHaveScreenshot(`entry-publication-dialog-${width}-${theme}.png`);
      await dialog.getByRole('combobox', { name: '方式', exact: true }).selectOption('dedicated');
      await expect(dialog.getByRole('combobox', { name: '方式', exact: true })).toHaveValue('dedicated');
      await dialog.getByRole('textbox', { name: '保留 TCP 端口' }).fill('22,2222,8000-8100');
      await expect(dialog.getByRole('button', { name: '保留检测到的 SSH 端口：22' })).toBeVisible();
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
      await expect(dialog.getByRole('group', { name: '跨机器传输' })).toHaveCount(0);
      await expect(dialog).toContainText('仅转发到入口机器上的实例');
      await dialog.getByRole('button', { name: '取消', exact: true }).click();
      await expect(dialog).toHaveCount(0);
      await page.getByRole('tab', { name: '容器', exact: true }).click();
      await page.getByRole('button', { name: 'home-workspace', exact: true }).click();
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const list = page.getByRole('list', { name: '访问入口列表' });
      await expect(list.getByRole('listitem')).toHaveCount(3);
      await expect(list).toHaveScreenshot(`instance-entry-list-${width}-${theme}.png`);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });
  }

for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`entry network configuration logs ${width} ${theme}`, async ({ page }) => {
    const errors = await open(page, width, theme, 'detail');
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const entry = page
      .getByRole('list', { name: '访问入口列表' })
      .getByRole('listitem')
      .filter({ hasText: '203.0.113.20:30022' });
    await entry.getByRole('button', { name: '查看入口网络日志', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '入口网络日志', exact: true });
    await expect(dialog).toContainText('Agent 将继续重试');
    await expect(dialog.getByText('Phantun server started; listening on port 44000')).toBeVisible();
    await expect(dialog).toHaveScreenshot(`entry-network-logs-${width}-${theme}.png`);
    await noOverflow(page);
    expect(errors).toEqual([]);
  });
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark'])
    for (const scene of ['detail', 'baremetal-detail']) {
      test(`NIC section configuration log control ${scene} ${width} ${theme}`, async ({ page }) => {
        const errors = await open(page, width, theme, scene);
        await page.getByRole('tab', { name: '网络', exact: true }).click();
        const nics = page.getByRole('region', { name: '网卡', exact: true });
        const trigger = nics.getByRole('button', { name: '查看实例网络配置日志', exact: true });
        await expect(trigger).toBeVisible();
        await expect(nics.locator(':scope > header')).toHaveScreenshot(
          `nic-log-control-${scene}-${width}-${theme}.png`,
        );
        await trigger.click();
        const dialog = page.getByRole('dialog', { name: '实例网络配置日志', exact: true });
        await expect(dialog.getByRole('list', { name: '网络执行日志' })).toBeVisible();
        await noOverflow(page);
        expect(errors).toEqual([]);
      });
    }
