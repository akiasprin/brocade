import { waitForVisualAssets } from '../helpers/visual-ready';
import { test, expect } from '@playwright/test';

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light'])
    for (const scene of ['hosts', 'empty'])
      test(`host tab ${scene} ${width} ${theme}`, async ({ page }) => {
        await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
        await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
        await page.setViewportSize({ width, height: 1000 });
        await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
        const board = page.getByRole('tab', { name: '容器', exact: true });
        const hosts = page.getByRole('tab', { name: '宿主', exact: true });
        await expect(board).toHaveAttribute('aria-selected', 'true');
        await hosts.click();
        await expect(page.getByRole('tabpanel', { name: '宿主', exact: true })).toBeVisible();
        await expect(page.getByText('能力报告新鲜', { exact: true })).toHaveCount(0);
        await expect(page.getByText('等待新的能力报告', { exact: true })).toHaveCount(0);
        await expect(page.getByRole('button', { name: /查看.*实例/ })).toHaveCount(0);
        await waitForVisualAssets(page);
        if (scene === 'hosts') {
          const table = page.getByRole('table', { name: '宿主' });
          await expect(table.getByRole('rowheader')).toHaveCount(4);
          await expect(table.getByText('会话 2 / 3')).toBeVisible();
          await expect(table.getByRole('button', { name: /网络配置日志$/ })).toHaveCount(4);
          const header = await page.locator('.cvm-page-head').boundingBox();
          const first = await table.boundingBox();
          expect(first!.y).toBeGreaterThanOrEqual(header!.y + header!.height);
          await expect(table.getByRole('status').filter({ hasText: '能力已过期' })).toHaveCount(1);
          await expect(table.getByRole('status').filter({ hasText: /^不可用$/ })).toHaveCount(1);
          const search = page.getByRole('searchbox', { name: '搜索宿主' });
          await search.fill('没有这台宿主');
          await expect(page.getByText('没有匹配的宿主')).toBeVisible();
          await search.fill('hkix');
          await expect(table.getByRole('rowheader')).toHaveCount(1);
          await search.clear();
          const row = table.getByRole('button', { name: '香港 HKIX', exact: true });
          await row.press('Enter');
          await expect(row).toHaveAttribute('aria-expanded', 'true');
          await expect(table.getByRole('heading', { name: '执行能力' })).toBeVisible();
          await expect(table.getByRole('button', { name: '移除宿主…' })).toBeDisabled();
          for (const hostRow of await table.locator('.cvm-hosts-row').all())
            expect(await hostRow.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        } else {
          await expect(page.getByText('尚无宿主', { exact: true })).toBeVisible();
          await expect(page.getByRole('table', { name: '宿主' })).toHaveCount(0);
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
        await expect(page).toHaveScreenshot(`host-workbench-${scene}-${width}-${theme}.png`, { fullPage: true });
        await hosts.focus();
        await page.keyboard.press('Home');
        await expect(board).toBeFocused();
        await expect(board).toHaveAttribute('aria-selected', 'true');
        await page.keyboard.press('End');
        const quotas = page.getByRole('tab', { name: '网络', exact: true });
        await expect(quotas).toBeFocused();
        await expect(quotas).toHaveAttribute('aria-selected', 'true');
      });

for (const [width, theme] of [
  [1440, 'light'],
  [320, 'dark'],
] as const) {
  for (const view of ['hosts', 'adopt'] as const) {
    test(`Direction A ${view} component ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: view === 'hosts' && width === 320 ? 1000 : 760 });
      await page.route('**/cvm/networks', route => route.fulfill({ json: { providers: [] } }));
      await page.route('**/tests/visual/cvm/hosts-preview.html?*', route =>
        route.fulfill({
          contentType: 'text/html; charset=utf-8',
          body: `<!doctype html><html lang="zh-CN"><meta name="viewport" content="width=device-width, initial-scale=1"><div id="root"></div><script type="module">
        import RefreshRuntime from '/@react-refresh';
        RefreshRuntime.injectIntoGlobalHook(window);
        window.$RefreshReg$ = () => {};
        window.$RefreshSig$ = () => type => type;
        window.__vite_plugin_react_preamble_installed__ = true;
        await import('/tests/visual/cvm/hosts-preview.tsx');
        </script></html>`,
        }),
      );
      await page.goto(`/tests/visual/cvm/hosts-preview.html?view=${view}&theme=${theme}`);
      if (view === 'hosts') {
        const hosts = page.getByRole('table', { name: '宿主' });
        await expect(hosts.getByRole('rowheader')).toHaveCount(2);
        await expect(hosts.getByRole('cell', { name: 'Cube + 宿主系统', exact: true })).toHaveCount(2);
        const search = page.getByRole('searchbox', { name: '搜索宿主' });
        await search.fill('hkix');
        await expect(hosts.getByRole('rowheader')).toHaveCount(1);
        await search.clear();
        await hosts.getByRole('button', { name: '构建裸机', exact: true }).click();
        await expect(hosts.getByRole('button', { name: '在此创建' })).toHaveCount(1);
        const host = hosts.getByRole('button', { name: '香港 HKIX', exact: true });
        await host.press('Enter');
        await expect(host).toHaveAttribute('aria-expanded', 'true');
        await expect(hosts.getByRole('button', { name: '查看宿主系统' })).toBeVisible();
        await expect(hosts.getByRole('button', { name: '移除宿主…' })).toBeDisabled();
        await expect(hosts.getByText('会话数未上报').first()).toBeVisible();
      } else {
        const form = page.getByRole('form', { name: '纳管裸机' });
        await expect(form.getByRole('button', { name: '确认纳管' })).toBeDisabled();
        await form.getByRole('checkbox').check();
        await expect(form.getByRole('button', { name: '确认纳管' })).toBeEnabled();
        await expect(page.getByRole('dialog')).toHaveCount(0);
      }
      await waitForVisualAssets(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`direction-a-${view}-${width}-${theme}.png`, { fullPage: true });
      if (view === 'hosts') {
        await page.getByRole('button', { name: '在此创建' }).click();
        await expect(
          page.getByRole('status', { name: '', exact: true }).filter({ hasText: /^hk-hkix$/ }),
        ).toBeVisible();
      } else {
        await page.getByRole('button', { name: '查看宿主' }).click();
        await expect(page.getByRole('table', { name: '宿主' })).toBeVisible();
      }
    });
  }
}
