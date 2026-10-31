import { test, expect, type Page } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

// Warm the product module graph once before screenshot timing; Vite's HTML readiness
// does not imply its first cold client transform has finished.
test.beforeAll(async ({ browser, baseURL }, info) => {
  info.setTimeout(60_000);
  const page = await browser.newPage();
  try {
    await page.goto(`${baseURL}/tests/fixtures/user-header-preview.html`);
    await expect(page.locator('.user-usage-card:not(.user-forwarding-usage)')).toBeVisible({ timeout: 45_000 });
  } finally {
    await page.close();
  }
});

const pageErrors = new WeakMap<Page, string[]>();
test.beforeEach(({ page }) => {
  const errors: string[] = [];
  pageErrors.set(page, errors);
  page.on('pageerror', error => errors.push(error.message));
});
test.afterEach(({ page }) => expect(pageErrors.get(page)).toEqual([]));

const evidence = fileURLToPath(new URL('../../../.tools/frontend-visual/screenshots/', import.meta.url));

const textContrast = (foreground: string, background: string) => {
  const luminance = (color: string) =>
    color
      .match(/[\d.]+/g)!
      .slice(0, 3)
      .map(Number)
      .map(value => (color.startsWith('color(srgb') ? value : value / 255))
      .map(value => (value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4))
      .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  const a = luminance(foreground);
  const b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
};

async function preview(page: Page, query = '') {
  await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
  await page.goto(`/tests/fixtures/user-header-preview.html?user=alice${query}`);
  await expect(page.locator('.qta-cap').first()).toContainText('500 GiB');
  await page.evaluate(() => document.fonts.ready);
}

for (const [name, width, theme] of [
  ['desktop', 1280, 'dark'],
  ['mobile', 390, 'dark'],
  ['light', 1280, 'light'],
] as const) {
  test(`${name}: app quota and exhaustion editor`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await preview(page, `&theme=${theme}`);
    const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
    await expect(card.getByText('共享总额度')).toHaveCount(0);
    await expect(card.locator('.user-usage-ledger .user-traffic-policy')).toHaveCount(0);
    await expect(card.locator('.qta-policy-flag')).toHaveText('用满后限速 5 Mbps');
    await expect(card).toHaveScreenshot(`access-${name}.png`);
    await page.getByRole('button', { name: 'APAC Premium · 额度与限速' }).click();
    const dialog = page.getByRole('form', { name: 'APAC Premium · 额度与限速' });
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(dialog.getByLabel('本期流量额度（GiB）')).toHaveValue('500');
    await expect(dialog.getByLabel('超额速度上限（Mbps）')).toHaveValue('5');
    await expect(card).toHaveScreenshot(`editor-${name}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    if (name === 'desktop') {
      await mkdir(evidence, { recursive: true });
      await page.screenshot({ path: `${evidence}user-traffic-policy-editor.png` });
      await page
        .locator('.user-traffic-policy-editor')
        .screenshot({ path: `${evidence}user-traffic-policy-editor-detail.png` });
    }
  });
}

test('forwarding has no user total quota and read-only viewers cannot edit', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await preview(page, '&role=readonly');
  await expect(page.getByRole('button', { name: 'APAC Premium · 额度与限速' })).toHaveCount(0);
  await page.getByRole('tab', { name: '转发', exact: true }).click();
  const card = page.locator('.user-forwarding-usage');
  await expect(card.locator('.user-traffic-policy')).toHaveCount(0);
  await expect(card.getByText('总额度', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '设置转发总额度' })).toHaveCount(0);
  await expect(card).toHaveScreenshot('forwarding-readonly.png');
});

test('exhaustion is explicit on a small screen', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1000 });
  await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
  await page.goto('/tests/fixtures/user-header-preview.html?traffic=exhausted');
  await expect(page.locator('.qta-policy-flag').first()).toHaveText('限速中 · 5 Mbps');
  await expect(page.locator('.user-usage-card:not(.user-forwarding-usage)')).toHaveScreenshot('exhausted-mobile.png');
});

test('pending synchronization is explicit', async ({ page }) => {
  await preview(page, '&traffic=pending');
  await expect(page.locator('.qta-policy-sync').first()).toHaveAttribute('title', '等待 1 台机器同步');
  await expect(page.locator('.qta-policy-spin')).toHaveCount(1);
});

test('quota boundary labels stay inside the track at intermediate widths and extreme overflow', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
  for (const traffic of ['pending', 'extreme']) {
    for (const width of [1440, 1100, 390, 360]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/user-header-preview.html?traffic=${traffic}`);
      const row = page.locator('.qta-r').first();
      await expect(row.locator('.qta-policy-sync')).toBeVisible();
      const layout = await row.evaluate(row => {
        const box = (selector: string) => {
          const rect = row.querySelector(selector)!.getBoundingClientRect();
          return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
        };
        return {
          amount: box('.qta-figs'),
          balance: box('.qta-left-value'),
          label: box('.qta-policy-flag'),
          track: box('.qta-meter-track'),
          overflow: row.scrollWidth > row.clientWidth,
        };
      });
      expect(layout.overflow, JSON.stringify({ traffic, width, layout })).toBe(false);
      expect(layout.label.left).toBeGreaterThanOrEqual(layout.track.left - 1);
      expect(layout.label.right).toBeLessThanOrEqual(layout.track.right + 1);
      expect(layout.balance.left >= layout.amount.right - 1 || layout.balance.top >= layout.amount.bottom - 1).toBe(
        true,
      );
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      if (traffic === 'extreme' && width === 360)
        await expect(page.locator('.user-usage-card:not(.user-forwarding-usage)')).toHaveScreenshot(
          'extreme-overflow-mobile.png',
        );
    }
  }
});

