import { expect, test, type Page } from '@playwright/test';
import { SCENES, agentState, nicView, pingLatest, reports, trafficItem } from '../fixtures/node-list-mockup-data';

const now = Date.UTC(2026, 9, 28, 12);
const machines = SCENES.typical.machines();

async function openWorkspace(page: Page, theme: string) {
  let enter!: () => void;
  const bootstrapPending = new Promise<void>(resolve => {
    enter = resolve;
  });
  let release!: () => void;
  const pending = new Promise<void>(resolve => {
    release = resolve;
  });
  const who = {
    operator_id: 'public',
    role: 'readonly',
    tenant_scope: null,
    token_prefix: null,
    masked_assets: true,
  };
  await page.clock.setFixedTime(now);
  await page.addInitScript(value => localStorage.setItem('brocade-console:theme', value), theme);
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    const body: Record<string, unknown> = {
      '/branding': { site_name: '网格预览', icon_data_url: null },
      '/bootstrap': { who, initial: { node_count: machines.length, chain_group_count: [] } },
      '/auth/state': { initialized: true, public_open: true },
      '/auth/whoami': who,
      '/nodes/agent-state': { nodes: machines.map(machine => agentState(machine, now / 1000)) },
      '/node-traffic': { nodes: machines.map(machine => trafficItem(machine, now / 1000)) },
      '/load/nodes/nic': {
        nodes: machines
          .filter(reports)
          .map(machine => nicView(machine, now / 1000, Number(url.searchParams.get('windows')) || 24)),
      },
      '/ping-probe/nodes/latest': { nodes: machines.flatMap(machine => pingLatest(machine, now / 1000) ?? []) },
      '/users': { users: [] },
      '/users/presence': { users: [] },
      '/usage/user-cycles': { periods: [], views: [], forwarding: [] },
      '/quotas': { quotas: [] },
      '/model/snapshot': {
        snapshot: { revision: 1, nodes: [], apps: [], external_outbounds: [] },
        node_egress_dns: [],
        redacted: true,
      },
      '/tenants': { tenants: [] },
    };
    if (path === '/bootstrap') await bootstrapPending;
    if (path === '/nodes/agent-state') await pending;
    if (path in body) await route.fulfill({ json: body[path] });
    else if (path === '/realtime/nodes/events') await route.fulfill({ contentType: 'text/event-stream', body: '' });
    else await route.continue();
  });
  await page.goto('/#/nodes');
  await expect(page.locator('#stage')).toBeVisible();
  return { enter, release };
}

const gridOrigin = (page: Page, selector = 'body') =>
  page.locator(selector).evaluate(el => getComputedStyle(el, '::before').backgroundPosition);

async function expectOrigin(page: Page, origin: string) {
  await expect.poll(() => gridOrigin(page)).toBe(origin);
}

async function gridPixels(page: Page) {
  // Loaded cards cast shadows into the narrow gutter. Hide their paint without changing layout
  // while comparing the grid itself; the full page screenshots below retain the actual content.
  const hidden = await page.addStyleTag({ content: '.fg-view, .fg-view * { visibility: hidden !important; }' });
  try {
    return await page.screenshot({ clip: { x: 2, y: 160, width: 8, height: 96 } });
  } finally {
    await hidden.evaluate(el => el.remove());
  }
}

async function expectAligned(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const surface = document.querySelector('.fg-view')!;
        const bounds = surface.getBoundingClientRect();
        const desk = surface.closest('.fg-desk')!;
        const grid = getComputedStyle(document.body, '::before');
        return {
          x: Math.abs(Number.parseFloat(grid.backgroundPositionX) - (bounds.left + scrollX)),
          y: Math.abs(Number.parseFloat(grid.backgroundPositionY) - (bounds.top + scrollY + desk.scrollTop)),
          size32: grid.backgroundSize.split(',').every(size => size.trim() === '32px 32px'),
        };
      }),
    )
    .toEqual({ x: 0, y: 0, size32: true });
}

async function scroll(page: Page, offset: number, touch: boolean) {
  await page.evaluate(
    ({ offset, touch }) => {
      if (touch) window.scrollTo(0, offset);
      else document.querySelector('.fg-desk')!.scrollTop = offset;
    },
    { offset, touch },
  );
  await expect
    .poll(() => page.evaluate(touch => (touch ? scrollY : document.querySelector('.fg-desk')!.scrollTop), touch))
    .toBe(offset);
}

