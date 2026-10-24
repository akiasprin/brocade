import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
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

before(async () => {
  cacheDirectory = await mkdtemp(join(tmpdir(), 'brocade-outflow-settings-'));
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL('..', import.meta.url)),
    plugins: [react()],
    cacheDir: join(cacheDirectory, 'node_modules/.vite'),
    server: { host: '127.0.0.1', port: 0 },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}/tests/fixtures/site-settings-preview.html`;
  browser = await chromium.launch();
});

after(async () => {
  await browser?.close();
  await server?.close();
  if (cacheDirectory) await rm(cacheDirectory, { recursive: true, force: true });
});

async function preview(t, width, query = {}) {
  const page = await browser.newPage({ viewport: { width, height: 1100 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('requestfailed', request => {
    if (['document', 'script', 'stylesheet'].includes(request.resourceType())) {
      errors.push(`${request.failure()?.errorText}: ${request.url()}`);
    }
  });
  t.after(async () => {
    const failures = [...errors];
    await page.close();
    assert.deepEqual(failures, [], 'no browser or React exceptions');
  });
  await page.goto(`${base}?${new URLSearchParams(query)}`);
  await page.getByLabel('每用户目标容量', { exact: true }).waitFor();
  return page;
}

async function style(locator, properties) {
  return locator.evaluate((element, names) => {
    const computed = getComputedStyle(element);
    return Object.fromEntries(names.map(name => [name, computed[name]]));
  }, properties);
}

for (const theme of ['light', 'dark']) {
  for (const width of [1280, 760, 390, 320]) {
    test(`outflow groups match host network tuning at ${width}px in ${theme}`, async t => {
      const page = await preview(t, width, { theme });
      const reference = page.locator('#set-host-network .settings-block');
      const appearance = ['backgroundColor', 'borderColor', 'borderRadius', 'padding'];
      for (const name of ['站点分类', '未分类域名', '嗅探失败目标']) {
        const section = page.getByRole('region', { name, exact: true });
        await expect(section.getByRole('button', { name: '开启', exact: true })).toHaveAttribute(
          'aria-pressed',
          'true',
        );
        assert.deepEqual(await style(section, appearance), await style(reference, appearance));
        assert.deepEqual(
          await style(section.getByRole('heading', { name, exact: true }), ['font', 'color']),
          await style(reference.locator('.eyebrow'), ['font', 'color']),
        );
      }
      const domain = page.getByRole('region', { name: '未分类域名', exact: true });
      const top = domain.getByLabel('每用户 Top N', { exact: true });
      const numericAppearance = ['width', 'fontFamily', 'fontSize', 'textAlign', 'fontVariantNumeric'];
      assert.deepEqual(
        await style(top, numericAppearance),
        await style(page.getByLabel('GRO 刷新等待', { exact: true }), numericAppearance),
      );
      const firstField = await top.boundingBox();
      const secondField = await domain.getByLabel('每用户域名容量', { exact: true }).boundingBox();
      assert.equal(firstField.x === secondField.x, width === 320, 'one column at the narrowest width');
      assert.equal(firstField.y === secondField.y, width !== 320, 'two columns when there is room');
      await top.fill('64');
      const save = domain.getByRole('button', { name: '保存未分类域名设置' });
      await expect(save).toBeEnabled();
      const groupBox = await domain.boundingBox();
      const saveBox = await save.boundingBox();
      assert.ok(saveBox.x >= groupBox.x && saveBox.x + saveBox.width <= groupBox.x + groupBox.width);
      await save.focus();
      await expect(save).toBeFocused();
      await page.keyboard.press('Enter');
      await expect(domain.getByRole('status')).toHaveText('已保存，待节点同步。');
      await expect(save).toHaveCount(0);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    });
  }
}

test('readonly and failed policy reads keep their controls isolated on narrow screens', async t => {
  const readonly = await preview(t, 320, { role: 'readonly', theme: 'dark' });
  for (const input of await readonly.getByRole('spinbutton').all()) await expect(input).toBeDisabled();
  for (const button of await readonly.getByRole('button', { name: '开启', exact: true }).all()) {
    await expect(button).toBeDisabled();
  }
  await expect(readonly.getByRole('button', { name: /^保存/ })).toHaveCount(0);

  const failed = await preview(t, 320, { failure: 'read' });
  const domain = failed.getByRole('region', { name: '未分类域名', exact: true });
  await expect(domain.getByText('未分类域名策略读取失败', { exact: true })).toBeVisible();
  await expect(domain.getByRole('button', { name: '开启' })).toBeDisabled();
  const targets = failed.getByRole('region', { name: '嗅探失败目标', exact: true });
  await targets.getByLabel('每用户 Top N', { exact: true }).fill('513');
  await expect(targets.getByRole('alert')).toBeVisible();
  await expect(targets.getByRole('button', { name: '保存嗅探失败目标设置' })).toBeDisabled();
  await expect(
    failed.getByRole('region', { name: '站点分类', exact: true }).getByRole('button', { name: '开启' }),
  ).toBeEnabled();
  assert.equal(await failed.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
});
