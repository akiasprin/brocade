import { expect, test, type Page } from '@playwright/test';
async function open(page: Page, scene: string, width = 390, height = 844, theme = 'light') {
  await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/ui-regressions.html?scene=${scene}&theme=${theme}`);
  await page.evaluate(() => document.fonts.ready);
}
for (const [width, theme] of [
  [320, 'light'],
  [390, 'dark'],
  [1440, 'light'],
] as const) {
  test(`diagnostics ${width}: readable code and expanded URL fit`, async ({ page }) => {
    await open(page, 'diagnostics', width, 844, theme);
    const table = page.locator('.dg-tbl');
    await expect(table).toBeVisible();
    if (width < 800) expect((await table.locator('.c-code').nth(1).boundingBox())!.width).toBeGreaterThanOrEqual(112);
    await expect(page).toHaveScreenshot(`diagnostics-${width}-collapsed.png`);
    await table.locator('.dg-r').first().click();
    const body = page.locator('.dg-b');
    expect(await body.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    await expect(page).toHaveScreenshot(`diagnostics-${width}-expanded.png`);
  });
  test(`reverse stale ${width}: long peers have a separate row`, async ({ page }) => {
    await open(page, 'reverse', width, 844, theme);
    await page
      .locator('details.rvh-fold')
      .filter({ has: page.locator('.ov-rev') })
      .first()
      .locator(':scope > summary')
      .click();
    const summary = page.locator('.ov-rev > summary').first();
    await expect(summary).toBeVisible();
    await expect(summary).toContainText('数据已过期');
    if (width < 800) {
      const peer = await summary.locator('.peer').boundingBox();
      const nums = await summary.locator('.nums').boundingBox();
      expect(peer!.width).toBeGreaterThan(180);
      expect(nums!.y).toBeGreaterThanOrEqual(peer!.y + peer!.height);
    }
    await expect(page).toHaveScreenshot(`reverse-stale-${width}.png`);
  });
}
test('deep rule heads keep the complete name outside the delete control', async ({ page }) => {
  await open(page, 'rules', 320, 844);
  const heads = page.locator('.chain-rule-node-head');
  await expect(heads).toHaveCount(12);
  const last = heads.last();
  await last.scrollIntoViewIfNeeded();

  const text = await last.locator('.who b').boundingBox();
  const control = await last.locator('.hopctl').boundingBox();
  expect(text!.x + text!.width).toBeLessThanOrEqual(control!.x);
  expect(await last.locator('.who b').evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  expect(await last.evaluate(el => parseFloat(getComputedStyle(el).paddingLeft))).toBeLessThanOrEqual(45);

  await expect(page).toHaveScreenshot('deep-rules-320.png');
});
test('advanced protocol fields keep the right border clickable', async ({ page }) => {
  await open(page, 'chains', 320, 844);
  const summary = page
    .locator('summary')
    .filter({ hasText: /AnyTLS/ })
    .first();
  await summary.click();
  const advanced = page.locator('.form-adv').filter({ hasText: /复用/ }).first();
  await expect(advanced).toBeAttached();
  if (!(await advanced.getAttribute('open'))) await advanced.locator('summary').click();
  await advanced.scrollIntoViewIfNeeded();
  const input = advanced.locator('input').first();
  await input.evaluate(el => el.scrollIntoView({ block: 'center' }));

  const clip = await advanced.evaluate(el => ({
    width: el.getBoundingClientRect().width,
    parent: el.parentElement!.getBoundingClientRect().width,
  }));
  expect(clip.width).toBeLessThanOrEqual(clip.parent + 1);
  await expect
    .poll(async () =>
      input.evaluate(el => {
        const r = el.getBoundingClientRect();
        return document.elementFromPoint(r.right - 2, r.top + r.height / 2) === el;
      }),
    )
    .toBe(true);

  await expect(advanced).toHaveScreenshot('protocol-advanced-320.png');
});
test('128-core history tooltip scrolls to the final core', async ({ page }) => {
  await open(page, 'telemetry', 390, 844, 'dark');
  await page.getByRole('button', { name: /CPU/ }).click();
  const chart = page.locator('.history-chart-card').filter({ hasText: '逐核繁忙度' });
  await chart.locator('.observe-plot').evaluate(el => el.scrollIntoView({ block: 'center' }));
  const canvas = chart.locator('canvas').first();
  await expect(canvas).toBeVisible();
  await canvas.hover({ position: { x: 100, y: 50 } });
  const tip = page.locator('.observe-chart-tip').filter({ hasText: 'CPU 127' });
  await expect(tip).toBeVisible();

  const rect = await tip.boundingBox();
  expect(rect!.height).toBeLessThanOrEqual(322);
  expect(await tip.evaluate(el => getComputedStyle(el).pointerEvents)).not.toBe('none');
  await tip.hover();
  await page.mouse.wheel(0, 5000);
  await expect.poll(() => tip.evaluate(el => el.scrollTop)).toBeGreaterThan(1500);

  await expect(tip).toHaveScreenshot('128-core-tooltip.png');
});
test('deployment menu remains clickable throughout its entrance', async ({ page }) => {
  await open(page, 'deploy');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.getByLabel('更多操作', { exact: true }).click();
  const second = page.getByRole('menuitem', { name: /取消并回滚/ });
  await expect(second).toBeVisible();

  const misses = await second.evaluate(async el => {
    const misses: number[] = [];
    for (let i = 0; i < 16; i++) {
      await new Promise(requestAnimationFrame);
      const b = el.getBoundingClientRect();
      if (!el.contains(document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2))) misses.push(i);
    }
    return misses;
  });
  expect(misses).toEqual([]);
  await second.click();
  await expect(page.getByRole('dialog')).toBeVisible();

  await expect(page).toHaveScreenshot('deployment-menu-confirm.png');
});
test('drain timeout preserves individual keystrokes and validates empty input', async ({ page }) => {
  await open(page, 'binary');
  await page.getByRole('radio', { name: '双跑交接 Beta' }).check();
  const input = page.getByLabel('旧进程排空等待秒数');
  await input.fill('');
  await expect(input).toHaveValue('');
  await input.pressSequentially('30');
  await expect(input).toHaveValue('30');
  await input.fill('120');
  await input.press('ControlOrMeta+A');
  await input.pressSequentially('30');
  await expect(input).toHaveValue('30');
  await input.fill('30');
  await expect(input).toHaveValue('30');
  await expect(page).toHaveScreenshot('drain-timeout-30.png');
});

for (const width of [390, 1440])
  test(`machine tooltip ${width}: last row flips above its anchor and escapes the card`, async ({ page }) => {
    await page.setViewportSize({ width, height: 568 });
    await page.goto('/tests/fixtures/user-machine-preview.html?scene=dense&theme=light');
    const cells = page.locator('.uxm-row:not(.total) button.uxm-cell');
    await expect(page.getByRole('button', { name: '展开其余 24 台机器' })).toBeVisible();
    const cell = cells.nth((12 - 1) * 24 + 10);
    await cell.scrollIntoViewIfNeeded();
    await cell.focus();
    await cell.evaluate(el => el.scrollIntoView({ block: 'end' }));
    const tip = page.getByRole('tooltip');
    await expect(tip).toBeVisible();
    await expect(tip).toContainText('下行');

    const b = await tip.boundingBox();
    expect(b!.y).toBeGreaterThanOrEqual(8);
    expect(b!.y + b!.height).toBeLessThanOrEqual(560);
    const anchor = await cell.boundingBox();
    expect(b!.y + b!.height).toBeLessThanOrEqual(anchor!.y - 5);
    expect(await tip.evaluate(el => el.parentElement === document.body)).toBe(true);
    expect(
      await tip.evaluate(el => {
        const r = el.getBoundingClientRect();
        return el.contains(document.elementFromPoint(r.x + 5, r.bottom - 5));
      }),
    ).toBe(true);

    await expect(page).toHaveScreenshot(`machine-tooltip-${width}.png`);
  });

test('a failed child module keeps the active editor and waits for its save before reloading', async ({ page }) => {
  await page.route('**/ui-regressions-module.tsx', route => route.abort('failed'));
  let navigations = 0;
  page.on('framenavigated', frame => {
    if (frame === page.mainFrame()) navigations++;
  });
  await open(page, 'recovery');
  await page.getByLabel('保留中的输入').fill('待保存的输入');
  await page.getByRole('button', { name: '打开子层' }).click();
  await expect(page.getByRole('alert')).toContainText('这部分界面未能加载');
  await expect(page.getByLabel('保留中的输入')).toHaveValue('待保存的输入');
  await expect(page.getByRole('status')).toContainText('完成当前操作');
  expect(navigations).toBe(1);
  await page.getByRole('button', { name: '完成编辑' }).click();
  await page.getByRole('button', { name: '重新加载界面' }).click();
  await expect.poll(() => navigations).toBe(2);
  await expect(page.getByLabel('保留中的输入')).toHaveValue('');
});
