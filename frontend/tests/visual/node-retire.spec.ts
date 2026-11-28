import { waitForVisualAssets } from './helpers/visual-ready';
import { test as base, expect, chromium, webkit, type Locator, type Page } from '@playwright/test';

// 机器退役与移出：详情页「更多操作」里的入口、弹层（电脑上居中，手机上是底部弹层）、退役后的状态横幅，
// 以及移出后回到机器列表的提示。路线经过 hk-hkix、hk-old 与 kr-old（见 node-list-preview 的 routes=on）。

async function detail(page: Page, id: string, name: string, width = 1440, theme = 'dark', extra = '') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.setFixedTime(new Date('2026-10-25T12:00:00Z'));
  await page.setViewportSize({ width, height: width <= 820 ? 844 : 900 });
  await page.goto(
    `/tests/fixtures/node-list-preview.html?theme=${theme}&scene=typical&routes=on&${extra}#/nodes/node/${id}`,
  );
  await expect(page.locator('.nd-page-head h1')).toHaveText(name);
  await expect(page.locator('[aria-label="加载中"]')).toHaveCount(0);
  await waitForVisualAssets(page);
  return errors;
}

async function settle(target: Locator) {
  await target.evaluate(async element => {
    await Promise.all(
      element
        .getAnimations({ subtree: true })
        .filter(animation => animation.effect?.getComputedTiming().iterations !== Infinity)
        .map(animation => animation.finished.catch(() => undefined)),
    );
  });
}

async function openRetire(page: Page, title: string) {
  const more = page.locator('.nd-page-head').getByRole('button', { name: '更多操作', exact: true });
  await more.click();
  await page.getByRole('menuitem', { name: /^退役\/移出/ }).click();
  const dialog = page.getByRole('dialog', { name: title, exact: true });
  await expect(dialog).toBeVisible();
  const surface = dialog.locator('.node-retire-dialog');
  await settle(surface);
  return { dialog, surface, more };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
}

