import { test, expect, type Page } from '@playwright/test';

type GateWindow = Window & {
  __nodeLoading: {
    release: (path: string, fail?: boolean) => void;
    hold: (path: string) => void;
    refresh: (key: readonly unknown[]) => void;
    requests: string[];
  };
};
async function open(page: Page, tab: string, holds: string[], width = 1440, theme = 'dark', readonly = false) {
  await page.clock.setFixedTime(new Date('2026-11-17T04:00:00Z'));
  await page.setViewportSize({ width, height: 1000 });
  const params = new URLSearchParams({ tab, theme });
  for (const path of holds) params.append('hold', path);
  if (readonly) params.set('readonly', '1');
  await page.goto(`/tests/fixtures/node-detail-loading.html?${params}`);
  await page.evaluate(() => document.fonts.ready);
  if (width < 820) {
    const tabs = page.getByRole('tablist');
    await expect(tabs).toBeVisible();
    expect(
      await tabs
        .getByRole('tab')
        .evaluateAll(elements =>
          elements.every(
            (el, i) => i === 0 || el.getBoundingClientRect().left >= elements[i - 1].getBoundingClientRect().right - 1,
          ),
        ),
    ).toBe(true);
  }
}
const release = (page: Page, path: string, fail = false) =>
  page.evaluate(([path, fail]) => (window as unknown as GateWindow).__nodeLoading.release(path, fail), [
    path,
    fail,
  ] as const);
const noOverflow = async (page: Page) => {
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
};