for (const engine of ['chromium', 'webkit'] as const) {
  for (const device of [
    { name: 'desktop', width: 1440, height: 900, touch: false },
    { name: 'tablet', width: 1024, height: 768, touch: true },
    { name: 'phone', width: 390, height: 844, touch: true },
  ]) {
    for (const theme of ['light', 'dark']) {
      test(`${engine} ${device.name} ${theme}: grid aligns during loading and stays still while scrolling`, async ({
        playwright,
        baseURL,
      }) => {
        const browser = await playwright[engine].launch();
        try {
          const page = await browser.newPage({
            baseURL,
            viewport: device,
            hasTouch: device.touch,
            isMobile: device.touch,
            reducedMotion: 'reduce',
            locale: 'zh-CN',
            timezoneId: 'Asia/Taipei',
          });
          const errors: string[] = [];
          page.on('pageerror', error => errors.push(error.message));
          const { enter, release } = await openWorkspace(page, theme);
          const origin = await gridOrigin(page, '#stage');
          const startup = await gridPixels(page);
          await test.info().attach('startup-grid', { body: startup, contentType: 'image/png' });
          enter();
          await expect(page.locator('.fg-view')).toBeVisible();
          await expect(page.locator('.fg-view .loading-boundary-guard')).toBeAttached();
          await expectOrigin(page, origin);
          expect((await gridPixels(page)).equals(startup), 'grid jumped when the shell mounted').toBe(true);
          await expectAligned(page);
          const name = `${engine}-${device.name}-${theme}`;
          // This gap is already aligned before either the pane module or its data has settled.
          await expect(page).toHaveScreenshot(`${name}-loading.png`, {
            clip: { x: 0, y: 0, width: Math.min(device.width, 720), height: 240 },
          });
          release();
          await expect(page.locator('.fg-view .loading-boundary-guard')).toHaveCount(0);
          await expect(page.locator('.ncard')).toHaveCount(machines.length);
          await page.evaluate(() => document.fonts.ready);
          await expectAligned(page);
          expect(await page.evaluate(() => getComputedStyle(document.body, '::before').backgroundPosition)).toBe(
            origin,
          );
          await expect(page).toHaveScreenshot(`${name}-ready.png`, {
            clip: { x: 0, y: 0, width: Math.min(device.width, 720), height: 320 },
          });

          const before = await gridPixels(page);
          await test.info().attach('loaded-grid', { body: before, contentType: 'image/png' });
          expect(before.equals(startup), 'grid jumped when data finished loading').toBe(true);
          await page.locator('body').evaluate(el => el.style.setProperty('--grid-image', 'none'));
          expect((await gridPixels(page)).equals(before), 'grid is visible in the gutter').toBe(false);
          await page.locator('body').evaluate(el => el.style.removeProperty('--grid-image'));
          const maxScroll = await page.evaluate(touch => {
            const scroller = touch ? document.scrollingElement! : document.querySelector('.fg-desk')!;
            return scroller.scrollHeight - scroller.clientHeight;
          }, device.touch);
          expect(maxScroll, 'the real list has enough content to exercise scrolling').toBeGreaterThan(200);
          for (const offset of [73, 173, maxScroll]) {
            await scroll(page, offset, device.touch);
            expect((await gridPixels(page)).equals(before), `grid moved at ${offset}px`).toBe(true);
          }

          // Content/chrome changes must never relocate a background that has already been painted.
          await page.locator('.fg-top').evaluate(el => (el.style.height = '63px'));
          await expectOrigin(page, origin);
          await page.locator('.fg-top').evaluate(el => el.style.removeProperty('height'));
          await page.evaluate(() => {
            document.documentElement.style.setProperty('--safe-top', '23px');
            document.documentElement.style.setProperty('--safe-left', '9px');
          });
          await expect
            .poll(() =>
              page.evaluate(() => Number.parseFloat(getComputedStyle(document.body, '::before').backgroundPositionX)),
            )
            .toBe(9);
          await expectAligned(page);
          const safeOrigin = await gridOrigin(page);
          await page.evaluate(() => {
            const draft = document.createElement('div');
            draft.className = 'fg-draft';
            draft.style.minHeight = '72px';
            document.querySelector('.fg-view')!.before(draft);
          });
          await expectOrigin(page, safeOrigin);
          await page.locator('.fg-draft').evaluate(el => el.remove());
          await expectOrigin(page, safeOrigin);
          const resized = { width: device.name === 'phone' ? 844 : 768, height: 600 };
          await page.setViewportSize(resized);
          await expectAligned(page);

          // Real navigation replaces .fg-view; the new surface uses the same alignment lifecycle.
          await page.getByRole('button', { name: '用户', exact: true }).click();
          await expect(page).toHaveURL(/#\/users$/);
          await expect(page.locator('.fg-view .loading-boundary-guard')).toHaveCount(0);
          await expectAligned(page);
          expect(errors).toEqual([]);
        } finally {
          await browser.close();
        }
      });
    }
  }
}