base.describe('retire dialog', () => {
  for (const theme of ['dark', 'light']) {
    base(`menu entry, retire and remove choices ${theme}`, async ({ page }) => {
      const errors = await detail(page, 'hk-hkix', '香港 HKIX', 1440, theme);
      await page.locator('.nd-page-head').getByRole('button', { name: '更多操作', exact: true }).click();
      const menu = page.locator('[data-node-action-menu]');
      await expect(menu).toBeVisible();
      await expect(menu.getByRole('menuitem', { name: /^退役\/移出/ })).toContainText('仅退役，或退役后从机器列表移出');
      for (const removed of ['退役机器', '强制退役', '恢复机器', '重建停用发布'])
        await expect(menu.getByRole('menuitem', { name: removed })).toHaveCount(0);
      await settle(menu);
      await expect(menu).toHaveScreenshot(`menu-${theme}.png`);
      await page.keyboard.press('Escape');

      const { dialog, surface, more } = await openRetire(page, '退役 香港 HKIX');
      await expect(dialog.getByRole('button', { name: '退役', exact: true })).toBeFocused();
      await expect(dialog.getByRole('radio', { name: '开启', exact: true })).toBeChecked();
      await expect(dialog.locator('.node-retire-routes')).toContainText('将暂停经过它的 2 条线路：');
      await expect(surface).toHaveScreenshot(`retire-${theme}.png`);

      await dialog.locator('label.node-retire-option.danger').click();
      await expect(dialog.locator('.node-retire-routes')).toContainText('将删除经过它的 2 条线路：');
      await expect(dialog.getByRole('button', { name: '退役且移出', exact: true })).toHaveClass(/solid/);
      await page.mouse.move(0, 0);
      await settle(surface);
      await expect(surface).toHaveScreenshot(`remove-${theme}.png`);

      await page.keyboard.press('Escape');
      await expect(dialog).toBeHidden();
      await expect(more).toBeFocused();
      expect(errors).toEqual([]);
    });
  }

  base('offline machine warns when force is turned off', async ({ page }) => {
    const errors = await detail(page, 'us-9929', '洛杉矶 9929');
    const { dialog, surface } = await openRetire(page, '退役 洛杉矶 9929');
    await expect(dialog.locator('.node-retire-offline')).toContainText('失联');
    await expect(dialog.locator('.node-retire-warn')).toHaveCount(0);
    await dialog.getByRole('radio', { name: '关闭', exact: true }).click();
    await expect(dialog.locator('.node-retire-warn')).toContainText('关闭强制会一直等到它重新上线');
    await expect(dialog.locator('.node-retire-routes')).toHaveText('没有线路经过它。');
    await page.mouse.move(0, 0);
    await settle(surface);
    await expect(surface).toHaveScreenshot('offline-unforced-dark.png');
    expect(errors).toEqual([]);
  });

  base('force removal returns to the list with a confirmation', async ({ page }) => {
    const errors = await detail(page, 'hk-hkix', '香港 HKIX');
    const { dialog } = await openRetire(page, '退役 香港 HKIX');
    await dialog.locator('label.node-retire-option.danger').click();
    await dialog.getByRole('button', { name: '退役且移出', exact: true }).click();
    await expect(page).toHaveURL(/#\/nodes$/);
    const toast = page.locator('.node-removal-toast');
    await expect(toast).toHaveText('已移出 香港 HKIX，删除 2 条线路');
    await expect(page.locator('.node-card')).toHaveCount(26);
    await settle(toast);
    await expect(toast).toHaveScreenshot('removed-toast-dark.png');
    const requests = await page.evaluate(() =>
      (window as unknown as { __nodePreviewRequests: string[] }).__nodePreviewRequests.filter(
        path => path.includes('/lifecycle/') || path === '/nodes',
      ),
    );
    expect(requests).toEqual(['/nodes/hk-hkix/lifecycle/abandon', '/nodes']);
    await toast.getByRole('button', { name: '关闭提示', exact: true }).click();
    await expect(toast).toHaveCount(0);
    expect(errors).toEqual([]);
  });

  base('scheduling removal shows the queued step and greys out the menu entry', async ({ page }) => {
    const errors = await detail(page, 'hk-hkix', '香港 HKIX', 1440, 'dark', 'teardown=running');
    const { dialog } = await openRetire(page, '退役 香港 HKIX');
    await dialog.locator('label.node-retire-option.danger').click();
    await dialog.getByRole('radio', { name: '关闭', exact: true }).click();
    await dialog.getByRole('button', { name: '退役且移出', exact: true }).click();
    await expect(dialog).toBeHidden();
    const banner = page.getByRole('region', { name: '退役状态', exact: true });
    await expect(banner.locator('.node-life-tag')).toHaveText('完成后移出');
    await expect(banner.locator('.node-life-steps li')).toHaveText([
      '创建停用发布',
      '等待机器回报',
      '停用完成',
      '移出',
    ]);
    await page.locator('.nd-page-head').getByRole('button', { name: '更多操作', exact: true }).click();
    await expect(page.getByRole('menuitem', { name: /^退役\/移出/ })).toBeDisabled();
    await expect(page.getByRole('menuitem', { name: /^退役\/移出/ })).toContainText('已安排：停用完成后自动移出');
    expect(errors).toEqual([]);
  });
});

base.describe('retirement banner', () => {
  const banners = [
    { scene: 'retiring', id: 'hk-old', name: '香港 旧 BGP', extra: 'teardown=running', themes: ['dark', 'light'] },
    {
      scene: 'retiring-queued',
      id: 'hk-old',
      name: '香港 旧 BGP',
      extra: 'teardown=running&removal=hk-old:queued',
      themes: ['dark'],
    },
    { scene: 'retired', id: 'kr-old', name: '首尔 旧机', extra: '', themes: ['dark', 'light'] },
    {
      scene: 'failed',
      id: 'kr-old',
      name: '首尔 旧机',
      extra: 'abandoned=kr-old&removal=kr-old:failed',
      themes: ['dark', 'light'],
    },
  ];
  for (const banner of banners)
    for (const theme of banner.themes) {
      base(`${banner.scene} ${theme}`, async ({ page }) => {
        const errors = await detail(page, banner.id, banner.name, 1440, theme, banner.extra);
        const region = page.getByRole('region', { name: '退役状态', exact: true });
        await expect(region).toBeVisible();
        if (banner.scene.startsWith('retiring')) {
          await expect(region.getByRole('button', { name: '查看发布 #58' })).toBeVisible();
          await expect(region.locator('li[aria-current="step"]')).toHaveText('等待机器回报');
        }
        if (banner.scene === 'retired')
          await expect(region.locator('.node-life-note')).toHaveText('经过它的 1 条线路已暂停：首尔备用。');
        if (banner.scene === 'failed') {
          await expect(region.getByRole('alert')).toHaveText('自动移出失败：机器仍被配置发布 #61 引用。可以手动移出。');
          await expect(region.getByRole('button', { name: '移出', exact: true })).toBeEnabled();
        }
        await page.mouse.move(0, 0);
        await settle(region);
        await expect(region).toHaveScreenshot(`banner-${banner.scene}-${theme}.png`);
        expect(errors).toEqual([]);
      });
    }
});

for (const engine of ['chromium', 'webkit'] as const) {
  const test = base.extend({
    browser: [
      async ({}, use) => {
        const browser = await (engine === 'chromium' ? chromium : webkit).launch();
        await use(browser);
        await browser.close();
      },
      { scope: 'worker' },
    ],
  });
  test.describe(`retire on a phone ${engine}`, () => {
    test.use({ isMobile: true, hasTouch: true });
    for (const theme of ['dark', 'light']) {
      test(`390 ${theme} bottom sheet`, async ({ page }) => {
        const errors = await detail(page, 'hk-hkix', '香港 HKIX', 390, theme);
        const { dialog, surface } = await openRetire(page, '退役 香港 HKIX');
        await dialog.locator('label.node-retire-option.danger').click();
        await page.mouse.move(0, 0);
        await settle(surface);
        const box = (await surface.boundingBox())!;
        // 手机上贴底展开、占满宽度，确认与取消都在可视区域内。
        expect(Math.abs(box.x)).toBeLessThanOrEqual(1);
        expect(Math.abs(box.width - 390)).toBeLessThanOrEqual(1);
        expect(Math.abs(box.y + box.height - 844)).toBeLessThanOrEqual(1);
        await expect(dialog.getByRole('button', { name: '退役且移出', exact: true })).toBeInViewport();
        await expect(dialog.getByRole('button', { name: '取消', exact: true })).toBeInViewport();
        await noOverflow(page);
        await expect(page).toHaveScreenshot(`phone-${engine}-${theme}.png`);
        expect(errors).toEqual([]);
      });
    }
    test('390 retiring banner', async ({ page }) => {
      const errors = await detail(page, 'hk-old', '香港 旧 BGP', 390, 'dark', 'teardown=running&removal=hk-old:queued');
      const region = page.getByRole('region', { name: '退役状态', exact: true });
      await expect(region).toBeVisible();
      await noOverflow(page);
      await settle(region);
      await expect(region).toHaveScreenshot(`phone-${engine}-banner-retiring.png`);
      expect(errors).toEqual([]);
    });
  });
}
