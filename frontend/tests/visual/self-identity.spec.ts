import { test as base, expect, chromium, webkit } from '@playwright/test';

const route = '/tests/fixtures/self-identity-preview.html';

for (const engine of ['chromium', 'webkit'] as const) {
  const test = base.extend({
    browser: [
      async ({}, use) => {
        const browser = await { chromium, webkit }[engine].launch();
        await use(browser);
        await browser.close();
      },
      { scope: 'worker' },
    ],
  });

  for (const width of [1440, 390]) {
    test.describe(`${engine} ${width}`, () => {
      test.use({ viewport: { width, height: 900 }, isMobile: width === 390, hasTouch: width === 390 });
      for (const theme of ['dark', 'light']) {
        for (const target of ['grace', 'bob']) {
          test(`${theme} views ${target}`, async ({ page }) => {
            const errors: string[] = [];
            page.on('pageerror', error => errors.push(error.message));
            await page.clock.setFixedTime(new Date('2026-11-11T12:00:00Z'));
            await page.goto(`${route}?theme=${theme}${width > 820 ? `#/users/user/${target}` : '#/users'}`);
            const roster = page.getByRole('listbox', { name: '用户列表' });
            const me = roster.getByRole('group', { name: '我' }).getByRole('option');
            await expect(me).toContainText('grace');
            await expect(roster.getByRole('group', { name: '其他用户' })).toHaveCount(1);
            await expect(me.locator('.user-row-chip')).toHaveCount(0);
            const avatar = me.locator('.user-generated-avatar');
            await expect(avatar).toHaveClass(/is-self/);
            await expect(avatar).toHaveCSS('width', '34px');
            await expect(me.locator('.user-row-name b')).toHaveCSS('font-size', '13.5px');
            const rowHeights = await roster.getByRole('option').evaluateAll(rows =>
              rows.map(row => ({
                name: row.querySelector('.user-row-name b')?.textContent,
                height: row.getBoundingClientRect().height,
              })),
            );
            expect(rowHeights.find(row => row.name === 'grace')?.height, JSON.stringify(rowHeights)).toBe(
              rowHeights.find(row => row.name === 'alice')?.height,
            );
            const identity = await avatar.getAttribute('data-avatar-mark');
            const color = await avatar.evaluate(el => getComputedStyle(el).color);
            await page.evaluate(() => document.fonts.ready);
            if (width > 820) {
              if (target === 'bob') {
                await expect(me).toHaveAttribute('aria-selected', 'false');
                await expect(me).toHaveCSS('box-shadow', 'none');
              }
              await expect(page).toHaveScreenshot(`${engine}-${width}-${theme}-${target}.png`);
            } else {
              await expect(page.locator('.user-split-roster')).toHaveScreenshot(
                `${engine}-${width}-${theme}-${target}-roster.png`,
              );
              await roster
                .getByRole('option')
                .filter({ has: page.locator('.user-row-name b', { hasText: new RegExp(`^${target}$`) }) })
                .click();
            }
            const header = page.locator('.user-dhead');
            await expect(header.locator('.dname > b')).toHaveText(target);
            await expect(header.locator('.user-self-chip')).toHaveCount(target === 'grace' ? 1 : 0);
            if (target === 'grace') {
              await expect(header.locator('.plate')).toHaveAttribute('data-avatar-mark', identity!);
              await expect(header.locator('.plate')).toHaveCSS('color', color);
              expect(
                await header
                  .locator('.dname')
                  .evaluate(el => [...el.children].slice(0, 3).map(child => child.className)),
              ).toEqual(['', 'user-self-chip', 'user-account-state']);
            }
            if (width === 390)
              await expect(header).toHaveScreenshot(`${engine}-${width}-${theme}-${target}-header.png`);
            const account = page.getByRole('button', { name: /账户与更多功能/ });
            await expect(account.locator('.user-generated-avatar')).toHaveAttribute('data-avatar-mark', identity!);
            await expect(account.locator('.user-generated-avatar')).toHaveCSS('width', '22px');
            await expect(account.locator('.user-generated-avatar')).toHaveCSS('color', color);
            await account.click();
            const menu = page.getByRole('menu', { name: '账户与更多功能' });
            await expect(menu).toBeVisible();
            await expect(menu.locator('.fg-menu-id .user-generated-avatar')).toHaveAttribute(
              'data-avatar-mark',
              identity!,
            );
            await expect(menu.locator('.fg-menu-id .user-generated-avatar')).toHaveCSS('width', '30px');
            await expect(menu.locator('.fg-menu-id .user-generated-avatar')).toHaveCSS('color', color);
            await expect(menu).toHaveScreenshot(`${engine}-${width}-${theme}-${target}-account.png`);
            await page.keyboard.press('Escape');
            await expect(account).toBeFocused();
            expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
            expect(errors).toEqual([]);
          });
        }
      }
    });
  }

  test(`${engine} administrator keeps its account plate and the original roster`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.clock.setFixedTime(new Date('2026-11-11T12:00:00Z'));
    await page.goto(`${route}?role=admin&theme=light#/users/user/bob`);
    await expect(page.getByRole('listbox', { name: '用户列表' })).toBeVisible();
    await expect(page.getByRole('group', { name: '我', exact: true })).toHaveCount(0);
    await expect(page.getByRole('group', { name: '其他用户', exact: true })).toHaveCount(0);
    await expect(page.locator('.user-generated-avatar.is-self')).toHaveCount(0);
    const account = page.getByRole('button', { name: /账户与更多功能/ });
    await expect(account.locator('.fg-who-plate')).toHaveText('O');
    await expect(page).toHaveScreenshot(`${engine}-administrator.png`);
  });

  test(`${engine} searching another user hides the self group`, async ({ page }) => {
    await page.goto(`${route}?theme=dark`);
    const search = page.getByRole('searchbox', { name: '搜索用户' });
    await expect(page.getByRole('group', { name: '我', exact: true })).toBeVisible();
    await search.fill('bob');
    await expect(page.getByRole('group', { name: '我', exact: true })).toHaveCount(0);
    await expect(page.getByRole('option')).toHaveCount(1);
    await search.fill('');
    await expect(page.getByRole('group', { name: '我', exact: true })).toBeVisible();
  });
}