test('unsupported nodes prevent enabling throttle', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1000 });
  await preview(page, '&traffic=unsupported');
  await page.getByRole('button', { name: 'APAC Premium · 额度与限速' }).click();
  const dialog = page.getByRole('form', { name: 'APAC Premium · 额度与限速' });
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(dialog.getByRole('alert')).toContainText('请先升级');
  await dialog.getByLabel('超额速度上限（Mbps）').fill('6');
  await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  await expect(page.locator('.user-usage-card:not(.user-forwarding-usage)')).toHaveScreenshot('unsupported-mobile.png');
});

for (const width of [1280, 390])
  test(`independent apps at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
    await page.goto('/tests/fixtures/user-header-preview.html?apps=two&traffic=exhausted');
    const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
    const first = card
      .getByRole('listitem')
      .filter({ has: page.getByRole('button', { name: 'APAC Premium · 额度与限速' }) });
    const second = card
      .getByRole('listitem')
      .filter({ has: page.getByRole('button', { name: 'JP Budget · 额度与限速' }) });
    await expect(first.getByText('限速中 · 5 Mbps', { exact: true })).toBeVisible();
    await expect(second.getByText('用满后停用', { exact: true })).toBeVisible();
    await expect(second).toContainText('剩余38.00 GiB');
    await expect(card).toHaveScreenshot(`independent-apps-${width}.png`);
    await second.getByRole('button', { name: 'JP Budget · 额度与限速' }).click();
    const dialog = page.getByRole('form', { name: 'JP Budget · 额度与限速' });
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(first.locator('form')).toHaveCount(0);
    await expect(dialog.getByLabel('本期流量额度（GiB）')).toHaveValue('50');
    await expect(dialog.getByLabel('超额速度上限（Mbps）')).toHaveCount(0);
    await expect(card).toHaveScreenshot(`inline-selected-app-${width}.png`);
    await mkdir(evidence, { recursive: true });
    await card.screenshot({ path: `${evidence}user-app-quota-inline-${width}.png` });
    await dialog.getByRole('button', { name: '取消' }).click();
    await expect(dialog).toHaveCount(0);
    await expect(second.getByRole('button', { name: 'JP Budget · 额度与限速' })).toBeFocused();
    await first.getByRole('button', { name: 'APAC Premium · 额度与限速' }).click();
    const appA = page.getByRole('form', { name: 'APAC Premium · 额度与限速' });
    await expect(appA.getByLabel('本期流量额度（GiB）')).toHaveValue('100');
    await expect(appA.getByLabel('超额速度上限（Mbps）')).toHaveValue('5');
    await expect(second.locator('form')).toHaveCount(0);
    await expect(card).toHaveScreenshot(`inline-rate-app-${width}.png`);
    await card.screenshot({ path: `${evidence}user-app-quota-inline-rate-${width}.png` });
  });

for (const width of [1280, 390])
  test(`suspended and normal apps at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
    await page.goto('/tests/fixtures/user-header-preview.html?apps=two&traffic=stopped');
    const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
    await expect(card.getByText('已停用', { exact: true })).toBeVisible();
    await expect(card.getByText('用满后停用', { exact: true })).toBeVisible();
    await expect(card).toHaveScreenshot(`suspended-apps-${width}.png`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

test('machine forwarding quota edits in place on mobile', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1000 });
  await preview(page, '&forwarding=quota');
  await page.getByRole('tab', { name: '转发', exact: true }).click();
  await expect(page.getByRole('button', { name: '设置转发总额度' })).toHaveCount(0);
  await page
    .getByRole('button', { name: /编辑 .* 的转发授权/ })
    .first()
    .click();
  const form = page.getByRole('form', { name: /编辑 .* 的转发授权/ });
  await expect(form.getByLabel('本期转发额度（GiB）')).toBeVisible();
  await form.getByLabel('本期转发额度（GiB）').fill('50');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await expect(page.locator('.user-forwarding-usage')).toHaveScreenshot('forwarding-inline-mobile.png');
});

