// Real KVM Console interaction checks; no fixtures or intercepted responses.
import { chromium } from '../../../frontend/node_modules/playwright/index.mjs';
import fs from 'node:fs/promises';
import path from 'node:path';
const output = process.argv[2];
const lab = JSON.parse(await fs.readFile(path.join(output, 'lab-3/lab.json'), 'utf8'));
const credentials = JSON.parse(await fs.readFile(path.join(output, 'services/credentials.json'), 'utf8'));
const state = JSON.parse(await fs.readFile(path.join(output, 'acceptance.json'), 'utf8'));
const origin = `http://127.0.0.1:${lab.console_port}`;
const directory = path.join(output, 'ui-chain');
await fs.mkdir(directory, { recursive: true, mode: 0o700 });
const browser = await chromium.launch({ headless: true });
const checks = [];
try {
  const context = await browser.newContext({ locale: 'zh-CN', timezoneId: 'Asia/Taipei', reducedMotion: 'reduce' });
  const login = await context.request.post(origin + '/auth/login', {
    data: { operator_id: credentials.operator_id, password: credentials.password },
  });
  if (!login.ok()) throw new Error(`real login failed: ${login.status()}`);
  const alphaResponse = await context.request.get(origin + '/cvm/' + state.instances.alpha);
  if (!alphaResponse.ok()) throw new Error(`alpha lookup failed: ${alphaResponse.status()}`);
  const alpha = await alphaResponse.json();
  for (const width of [1440, 390, 320]) {
    const page = await context.newPage();
    await page.setViewportSize({ width, height: width === 1440 ? 2000 : 1050 });
    const errors = [],
      previews = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('response', r => {
      const url = new URL(r.url());
      if (url.pathname.endsWith('/network-edit') && url.searchParams.get('preview') === 'true')
        previews.push(r.status());
    });
    await page.goto(origin + '/#/cvm');
    await page.locator('.cvm-page').waitFor();
    const fold = page.getByRole('button', { name: '收起首次上线清单', exact: true });
    if (await fold.isVisible()) await fold.click();
    const list = page.getByRole('table', { name: '按宿主分组的容器', exact: true });
    const alphaLink = list.getByRole('button', { name: alpha.desired.spec.name, exact: true });
    await alphaLink.waitFor();
    if (width === 1440) {
      await alphaLink.click();
      await page.getByRole('heading', { name: alpha.desired.spec.name, exact: true }).waitFor();
      await page.goBack();
      await list.waitFor();
    }
    await page.screenshot({ path: path.join(directory, `board-${width}.png`), fullPage: true });
    const before = await (await context.request.get(origin + '/cvm/' + state.instances.gamma)).json();
    await page.goto(origin + '/#/cvm/instance/' + state.instances.gamma);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const routes = page.getByRole('table', { name: '目标路由', exact: true });
    await routes.waitFor();
    await routes
      .getByRole('button', { name: /^编辑路由 / })
      .first()
      .click();
    await routes.getByRole('textbox', { name: '目标 IP／网段', exact: true }).fill('198.18.0.0/15');
    await routes.getByRole('button', { name: '应用修改', exact: true }).click();
    await page.getByRole('button', { name: '查看配置差异', exact: true }).click();
    await page.getByRole('status').filter({ hasText: '服务端配置检查通过' }).waitFor();
    await page.screenshot({ path: path.join(directory, `routes-preview-${width}.png`), fullPage: true });
    const discard = page.getByRole('button', { name: '丢弃当前草稿', exact: true });
    await discard.click();
    await discard.waitFor({ state: 'detached' });
    const after = await (await context.request.get(origin + '/cvm/' + state.instances.gamma)).json();
    if (JSON.stringify(before.desired) !== JSON.stringify(after.desired))
      throw new Error('preview changed actual desired generation');
    await page.getByRole('tab', { name: '产物', exact: true }).click();
    await page.getByText('冻结产物', { exact: true }).waitFor();
    if (await fold.isVisible()) await fold.click();
    await page.locator('summary').filter({ hasText: '冻结输入与镜像清单' }).click();
    await page.screenshot({ path: path.join(directory, `input-manifest-${width}.png`), fullPage: true });
    if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth))
      throw new Error(`overflow at ${width}`);
    if (errors.length || previews.some(s => s !== 200) || !previews.length)
      throw new Error(`real interaction failed at ${width}: ${JSON.stringify({ errors, previews })}`);
    checks.push({ width, errors, previews, unchanged_desired: true });
    console.log(`PASS real KVM chain UI at ${width}px: board, real preview, primary routes and frozen manifest`);
    await page.close();
  }
} finally {
  await fs.writeFile(path.join(directory, 'checks.json'), JSON.stringify(checks, null, 2));
  await browser.close();
}
