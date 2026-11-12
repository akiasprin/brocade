import { test as base, expect, chromium, webkit, type Locator, type Page } from '@playwright/test';

async function open(page: Page, width: number, height: number, theme: string, scene = 'idle') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      value: {
        writeText: async (text: string) => {
          (window as unknown as { nodeCopied: string }).nodeCopied = text;
        },
      },
    });
  });
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/user-header-preview.html?theme=${theme}&nodes=regional&nodeProbe=${scene}`);
  await page.getByRole('button', { name: '节点', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
  await expect(dialog.getByRole('button', { name: '全部 31', exact: true })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return { dialog, errors };
}

async function contained(dialog: Locator) {
  const overflows = await dialog
    .locator('.sub-dialog, .node-list, .node-copy, .node-copy-host')
    .evaluateAll(elements =>
      elements.filter(element => element.scrollWidth > element.clientWidth + 1).map(element => element.className),
    );
  expect(overflows).toEqual([]);
  await expect(dialog.locator('.sub-dialog-close')).toBeInViewport({ ratio: 1 });
  await expect(dialog.locator('.sub-dialog-filter, .sub-dialog-foot')).toHaveCount(0);
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

  for (const theme of ['light', 'dark']) {
    test(`regional node browser ${engine} desktop ${theme}`, async ({ page }) => {
      const { dialog, errors } = await open(page, 1440, 900, theme);
      expect((await dialog.locator('.sub-dialog').boundingBox())!.width).toBe(860);
      expect((await dialog.locator('.node-sidebar').boundingBox())!.width).toBe(178);
      await contained(dialog);
      await expect(dialog).toHaveScreenshot(`nodes-${engine}-desktop-${theme}.png`);
      const japan = dialog.getByRole('button', { name: '日本 10', exact: true });
      await japan.focus();
      await japan.press('Enter');
      await expect(japan).toHaveAttribute('aria-current', 'true');
      await expect(dialog.locator('.node-group')).toHaveCount(4);
      await expect(dialog.locator('.node-region-head')).toHaveCount(0);
      await dialog.getByRole('button', { name: '复制 10 个', exact: true }).click();
      const copied = await page.evaluate(() => (window as unknown as { nodeCopied: string }).nodeCopied);
      expect(copied.split('\n')).toHaveLength(10);
      expect(copied.split('\n').every(line => decodeURIComponent(line).includes('东京示例'))).toBe(true);
      await expect(dialog.getByRole('button', { name: '拨测 10 个', exact: true })).toBeVisible();
      await contained(dialog);
      await expect(dialog).toHaveScreenshot(`nodes-${engine}-japan-${theme}.png`);
      await dialog.getByRole('button', { name: '全部 31', exact: true }).click();
      const list = dialog.locator('.node-list');
      const region = dialog.locator('.node-region').nth(1);
      await list.evaluate(
        (element, y) => {
          element.scrollTop = y;
        },
        await region.evaluate(
          element => (element as HTMLElement).offsetTop - (element.parentElement as HTMLElement).offsetTop + 50,
        ),
      );
      const head = (await region.locator('.node-region-head').boundingBox())!;
      expect(Math.abs(head.y - (await list.boundingBox())!.y)).toBeLessThan(2);
      await expect(dialog.locator('.node-toolbar')).toBeInViewport({ ratio: 1 });
      expect(errors).toEqual([]);
    });

    test(`failed node browser ${engine} ${theme}`, async ({ page }) => {
      const { dialog, errors } = await open(page, 1440, 900, theme, 'done');
      await dialog.getByRole('button', { name: '失败 2', exact: true }).click();
      await expect(dialog.locator('.node-copy')).toHaveCount(2);
      await expect(dialog.getByText('握手超时，请检查入口连接')).toBeVisible();
      await expect(dialog.getByText('IPv6 连接被拒绝')).toBeVisible();
      await contained(dialog);
      await expect(dialog).toHaveScreenshot(`nodes-${engine}-failed-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }

  for (const [width, height, theme] of [
    [390, 844, 'light'],
    [320, 568, 'dark'],
  ] as const) {
    test(`mobile node browser ${engine} ${width} ${theme}`, async ({ page }) => {
      const { dialog, errors } = await open(page, width, height, theme, 'done');
      const sidebar = dialog.getByRole('navigation', { name: '按地区筛选' });
      expect(await sidebar.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(true);
      await dialog.getByRole('button', { name: '日本 10', exact: true }).click();
      await expect(dialog.getByRole('button', { name: '复制 10 个', exact: true })).toBeVisible();
      for (const control of [
        dialog.getByRole('button', { name: '复制 10 个', exact: true }),
        dialog.getByRole('button', { name: '拨测 10 个', exact: true }),
        dialog.locator('.node-copy').first(),
      ]) {
        const bounds = await control.boundingBox();
        expect(bounds).not.toBeNull();
        expect(bounds!.height).toBeGreaterThanOrEqual(36);
      }
      await contained(dialog);
      await expect(dialog).toHaveScreenshot(`nodes-${engine}-phone-${width}-${theme}.png`);
      const last = dialog.locator('.node-copy').last();
      await last.scrollIntoViewIfNeeded();
      await expect(last).toBeInViewport({ ratio: 1 });
      await contained(dialog);
      expect(errors).toEqual([]);
    });
  }

  test(`running node probe ${engine}`, async ({ page }) => {
    const { dialog, errors } = await open(page, 390, 844, 'dark', 'running');
    await expect(dialog.getByRole('button', { name: '取消拨测', exact: true })).toBeVisible();
    await expect(dialog.getByRole('button', { name: /^失败 \d/ })).toHaveCount(0);
    await contained(dialog);
    await expect(dialog).toHaveScreenshot(`nodes-${engine}-running.png`);
    await dialog.getByRole('button', { name: '取消拨测', exact: true }).click();
    await expect(dialog.getByRole('status')).toContainText('已取消拨测');
    expect(errors).toEqual([]);
  });
}
