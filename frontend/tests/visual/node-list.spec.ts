import { test as base, expect, chromium, webkit, type Page } from '@playwright/test';
import { SCENES, type SceneKey } from '../fixtures/node-roster-data';

async function preview(page: Page, width = 1440, theme = 'dark', scene: SceneKey = 'typical', extra = '') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'warning' && message.text().includes('ECharts')) errors.push(message.text());
  });
  await page.clock.setFixedTime(new Date('2026-10-25T12:00:00Z'));
  await page.setViewportSize({ width, height: width <= 820 ? 844 : 900 });
  await page.goto(`/tests/fixtures/node-list-preview.html?theme=${theme}&scene=${scene}&${extra}#/nodes`);
  await expect(page.locator('.node-cardpage > .node-list-panel > .nd-page-head h1')).toHaveText('机器');
  await expect(page.locator('.node-card')).toHaveCount(SCENES[scene].machines().length);
  await expect(page.locator('.node-split, .node-roster-options, .node-split-detail')).toHaveCount(0);
  if (scene !== 'empty' && !extra.includes('metrics=')) {
    await expect(page.locator('.node-card-rate:not(.none)').first()).toBeVisible();
    await expect(page.locator('.node-card-month > b:not(.none)').first()).toBeVisible();
    await expect(page.locator('.node-card-resources[data-state="ready"]').first()).toBeVisible();
  }
  await page.evaluate(() => document.fonts.ready);
  return errors;
}
const card = (page: Page, name: string) => page.getByRole('button', { name: `查看 ${name} 的机器详情`, exact: true });
async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  const widths = await page
    .locator('.node-cardpage, .node-card-grid, .node-card')
    .evaluateAll(elements =>
      elements
        .filter(element => element.getBoundingClientRect().width > 0)
        .map(element => element.scrollWidth - element.clientWidth),
    );
  for (const width of widths) expect(width).toBeLessThanOrEqual(1);
}
async function detailReady(page: Page, name: string) {
  await expect(page.locator('.nd-page-head h1')).toHaveText(name);
  await expect(page.locator('.node-card-grid, .node-split, .node-list-panel')).toHaveCount(0);
  await expect(page.locator('.nd-throughput-panel .load-network-legend').first()).toBeVisible();
  await expect(page.locator('[aria-label="加载中"], [aria-label="负载加载中"]')).toHaveCount(0);
}
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
  test.describe(`Comet health cards ${engine}`, () => {
    test('live NIC frames update current rates and preserve gaps', async ({ page }) => {
      const errors = await preview(page, 390, 'dark');
      const target = card(page, '香港 HKIX');
      await target.scrollIntoViewIfNeeded();
      const curve = await target.locator('.node-card-comet').innerHTML();
      const send = async (gap: boolean) =>
        page.evaluate(gap => {
          const push = (window as unknown as { __nodePreviewPush: (url: string, event: string, data: unknown) => void })
            .__nodePreviewPush;
          push('/realtime/nodes/events', 'sample', {
            node_id: 'hk-hkix',
            received_at_unix_millis: Date.now() + (gap ? 1 : 0),
            sample: { rx_bytes_per_sec: 15_000_000, tx_bytes_per_sec: 5_000_000, has_gap: gap },
          });
        }, gap);
      await send(false);
      await expect(target.locator('.node-card-rate')).toHaveText('160Mbit/s');
      await expect(target.locator('.node-card-rate')).toHaveAttribute('title', /实时采样/);
      expect(await target.locator('.node-card-comet').innerHTML()).toBe(curve);
      await noOverflow(page);
      await expect(page).toHaveScreenshot(`comet-${engine}-390-live.png`);
      await send(true);
      await expect(target.locator('.node-card-rate')).toHaveText('—');
      await expect(page).toHaveScreenshot(`comet-${engine}-390-live-gap.png`);
      expect(errors).toEqual([]);
    });
    for (const width of [1440, 900])
      for (const theme of ['dark', 'light']) {
        test(`desktop ${width} ${theme}`, async ({ page }) => {
          const errors = await preview(page, width, theme);
          await noOverflow(page);
          await expect(page.locator('.node-card .nc-tcp-latest')).toHaveCount(0);
          await page.mouse.move(0, 0);
          await expect(page).toHaveScreenshot(`comet-${engine}-${width}-${theme}.png`);
          expect(errors).toEqual([]);
        });
      }
    test('sorting, retirement, multi-select and removal', async ({ page }) => {
      const errors = await preview(page);
      const names = await page.locator('.node-card-name').allTextContents();
      expect(names.slice(-3)).toEqual(['洛杉矶 旧 GIA', '首尔 旧机', '香港 旧 BGP']);
      await page.getByRole('button', { name: '多选', exact: true }).click();
      await page.getByRole('checkbox', { name: '选中 东京 IIJ', exact: true }).check();
      await page.getByRole('checkbox', { name: '选中 首尔 旧机', exact: true }).check();
      await expect(page.getByRole('checkbox', { name: '选中 香港 旧 BGP', exact: true })).toBeDisabled();
      await expect(page.getByRole('button', { name: '退役下线 1', exact: true })).toBeEnabled();
      page.once('dialog', dialog => dialog.accept());
      await page.getByRole('button', { name: '移除选中 1', exact: true }).click();
      await expect(page.locator('.node-card')).toHaveCount(26);
      await expect(page.getByText('已移除 1 台机器，并清理 0 条关联线路。')).toBeVisible();
      await expect(page.getByRole('checkbox')).toHaveCount(0);
      expect(errors).toEqual([]);
    });
    test('keyboard opens a full-page detail and history restores the list', async ({ page }) => {
      const errors = await preview(page);
      await card(page, '澳门 CTM').focus();
      await card(page, '澳门 CTM').press('Enter');
      await expect(page).toHaveURL(/#\/nodes\/node\/mo-ctm$/);
      await detailReady(page, '澳门 CTM');
      await expect(page).toHaveScreenshot(`comet-${engine}-detail.png`);
      await page.goBack();
      await expect(page.locator('.node-card')).toHaveCount(27);
      await card(page, '东京 IIJ').focus();
      await card(page, '东京 IIJ').press('Space');
      await detailReady(page, '东京 IIJ');
      await page.goBack();
      await page.goForward();
      await detailReady(page, '东京 IIJ');
      expect(errors).toEqual([]);
    });
    test('read-only and visitor permissions', async ({ page }) => {
      const errors = await preview(page, 1440, 'dark', 'typical', 'role=readonly');
      await expect(page.getByRole('button', { name: '多选', exact: true })).toBeDisabled();
      await expect(page.getByRole('button', { name: '＋ 纳管机器', exact: true })).toBeDisabled();
      await card(page, '澳门 CTM').click();
      await detailReady(page, '澳门 CTM');
      await page.goto('/tests/fixtures/node-list-preview.html?role=public#/nodes');
      await expect(page.locator('.node-card')).toHaveCount(27);
      await expect(page.getByRole('button', { name: '多选', exact: true })).toHaveCount(0);
      await expect(page.getByRole('button', { name: '＋ 纳管机器', exact: true })).toHaveCount(0);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });
    for (const scene of ['trouble', 'many', 'empty'] as const) {
      test(`scenario ${scene}`, async ({ page }) => {
        const errors = await preview(page, 1440, 'dark', scene);
        await noOverflow(page);
        if (scene === 'empty') await expect(page.getByText('还没有机器')).toBeVisible();
        await expect(page).toHaveScreenshot(`comet-${engine}-${scene}.png`);
        expect(errors).toEqual([]);
      });
    }
    for (const metrics of ['failed', 'stale', 'gap']) {
      test(`metrics ${metrics} keep unknown readings`, async ({ page }) => {
        const errors = await preview(page, 1440, 'dark', 'typical', `metrics=${metrics}`);
        await expect(page.locator('.node-card-rate:not(.none)')).toHaveCount(0);
        await expect(page.locator('.node-card-resource.none')).toHaveCount(81);
        await expect(page.locator('.node-card-comet-now')).toHaveCount(0);
        await noOverflow(page);
        await expect(page).toHaveScreenshot(`comet-${engine}-metrics-${metrics}.png`);
        expect(errors).toEqual([]);
      });
    }
    test('health thresholds, offline state and batched loading', async ({ page }) => {
      const errors = await preview(page);
      await expect(card(page, '东京 IIJ').locator('.node-card-resource.warn')).toContainText('CPU84%');
      await expect(card(page, '法兰克福 标准').locator('.node-card-resource.warn')).toContainText('磁盘96%');
      await expect(card(page, '洛杉矶 CMIN2').locator('.node-card-resource.bad')).toContainText('内存96%');
      await expect(card(page, '洛杉矶 9929').locator('.node-card-resource.none')).toHaveCount(3);
      await expect(card(page, '洛杉矶 9929').locator('.node-card-comet-now')).toHaveCount(0);
      const requests = await page.evaluate(() =>
        (window as unknown as { __nodePreviewRequests: string[] }).__nodePreviewRequests.filter(
          path => path.startsWith('/load/') || path.startsWith('/ping-probe/'),
        ),
      );
      expect(requests.filter(path => path === '/load/nodes?windows=6')).toHaveLength(1);
      expect(requests.filter(path => path === '/load/nodes/nic?windows=24')).toHaveLength(1);
      expect(requests.every(path => path.startsWith('/load/nodes?') || path.startsWith('/load/nodes/nic?'))).toBe(true);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });
    test('measured zero has a curve and endpoint instead of an empty placeholder', async ({ page }) => {
      const errors = await preview(page, 390, 'light', 'typical', 'metrics=zero');
      const target = card(page, '澳门 CTM');
      await expect(target.locator('.node-card-rate')).toHaveText('0.00bit/s');
      await expect(target.locator('.node-card-comet .trail')).toHaveCount(1);
      await expect(target.locator('.node-card-comet-now')).toBeVisible();
      await expect(target.locator('.node-card-comet.empty')).toHaveCount(0);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });
    for (const width of [820, 821]) {
      test(`breakpoint ${width} has no automatic detail`, async ({ page }) => {
        const errors = await preview(page, width);
        await noOverflow(page);
        await expect(page.locator('.nd-throughput-panel')).toHaveCount(0);
        expect(errors).toEqual([]);
      });
    }
  });
  test.describe(`Comet health cards ${engine} phone`, () => {
    test.use({ isMobile: true, hasTouch: true });
    for (const theme of ['dark', 'light']) {
      test(`390 ${theme} title, selection and full-page navigation`, async ({ page }) => {
        const errors = await preview(page, 390, theme);
        await noOverflow(page);
        const head = page.locator('.node-cardpage .nd-page-head');
        const title = await head.locator('h1').boundingBox();
        const tools = await head.locator('.nd-tools').boundingBox();
        expect(Math.abs(title!.y + title!.height / 2 - tools!.y - tools!.height / 2)).toBeLessThanOrEqual(1);
        await expect(page).toHaveScreenshot(`comet-${engine}-390-${theme}.png`);
        await page.getByRole('button', { name: '多选', exact: true }).click();
        await page.getByRole('checkbox', { name: '选中 澳门 CTM', exact: true }).check();
        await noOverflow(page);
        await expect(page).toHaveScreenshot(`comet-${engine}-390-${theme}-selection.png`);
        await page.getByRole('button', { name: '取消', exact: true }).click();
        await card(page, '澳门 CTM').click();
        await detailReady(page, '澳门 CTM');
        await expect(page).toHaveScreenshot(`comet-${engine}-390-${theme}-detail.png`);
        await page.goBack();
        await expect(page.locator('.node-card')).toHaveCount(27);
        expect(errors).toEqual([]);
      });
    }
    test('320 header and mixed selection do not scroll sideways', async ({ page }) => {
      const errors = await preview(page, 320);
      await noOverflow(page);
      await page.getByRole('button', { name: '多选', exact: true }).click();
      await page.getByRole('checkbox', { name: '选中 澳门 CTM', exact: true }).check();
      await page.getByRole('checkbox', { name: '选中 首尔 旧机', exact: true }).check();
      await page.evaluate(() => window.scrollTo(0, 0));
      await expect(page.locator('.node-cardpage .nd-page-head')).toBeInViewport();
      const title = page.getByRole('heading', { name: '机器', exact: true });
      expect(await title.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
      await noOverflow(page);
      await expect(page).toHaveScreenshot(`comet-${engine}-320-selection.png`);
      expect(errors).toEqual([]);
    });
  });
}
