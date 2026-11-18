// Actual KVM Console UI acceptance. No fixture server and no intercepted API responses.
import { chromium } from '../../../frontend/node_modules/playwright/index.mjs';
import fs from 'node:fs/promises';
import path from 'node:path';
const output = process.argv[2];
const lab = JSON.parse(await fs.readFile(path.join(output, 'lab-3/lab.json'), 'utf8'));
const credentials = JSON.parse(await fs.readFile(path.join(output, 'services/credentials.json'), 'utf8'));
const state = JSON.parse(await fs.readFile(path.join(output, 'acceptance.json'), 'utf8'));
const origin = `http://127.0.0.1:${lab.console_port}`;
const directory = path.join(output, 'ui');
await fs.mkdir(directory, { recursive: true, mode: 0o700 });
const browser = await chromium.launch({ headless: true });
const checks = [];
try {
  const context = await browser.newContext({ locale: 'zh-CN', timezoneId: 'Asia/Taipei', reducedMotion: 'reduce' });
  const login = await context.request.post(origin + '/auth/login', {
    data: { operator_id: credentials.operator_id, password: credentials.password },
  });
  if (!login.ok()) throw new Error(`actual Console login: ${login.status()}`);
  for (const width of [1440, 390, 320]) {
    const page = await context.newPage();
    await page.setViewportSize({ width, height: 1050 });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const requests = [];
    page.on('response', response => {
      const url = new URL(response.url());
      if (url.pathname.includes('/artifacts')) requests.push({ path: url.pathname, status: response.status() });
    });
    await page.goto(origin + '/#/deploy/detail/' + state.ui_deployment);
    await page.getByText('产物记录', { exact: true }).waitFor();
    const foldOnboarding = page.getByRole('button', { name: '收起首次上线清单', exact: true });
    if (await foldOnboarding.isVisible()) await foldOnboarding.click();
    const disclosure = page.locator('.cg-artifact-review');
    if (!(await disclosure.evaluate(node => node.open))) await disclosure.locator('summary').click();
    const native = page.getByRole('button', { name: /容器 alpha · cube\/attachments.json/ });
    await native.click();
    await page.locator('.cga-detail').first().waitFor();
    if ((await page.getByText('xray.json', { exact: true }).count()) < 1)
      throw new Error('ordinary machine artifact missing from mixed release');
    if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth))
      throw new Error(`mixed release horizontal overflow at ${width}`);
    await page.screenshot({ path: path.join(directory, `mixed-${width}.png`), fullPage: true });
    await page.goto(origin + '/#/cvm/instance/' + state.instances.alpha);
    await page.getByRole('tab', { name: '产物', exact: true }).click();
    if (await foldOnboarding.isVisible()) await foldOnboarding.click();
    await page.getByText('冻结产物', { exact: true }).waitFor();
    for (const kind of ['cube/sandbox.json', 'cube/egress.json', 'cube/attachments.json', 'guest/init.json']) {
      await page.locator('.cvm-artifact-file').filter({ hasText: kind }).click();
      await page.getByRole('button', { name: '全文', exact: true }).click();
      await page.locator('.cvm-artifacts pre').first().waitFor();
    }
    if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth))
      throw new Error(`native artifact horizontal overflow at ${width}`);
    if (
      !requests.some(
        request => request.path === '/cvm/' + state.instances.alpha + '/artifacts' && request.status === 200,
      )
    )
      throw new Error('native artifact detail did not load actual generation from KVM Console');
    if (errors.length) throw new Error('actual page runtime error: ' + errors[0]);
    await page.screenshot({ path: path.join(directory, `native-${width}.png`), fullPage: true });
    checks.push({ width, mixed: true, native_files: 4, runtime_errors: errors, real_artifact_requests: requests });
    console.log(`PASS actual KVM UI: mixed release and all native artifacts at ${width}px`);
    await page.close();
  }
} finally {
  await fs.writeFile(path.join(directory, 'checks.json'), JSON.stringify(checks, null, 2));
  await browser.close();
}
