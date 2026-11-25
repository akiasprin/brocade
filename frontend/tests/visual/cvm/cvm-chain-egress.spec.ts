import { expect, test, type Page } from '@playwright/test';
import type { CvmRouteMatchRequest, CvmRouteMatchResult } from '../../../src/api';
import { waitForVisualAssets } from '../helpers/visual-ready';

async function open(page: Page, width: number, theme: string, available = true) {
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1050 });
  await page.goto(
    `/tests/fixtures/cvm/preview.html?scene=chain-detail&theme=${theme}&chain=${available ? 'ready' : 'unavailable'}`,
  );
  await expect(page.locator('.cvm-page')).toBeVisible();
  await waitForVisualAssets(page);
}

const routeEditor = (page: Page) => page.getByRole('region', { name: '路由与命中测试' });

for (const theme of ['light', 'dark']) {
  test(`inline routes explain host restrictions and enforce line authorization ${theme}`, async ({ page }) => {
    await open(page, 390, theme, false);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const editor = routeEditor(page);
    await expect(editor.getByText('此宿主尚不支持线路出口，请管理员检查并更新宿主组件。')).toBeVisible();
    await expect(editor.getByRole('option', { name: /洛杉矶.*宿主未就绪/ }).first()).toHaveJSProperty('disabled', true);
    await expect(editor.getByRole('option', { name: /香港/ }).first()).toHaveJSProperty('disabled', false);

    await open(page, 390, theme);
    await page.evaluate(() => {
      const prior = globalThis.fetch;
      globalThis.fetch = async (input, init) => {
        const response = await prior(input, init);
        if (!String(input).startsWith('/cvm/chain-egress?')) return response;
        const catalog = await response.json();
        return Response.json({
          ...catalog,
          chains: catalog.chains.map((chain: { id: string }) => ({
            ...chain,
            authorized: chain.id !== 'chain-jp',
          })),
        });
      };
    });
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await expect(editor.getByRole('option', { name: /东京 IIJ.*未获授权/ }).first()).toHaveJSProperty('disabled', true);
    await expect(editor.getByRole('option', { name: '洛杉矶 · 三跳线路' }).first()).toHaveJSProperty('disabled', false);
    await expect(editor.getByText(/alice.*授权、额度与限速，仅支持 TCP／UDP/)).toBeVisible();
  });

  for (const width of [1440, 390, 320])
    test(`inline routes and authoritative match ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await page.evaluate(() => {
        const prior = globalThis.fetch;
        globalThis.fetch = async (input, init) => {
          if (!String(input).endsWith('/routes/match') || init?.method !== 'POST') return prior(input, init);
          const body = JSON.parse(String(init.body)) as CvmRouteMatchRequest;
          const routing = body.network_edit?.routes?.routes;
          const index = routing?.routes.findIndex(
            route => route.destination === '198.51.100.0/24' && route.chain_id === 'chain-us',
          );
          const priority = routing?.priorities.find(priority => priority.chain_id === 'chain-us')?.ipv4;
          if (
            body.destination !== '198.51.100.9' ||
            body.protocol !== 'tcp' ||
            body.port !== 443 ||
            !routing ||
            index === undefined ||
            index < 0 ||
            priority == null
          )
            return Response.json({ error: 'unexpected route probe' }, { status: 422 });
          return Response.json({
            kind: 'route',
            route_index: index,
            route: routing.routes[index],
            priority,
          } satisfies CvmRouteMatchResult);
        };
      });
      const editor = routeEditor(page);
      await expect(editor.getByRole('list', { name: '出口优先级' })).toBeVisible();
      // 常驻的添加行同时列出网卡和线路两组出口。
      await expect(editor.getByRole('combobox', { name: '经由' }).locator('optgroup')).toHaveCount(2);
      const routes = editor.getByRole('table', { name: '目标路由' });
      await routes
        .getByRole('button', { name: /^编辑路由 / })
        .first()
        .click();
      const editing = routes.locator('tr[data-editing]');
      await editing.getByRole('combobox', { name: '经由' }).selectOption('chain:chain-us');
      await editing.getByRole('textbox', { name: '目标 IP／网段' }).fill('198.51.100.0/24');
      await editing.getByRole('button', { name: '应用修改', exact: true }).click();
      await expect(editing).toHaveCount(0);
      await editor.getByRole('textbox', { name: '测试目标' }).fill('198.51.100.9 tcp/443');
      await editor.getByRole('button', { name: '测试命中' }).click();
      const hit = routes.getByRole('row').filter({ has: page.getByText('命中', { exact: true }) });
      await expect(hit).toHaveCount(1);
      await expect(hit.getByRole('cell').first()).toHaveText('198.51.100.0/24');
      await expect(editor.getByRole('form', { name: '路由命中测试' }).getByRole('status')).toContainText(
        /命中 198.51.100.0\/24.*洛杉矶.*优先级 200/,
      );
      await expect(page.getByLabel('容器草稿与待发布变更').getByRole('button', { name: '保存网络变更' })).toBeEnabled();
      expect(await editor.evaluate(n => n.scrollWidth <= n.clientWidth + 1)).toBe(true);
      if ((width === 1440 && theme === 'light') || (width === 320 && theme === 'dark'))
        await expect(editor).toHaveScreenshot(`inline-f7-match-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
}
