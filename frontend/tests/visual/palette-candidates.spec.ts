import { test, expect, type Page } from '@playwright/test';

const palettes = ['jinzi', 'songlv', 'oufen', 'xuanmo'];
const keys = ['blue-reference', 'v1', 'v2', 'v3', 'v4', 'v5'];
// Small compositing tolerance; no differing pixels are allowed beyond it.
const screenshotOptions = { threshold: 0.05 };

async function specimens(page: Page) {
  await expect(page.locator('.study-card')).toHaveCount(6);
  let reference: string[] | undefined;
  for (const key of keys) {
    const frame = page.frameLocator(`iframe[data-candidate='${key}']`);
    await expect(frame.locator('html')).toHaveAttribute('data-ready', 'true');
    await expect(frame.locator('.spec-card')).toBeVisible();
    await expect(frame.locator('html')).toHaveAttribute('data-candidate', key === 'blue-reference' ? 'current' : key);
    const colors = await frame
      .locator('html')
      .evaluate(root =>
        ['--ink', '--ink-2', '--ink-3', '--ink-4', '--ok', '--gold', '--err'].map(name =>
          getComputedStyle(root).getPropertyValue(name).trim(),
        ),
      );
    if (key === 'blue-reference') {
      await expect(frame.locator('html')).toHaveAttribute('data-palette', 'dailan');
      reference = colors;
      expect(await frame.locator('html').evaluate(root => root.style.getPropertyValue('--action'))).toBe('');
    } else expect(colors).toEqual(reference);
    const states = await frame
      .locator('[aria-label="主按钮状态"] .btn')
      .evaluateAll(buttons => buttons.map(button => getComputedStyle(button).backgroundColor));
    expect(new Set(states).size).toBe(3);
    await frame.locator('html').evaluate(() => document.fonts.ready);
  }
  await expect(page.locator('.study-metrics dt')).toHaveCount(6);
}

async function app(page: Page, variant: string) {
  const frame = page.frameLocator(`iframe[data-candidate='${variant}']`);
  await expect(frame.locator('html')).toHaveAttribute('data-ready', 'true');
  await expect(frame.locator('.node-list-panel > header')).toBeVisible();
  await expect(frame.locator('.node-nic-plot:not(.empty)').first()).toBeVisible();
  await frame.locator('html').evaluate(() => document.fonts.ready);
  return frame;
}

for (const palette of palettes)
  for (const theme of ['dark', 'light']) {
    test(`${palette} ${theme} offers five candidates with a locked current-blue reference`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.setViewportSize({ width: 1800, height: 2080 });
      await page.goto(`/tests/fixtures/palette-candidates.html?palette=${palette}&theme=${theme}`);
      await specimens(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await expect(page).toHaveScreenshot(`${palette}-${theme}-candidates.png`, screenshotOptions);
      expect(errors).toEqual([]);
    });
    test(`${palette} ${theme} recommendation uses the complete app`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
      await page.setViewportSize({ width: 1440, height: 1080 });
      await page.goto(
        `/tests/fixtures/palette-candidates.html?palette=${palette}&candidate=v1&theme=${theme}&mode=full`,
      );
      const frame = await app(page, 'v1');
      await expect(frame.locator('html')).toHaveAttribute('data-palette', palette);
      await expect(frame.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page).toHaveScreenshot(`${palette}-${theme}-app.png`, screenshotOptions);
      expect(errors).toEqual([]);
    });
  }

for (const theme of ['dark', 'light'])
  test(`dailan ${theme} remains production-only even with an old candidate URL`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
    await page.setViewportSize({ width: 1440, height: 1080 });
    await page.goto(
      `/tests/fixtures/palette-candidates.html?palette=dailan&candidate=mineral&theme=${theme}&mode=compare`,
    );
    const frame = await app(page, 'current');
    await expect(frame.locator('html')).toHaveAttribute('data-palette', 'dailan');
    for (const name of ['--ground', '--ink', '--action', '--primary', '--data']) {
      expect(await frame.locator('html').evaluate((root, name) => root.style.getPropertyValue(name), name)).toBe('');
    }
    await expect(page.getByRole('button', { name: '方案对照', exact: true })).toBeDisabled();
    await expect(page.getByLabel('完整页面方案', { exact: true })).toBeHidden();
    await expect(page).toHaveScreenshot(`dailan-${theme}-fixed.png`, screenshotOptions);
  });

test('candidate five, theme changes, and detail pages preserve the selected study', async ({ page }) => {
  await page.goto('/tests/fixtures/palette-candidates.html?palette=jinzi');
  await specimens(page);
  await page.locator('.study-card[data-candidate="v5"]').getByRole('button', { name: '查看完整界面' }).click();
  await app(page, 'v5');
  await page.getByRole('button', { name: '亮色', exact: true }).click();
  let frame = await app(page, 'v5');
  await expect(frame.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.getByLabel('调色盘', { exact: true }).selectOption('songlv');
  frame = await app(page, 'v1');
  await expect(frame.locator('html')).toHaveAttribute('data-palette', 'songlv');
  await page.getByLabel('完整页面方案', { exact: true }).selectOption('v3');
  await app(page, 'v3');
  await page.getByLabel('查看页面', { exact: true }).selectOption('detail');
  await expect(page.frameLocator('iframe').locator('.nd-page')).toBeVisible();
  await page.getByLabel('查看页面', { exact: true }).selectOption('user');
  await expect(page.frameLocator('iframe').locator('.user-dhead')).toBeVisible();
});

for (const theme of ['dark', 'light'])
  test(`a phone can review xuanmo ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
    await page.setViewportSize({ width: 390, height: 1100 });
    await page.goto(`/tests/fixtures/palette-candidates.html?palette=xuanmo&candidate=v1&theme=${theme}&mode=full`);
    await app(page, 'v1');
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page).toHaveScreenshot(`xuanmo-${theme}-phone.png`, screenshotOptions);
  });

test('separate review tabs retain their own palette and theme', async ({ page, context }) => {
  await page.goto('/tests/fixtures/palette-candidates.html?palette=jinzi&candidate=v1&theme=dark&mode=full');
  const first = await app(page, 'v1');
  const second = await context.newPage();
  await second.goto('/tests/fixtures/palette-candidates.html?palette=oufen&candidate=v5&theme=light&mode=full');
  const other = await app(second, 'v5');
  await expect(first.locator('html')).toHaveAttribute('data-palette', 'jinzi');
  await expect(first.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(other.locator('html')).toHaveAttribute('data-palette', 'oufen');
  await expect(other.locator('html')).toHaveAttribute('data-theme', 'light');
  await second.close();
});
