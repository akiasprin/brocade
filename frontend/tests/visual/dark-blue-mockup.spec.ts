import { test, expect, type Page } from '@playwright/test';

async function ready(page: Page, scheme: string) {
  const frame = page.frameLocator(`iframe[data-scheme='${scheme}']`);
  await expect(frame.locator('.node-list-panel > header')).toBeVisible();
  await expect(frame.locator('.node-nic-plot:not(.empty)').first()).toBeVisible();
  await expect(page.locator('.review-swatch')).toHaveCount(3);
  await expect(page.locator('.review-ratio')).toHaveCount(6);
  await frame.locator('html').evaluate(() => document.fonts.ready);
  return frame;
}

for (const theme of ['dark', 'light'])
  for (const scheme of ['current', 'soft', 'mist']) {
    test(`${theme} ${scheme} uses the real app at desktop width`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
      await page.setViewportSize({ width: 1440, height: 1080 });
      await page.goto(`/tests/fixtures/dark-blue-mockup.html?scheme=${scheme}&theme=${theme}`);
      await ready(page, scheme);
      await expect(page.frameLocator('iframe').locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page).toHaveScreenshot(`${scheme}-${theme}-desktop.png`);
    });
  }

test('compare keeps the backgrounds identical and reduces data and button luminance', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
  await page.setViewportSize({ width: 1640, height: 1080 });
  await page.goto('/tests/fixtures/dark-blue-mockup.html?scheme=soft&mode=compare');
  await ready(page, 'current');
  await ready(page, 'soft');
  const colors = await Promise.all(
    ['current', 'soft'].map(scheme =>
      page
        .frameLocator(`iframe[data-scheme='${scheme}']`)
        .locator('html')
        .evaluate(root => {
          const css = getComputedStyle(root);
          return Object.fromEntries(
            ['--ground', '--sheet', '--block', '--ok', '--gold', '--err', '--data', '--primary-on'].map(name => [
              name,
              css.getPropertyValue(name).trim(),
            ]),
          );
        }),
    ),
  );
  for (const name of ['--ground', '--sheet', '--block', '--ok', '--gold', '--err'])
    expect(colors[0][name]).toBe(colors[1][name]);
  const luminance = (hex: string) =>
    hex
      .slice(1)
      .match(/../g)!
      .map(channel => Number.parseInt(channel, 16) / 255)
      .map(channel => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
      .reduce((total, channel, index) => total + channel * [0.2126, 0.7152, 0.0722][index], 0);
  for (const token of ['--data', '--primary-on'])
    expect(luminance(colors[1][token])).toBeLessThan(luminance(colors[0][token]));
  await expect(page).toHaveScreenshot('soft-compare.png');
});

