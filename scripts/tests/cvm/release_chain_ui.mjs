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
  for (const width of [1440, 390, 320]) {
    const page = await context.newPage();
    await page.setViewportSize({ width, height: width === 1440 ? 2000 : 1050 });
    const errors = [],
      previews = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('response', r => {
      if (new URL(r.url()).pathname.endsWith('/artifacts/preview')) previews.push(r.status());
    });
    await page.goto(origin + '/#/cvm');
    await page.locator('.cvm-page').waitFor();
    const fold = page.getByRole('button', { name: '收起首次上线清单', exact: true });
    if (await fold.isVisible()) await fold.click();
    if (width === 1440) {
      await page.locator(`[data-pill="${state.instances.alpha}"]`).click();
      const exit = page.locator(`[data-exit="chain:${state.chain.id}"]`);
      await exit.waitFor();
      await page.getByRole('button', { name: '＋ 出口' }).click();
      await exit.click();
      const proposal = page.getByRole('region', { name: '待确认的组合变更' });
      await proposal.waitFor();
      if (!(await proposal.innerText()).includes('alice')) throw new Error('proposal has wrong authorization owner');
      await proposal.getByRole('button', { name: '查看配置差异', exact: true }).click();
      await proposal.getByText('查看生成文件', { exact: true }).click();
      await proposal.locator('.cvm-artifact-content').first().waitFor();
      if (
        !(await proposal.evaluate(
          n =>
            n.scrollWidth <= n.clientWidth + 1 &&
            n.getBoundingClientRect().right <= n.parentElement.getBoundingClientRect().right + 1,
        ))
      )
        throw new Error('compiled proposal escapes inspector');
      if (await fold.isVisible()) await fold.click();
      await page.screenshot({ path: path.join(directory, 'board-proposal.png'), fullPage: true });
      await proposal.getByRole('button', { name: '取消', exact: true }).click();
    }
    await page.screenshot({ path: path.join(directory, `board-${width}.png`), fullPage: true });
    const before = await (await context.request.get(origin + '/cvm/' + state.instances.gamma)).json();
    await page.goto(origin + '/#/cvm/instance/' + state.instances.gamma);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    await page.getByRole('button', { name: '路由表', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '容器路由表' });
    await dialog
      .getByRole('option', { name: /KVM split chain/ })
      .first()
      .waitFor({ state: 'attached' });
    const save = dialog.getByRole('button', { name: '保存变更', exact: true });
    if (await save.isDisabled()) throw new Error('save should check configuration automatically');
    await dialog.getByRole('button', { name: '查看配置差异', exact: true }).click();
    await save.waitFor();
    await page.waitForFunction(() => !document.querySelector('.cvm-dialog footer button.btn.primary')?.disabled);
    await dialog.getByText('查看生成文件', { exact: true }).click();
    await dialog.locator('.cvm-artifact-content').first().waitFor();
    if (!(await dialog.locator('.cvm-dialog-body').evaluate(n => n.scrollWidth <= n.clientWidth + 1)))
      throw new Error('compiled route dialog clips its children');
    await page.screenshot({ path: path.join(directory, `routes-preview-${width}.png`), fullPage: true });
    await dialog.getByRole('button', { name: '取消', exact: true }).click();
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
