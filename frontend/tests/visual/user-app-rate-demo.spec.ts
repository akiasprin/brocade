import { test, expect, type Page } from '@playwright/test';
const route = '/tests/fixtures/user-app-rate-demo.html';
const errors = new WeakMap<Page, string[]>();
test.beforeEach(({ page }) => {
  const messages: string[] = [];
  errors.set(page, messages);
  page.on('pageerror', error => messages.push(error.message));
  page.on('request', request => {
    if (['fetch', 'xhr', 'eventsource', 'websocket'].includes(request.resourceType()))
      messages.push(`Unexpected product request: ${request.url()}`);
  });
});
test.afterEach(({ page }) => expect(errors.get(page)).toEqual([]));
const names: Record<string, string> = { 'apac-premium': 'APAC Premium', 'apac-promising': 'APAC Promising' };
const group = (page: Page, id: string) =>
  page
    .getByRole('listitem')
    .filter({ has: page.getByRole('button', { name: `${names[id]} · 额度与限速`, exact: true }) });
const flags = (page: Page, id: string) => group(page, id).locator('.qta-policy-flag');
async function open(page: Page, query = '') {
  await page.goto(`${route}?${query}`);
  await expect(page.getByRole('heading', { name: '用量与额度', exact: true })).toBeVisible();
  await expect(page.locator('.user-access-quotas .qta-r')).toHaveCount(7);
  await expect(flags(page, 'apac-promising')).toContainText('常态限速 50 Mbps');
  if ((page.viewportSize()?.width ?? 1280) > 820) {
    await expect(page.getByRole('listbox', { name: '用户列表' })).toBeVisible();
    await expect(page.getByRole('listbox', { name: '用户列表' })).not.toContainText('额度已用满');
  }
  await page.evaluate(() => document.fonts.ready);
}
async function edit(page: Page, id: string) {
  const row = group(page, id);
  await row.getByRole('button', { name: /额度与限速/ }).click();
  const form = row.getByRole('form');
  await expect(form).toBeVisible();
  return form;
}
for (const [name, width, theme] of [
  ['desktop-dark', 1440, 'dark'],
  ['desktop-light', 1440, 'light'],
  ['mobile-dark', 390, 'dark'],
  ['mobile-light', 390, 'light'],
] as const) {
  test(`${name}: real shell, roster and quota rows`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await open(page, `theme=${theme}&controls=0`);
    await expect(page.getByRole('button', { name: '返回机器首页' })).toBeVisible();
    await expect(page).toHaveScreenshot(`${name}.png`, { fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(await group(page, 'apac-premium').evaluate(row => row.scrollWidth > row.clientWidth)).toBe(false);
  });
}
test('production page with synthetic data and no substitute rows', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${route}?feature=0&controls=0`);
  await expect(page.getByRole('heading', { name: '用量与额度', exact: true })).toBeVisible();
  await expect(page.locator('.user-access-quotas .qta-r')).toHaveCount(7);
  await expect(page.locator('.rate-demo-slot,.rate-demo-row')).toHaveCount(0);
  await page.evaluate(() => document.fonts.ready);
  await expect(page).toHaveScreenshot('production-reference-desktop.png', { fullPage: true });
});
for (const [name, width, theme, id] of [
  ['combined-desktop', 1440, 'light', 'apac-premium'],
  ['combined-mobile', 390, 'dark', 'apac-promising'],
] as const) {
  test(`${name}: native inline editor with the new base rate field`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1100 });
    await open(page, `theme=${theme}&controls=0`);
    const form = await edit(page, id);
    await form.getByLabel('常态速度上限（Mbps）').scrollIntoViewIfNeeded();
    await expect(form.getByLabel('常态速度上限（Mbps）')).toHaveValue(id === 'apac-premium' ? '100' : '50');
    await expect(page).toHaveScreenshot(`${name}.png`, { fullPage: true });
    expect(await form.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
  });
}
test('save a base speed without a quota; preserve the other groups and focus', async ({ page }) => {
  await open(page);
  const form = await edit(page, 'apac-promising');
  await expect(form.getByLabel('本期流量额度（GiB）')).toHaveValue('');
  await form.getByLabel('常态速度上限（Mbps）').fill('25');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-promising')).toContainText('常态限速 25 Mbps');
  await expect(flags(page, 'apac-promising')).toContainText('同步中');
  await expect(flags(page, 'apac-promising')).not.toContainText('同步中');
  await expect(group(page, 'apac-promising').locator('.qta-left')).toHaveText('不限额度');
  await expect(group(page, 'apac-promising').getByRole('button')).toBeFocused();
  await expect(flags(page, 'apac-premium')).toContainText('常态限速 100 Mbps');
});
test('exhaustion and a new billing cycle retain the base cap', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: '模拟 APAC Premium 用满', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('限速中 · 5 Mbps');
  await expect(flags(page, 'apac-premium')).toContainText('常态限速 100 Mbps');
  await page.getByRole('button', { name: '新账期', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('用满后限速 5 Mbps');
  await expect(flags(page, 'apac-premium')).toContainText('常态限速 100 Mbps');
  await expect(group(page, 'apac-premium').locator('.qta-figs')).toContainText('0');
  await expect(page.locator('.user-usage-ledger')).toContainText('12 月 1 日');
});
test('removing quota retains speed; removing speed retains the exhaustion action', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: '模拟 APAC Premium 用满', exact: true }).click();
  let form = await edit(page, 'apac-premium');
  await form.getByLabel('常态速度上限（Mbps）').fill('');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-premium')).not.toContainText('常态限速');
  await expect(flags(page, 'apac-premium')).toContainText('限速中 · 5 Mbps');
  form = await edit(page, 'apac-premium');
  await form.getByLabel('常态速度上限（Mbps）').fill('80');
  await form.getByLabel('本期流量额度（GiB）').fill('');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('常态限速 80 Mbps');
  await expect(group(page, 'apac-premium').locator('.qta-left')).toHaveText('不限额度');
  await expect(group(page, 'apac-premium').locator('.qta-meter')).toHaveCount(0);
});
test('validation and Escape keep the saved policy unchanged', async ({ page }) => {
  await open(page);
  const form = await edit(page, 'apac-promising');
  await form.getByLabel('常态速度上限（Mbps）').fill('0');
  await expect(form.getByRole('alert')).toContainText('速度需在');
  await expect(form.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  await form.getByLabel('常态速度上限（Mbps）').press('Escape');
  await expect(form).toHaveCount(0);
  await expect(flags(page, 'apac-promising')).toContainText('常态限速 50 Mbps');
});
test('the lower cap applies after exhaustion; suspension still takes priority', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: '模拟 APAC Premium 用满', exact: true }).click();
  let form = await edit(page, 'apac-premium');
  await form.getByLabel('常态速度上限（Mbps）').fill('3');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('限速中 · 3 Mbps');
  form = await edit(page, 'apac-premium');
  await form.getByRole('button', { name: '停用', exact: true }).click();
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('已停用');
});
test('switching users keeps changes scoped to the selected user', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await open(page);
  const form = await edit(page, 'apac-promising');
  await form.getByLabel('常态速度上限（Mbps）').fill('25');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await page.getByRole('option', { name: /^carol / }).click();
  await expect(flags(page, 'apac-promising')).toContainText('常态限速 50 Mbps');
  await page.getByRole('option', { name: /^alice / }).click();
  await expect(flags(page, 'apac-promising')).toContainText('常态限速 25 Mbps');
});

test('both speed controls switch units without changing the rate', async ({ page }) => {
  await open(page);
  const form = await edit(page, 'apac-premium');
  await form.getByLabel('常态速度单位').selectOption('Kbps');
  await expect(form.getByLabel('常态速度上限（Kbps）')).toHaveValue('100000');
  await form.getByLabel('超额速度单位').selectOption('Kbps');
  await expect(form.getByLabel('超额速度上限（Kbps）')).toHaveValue('5000');
  await expect(form.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  await form.getByLabel('超额速度上限（Kbps）').fill('512');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(flags(page, 'apac-premium')).toContainText('用满后限速 512 Kbps');
});
