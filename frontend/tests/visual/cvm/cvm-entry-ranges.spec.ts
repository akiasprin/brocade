import { expect, test } from '@playwright/test';
import { openListInstance } from '../helpers/cvm-instance';
import { waitForVisualAssets } from '../helpers/visual-ready';

for (const width of [1440, 390, 320])
  for (const theme of ['light', 'dark']) {
    test(`protocol range mapping and inline validation ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=list&range=1&theme=${theme}`);
      const list = page.getByRole('region', { name: '容器清单', exact: true });
      await expect(
        list.getByRole('button', { name: 'TCP 203.0.113.20:30040-30049 → 容器 20-29 · 已生效', exact: true }),
      ).toBeVisible();
      await list.getByRole('searchbox', { name: '搜索容器', exact: true }).fill('30042');
      await expect(list.getByRole('button', { name: 'home-workspace', exact: true })).toBeVisible();
      await waitForVisualAssets(page);
      await expect(list).toHaveScreenshot(`entry-range-list-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await openListInstance(page);
      await expect(page.getByRole('region', { name: 'SSH 登录' })).toContainText('ssh -l cube -p 30042');
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const entries = page.getByRole('region', { name: '入站方式', exact: true });
      await entries.getByRole('button', { name: '添加入站方式', exact: true }).click();
      const form = entries.getByRole('form', { name: '添加入站方式' });
      await form
        .getByRole('radiogroup', { name: '入口地址' })
        .locator('input[value="entrypub-hk:203.0.113.20"]')
        .check();
      const listen = form.getByRole('textbox', { name: 'TCP 入口端口' });
      const add = form.getByRole('button', { name: '加入变更', exact: true });
      await listen.fill('30079-30082');
      await form.getByRole('textbox', { name: 'TCP 端口映射（可选）' }).fill('30079-30082 → 80-83');
      await expect(form.getByRole('status')).toHaveText('所选地址或端口已被占用。');
      await expect(add).toBeDisabled();
      await listen.fill('30060-30063');
      await form.getByRole('textbox', { name: 'TCP 端口映射（可选）' }).fill('30060-30063 → 80-83');
      await expect(add).toBeEnabled();
      await expect(form.getByRole('status')).toHaveCount(0);
      await expect(form).toHaveScreenshot(`entry-range-inline-${width}-${theme}.png`);
      expect(await form.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await add.click();
      await expect(entries.getByRole('list', { name: '入站方式列表' })).toContainText(
        '203.0.113.20 · TCP 30060-30063；映射 30060-30063 → 80-83',
      );
      expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual(
        [],
      );
      expect(errors).toEqual([]);
    });
  }

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`dedicated protocol policy with reserved host SSH ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&policy=1&theme=${theme}`);
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      const entries = page.getByRole('region', { name: '入站方式', exact: true });
      await entries.getByRole('button', { name: '添加入站方式', exact: true }).click();
      const form = entries.getByRole('form', { name: '添加入站方式' });
      await form
        .getByRole('radiogroup', { name: '入口地址' })
        .locator('input[value="entrypub-local:10.8.0.31"]')
        .check();
      await expect(form.getByRole('checkbox', { name: 'ICMP', exact: true })).toBeChecked();
      await expect(form.getByRole('checkbox', { name: '其他 IP 协议', exact: true })).toBeChecked();
      await expect(form).toContainText('TCP 22');
      // 宿主保留 22，未映射时不能把 TCP 22 当成容器 SSH 入口。
      const preview = form.locator('.cvm-composer-preview');
      await expect(preview).toContainText('198.51.100.31');
      await expect(preview.getByText(/SSH 端口/)).toHaveCount(0);
      await form.getByRole('textbox', { name: 'TCP 端口映射（可选）' }).fill('30022 → 22');
      await expect(preview.getByText('· SSH 端口 30022', { exact: true })).toBeVisible();
      await waitForVisualAssets(page);
      await expect(form).toHaveScreenshot(`entry-protocol-policy-${width}-${theme}.png`);
      expect(await form.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await form.getByRole('button', { name: '加入变更', exact: true }).click();
      await page.getByRole('button', { name: '保存网络变更', exact: true }).click();
      await expect(page.getByRole('button', { name: '1 个容器待发布', exact: true })).toBeVisible();
      expect(
        await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations),
      ).toMatchObject([
        {
          entry_additions: [
            {
              address: '10.8.0.31',
              mode: {
                kind: 'protocols',
                tcp: {
                  kind: 'forward',
                  ports: { kind: 'all' },
                  mappings: [{ listen: { start: 30022, end: 30022 }, target: { start: 22, end: 22 } }],
                },
                udp: { kind: 'forward', ports: { kind: 'all' }, mappings: [] },
                icmp: true,
                other_ip: true,
              },
            },
          ],
        },
      ]);
      expect(errors).toEqual([]);
    });
  }