for (const width of [1440, 390])
  for (const theme of ['dark', 'light']) {
    test(`machine flow loading and ready at ${width}px ${theme}`, async ({ page, browserName }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await open(page, 'chains', ['/compile/1'], width, theme);
      const card = page.locator('.node-chain-sheet');
      await expect(card.locator('.flow-well-loading')).toHaveCount(2);
      await expect(card.locator('.chain-rule-section-name').first()).toContainText('香港');
      await expect(card.locator('.flow-mode').first()).toBeDisabled();
      await expect(card.locator('.rule-draft-save')).toHaveCount(0);
      await expect(card.locator('.flow-machine')).toHaveCount(0);
      const titleTop = await card
        .locator('.chain-rule-section-head')
        .first()
        .evaluate(el => el.getBoundingClientRect().top - el.closest('.node-chain-sheet')!.getBoundingClientRect().top);
      await noOverflow(page);
      await expect(width < 820 ? page : card.locator('.chain-rule-section').first()).toHaveScreenshot(
        `${browserName}-rules-${width}-${theme}-loading.png`,
      );
      await release(page, '/compile/1');
      await expect(card.locator('.flow-well-loading')).toHaveCount(0);
      await expect(card.locator('.rule-draft-save')).toBeVisible();
      await expect(card.locator('.flow-machine').first()).toBeVisible();
      await expect(card.locator('.flow-mode').first()).toBeEnabled();
      await expect(width < 820 ? page : card.locator('.chain-rule-section').first()).toHaveScreenshot(
        `${browserName}-rules-${width}-${theme}-ready.png`,
      );
      expect(
        await card
          .locator('.chain-rule-section-head')
          .first()
          .evaluate(
            el => el.getBoundingClientRect().top - el.closest('.node-chain-sheet')!.getBoundingClientRect().top,
          ),
      ).toBe(titleTop);
      await noOverflow(page);
      expect(errors).toEqual([]);
    });

    test(`machine configuration reserves pending traffic and animates maintenance at ${width}px ${theme}`, async ({
      page,
      browserName,
    }) => {
      const path = '/nodes/hk-fxt/ip-maintenance';
      await open(page, 'config', ['/node-traffic', path], width, theme);
      await expect(page.getByRole('status', { name: '读取流量统计…' })).toBeVisible();
      const care = page.locator('.care-config');
      await expect(care.locator('.loading-mark-field')).toBeVisible();
      const followingCard = await page
        .locator('.node-config-card-state[data-kind="usage"]')
        .evaluateHandle(el => el.nextElementSibling!);
      const nextTop = () =>
        followingCard.evaluate(el => el.getBoundingClientRect().top - el.parentElement!.getBoundingClientRect().top);
      const before = await nextTop();
      await expect(page.locator('.node-config-card-state[data-kind="usage"]')).toHaveScreenshot(
        `${browserName}-traffic-${width}-${theme}-loading.png`,
      );
      await expect(care).toHaveScreenshot(`${browserName}-maintenance-${width}-${theme}-loading.png`);
      await release(page, '/node-traffic');
      await release(page, path);
      await expect(page.getByRole('combobox', { name: '机器流量计量口径' })).toBeVisible();
      await expect(care.locator('.loading-mark-field')).toHaveCount(0);
      await expect(page.locator('.node-traffic-accounting')).toHaveScreenshot(
        `${browserName}-traffic-${width}-${theme}-ready.png`,
      );
      await expect(care).toHaveScreenshot(`${browserName}-maintenance-${width}-${theme}-ready.png`);
      expect(Math.abs((await nextTop()) - before)).toBeLessThan(40);
      await noOverflow(page);
    });

    test(`machine configuration retains the DNS slot during snapshot loading at ${width}px ${theme}`, async ({
      page,
      browserName,
    }) => {
      await open(page, 'config', ['/model/snapshot'], width, theme);
      await expect(page.getByRole('status', { name: '读取DNS…' })).toBeVisible();
      const care = page.locator('.care-config');
      await expect(care).toBeVisible();
      const gap = () =>
        care.evaluate(el => el.getBoundingClientRect().top - el.parentElement!.getBoundingClientRect().top);
      const before = await gap();
      await expect(page).toHaveScreenshot(`${browserName}-config-snapshot-${width}-${theme}-loading.png`);
      await release(page, '/model/snapshot');
      await expect(page.getByRole('status', { name: '读取DNS…' })).toHaveCount(0);
      await expect(care).toBeVisible();
      expect(Math.abs((await gap()) - before)).toBeLessThan(40);
      await noOverflow(page);
    });

    test(`machine unlock quiet reads and service entrance at ${width}px ${theme}`, async ({ page, browserName }) => {
      const path = '/nodes/hk-fxt/unlock-probe?family=v4';
      await open(page, 'unlock', [path], width, theme);
      const panel = page.locator('.node-unlock');
      await expect(panel).toHaveAttribute('aria-busy', 'true');
      await expect(panel.getByRole('status')).toHaveCount(0);
      await expect(page.getByText('读取解锁结果…', { exact: true })).toHaveCount(0);
      await expect(page.getByRole('article', { name: 'ChatGPT' })).toHaveCount(0);
      await expect(width < 820 ? page : panel).toHaveScreenshot(`${browserName}-unlock-${width}-${theme}-loading.png`);
      await release(page, path);
      await expect(page.getByRole('article', { name: 'ChatGPT' })).toBeVisible();
      await expect(panel).toHaveAttribute('aria-busy', 'false');
      await expect(page.getByRole('article')).toHaveCount(13);
      await expect(width < 820 ? page : panel).toHaveScreenshot(`${browserName}-unlock-${width}-${theme}-ready.png`);
      if (width < 820) {
        await page.getByRole('article', { name: 'Reddit' }).scrollIntoViewIfNeeded();
        await expect(page).toHaveScreenshot(`${browserName}-unlock-${width}-${theme}-ready-bottom.png`);
      }
      await noOverflow(page);
      // Re-entering a cached tab should show its real contents immediately.
      await page.getByRole('tab', { name: /规则/ }).click();
      await page.getByRole('tab', { name: '解锁', exact: true }).click();
      await expect(page.getByRole('article', { name: 'ChatGPT' })).toBeVisible();
      await expect(page.getByText('读取解锁结果…', { exact: true })).toHaveCount(0);
    });
  }

test('machine rule module download and failed compilation both remain visible', async ({ page, browserName }) => {
  let allowModule!: () => void;
  const moduleGate = new Promise<void>(done => {
    allowModule = done;
  });
  await page.route('**/src/panes/chains.tsx*', async route => {
    await moduleGate;
    await route.continue();
  });
  await open(page, 'chains', ['/compile/1']);
  const card = page.locator('.node-chain-sheet');
  await expect(card.getByRole('status', { name: '加载中…' })).toBeVisible();
  await expect(card.locator('.rule-draft-save')).toHaveCount(0);
  allowModule();
  await expect(card.locator('.flow-well-loading')).toHaveCount(2);
  await release(page, '/compile/1', true);
  await expect(card.locator('.flow-well-loading')).toHaveCount(0);
  await expect(card.getByText('合成接口暂时不可用', { exact: true })).toHaveCount(2);
  await expect(card.locator('.rule-draft-save')).toHaveCount(0);
  await expect(card).toHaveScreenshot(`${browserName}-rules-read-error.png`);
});

