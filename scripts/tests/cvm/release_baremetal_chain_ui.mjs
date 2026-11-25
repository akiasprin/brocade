// Real owned KVM baremetal UI and Undo acceptance; no intercepted API responses.
import fs from 'node:fs/promises';
import crypto from 'node:crypto';
const root = process.argv[2];
if (!root) throw new Error('Pass the private owned release KVM output directory');
const lab = JSON.parse(await fs.readFile(root + '/lab-3/lab.json', 'utf8'));
const state = JSON.parse(await fs.readFile(root + '/acceptance.json', 'utf8'));
const creds = JSON.parse(await fs.readFile(root + '/services/credentials.json', 'utf8'));
import { chromium } from '../../../frontend/node_modules/playwright/index.mjs';
const origin = 'http://127.0.0.1:' + lab.console_port;
const endpoint = '/cvm/' + state.instances.gamma;
const browser = await chromium.launch({ headless: true });
await fs.mkdir(root + '/ui-native', { recursive: true, mode: 0o700 });
const checks = [];
try {
  const context = await browser.newContext({
    locale: 'zh-CN',
    timezoneId: 'Asia/Taipei',
    reducedMotion: 'reduce',
  });
  async function api(method, url, data) {
    const response = await context.request.fetch(origin + url, {
      method,
      data,
      headers: { Origin: origin, 'x-brocade-protocol-version': '76' },
    });
    if (!response.ok()) throw new Error(`${method} ${url}: ${response.status()}`);
    return response.status() === 204 ? null : response.json();
  }
  await api('POST', '/auth/login', {
    operator_id: creds.operator_id,
    password: creds.password,
  });
  for (const width of [1440, 390, 320]) {
    const page = await context.newPage();
    await page.setViewportSize({ width, height: 1050 });
    await page.goto(origin + '/#/cvm/instance/' + state.instances.gamma);
    await page.locator('.cvm-page-head').waitFor();
    const text = await page.locator('body').innerText();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
    if (overflow) throw new Error('baremetal detail overflows at ' + width);
    if (text.includes('此裸机暂不支持线路出口')) throw new Error('old unsupported warning visible');
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await page.getByRole('button', { name: '路由表', exact: true }).click();
    const dialog = page.getByRole('dialog', {
      name: '容器路由表',
      exact: true,
    });
    await dialog.waitFor();
    const options = dialog.getByRole('option', { name: /KVM split chain/ });
    await options.first().waitFor({ state: 'attached' });
    if (!(await options.first().isEnabled())) throw new Error('real baremetal chain choice disabled');
    await page.screenshot({
      path: root + `/ui-native/routes-${width}.png`,
      fullPage: true,
    });
    if (width === 390) {
      const before = await api('GET', endpoint);
      const priority = dialog.getByRole('spinbutton', { name: 'IPv4 优先级', exact: true }).first();
      await priority.fill('20');
      const save = page.waitForResponse(
        r => new URL(r.url()).pathname === endpoint + '/routes' && r.request().method() === 'PUT',
      );
      await dialog.getByRole('button', { name: '保存变更', exact: true }).click();
      if (!(await save).ok()) throw new Error('actual native UI save failed');
      await dialog.waitFor({ state: 'hidden' });
      await page.locator('.cvm-release-notice').filter({ hasText: '等待发布' }).waitFor();
      const undo = page.waitForResponse(
        r => new URL(r.url()).pathname === endpoint + '/operations' && r.request().method() === 'POST',
      );
      await page.getByRole('button', { name: '撤回待发布变更', exact: true }).click();
      if (!(await undo).ok()) throw new Error('actual native UI Undo failed');
      const revision = (await api('GET', '/revisions?limit=1')).current_revision;
      const order = await api('POST', '/deployments', {
        revision_id: revision,
        idempotency_key: crypto.randomUUID(),
      });
      const start = performance.now();
      for (;;) {
        const d = await api('GET', '/deployments/' + order.deployment_id);
        if (d.status === 'succeeded') break;
        if (d.status === 'halted' || performance.now() - start > 240000)
          throw new Error('native UI recovery publication incomplete');
        await new Promise(resolve => setTimeout(resolve, 1000));
      }
      const after = await api('GET', endpoint);
      if (JSON.stringify(before.desired.spec.routing.routes) !== JSON.stringify(after.desired.spec.routing.routes))
        throw new Error('native UI Undo did not restore original routes');
      checks.push({
        width,
        actual_ui_save: true,
        actual_ui_undo: true,
        recovery_publication: order.deployment_id,
      });
    } else {
      await dialog.locator('header').getByRole('button', { name: '关闭', exact: true }).click();
      checks.push({ width, real_chain_option_enabled: true });
    }
    console.log('PASS actual baremetal chain UI at ' + width + 'px');
    await page.close();
  }
  await fs.writeFile(root + '/ui-native/checks.json', JSON.stringify(checks, null, 2) + '\n', { mode: 0o600 });
} finally {
  await browser.close();
}
