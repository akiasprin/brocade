import { test as base, expect, chromium, webkit, type Locator, type Page } from '@playwright/test';
import jsQR from 'jsqr';
import { readFile } from 'node:fs/promises';

// Full-viewport captures include a blurred background with slight color rounding.
// QR contents are decoded separately from screenshots and exported SVGs.
// WebKit SVG rasterization depends on OS libraries; use the baseline environment
// documented in .agents/standards/testing.md when regenerating snapshots.
const SCREENSHOT_OPTIONS = { threshold: 0.1 };

async function decodeImage(page: Page, source: string) {
  const pixels = await page.evaluate(async src => {
    const image = new Image();
    image.src = src;
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;
    const context = canvas.getContext('2d')!;
    context.drawImage(image, 0, 0);
    return {
      data: Array.from(context.getImageData(0, 0, canvas.width, canvas.height).data),
      width: canvas.width,
      height: canvas.height,
    };
  }, source);
  return jsQR(new Uint8ClampedArray(pixels.data), pixels.width, pixels.height)?.data;
}

async function decodeQr(qr: Locator) {
  await qr.locator('..').scrollIntoViewIfNeeded();
  const png = await qr.screenshot();
  return decodeImage(qr.page(), 'data:image/png;base64,' + png.toString('base64'));
}