for (const palette of ['jinzi', 'songlv', 'oufen', 'xuanmo'])
  for (const theme of ['dark', 'light'])
    test(`review production ${palette} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
      await page.setViewportSize({ width: 1440, height: 1200 });
      await page.goto(`/tests/fixtures/dark-blue-mockup.html?palette=${palette}&theme=${theme}`);
      const frame = await ready(page, 'mist');
      await expect(frame.locator('html')).toHaveAttribute('data-palette', palette);
      await expect(page.getByRole('group', { name: '蓝色方案' })).toBeHidden();
      await expect(page).toHaveScreenshot(`audit-${palette}-${theme}.png`);
      if (theme === 'light') {
        await frame.getByRole('button', { name: /纳管机器/ }).hover();
        await expect(frame.locator('.node-list-panel > header')).toHaveScreenshot(`audit-${palette}-hover.png`);
      }
      expect(errors).toEqual([]);
    });

test('palette selection preserves the chosen theme and restores the blue comparisons', async ({ page }) => {
  await page.goto('/tests/fixtures/dark-blue-mockup.html?theme=light&scheme=mist');
  await ready(page, 'mist');
  await page.getByLabel('调色盘', { exact: true }).selectOption('songlv');
  let frame = await ready(page, 'mist');
  await expect(frame.locator('html')).toHaveAttribute('data-palette', 'songlv');
  await expect(frame.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.getByRole('group', { name: '蓝色方案' })).toBeHidden();
  await page.getByLabel('调色盘', { exact: true }).selectOption('dailan');
  frame = await ready(page, 'mist');
  await expect(frame.locator('html')).toHaveAttribute('data-palette', 'dailan');
  await expect(frame.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.getByRole('group', { name: '蓝色方案' })).toBeVisible();
});

for (const theme of ['dark', 'light'])
  test(`chosen ${theme} C uses production colors without mockup overrides`, async ({ page }) => {
    await page.goto(`/tests/fixtures/dark-blue-mockup.html?scheme=mist&theme=${theme}`);
    const frame = await ready(page, 'mist');
    const roles = await frame.locator('html').evaluate(root => {
      const names = [
        '--action',
        '--action-hover',
        '--action-active',
        '--action-wash',
        '--data',
        '--data-wash',
        '--observe-1',
        '--primary',
        '--primary-hover',
        '--primary-active',
        '--primary-on',
        '--primary-line',
      ];
      const read = () => names.map(name => getComputedStyle(root).getPropertyValue(name).trim());
      const chosen = read();
      root.removeAttribute('data-blue');
      return { chosen, production: read() };
    });
    expect(roles.chosen).toEqual(roles.production);
  });

test('light schemes preserve contrast and compare with the original palette', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
  await page.setViewportSize({ width: 1640, height: 1080 });
  await page.goto('/tests/fixtures/dark-blue-mockup.html?scheme=mist&theme=light&mode=compare');
  await ready(page, 'current');
  await ready(page, 'mist');
  for (const scheme of ['soft', 'mist']) {
    if (scheme === 'soft') await page.getByRole('button', { name: 'B · 柔和黛蓝', exact: true }).click();
    else await page.getByRole('button', { name: 'C · 灰雾蓝', exact: true }).click();
    const frame = await ready(page, scheme);
    const ratios = await frame.locator('html').evaluate(root => {
      const style = getComputedStyle(root);
      const luminance = (color: string) => {
        const probe = document.createElement('span');
        probe.style.color = color;
        root.append(probe);
        const channels = getComputedStyle(probe)
          .color.match(/[\d.]+/g)!
          .slice(0, 3)
          .map(Number);
        probe.remove();
        return channels
          .map(channel => channel / 255)
          .map(channel => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
          .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
      };
      const ratio = (a: string, b: string) => {
        const values = [a, b].map(name => luminance(style.getPropertyValue(name).trim())).sort((a, b) => b - a);
        return (values[0] + 0.05) / (values[1] + 0.05);
      };
      return [
        ratio('--action', '--sheet'),
        ratio('--data', '--block'),
        ...['--primary', '--primary-hover', '--primary-active'].map(state => ratio('--primary-on', state)),
      ];
    });
    for (const ratio of ratios) expect(ratio).toBeGreaterThanOrEqual(4.5);
  }
  await expect(page).toHaveScreenshot('mist-light-compare.png');
});

test('mobile switches themes and opens the real machine detail', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-28T04:00:00Z'));
  await page.setViewportSize({ width: 390, height: 1100 });
  await page.goto('/tests/fixtures/dark-blue-mockup.html');
  await ready(page, 'mist');
  await expect(page.getByRole('button', { name: 'C · 灰雾蓝', exact: true })).toHaveAttribute('aria-pressed', 'true');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await expect(page).toHaveScreenshot('mist-mobile.png');
  await page.getByRole('button', { name: '亮色', exact: true }).click();
  await ready(page, 'mist');
  await expect(page.frameLocator('iframe').locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page).toHaveScreenshot('mist-light-mobile.png');
  await page.getByLabel('查看页面').selectOption('detail');
  await expect(page.frameLocator('iframe').locator('.nd-page')).toBeVisible();
  await expect(page).toHaveURL(/scene=detail/);
});
