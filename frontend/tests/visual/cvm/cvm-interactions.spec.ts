import { expect, test, type Page } from '@playwright/test';
import { waitForVisualAssets } from '../helpers/visual-ready';

async function open(page: Page, width: number, theme: string, scene: string) {
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=${scene}&theme=${theme}`);
  await expect(page.locator('.cvm-page')).toBeVisible();
  await waitForVisualAssets(page);
}

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`uncertain command retries after a real version refresh ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, 'detail');
      await page.evaluate(() => {
        const prior = globalThis.fetch;
        const calls: unknown[] = [];
        let accepted: unknown;
        (window as unknown as { commandCalls: unknown[] }).commandCalls = calls;
        globalThis.fetch = async (input, init) => {
          if (String(input).endsWith('/operations') && init?.method === 'POST') {
            calls.push(JSON.parse(String(init.body)));
            if (calls.length === 1) {
              accepted = await (await prior(input, init)).json();
              return Response.json({ error: '保存响应丢失，请重试' }, { status: 503 });
            }
            return Response.json(accepted);
          }
          return prior(input, init);
        };
      });
      await page.getByRole('button', { name: '重启', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: '重启实例' });
      await dialog.getByRole('button', { name: '保存变更' }).click();
      await expect(page.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
      await expect(dialog.getByText(/上次保存结果尚未确认/)).toBeVisible();
      const errorNotice = dialog.locator('.callout.err');
      await expect(errorNotice).toHaveText('保存响应丢失，请重试');
      await expect(errorNotice).toHaveCSS('opacity', '1');
      await expect(dialog.getByRole('button', { name: '保存变更' })).toBeEnabled();
      await expect(dialog).toHaveScreenshot(`uncertain-command-${width}-${theme}.png`);
      await dialog.getByRole('button', { name: '保存变更' }).click();
      await expect(dialog).toBeHidden();
      const calls = await page.evaluate(() => (window as unknown as { commandCalls: unknown[] }).commandCalls);
      expect(calls).toHaveLength(2);
      expect(calls[0]).toEqual(calls[1]);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });

    if (width === 390) {
      test(`DNS validation explains the field and preserves input ${theme}`, async ({ page }) => {
        await open(page, width, theme, 'detail');
        await page.getByRole('tab', { name: '网络', exact: true }).click();
        const editor = page.getByRole('region', { name: '路由' });
        const dns = editor.getByRole('textbox', { name: '容器 DNS', exact: true });
        await dns.fill('127.0.0.1');
        const message = '容器 DNS 不能使用回环、组播或未指定地址，请填写容器可访问的 DNS 服务器 IP';
        await page.evaluate(message => {
          const prior = globalThis.fetch;
          globalThis.fetch = async (input, init) =>
            String(input).endsWith('/network-edit?preview=true')
              ? Response.json({ error: message }, { status: 400 })
              : prior(input, init);
        }, message);
        const bar = page.getByLabel('容器草稿与待发布变更');
        await bar.getByRole('button', { name: '保存网络变更' }).click();
        await expect(bar.getByText(message, { exact: true })).toBeVisible();
        await expect(dns).toHaveValue('127.0.0.1');
        await expect(dns).toBeEnabled();
      });

      test(`entry-only changes keep the pending tone ${theme}`, async ({ page }) => {
        await open(page, 390, theme, 'staged-entry');
        const state = page.locator('.cvm-page-head').getByRole('status');
        await expect(state).toHaveText('待发布');
        await expect(state).toHaveClass('st st-pending');
        await expect(page.getByRole('img', { name: '运行中', exact: true })).toBeVisible();
        await expect(page).toHaveScreenshot(`entry-only-pending-${theme}.png`);
      });

      test(`automatic check errors remain recoverable ${theme}`, async ({ page }) => {
        await open(page, 390, theme, 'detail');
        await page.getByRole('tab', { name: '网络', exact: true }).click();
        const editor = page.getByRole('region', { name: '路由' });
        const routes = editor.getByRole('table', { name: '目标路由' });
        // 路由编辑器先在本地校验目标格式；这里让一条合法路由被服务端检查拒绝。
        await routes.getByRole('textbox', { name: '目标 IP／网段' }).fill('203.0.113.0/24');
        const via = routes.getByRole('combobox', { name: '经由' });
        await via.selectOption({ index: 1 });
        await routes.getByRole('button', { name: '添加路由', exact: true }).click();
        const added = routes.getByRole('button', { name: '编辑路由 203.0.113.0/24', exact: true });
        await expect(added).toBeVisible();
        await page.evaluate(() => {
          const prior = globalThis.fetch;
          let checks = 0;
          (window as unknown as { routeWrites: number }).routeWrites = 0;
          globalThis.fetch = async (input, init) => {
            const url = String(input);
            if (url.endsWith('/network-edit?preview=true') && checks++ === 0)
              return Response.json({ error: '目标网段与现有路由冲突' }, { status: 422 });
            if (url.endsWith('/network-edit') && init?.method === 'PUT')
              (window as unknown as { routeWrites: number }).routeWrites++;
            return prior(input, init);
          };
        });
        const bar = page.getByLabel('容器草稿与待发布变更');
        await bar.getByRole('button', { name: '保存网络变更' }).click();
        await expect(bar.getByText('目标网段与现有路由冲突', { exact: true })).toBeVisible();
        // 检查失败不丢草稿：路由仍在表里，可以继续编辑后重试。
        await expect(added).toBeEnabled();
        expect(await page.evaluate(() => (window as unknown as { routeWrites: number }).routeWrites)).toBe(0);
        await bar.getByRole('button', { name: '保存网络变更' }).click();
        await expect(bar.getByRole('button', { name: '保存网络变更' })).toHaveCount(0);
        expect(await page.evaluate(() => (window as unknown as { routeWrites: number }).routeWrites)).toBe(1);
      });
    }

    test(`pure container pending badges ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, 'staged');
      const bar = page.getByLabel('容器草稿与待发布变更');
      const pending = bar.getByRole('button', { name: '1 个容器待发布' });
      await expect(pending).toBeVisible();
      await expect(bar).toHaveCSS('position', 'static');
      const header = await page.getByRole('heading', { name: '容器', exact: true }).boundingBox();
      expect((await bar.boundingBox())!.y).toBeLessThan(header!.y);
      await expect(page.locator('.cvm-list-row').getByRole('status')).toHaveText('待发布');
      await pending.click();
      await expect(bar.getByRole('button', { name: 'home-workspace' })).toBeVisible();
      await expect(bar.getByText('1 台机器待更新')).toBeVisible();
      if (width > 820)
        await expect(page.locator('.fg-nv').filter({ hasText: /^发布/ }).locator('.fg-badge')).toHaveText('待发布');
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`pending-badges-${width}-${theme}.png`);
    });

    test(`one network draft saves an entry, NIC and route with safe retry ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, 'detail');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await page.getByRole('button', { name: '添加入站方式', exact: true }).click();
      const entry = page.getByRole('form', { name: '添加入站方式' });
      await entry
        .getByRole('radiogroup', { name: '入口地址' })
        .locator('input[value="entrypub-hk:203.0.113.20"]')
        .check();
      const listen = entry.getByRole('textbox', { name: 'TCP 入口端口' });
      await listen.fill('30080-30081');
      await expect(entry.getByRole('status')).toHaveText('所选地址或端口已被占用。');
      await expect(entry.getByRole('button', { name: '加入变更' })).toBeDisabled();
      await listen.fill('30054-30055');
      await entry.getByRole('textbox', { name: 'TCP 端口映射（可选）' }).fill('30054-30055 → 22-23');
      await expect(entry.getByText('· SSH 端口 30054', { exact: true })).toBeVisible();
      await waitForVisualAssets(page);
      await expect(entry).toHaveScreenshot(`inline-entry-editor-${width}-${theme}.png`);
      await entry.getByRole('button', { name: '加入变更' }).click();
      await page.getByRole('button', { name: '添加出站方式' }).click();
      const mount = page.getByRole('form', { name: '添加出站方式' });
      await mount.getByRole('radiogroup', { name: '网络' }).locator('input[value="network-hk"]').check();
      await mount.getByRole('button', { name: '加入变更' }).click();
      const routes = page.getByRole('region', { name: '路由' });
      // 常驻添加行：未保存的新出站方式也能直接作为出口。
      await routes.getByRole('textbox', { name: '目标 IP／网段' }).fill('198.51.100.0/24');
      const via = routes.getByRole('combobox', { name: '经由', exact: true });
      const staged = await via.locator('option[value^="draft-"]').getAttribute('value');
      await via.selectOption(staged!);
      await routes.getByRole('button', { name: '添加路由', exact: true }).click();
      const bar = page.getByLabel('容器草稿与待发布变更');
      await expect(bar.getByRole('button', { name: '3 处未保存' })).toBeVisible();
      await bar.getByRole('button', { name: '查看配置差异' }).click();
      await expect(bar.getByText(/服务端配置检查通过/)).toBeVisible();
      expect(
        await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations),
      ).toHaveLength(0);
      await page.evaluate(() => {
        const prior = globalThis.fetch;
        const calls: unknown[] = [];
        let accepted: unknown;
        (window as unknown as { networkCalls: unknown[] }).networkCalls = calls;
        globalThis.fetch = async (input, init) => {
          if (String(input).endsWith('/network-edit') && init?.method === 'PUT') {
            calls.push({
              body: JSON.parse(String(init.body)),
              digest: new Headers(init.headers).get('x-brocade-cvm-input-sha256'),
            });
            if (calls.length === 1) {
              accepted = await (await prior(input, init)).json();
              return Response.json({ error: '保存响应丢失，请重试' }, { status: 503 });
            }
            return Response.json(accepted);
          }
          return prior(input, init);
        };
      });
      await bar.getByRole('button', { name: '保存网络变更' }).click();
      await expect(bar.getByText('保存响应丢失，请重试')).toBeVisible();
      await expect(bar.getByRole('button', { name: '丢弃当前草稿' })).toBeDisabled();
      await bar.getByRole('button', { name: '重试原保存' }).click();
      await expect(bar.getByRole('button', { name: '1 个容器待发布' })).toBeVisible();
      await expect(bar.getByRole('button', { name: '3 处未保存' })).toHaveCount(0);
      const calls = await page.evaluate(() => (window as unknown as { networkCalls: unknown[] }).networkCalls);
      expect(calls).toHaveLength(2);
      expect(calls[0]).toEqual(calls[1]);
      expect(calls[0]).toMatchObject({
        digest: 'a'.repeat(64),
        body: {
          entry_additions: [
            {
              mode: {
                kind: 'protocols',
                tcp: {
                  kind: 'forward',
                  ports: { kind: 'ranges', ranges: [{ start: 30054, end: 30055 }] },
                  mappings: [{ listen: { start: 30054, end: 30055 }, target: { start: 22, end: 23 } }],
                },
                udp: { kind: 'disabled' },
              },
            },
          ],
          mounts: [{ publication_id: 'network-hk', draft_id: staged }],
          routes: {
            routes: { routes: expect.arrayContaining([{ destination: '198.51.100.0/24', interface_id: staged }]) },
          },
        },
      });
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    });
  }
