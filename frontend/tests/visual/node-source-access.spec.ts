import { test, expect, type Page, type Locator } from '@playwright/test';

const scenes = ['typical', 'useronly', 'empty', 'many', 'pending', 'error', 'unsupported', 'legacy'] as const;
const setups = [
  ['desktop-dark', 1440, 'dark'],
  ['desktop-light', 1440, 'light'],
  ['mobile-dark', 390, 'dark'],
  ['mobile-light', 390, 'light'],
] as const;
async function load(page: Page, width: number, theme: string, scene = 'typical') {
  page.on('pageerror', error => console.error(error.message));
  await page.clock.install({ time: new Date('2026-10-29T08:00:00Z') });
  await page.setViewportSize({ width, height: 1100 });
  await page.goto(`/tests/fixtures/node-source-access-preview.html?scene=${scene}&theme=${theme}`);
  const panel = page.getByRole('region', { name: '来源 IP 封禁', exact: true });
  await expect(panel.getByRole('button', { name: '封禁 IP', exact: true })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return panel;
}
async function checkOverflow(page: Page, panel: Locator) {
  expect(await panel.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}

/** 地址保持单行，地区在其右侧；只允许地址自身在窄屏横向滚动。 */
async function checkAddressRows(panel: Locator, full = false) {
  for (const row of await panel.locator('.source-access-row').all()) {
    const layout = await row.evaluate(element => {
      const address = element.querySelector<HTMLElement>('.source-access-address')!;
      const code = address.querySelector('code')!;
      const region = element.querySelector<HTMLElement>('.source-access-place')!;
      const ip = code.getBoundingClientRect();
      const place = region.getBoundingClientRect();
      return {
        singleLine: ip.height <= parseFloat(getComputedStyle(code).lineHeight) + 1,
        regionToRight: place.left >= address.getBoundingClientRect().right,
        sameLine: place.top < ip.bottom && place.bottom > ip.top,
        full: address.scrollWidth <= address.clientWidth,
      };
    });
    expect(layout.singleLine).toBe(true);
    expect(layout.regionToRight).toBe(true);
    expect(layout.sameLine).toBe(true);
    if (full) expect(layout.full).toBe(true);
  }
}

for (const [name, width, theme] of setups) {
  for (const scene of scenes) {
    test(`${name}: C ${scene}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      const panel = await load(page, width, theme, scene);
      const count = panel.locator('header .header-stat');
      if (width <= 820) await expect(count).toBeHidden();
      else await expect(count).toBeVisible();
      await expect(panel.getByRole('status')).toHaveText(
        scene === 'pending' ? '同步中' : scene === 'error' ? '执行异常' : scene === 'unsupported' ? '待升级' : '已生效',
      );
      if (scene === 'empty' || scene === 'useronly') await expect(panel).toContainText('本机未封禁任何来源');
      if (scene === 'legacy') await expect(panel).toContainText('用户名单需升级 Console 后显示');
      if (scene === 'error') await expect(panel).toContainText('Xray 接入策略状态异常');
      if (scene === 'unsupported') await expect(panel).toContainText('这台机器需升级 Agent 与 Xray');
      if (scene === 'many') await expect(panel.getByRole('button', { name: '显示其余 6 个地址' })).toBeVisible();
      await expect(panel.getByRole('list', { name: '用户封禁', exact: true })).toHaveCount(0);
      await checkOverflow(page, panel);
      await checkAddressRows(panel, width === 1440);
      await expect(panel).toHaveScreenshot(`${name}-${scene}.png`);
      if (scene === 'typical') {
        await expect(page).toHaveScreenshot(`${name}-machine-context.png`);
        const usersToggle = panel.getByRole('button', { name: /3 位用户另有 4 个封禁/ });
        await usersToggle.click();
        await expect(usersToggle).toHaveAttribute('aria-expanded', 'true');
        const users = panel.getByRole('list', { name: '用户封禁', exact: true });
        await expect(users.getByRole('button', { name: /解除/ })).toHaveCount(0);
        await expect(users.getByText('203.0.113.17', { exact: true })).toHaveCount(2);
        await expect(users).toContainText('荷兰');
        await checkOverflow(page, panel);
        await checkAddressRows(panel, width === 1440);
        await expect(panel).toHaveScreenshot(`${name}-users-expanded.png`);
      }
      expect(errors).toEqual([]);
    });
  }

  test(`${name}: full IPv6 stays on one line with its region on the right`, async ({ page }) => {
    const panel = await load(page, width, theme);
    const ip = '2001:db8:abcd:ffff:1111:2222:3333:4444';
    await panel.getByRole('button', { name: '封禁 IP', exact: true }).click();
    await panel.getByRole('textbox', { name: '来源 IP' }).fill(ip);
    await panel.getByRole('button', { name: '封禁', exact: true }).click();
    await expect(panel.getByRole('button', { name: `解除本机封禁 ${ip}` })).toBeVisible();
    await panel.getByRole('button', { name: /3 位用户另有 4 个封禁/ }).click();
    await checkAddressRows(panel, width === 1440);
    await checkOverflow(page, panel);
    const row = panel.getByRole('list', { name: '本机封禁', exact: true }).locator('.source-access-row').last();
    const address = row.locator('.source-access-address');
    await expect(address).toHaveText(ip);
    if (width === 390) {
      const region = row.locator('.source-access-place');
      const before = await region.boundingBox();
      await address.focus();
      await address.press('ArrowRight');
      await expect.poll(() => address.evaluate(element => element.scrollLeft)).toBeGreaterThan(0);
      expect((await region.boundingBox())!.x).toBe(before!.x);
    }
    await page.mouse.move(0, 0);
    await expect(panel).toHaveScreenshot(`${name}-ipv6-inline.png`);
  });

  test(`${name}: inline hints, validation, save and lift`, async ({ page }) => {
    const panel = await load(page, width, theme);
    await panel.getByRole('button', { name: '封禁 IP', exact: true }).click();
    const input = panel.getByRole('textbox', { name: '来源 IP' });
    await expect(input).toBeFocused();
    await input.fill('203.0.113.17');
    await expect(panel).toContainText('已在本机名单中');
    await expect(panel.getByRole('button', { name: '封禁', exact: true })).toBeDisabled();
    await input.fill('198.51.100.77');
    await expect(panel).toContainText('已在 bob 的名单中');
    await input.fill('203.0.113.24');
    await expect(panel).toContainText('alice、bob 正在使用，封禁后断开');
    await expect(panel).toContainText('中国电信');
    await checkOverflow(page, panel);
    await expect(panel).toHaveScreenshot(`${name}-add-online.png`);
    await input.fill('invalid');
    await panel.getByRole('button', { name: '封禁', exact: true }).click();
    await expect(panel).toContainText('请输入有效的 IPv4 或 IPv6 地址');
    await expect(input).toHaveValue('invalid');
    await expect(input).toBeFocused();
    await expect(panel).toHaveScreenshot(`${name}-invalid.png`);
    await input.press('Escape');
    await expect(input).toHaveCount(0);
    await expect(panel.getByRole('button', { name: '封禁 IP', exact: true })).toBeFocused();
    await panel.getByRole('button', { name: '封禁 IP', exact: true }).click();
    await input.fill('203.0.113.24');
    await panel.getByRole('button', { name: '封禁', exact: true }).click();
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(panel.getByRole('button', { name: '解除本机封禁 203.0.113.24' })).toBeVisible();
    await expect(panel.getByRole('status')).toHaveText('同步中');
    await expect(panel.getByRole('status')).toHaveText('已生效', { timeout: 10_000 });
    await panel.getByRole('button', { name: '解除本机封禁 203.0.113.24' }).click();
    await expect(panel.getByRole('button', { name: '解除本机封禁 203.0.113.24' })).toHaveCount(0);
    await expect(panel.getByRole('status')).toHaveText('同步中');
    await checkOverflow(page, panel);
  });
}

test('long lists expand and user navigation opens the real user page', async ({ page }) => {
  const panel = await load(page, 1440, 'dark', 'many');
  await panel.getByRole('button', { name: '显示其余 6 个地址' }).click();
  await expect(panel.getByRole('list', { name: '本机封禁', exact: true }).locator(':scope > li')).toHaveCount(14);
  await panel.getByRole('button', { name: /6 位用户另有 8 个封禁/ }).click();
  await panel.getByRole('button', { name: '显示其余 1 位用户' }).click();
  await expect(panel.getByRole('list', { name: '用户封禁', exact: true }).locator(':scope > li')).toHaveCount(6);
  // A tall viewport keeps the sticky machine header from obscuring the top of this long panel.
  await page.setViewportSize({ width: 1440, height: 2400 });
  await panel.scrollIntoViewIfNeeded();
  await checkOverflow(page, panel);
  await expect(panel).toHaveScreenshot('desktop-dark-many-expanded.png');
  await panel.getByRole('button', { name: 'alice', exact: true }).click();
  await expect(page).toHaveURL(/#\/users\/user\/alice/);
  await expect(page.getByRole('tab', { name: '接入', exact: true })).toBeVisible();
});

test('820px hides header counts and 821px retains them', async ({ page }) => {
  const panel = await load(page, 820, 'dark');
  await expect(panel.locator('header .header-stat')).toBeHidden();
  await page.setViewportSize({ width: 821, height: 1100 });
  await expect(panel.locator('header .header-stat')).toBeVisible();
});