// Each scene uses the production pane directly; B has no portal or substitute quota rows.
for (const scene of ['all', 'normal', 'single', 'throttled', 'suspended', 'pending', 'paused']) {
  for (const width of [1440, 390]) {
    for (const theme of ['dark', 'light']) {
      for (const role of ['system-admin', 'readonly']) {
        test(`B ${scene} ${width} ${theme} ${role}`, async ({ page }) => {
          await page.setViewportSize({ width, height: 1100 });
          await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
          await page.goto(
            `/tests/fixtures/user-quota-status-mockup.html?option=b&scene=${scene}&role=${role}&theme=${theme}&nav=0&run=0`,
          );
          const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
          await expect(card.locator('.qta-r').first()).toBeVisible();
          await expect(card.locator('.qs-own, .mk-slot')).toHaveCount(0);
          if (scene === 'throttled' || scene === 'all') {
            await expect(card.getByText('限速中 · 2 Mbps', { exact: true })).toBeVisible();
            await expect(card.locator('.throttled .qta-overflow')).toHaveCount(1);
          }
          if (scene === 'suspended' || scene === 'all')
            await expect(card.getByText('已停用', { exact: true })).toBeVisible();
          if (scene === 'pending') await expect(card.locator('.qta-policy-sync')).toHaveCount(1);
          if (scene === 'paused') {
            await expect(card.getByText('服务暂停', { exact: true })).toHaveCount(2);
            const pausedColors = await card.locator('.qta-r.paused').evaluateAll(rows =>
              rows.map(row => ({
                fill: getComputedStyle(row.querySelector('.qta-t > i')!).backgroundColor,
                percent: getComputedStyle(row.querySelector('.qta-pct')!).color,
                state: getComputedStyle(row.querySelector('.qta-policy-flag')!).color,
              })),
            );
            expect(new Set(pausedColors.map(row => row.fill)).size).toBe(1);
            expect(pausedColors.every(row => row.percent === row.state)).toBe(true);
          }
          if (role === 'readonly') await expect(card.locator('.qta-pencil')).toHaveCount(0);
          expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
          const labelColors = await card.locator('.qta-policy-flag, .qta-policy-sync, .qta-pct').evaluateAll(labels =>
            labels.map(label => ({
              text: label.textContent,
              foreground: getComputedStyle(label).color,
              background: getComputedStyle(label.closest('.user-usage-card')!).backgroundColor,
            })),
          );
          for (const label of labelColors)
            expect(textContrast(label.foreground, label.background), label.text ?? '').toBeGreaterThanOrEqual(4.5);
          await page.evaluate(() => document.fonts.ready);
          await expect(card).toHaveScreenshot(`b-${scene}-${width}-${theme}-${role}.png`);
        });
      }
    }
  }
}

test('B edits a suspended app into throttling and then completes synchronization', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 1100 });
  await page.goto('/tests/fixtures/user-quota-status-mockup.html?option=b&scene=suspended&nav=0');
  const row = page.getByRole('listitem').filter({ has: page.getByRole('button', { name: 'EU Lite · 额度与限速' }) });
  await expect(row.getByText('已停用', { exact: true })).toBeVisible();
  await row.getByRole('button', { name: 'EU Lite · 额度与限速' }).click();
  const form = row.getByRole('form');
  await form.getByRole('button', { name: '限速', exact: true }).click();
  await form.getByLabel('超额速度上限（Mbps）').fill('3');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(form).toHaveCount(0);
  await expect(row.locator('.qta-policy-flag')).toContainText('限速中 · 3 Mbps');
  await expect(row.locator('.qta-policy-sync')).toBeVisible();
  await expect(row).toHaveClass(/throttled/);
  await expect(row.locator('.qta-policy-sync')).toHaveCount(0, { timeout: 7000 });
  await expect(row.getByText('限速中 · 3 Mbps', { exact: true })).toBeVisible();
});
