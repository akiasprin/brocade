import { test as base, expect, chromium, webkit, type Page, type Locator } from '@playwright/test';

type Kind = 'machine' | 'chain';

async function open(page: Page, kind: Kind, theme = 'light') {
  await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
  const path =
    kind === 'machine'
      ? `node-list-preview.html?scene=typical&theme=${theme}#/nodes`
      : `chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains`;
  await page.goto(`/tests/fixtures/${path}`);
  const card = page.locator(kind === 'machine' ? '.node-card.tone-ok' : '.chain-card.tone-ok').first();
  await expect(card).toBeVisible();
  if (kind === 'machine') await expect(card.locator('.node-card-resources')).toHaveAttribute('data-state', 'ready');
  else await expect(card.locator('.chain-card-latency b')).toHaveText(/^\d+$/);
  await page.evaluate(() => document.fonts.ready);
  // Keep the card clear of the sticky page header during locator screenshots.
  await card.evaluate(element => element.scrollIntoView({ block: 'center', behavior: 'instant' }));
  return card;
}

async function scale(card: Locator) {
  return card.evaluate(element => new DOMMatrixReadOnly(getComputedStyle(element).transform).a);
}

async function settle(card: Locator) {
  await card.evaluate(async element => {
    await Promise.all(
      element
        .getAnimations({ subtree: true })
        .filter(animation => animation.effect?.getComputedTiming().iterations !== Infinity)
        .map(animation => animation.finished.catch(() => undefined)),
    );
  });
}

async function hoverSurface(card: Locator) {
  await card.hover({ position: { x: 150, y: 18 } });
  await settle(card);
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
  test.describe(engine, () => {
    test.use({ reducedMotion: 'no-preference', viewport: { width: 1440, height: 1000 } });

    for (const theme of ['dark', 'light']) {
      for (const kind of ['machine', 'chain'] as const) {
        test(`${kind} hover and press ${theme}`, async ({ page }) => {
          const card = await open(page, kind, theme);
          // Finish entrance animations before measuring the resting card.
          await expect.poll(() => scale(card)).toBe(1);
          await page.mouse.move(0, 0);
          await settle(card);
          const resting = await card.evaluate(element => {
            const style = getComputedStyle(element);
            return { shadow: style.boxShadow, width: element.getBoundingClientRect().width };
          });
          await hoverSurface(card);
          await expect
            .poll(() =>
              card.evaluate(element => {
                const style = getComputedStyle(element);
                return style.boxShadow;
              }),
            )
            .not.toBe(resting.shadow);
          await expect(card).toHaveScreenshot(`${engine}-${kind}-hover-${theme}.png`);
          const transitions = await card.evaluate(element => {
            const style = getComputedStyle(element);
            return style.transitionProperty.split(',').map((property, index) => ({
              property: property.trim(),
              seconds: Number.parseFloat(style.transitionDuration.split(',')[index]),
            }));
          });
          expect(transitions.find(item => item.property === 'transform')?.seconds).toBeGreaterThan(0);
          await page.mouse.down();
          await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
          expect((await card.boundingBox())!.width / resting.width).toBeCloseTo(0.985, 3);
          await expect(card).toHaveScreenshot(`${engine}-${kind}-pressed-${theme}.png`);
          await page.mouse.up();
          await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
        });
      }
    }
  });
}

for (const kind of ['machine', 'chain'] as const) {
  base(`${kind} reduced motion retains immediate press feedback`, async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' });
    const card = await open(page, kind);
    await card.hover();
    await page.mouse.down();
    await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
    const durations = await card.evaluate(element => getComputedStyle(element).transitionDuration);
    expect(durations.split(',').every(value => Number.parseFloat(value) <= 0.00001)).toBe(true);
    await page.mouse.up();
    await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
  });
}

base('machine hover preserves the stronger selected ring', async ({ page }) => {
  await open(page, 'machine');
  await page.getByRole('button', { name: '多选', exact: true }).click();
  const card = page
    .locator('.node-card')
    .filter({ has: page.getByRole('checkbox', { name: '选中 澳门 CTM', exact: true }) });
  await card.getByRole('checkbox').check();
  await expect(card).toHaveClass(/picked/);
  await page.mouse.move(0, 0);
  await settle(card);
  const selected = await card.evaluate(element => getComputedStyle(element).boxShadow);
  await hoverSurface(card);
  expect(await card.evaluate(element => getComputedStyle(element).boxShadow)).toBe(selected);
});

const touch = base.extend({
  browser: [
    async ({}, use) => {
      const browser = await webkit.launch();
      await use(browser);
      await browser.close();
    },
    { scope: 'worker' },
  ],
});
touch.describe('WebKit touch', () => {
  touch.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 844 } });
  for (const kind of ['machine', 'chain'] as const) {
    touch(`${kind} still opens on the first tap`, async ({ page }) => {
      const card = await open(page, kind);
      await card.tap();
      await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
    });
  }
});

