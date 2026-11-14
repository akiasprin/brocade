import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
const links =
  'socks5://fixture:fixture-one@us.example.invalid:1080#US\nhttps://fixture:fixture-two@jp.example.invalid:443#JP';
async function open(page: import('@playwright/test').Page, width: number, height: number, theme: string) {
  await page.setViewportSize({ width, height });
  await page.clock.setFixedTime(new Date('2026-11-13T00:00:00Z'));
  await page.goto(`/tests/fixtures/user-header-preview.html?theme=${theme}`);
  await page.getByRole('button', { name: '订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
  await dialog.getByRole('button', { name: '规则', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '保存规则', exact: true })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return dialog;
}
for (const [width, height, theme] of [
  [1440, 1000, 'light'],
  [390, 844, 'dark'],
] as const) {
  test(`D ordinary preferences ${width} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', e => errors.push(e.message));
    const dialog = await open(page, width, height, theme);
    const add = dialog.getByRole('textbox', { name: '添加域名、IP 段或进程' });
    await add.fill('example.invalid');
    await add.press('Enter');
    await dialog.getByRole('button', { name: '收起规则', exact: true }).click();
    const confirm = page.getByRole('dialog', { name: '放弃尚未保存的更改？' });
    await expect(confirm).toBeVisible();
    await confirm.getByRole('button', { name: '取消', exact: true }).click();
    await dialog.getByRole('button', { name: '保存规则', exact: true }).click();
    await expect(dialog.getByText('规则已保存，更新订阅后生效。', { exact: true })).toBeVisible();
    await dialog.getByRole('button', { name: '收起规则', exact: true }).click();
    await dialog.getByRole('button', { name: '规则', exact: true }).click();
    await expect(dialog.getByText('example.invalid', { exact: true })).toBeVisible();
    await expect(page).toHaveScreenshot(`clash-d-list-${width}-${theme}.png`, { threshold: 0.1 });
    await dialog.getByRole('textbox', { name: '试一试：域名、IP 或进程名', exact: true }).fill('clash-core');
    await dialog.getByRole('combobox', { name: '匹配类型', exact: true }).click();
    await page.getByRole('option', { name: '进程', exact: true }).click();
    await dialog.getByRole('button', { name: '试一试', exact: true }).click();
    await expect(dialog.getByRole('status').filter({ hasText: 'AI 服务' })).toBeVisible();
    const probeRequest = await page.evaluate(() =>
      (window as unknown as { __userPreviewRequests: { path: string; body: string | null }[] }).__userPreviewRequests
        .filter(request => request.path.endsWith('/probe'))
        .at(-1),
    );
    expect(JSON.parse(probeRequest!.body!).kind).toBe('process');
    await dialog.getByRole('button', { name: 'YAML', exact: true }).click();
    await expect(dialog.locator('.clash-rule-yaml')).toContainText('proxy-groups:');
    expect(errors).toEqual([]);
  });
}
for (const [width, height, theme] of [
  [1440, 1000, 'light'],
  [390, 844, 'dark'],
] as const) {
  test(`D refetch preserves edits ${width} ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    const dialog = await open(page, width, height, theme);
    const remoteUpdate = async (value: string) => {
      await page.evaluate(async value => {
        const path = '/users/preview/alice/clash-rules';
        const current = await (await fetch(path)).json();
        const response = await fetch(path, {
          method: 'PUT',
          body: JSON.stringify({
            version: current.version,
            rules: { ...current.rules, custom: [{ id: value, kind: 'domain', value, target: 'direct' }] },
          }),
        });
        if (!response.ok) throw new Error('Synthetic remote save failed');
        await (
          window as unknown as { __userPreviewRefreshClashRules: () => Promise<void> }
        ).__userPreviewRefreshClashRules();
      }, value);
    };
    await remoteUpdate('first.example.invalid');
    await expect(dialog.getByText('first.example.invalid', { exact: true })).toBeVisible();
    const add = dialog.getByRole('textbox', { name: '添加域名、IP 段或进程' });
    await add.fill('draft.example.invalid');
    await add.press('Enter');
    await remoteUpdate('latest.example.invalid');
    await expect(dialog.getByRole('status').filter({ hasText: '当前更改已保留' })).toBeVisible();
    await expect(dialog.getByText('draft.example.invalid', { exact: true })).toBeVisible();
    await expect(dialog.getByText('latest.example.invalid', { exact: true })).toHaveCount(0);
    await expect(page).toHaveScreenshot(`clash-d-refetch-${width}-${theme}.png`, { threshold: 0.1 });
    await dialog.getByRole('button', { name: '重新载入规则', exact: true }).click();
    const confirm = page.getByRole('dialog', { name: '放弃尚未保存的更改？' });
    await expect(confirm).toBeVisible();
    await confirm.getByRole('button', { name: '取消', exact: true }).click();
    await expect(confirm).toHaveCount(0);
    await expect(dialog.getByText('draft.example.invalid', { exact: true })).toBeVisible();
    await dialog.getByRole('button', { name: '重新载入规则', exact: true }).click();
    await confirm.getByRole('button', { name: '放弃并离开', exact: true }).click();
    await expect(dialog.getByText('latest.example.invalid', { exact: true })).toBeVisible();
    await expect(dialog.getByText('draft.example.invalid', { exact: true })).toHaveCount(0);
    await expect(dialog.getByRole('button', { name: '保存规则', exact: true })).toBeDisabled();
    expect(errors).toEqual([]);
  });
}
for (const [width, height, theme] of [
  [1440, 1000, 'dark'],
  [390, 844, 'light'],
] as const) {
  test(`D private nodes remain local ${width} ${theme}`, async ({ page }) => {
    const dialog = await open(page, width, height, theme);
    await dialog.getByRole('button', { name: '链式代理', exact: true }).click();
    await dialog.getByRole('textbox', { name: '自建节点链接', exact: true }).fill(links);
    await dialog.getByRole('button', { name: '添加到列表', exact: true }).click();
    await expect(dialog.getByText('链式代理 01 | US', { exact: true })).toBeVisible();
    await expect(dialog.getByText('链式代理 02 | JP', { exact: true })).toBeVisible();
    await dialog.getByRole('button', { name: '列表', exact: true }).click();
    const confirm = page.getByRole('dialog', { name: '清空本页的私有节点？' });
    await expect(confirm).toBeVisible();
    await confirm.getByRole('button', { name: '取消', exact: true }).click();
    await dialog.locator('.clash-editor-body').evaluate(el => {
      el.scrollTop = 0;
    });
    await expect(page).toHaveScreenshot(`clash-d-private-${width}-${theme}.png`, { threshold: 0.1 });
    const download = page.waitForEvent('download');
    await dialog.getByRole('button', { name: '导出到客户端', exact: true }).click();
    const script = await readFile((await (await download).path())!, 'utf8');
    expect(script).toContain('链式代理 01 | US');
    expect(script).toContain('链式代理 02 | JP');
    expect(script).toContain('"mode": "selection"');
    const requests = await page.evaluate(
      () =>
        (window as unknown as { __userPreviewRequests: { path: string; body: string | null }[] }).__userPreviewRequests,
    );
    expect(requests.some(request => request.path.endsWith('/clash-rules'))).toBe(true);
    expect(JSON.stringify(requests)).not.toContain('fixture-one');
    expect(JSON.stringify(requests)).not.toContain('us.example.invalid');
    const stored = await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }));
    expect(stored).not.toContain('fixture-one');
    await dialog.getByRole('button', { name: '列表', exact: true }).click();
    await dialog.getByRole('button', { name: '链式代理', exact: true }).click();
    await expect(dialog.getByRole('region', { name: '已添加的自建出口' })).toHaveCount(0);
    expect(await dialog.locator('.clash-editor-pane-inner').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(
      false,
    );
  });
}