async function preview(page: Page, width: number, height: number, theme: string, extra = '') {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.setFixedTime(new Date('2026-11-06T12:00:00Z'));
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/user-header-preview.html?theme=${theme}&subscription=deferred${extra}`);
  await expect(page.locator('.user-account-state')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

async function reply(page: Page) {
  // Release only after the loading surface has been laid out, so a stale alignment cannot hide
  // behind a cache hit or a response that arrives before the browser's first layout.
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __userPreviewResolveSubscriptions: () => number }).__userPreviewResolveSubscriptions(),
      ),
    )
    .toBeGreaterThan(0);
}

async function placed(page: Page, dialog: Locator, bottomSheet = true) {
  const viewport = page.viewportSize()!;
  // Emulated rotation can finish before dvh updates; wait for the viewport-sized layer itself.
  await expect
    .poll(() => dialog.evaluate(element => ({ width: element.clientWidth, height: element.clientHeight })))
    .toEqual(viewport);
  const surface = dialog.locator('.sub-dialog');
  await expect(surface).toBeVisible();
  const rect = (await surface.boundingBox())!;
  expect(rect.x).toBeGreaterThanOrEqual(0);
  expect(rect.y).toBeGreaterThanOrEqual(bottomSheet ? 24 : 0);
  expect(rect.x + rect.width).toBeLessThanOrEqual(viewport.width + 1);
  expect(rect.y + rect.height).toBeLessThanOrEqual(viewport.height + 1);
  if (bottomSheet) {
    expect(rect.x).toBeCloseTo(0, 0);
    expect(rect.width).toBeCloseTo(viewport.width, 0);
    expect(rect.y + rect.height).toBeCloseTo(viewport.height, 0);
  } else {
    expect(rect.x + rect.width / 2).toBeCloseTo(viewport.width / 2, 0);
    expect(rect.y + rect.height / 2).toBeCloseTo(viewport.height / 2, 0);
  }
  expect(await surface.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(false);
  await expect(dialog.locator('.sub-dialog-close')).toBeInViewport({ ratio: 1 });
}

async function compactControls(dialog: Locator, tab: 'clash' | 'uri') {
  const sizes = await dialog
    .locator(
      tab === 'clash'
        ? '.sub-dialog-tabs button, .sub-dialog-close, .sub-pass-actions > .btn'
        : '.sub-dialog-tabs button, .sub-dialog-close, .node-toolbar .sub-pass-pick, .node-copy-all, .node-probe-all, .node-copy',
    )
    .evaluateAll(elements => elements.map(element => element.getBoundingClientRect().height));
  expect(sizes.length).toBeGreaterThan(tab === 'clash' ? 3 : 4);
  for (const height of sizes) expect(height).toBeGreaterThanOrEqual(36);
  if (tab === 'clash') return;
  await expect(dialog.getByRole('combobox')).toHaveCount(2);
}

// 地址行随筛选参数变长时只截断自己：二维码始终在卡面正中。
async function qrCentered(dialog: Locator) {
  const pass = (await dialog.locator('.sub-pass').boundingBox())!;
  const qr = (await dialog.locator('.sub-qr').boundingBox())!;
  expect(Math.abs(qr.x + qr.width / 2 - (pass.x + pass.width / 2))).toBeLessThan(1);
}

async function panelTypography(page: Page, dialog: Locator, tab: 'clash' | 'uri') {
  const font = (element: Element) => {
    const style = getComputedStyle(element);
    return {
      family: style.fontFamily,
      size: style.fontSize,
      weight: style.fontWeight,
      lineHeight: style.lineHeight,
    };
  };
  const body = await page.locator('.user-usage-ledger').evaluate(font);
  expect(await dialog.locator('.sub-dialog').evaluate(font)).toEqual(body);
  const tabs = await page.locator('.user-detail-tabs [aria-selected="true"]').evaluate(font);
  expect(await dialog.locator('.sub-dialog-tabs [aria-selected="true"]').evaluate(font)).toEqual(tabs);
  const title = await page.locator('.qta-app').first().evaluate(font);
  if (tab === 'uri') expect(await dialog.locator('.node-group-head > b').first().evaluate(font)).toEqual(title);
  else expect((await dialog.locator('.sub-pass-name > b').evaluate(font)).family).toBe(body.family);
  const address = await dialog
    .locator(tab === 'clash' ? '.sub-pass-address code' : '.node-copy')
    .first()
    .evaluate(font);
  expect(address.family).toContain('monospace');
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

  test.describe(`Subscription placement ${engine}`, () => {
    test.use({ isMobile: true, hasTouch: true });

    for (const [width, height, theme] of [
      [390, 844, 'dark'],
      [390, 844, 'light'],
      [320, 568, 'dark'],
      [780, 390, 'light'],
      [820, 1024, 'dark'],
    ] as const) {
      test(`both entry points stay at the bottom after loading ${width}x${height} ${theme}`, async ({ page }) => {
        const errors = await preview(page, width, height, theme);
        for (const [entry, tab] of [
          ['订阅', 'clash'],
          ['节点', 'uri'],
        ] as const) {
          await page.getByRole('button', { name: entry, exact: true }).click();
          const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
          await placed(page, dialog);
          const loading = (await dialog.locator('.sub-dialog').boundingBox())!;
          await reply(page);
          if (tab === 'clash') await expect(dialog.getByRole('region', { name: 'Clash 订阅' })).toBeVisible();
          else await expect(dialog.getByRole('button', { name: '全部 18' })).toBeVisible();
          await placed(page, dialog);
          expect((await dialog.locator('.sub-dialog').boundingBox())!.height).toBeGreaterThan(loading.height);
          await compactControls(dialog, tab);
          await panelTypography(page, dialog, tab);
          if (tab === 'uri' && width >= 390) {
            const copy = (await dialog.locator('.node-copy-all').boundingBox())!;
            const probe = (await dialog.locator('.node-probe-all').boundingBox())!;
            expect(copy.y).toBeCloseTo(probe.y, 0);
          }
          await expect(page).toHaveScreenshot(
            `subscription-${engine}-${tab}-${width}x${height}-${theme}.png`,
            SCREENSHOT_OPTIONS,
          );
          if (tab === 'uri') {
            const body = dialog.locator('.sub-dialog-body');
            expect(await body.evaluate(element => element.scrollHeight > element.clientHeight)).toBe(true);
            await body.evaluate(element => {
              element.scrollTop = element.scrollHeight;
            });
            await placed(page, dialog);
            await expect(dialog.locator('.node-copy').last()).toBeInViewport({ ratio: 1 });
            await expect(dialog.locator('.sub-dialog-foot, .sub-dialog-filter')).toHaveCount(0);
          }
          await dialog.locator('.sub-dialog-close').click();
          await expect(dialog).toHaveCount(0);
        }
        expect(errors).toEqual([]);
      });
    }

    test('tab changes, viewport resize and the self-service entry keep the sheet in view', async ({ page }) => {
      const errors = await preview(page, 390, 844, 'light', '&role=user');
      await page.getByRole('button', { name: '订阅', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
      await placed(page, dialog);
      await reply(page);
      await expect(dialog.getByRole('region', { name: 'Clash 订阅' })).toBeVisible();
      await placed(page, dialog);
      await dialog.getByRole('tab', { name: '节点', exact: true }).click();
      await reply(page);
      await expect(dialog.getByRole('button', { name: '全部 18' })).toBeVisible();
      await placed(page, dialog);
      await page.setViewportSize({ width: 780, height: 390 });
      await placed(page, dialog);
      await page.setViewportSize({ width: 390, height: 844 });
      await placed(page, dialog);
      await dialog.getByRole('tab', { name: '订阅', exact: true }).click();
      await expect(dialog.getByRole('region', { name: 'Clash 订阅' })).toBeVisible();
      await placed(page, dialog);
      await dialog.locator('.sub-dialog-close').click();
      await expect(dialog).toHaveCount(0);
      expect(errors).toEqual([]);
    });

    test('a failed response still leaves the whole sheet visible', async ({ page }) => {
      const errors = await preview(page, 390, 844, 'dark', '&subscriptionError=1');
      await page.getByRole('button', { name: '订阅', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
      await placed(page, dialog);
      await reply(page);
      await expect(dialog.getByText('订阅暂时不可用，请稍后重试', { exact: true })).toBeVisible();
      await placed(page, dialog);
      await expect(page).toHaveScreenshot(`subscription-${engine}-error-390-dark.png`, SCREENSHOT_OPTIONS);
      expect(errors).toEqual([]);
    });

    test('safe areas and entry/exit motion keep the close control reachable', async ({ page }) => {
      await page.emulateMedia({ reducedMotion: 'no-preference' });
      const errors = await preview(page, 390, 844, 'dark');
      await page.evaluate(() => {
        document.documentElement.style.setProperty('--safe-top', '44px');
        document.documentElement.style.setProperty('--safe-bottom', '34px');
      });
      await page.getByRole('button', { name: '节点', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
      await reply(page);
      await expect(dialog.getByRole('button', { name: '全部 18' })).toBeVisible();
      await expect(dialog).toHaveAttribute('data-motion-state', 'entered');
      await placed(page, dialog);
      expect((await dialog.locator('.sub-dialog').boundingBox())!.y).toBeGreaterThanOrEqual(44);
      await dialog.locator('.sub-dialog-close').click();
      await expect(dialog).toHaveCount(0);
      await expect(page.getByRole('button', { name: '节点', exact: true })).toBeInViewport();
      expect(errors).toEqual([]);
    });
  });

  test(`Subscription placement ${engine} desktop stays centered`, async ({ page }) => {
    const errors = await preview(page, 1440, 900, 'light');
    await page.getByRole('button', { name: '订阅', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
    await reply(page);
    await expect(dialog.getByRole('region', { name: 'Clash 订阅' })).toBeVisible();
    await placed(page, dialog, false);
    await panelTypography(page, dialog, 'clash');
    await expect(page).toHaveScreenshot(`subscription-${engine}-desktop-light.png`, SCREENSHOT_OPTIONS);
    expect(errors).toEqual([]);
  });

  for (const [width, height] of [
    [1440, 900],
    [390, 844],
  ] as const) {
    test(`Subscription administrator second card ${engine} ${width}x${height}`, async ({ page }) => {
      const errors = await preview(page, width, height, 'dark', '&haitun=active');
      await page.getByRole('button', { name: '订阅', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
      await reply(page);
      // 测速订阅是后面那张卡：只露出卡头，撤销按钮还不在页面上。
      const peek = dialog.getByRole('button', { name: '切换到 Koipy 测速订阅（可用）' });
      await expect(peek).toBeInViewport();
      await expect(peek.getByText('Koipy 测速订阅', { exact: true })).toBeInViewport({ ratio: 1 });
      await expect(dialog.getByRole('button', { name: '撤销测速地址' })).toHaveCount(0);
      await placed(page, dialog, width <= 820);
      const before = (await dialog.locator('.sub-dialog').boundingBox())!;
      await expect(page).toHaveScreenshot(`subscription-${engine}-admin-${width}-clash.png`, SCREENSHOT_OPTIONS);
      // 卡头露出的部分才可点：从露出的那条上沿点。
      await peek.click({ position: { x: 40, y: 12 } });
      await expect(dialog.getByRole('region', { name: 'Koipy 测速订阅' })).toBeVisible();
      await expect(dialog.getByRole('button', { name: '切换到 Clash 订阅' })).toBeVisible();
      // 两张卡同一版式、同一高度：换卡时弹窗不跳。
      const after = (await dialog.locator('.sub-dialog').boundingBox())!;
      expect(Math.abs(after.height - before.height)).toBeLessThan(1);
      expect(Math.abs(after.y - before.y)).toBeLessThan(1);
      await qrCentered(dialog);
      const revoke = dialog.getByRole('button', { name: '撤销测速地址' });
      await revoke.scrollIntoViewIfNeeded();
      await expect(revoke).toBeInViewport({ ratio: 1 });
      await expect(dialog.getByRole('button', { name: '复制测速地址' })).toBeInViewport({ ratio: 1 });
      await placed(page, dialog, width <= 820);
      await expect(page).toHaveScreenshot(`subscription-${engine}-admin-${width}-koipy.png`, SCREENSHOT_OPTIONS);
      expect(errors).toEqual([]);
    });
  }

  for (const [width, height, theme, selfService] of [
    [1440, 900, 'light', false],
    [1440, 900, 'dark', true],
    [390, 844, 'light', true],
    [320, 568, 'dark', false],
    [780, 390, 'light', true],
  ] as const) {
    test(`Subscription QR ${engine} ${width}x${height} ${theme}`, async ({ page }) => {
      const errors = await preview(page, width, height, theme, selfService ? '&role=user' : '');
      await page.getByRole('button', { name: '订阅', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
      await reply(page);
      const qr = dialog.getByRole('img', { name: 'Clash 订阅地址二维码' });
      await expect(qr).toBeVisible();
      await expect(dialog.getByRole('button', { name: /标准配色|跟随主题|显示.*二维码|收起.*二维码/ })).toHaveCount(0);
      await expect(dialog.getByRole('button', { name: 'MTProxy', exact: true })).toHaveCount(0);
      const url = 'https://sub.example.test/sub/v1/00000000-0000-4000-8000-000000000000/clash.yaml';
      expect(await decodeQr(qr)).toBe(url);
      await dialog.locator('.sub-qr').evaluate(element => element.scrollIntoView({ block: 'center' }));
      await expect(qr).toBeInViewport({ ratio: 1 });
      await placed(page, dialog, width <= 820);
      await expect(page).toHaveScreenshot(
        `subscription-${engine}-qr-${width}x${height}-${theme}.png`,
        SCREENSHOT_OPTIONS,
      );

      // 卡上的两个栏位是共用菜单：桌面贴着栏位弹出，手机从底部升起；选项在弹窗之外的图层里。
      const select = async (name: string, label: string) => {
        await dialog.getByRole('combobox', { name }).click();
        await page.getByRole('option', { name: label, exact: true }).click();
        await expect(page.getByRole('listbox', { name })).toHaveCount(0);
        await expect(qr).toBeVisible();
        await qrCentered(dialog);
      };
      await qrCentered(dialog);
      await select('协议', 'Hysteria 2');
      expect(await decodeQr(qr)).toBe(`${url}?protocol=hysteria2`);
      await select('地址族', 'IPv6');
      expect(await decodeQr(qr)).toBe(`${url}?family=v6&protocol=hysteria2`);
      if (height < 600) {
        const actions = dialog.locator('.sub-pass-actions');
        await actions.scrollIntoViewIfNeeded();
        await expect(actions.getByRole('button', { name: '复制订阅地址' })).toBeInViewport({ ratio: 1 });
        await expect(actions.getByRole('button', { name: '下载二维码' })).toBeInViewport({ ratio: 1 });
        await expect(page).toHaveScreenshot(
          `subscription-${engine}-actions-${width}x${height}-${theme}.png`,
          SCREENSHOT_OPTIONS,
        );
      }
      if (width === 1440) {
        const downloadEvent = page.waitForEvent('download');
        await dialog.getByRole('button', { name: '下载二维码' }).click();
        const download = await downloadEvent;
        expect(download.suggestedFilename()).toBe('clash-subscription.svg');
        const svg = await readFile((await download.path())!, 'utf8');
        expect(svg).not.toContain('var(');
        expect(await decodeImage(page, 'data:image/svg+xml;base64,' + Buffer.from(svg).toString('base64'))).toBe(
          `${url}?family=v6&protocol=hysteria2`,
        );
        // 白底黑码：任何主题都不反色，导出的文件也一样。
        const fills = () => qr.locator('path').evaluateAll(paths => paths.map(path => getComputedStyle(path).fill));
        const paper = ['rgb(255, 255, 255)', 'rgb(16, 17, 18)'];
        expect(await fills()).toEqual(paper);
        for (const fill of paper) expect(svg).toContain(fill);
        await page.evaluate(() =>
          (window as unknown as { __userPreviewToggleTheme: () => void }).__userPreviewToggleTheme(),
        );
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme === 'dark' ? 'light' : 'dark');
        expect(await fills()).toEqual(paper);
        expect(await decodeQr(qr)).toBe(`${url}?family=v6&protocol=hysteria2`);
        const switchedDownloadEvent = page.waitForEvent('download');
        await dialog.getByRole('button', { name: '下载二维码' }).click();
        const switchedSvg = await readFile((await (await switchedDownloadEvent).path())!, 'utf8');
        expect(switchedSvg).not.toContain('var(');
        for (const fill of paper) expect(switchedSvg).toContain(fill);
        expect(
          await decodeImage(page, 'data:image/svg+xml;base64,' + Buffer.from(switchedSvg).toString('base64')),
        ).toBe(`${url}?family=v6&protocol=hysteria2`);
      }
      await dialog.locator('.sub-dialog-close').click();
      await expect(dialog).toHaveCount(0);
      expect(errors).toEqual([]);
    });
  }
}
