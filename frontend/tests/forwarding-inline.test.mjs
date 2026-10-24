import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, before, test } from 'node:test';
import { chromium, expect } from '@playwright/test';
import react from '@vitejs/plugin-react';
import { createServer } from 'vite';

let server;
let browser;
let base;
let cacheDirectory;
const output = '/tmp/brocade-forwarding-a-acceptance';
before(async () => {
  cacheDirectory = await mkdtemp(join(tmpdir(), 'brocade-forwarding-inline-'));
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL('..', import.meta.url)),
    plugins: [react()],
    cacheDir: join(cacheDirectory, 'node_modules/.vite'),
    server: { host: '127.0.0.1', port: 0 },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}/tests/fixtures/forwarding-inline-preview.html`;
  browser = await chromium.launch();
  await mkdir(output, { recursive: true });
});
after(async () => {
  await browser?.close();
  await server?.close();
  if (cacheDirectory) await rm(cacheDirectory, { recursive: true, force: true });
});

async function preview(t, query = {}, width = 1180) {
  const page = await browser.newPage({
    viewport: { width, height: 1000 },
    reducedMotion: 'reduce',
    hasTouch: width <= 620,
  });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('requestfailed', request => {
    if (['document', 'script', 'stylesheet'].includes(request.resourceType())) {
      errors.push(`${request.failure()?.errorText}: ${request.url()}`);
    }
  });
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  t.after(async () => {
    const failures = [...errors];
    await page.close();
    assert.deepEqual(failures, [], 'no browser errors');
  });
  await page.goto(`${base}?${new URLSearchParams(query)}`);
  await page.locator('.fwr-item').first().waitFor();
  return page;
}
const opener = (page, name = '东京入口') => page.getByRole('button', { name: `编辑 ${name} 的转发授权`, exact: true });
const group = (page, name = '东京入口') => page.getByRole('region', { name: `${name}的规则`, exact: true });

for (const width of [1180, 900, 620, 390]) {
  for (const theme of ['light', 'dark']) {
    test(`production A: ${width}px ${theme} edits, confirms and retains TCP Ping`, async t => {
      const page = await preview(t, { theme }, width);
      await page.getByRole('combobox', { name: '筛选转发机器' }).selectOption('jp-1');
      await expect(page.locator('.fwr-item')).toHaveCount(4);
      await opener(page).click();
      let form = page.getByRole('form', { name: '编辑 东京入口 的转发授权', exact: true });
      await expect(form).toBeVisible();
      await expect(page.locator('.qta-figs').first()).toContainText('205.00 GiB');
      const portWidths = await form
        .getByRole('textbox', { name: /^端口/ })
        .evaluateAll(elements => elements.map(element => element.getBoundingClientRect().width));
      assert.deepEqual(portWidths, [76, 76], 'port fields keep their compact width');
      await form.getByRole('button', { name: /^配额工单/ }).click();
      await expect(form.getByRole('article', { name: '配额工单 #91' })).toBeVisible();
      await page.screenshot({ path: `${output}/${width}-${theme}-authorization.png`, fullPage: true });
      await form.getByLabel('规则上限', { exact: true }).fill('101');
      await form.getByLabel('本期转发额度（GiB）', { exact: true }).fill('1100');
      await form.getByRole('button', { name: '保存', exact: true }).click();
      await expect(form).toHaveCount(0);
      await expect(opener(page)).toBeFocused();
      await expect(page.locator('.qta-figs').first()).toContainText('1.07 TiB');
      await opener(page).click();
      await opener(page, '洛杉矶入口').click();
      await expect(page.getByRole('form')).toHaveCount(1);
      await page.keyboard.press('Escape');
      await expect(opener(page, '洛杉矶入口')).toBeFocused();
      await page.getByRole('button', { name: '编辑 东京入口 :30000', exact: true }).click();
      form = page.getByRole('form', { name: '编辑入口 30000 的规则', exact: true });
      await expect(group(page).locator('.fwd-cycle').first()).toContainText('96.00 GiB');
      await form.getByLabel('名称', { exact: true }).fill('回归测试');
      await form.getByRole('button', { name: '保存', exact: true }).click();
      await expect(form).toHaveCount(0);
      await group(page)
        .getByRole('button', { name: '回归测试，入口 30000 转发到 example.com:443', exact: true })
        .click();
      const probe = group(page).getByRole('region', { name: '入口 30000 TCP Ping', exact: true });
      await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
      await expect(probe.getByRole('status')).toContainText('TCP 连通');
      await group(page).screenshot({ path: `${output}/${width}-${theme}-rule.png` });
      await opener(page).click();
      form = page.getByRole('form', { name: '编辑 东京入口 的转发授权', exact: true });
      await form.getByRole('button', { name: '停止转发', exact: true }).click();
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await expect(form.getByRole('alert')).toContainText('4 条规则停止监听');
      await form.getByRole('button', { name: '停止转发', exact: true }).click();
      await expect(form).toHaveCount(0);
      await expect(page.getByRole('button', { name: '新建规则', exact: true })).toBeDisabled();
      await page.getByRole('button', { name: '恢复 东京入口 的转发', exact: true }).click();
      await expect(page.getByRole('button', { name: '新建规则', exact: true })).toBeEnabled();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    });
  }
}

test('production A: mobile partial save retries the quota without rewriting the saved authorization', async t => {
  const page = await preview(t, { fail: 'quota-once' }, 390);
  await page.evaluate(() => {
    const original = window.fetch;
    window.savedPaths = [];
    window.fetch = (input, init) => {
      if (init?.method === 'PUT') window.savedPaths.push(String(input));
      return original(input, init);
    };
  });
  await opener(page).click();
  const form = page.getByRole('form', { name: '编辑 东京入口 的转发授权', exact: true });
  await form.getByLabel('规则上限', { exact: true }).fill('101');
  await form.getByLabel('本期转发额度（GiB）', { exact: true }).fill('1100');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(form).toContainText('授权已保存，配额保存失败');
  await expect(form.getByLabel('本期转发额度（GiB）', { exact: true })).toHaveValue('1100');
  await page.screenshot({ path: `${output}/390-partial-save.png`, fullPage: true });
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(form).toHaveCount(0);
  assert.deepEqual(await page.evaluate(() => window.savedPaths), [
    '/users/ops/alice/forwarding-nodes/jp-1',
    '/users/ops/alice/forwarding-nodes/jp-1/quota',
    '/users/ops/alice/forwarding-nodes/jp-1/quota',
  ]);
});

test('production A: read-only narrow details keep IPv6 and hide write and probe actions', async t => {
  const page = await preview(t, { readonly: '1' }, 390);
  await expect(opener(page)).toHaveCount(0);
  await expect(page.getByRole('button', { name: '新建规则', exact: true })).toHaveCount(0);
  await group(page)
    .getByRole('button', { name: /入口 30002 转发到/ })
    .click();
  const detail = group(page).locator('.fwr-detail');
  await expect(detail).toContainText('[2001:db8:1234:5678:abcd:ffff:1234:5678]:443');
  await expect(detail.getByRole('button', { name: '测试延迟', exact: true })).toHaveCount(0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
});