test('readonly machine rules do not require an unreadable settings request', async ({ page }) => {
  await open(page, 'chains', ['/settings'], 390, 'light', true);
  await expect(page.locator('.flow-machine').first()).toBeVisible();
  await expect(page.locator('.flow-well-loading')).toHaveCount(0);
  expect(
    await page.evaluate(() => (window as unknown as GateWindow).__nodeLoading.requests.includes('/settings')),
  ).toBe(false);
  await expect(page.locator('.node-chain-sheet .rule-draft-save')).toHaveCount(0);
});

test('four-second pending request keeps the sweep active, then content enters once and stays through refresh', async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await open(page, 'chains', ['/compile/1']);
  const wait = page.locator('.flow-well-loading').first();
  await expect(wait).toBeVisible();
  expect(await wait.evaluate(el => getComputedStyle(el, '::after').animationName)).toBe('flow-loading-sweep');
  const before = await wait.evaluate(el => getComputedStyle(el, '::after').maskPosition);
  await page.waitForTimeout(4000);
  await expect(wait).toBeVisible();
  expect(await wait.evaluate(el => getComputedStyle(el, '::after').maskPosition)).not.toBe(before);
  await release(page, '/compile/1');
  const machine = page.locator('.flow-machine').first();
  await expect(machine).toBeVisible();
  const column = page.locator('.flow-column').first();
  expect(await column.evaluate(el => getComputedStyle(el).animationName)).toBe('flow-content-reveal');
  await page.waitForTimeout(500);
  const ref = await machine.elementHandle();
  await page.evaluate(() => {
    const gate = (window as unknown as GateWindow).__nodeLoading;
    gate.hold('/compile/1');
    gate.refresh(['compile', 1]);
  });
  await expect(page.locator('.flow-well-loading')).toHaveCount(0);
  expect(await ref!.evaluate(el => el.isConnected)).toBe(true);
  expect(await column.evaluate(el => el.getAnimations().length)).toBe(0);
  await release(page, '/compile/1');
});

test('reduced motion keeps the loading label without sweep or delayed content', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await open(page, 'chains', ['/compile/1'], 390, 'light');
  const wait = page.locator('.flow-well-loading').first();
  await expect(wait.getByRole('status')).toHaveText('加载中…');
  expect(await wait.evaluate(el => getComputedStyle(el, '::after').animationName)).toBe('none');
  await release(page, '/compile/1');
  await expect(page.locator('.flow-machine').first()).toBeVisible();
  expect(
    await page
      .locator('.flow-column')
      .first()
      .evaluate(el => getComputedStyle(el).animationName),
  ).toBe('none');
});

test('resizing during loading uses the current phone layout for the height handoff', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await open(page, 'chains', ['/compile/1']);
  const wait = page.locator('.flow-well-loading').first();
  await expect(wait).toBeVisible();
  const desktopHeight = await wait.evaluate(el => (el as HTMLElement).offsetHeight);
  await page.setViewportSize({ width: 390, height: 1000 });
  await expect.poll(() => wait.evaluate(el => (el as HTMLElement).offsetHeight)).toBeGreaterThan(desktopHeight);
  // ResizeObserver receives the phone dimensions before the request is released.
  await page.evaluate(
    () => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))),
  );
  const phoneHeight = await wait.evaluate(el => (el as HTMLElement).offsetHeight);
  await release(page, '/compile/1');
  const well = page.locator('.flow-well').first();
  await expect(page.locator('.flow-well-loading')).toHaveCount(0);
  const from = await well.evaluate(el => {
    for (const animation of el.getAnimations()) {
      if (!(animation.effect instanceof KeyframeEffect)) continue;
      if (animation.effect.getKeyframes()[0].height) {
        animation.pause();
        animation.currentTime = 0;
        return el.getBoundingClientRect().height;
      }
    }
    return null;
  });
  expect(from).toBe(phoneHeight);
});