for (const kind of ['machine', 'chain'] as const) {
  base(`${kind} Space waits for release and preserves the pressed state`, async ({ page }) => {
    const card = await open(page, kind);
    await page.keyboard.press('Tab');
    await card.focus();
    await expect(card).toHaveCSS('outline-style', 'solid');
    await page.keyboard.down('Space');
    await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
    await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes$/ : /#\/chains$/);
    await page.keyboard.up('Space');
    await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
  });
  base(`${kind} selected press and checkbox focus preserve selection`, async ({ page }) => {
    await open(page, kind);
    await page.getByRole('button', { name: '多选', exact: true }).click();
    const card = page.locator(kind === 'machine' ? '.node-card.tone-ok' : '.chain-card.tone-ok').first();
    const checkbox = card.getByRole('checkbox');
    await checkbox.check();
    await expect(card).toHaveClass(/picked/);
    await page.mouse.move(0, 0);
    await settle(card);
    const selected = await card.evaluate(element => {
      const style = getComputedStyle(element);
      return { shadow: style.boxShadow, background: style.backgroundColor, image: style.backgroundImage };
    });
    await hoverSurface(card);
    expect(await card.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(selected.background);
    await page.mouse.down();
    await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
    expect(await card.evaluate(element => getComputedStyle(element).boxShadow)).toBe(selected.shadow);
    await expect(card).toHaveScreenshot(`${kind}-selected-pressed.png`);
    await page.mouse.up();
    await checkbox.check();
    await page.keyboard.press('Tab');
    await checkbox.focus();
    await expect(card).toHaveCSS('outline-style', 'solid');
    await expect(card).toHaveCSS('outline-width', '2px');
    expect(await card.evaluate(element => getComputedStyle(element).boxShadow)).toBe(selected.shadow);
    await expect(card).toHaveScreenshot(`${kind}-selected-checkbox-focus.png`);
    await page.keyboard.press('Space');
    await expect(checkbox).not.toBeChecked();
    await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes$/ : /#\/chains$/);
  });
}

base('chain Space cancels when focus leaves or the window blurs', async ({ page }) => {
  const card = await open(page, 'chain');
  for (const cancel of ['focus', 'window']) {
    await card.focus();
    await page.keyboard.down('Space');
    await expect(card).toHaveAttribute('data-card-pressed', 'true');
    if (cancel === 'focus') await page.getByRole('button', { name: '返回机器首页' }).focus();
    else await page.evaluate(() => window.dispatchEvent(new Event('blur')));
    await expect(card).not.toHaveAttribute('data-card-pressed', 'true');
    await expect.poll(() => scale(card)).toBe(1);
    await page.keyboard.up('Space');
    await expect(page).toHaveURL(/#\/chains$/);
  }
});

touch.describe('coarse-pointer held styles', () => {
  touch.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 844 } });
  for (const theme of ['dark', 'light'])
    for (const kind of ['machine', 'chain'] as const) {
      touch(`${kind} coarse press has its own edge without hover ${theme}`, async ({ page }) => {
        const card = await open(page, kind, theme);
        await expect
          .poll(() => page.evaluate(() => matchMedia('(hover: hover) and (pointer: fine)').matches))
          .toBe(false);
        const resting = await card.evaluate(element => getComputedStyle(element).boxShadow);
        // Mouse-down holds :active for a screenshot; the context remains coarse with hover disabled.
        // Native touch down/up and scroll cancellation are tested separately below.
        await card.hover();
        expect(await card.evaluate(element => getComputedStyle(element).boxShadow)).toBe(resting);
        await page.mouse.down();
        await expect.poll(() => card.evaluate(element => getComputedStyle(element).boxShadow)).not.toBe(resting);
        await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
        await expect(card).toHaveScreenshot(`webkit-coarse-${kind}-press-${theme}.png`);
        await page.mouse.up();
        await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
      });
    }
});

base.describe('native Chromium touch input', () => {
  base.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 844 } });
  for (const kind of ['machine', 'chain'] as const)
    for (const cancel of [false, true]) {
      base(
        `${kind} native touch ${cancel ? 'scroll cancels press' : 'highlights before release'}`,
        async ({ page }) => {
          const card = await open(page, kind);
          await card.scrollIntoViewIfNeeded();
          const resting = await card.evaluate(element => getComputedStyle(element).boxShadow);
          const bounds = (await card.boundingBox())!;
          const x = bounds.x + bounds.width / 2,
            y = bounds.y + bounds.height / 2;
          const input = await page.context().newCDPSession(page);
          await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y, id: 1 }] });
          await expect(card).toHaveAttribute('data-card-pointer-pressed', 'true');
          await expect.poll(() => card.evaluate(element => getComputedStyle(element).boxShadow)).not.toBe(resting);
          await expect.poll(() => scale(card)).toBeCloseTo(0.985, 3);
          if (!cancel) await expect(card).toHaveScreenshot(`${kind}-native-touch-press.png`);
          if (cancel) {
            await input.send('Input.dispatchTouchEvent', {
              type: 'touchMove',
              touchPoints: [{ x, y: y - 120, id: 1 }],
            });
            await expect(card).not.toHaveAttribute('data-card-pointer-pressed', 'true');
          }
          await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
          if (cancel) {
            await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes$/ : /#\/chains$/);
            await expect.poll(() => scale(card)).toBe(1);
            await expect.poll(() => card.evaluate(element => getComputedStyle(element).boxShadow)).toBe(resting);
          } else await expect(page).toHaveURL(kind === 'machine' ? /#\/nodes\/node\// : /#\/chains\/chain\//);
        },
      );
    }
});
