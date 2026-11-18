import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test, type Page } from '@playwright/test';
import {
  SCENES,
  agentState,
  nicView,
  pingLatest,
  reports,
  trafficItem,
  loadOverview,
  usageSeries,
  pingSeries,
} from '../fixtures/node-roster-data';

const now = Date.UTC(2026, 9, 28, 12);
const machines = SCENES.typical.machines();
const cvmEnabled = process.env.BROCADE_PRODUCT_FLAVOR !== 'base';

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
      '/bootstrap': {
        features: { cvm: cvmEnabled },
        who,
        initial: { node_count: machines.length, chain_group_count: [] },
      },
      '/auth/state': { initialized: true, public_open: true },
      '/whoami': who,
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
    const rangeEnd = now / 1000;
    const rangeStart = rangeEnd - 3600;
    const overview = path.match(/^\/load\/nodes\/([^/]+)\/overview$/);
    if (overview) {
      const machine = machines.find(machine => machine.id === overview[1])!;
      return route.fulfill({ json: loadOverview(machine, rangeStart, rangeEnd) });
    }
    if (path === '/usage/node-series') {
      const machine = machines.find(machine => machine.id === url.searchParams.get('node_id'))!;
      return route.fulfill({ json: usageSeries(machine, rangeStart, rangeEnd) });
    }
    const ping = path.match(/^\/ping-probe\/nodes\/([^/]+)\/series$/);
    if (ping) {
      const machine = machines.find(machine => machine.id === ping[1])!;
      return route.fulfill({ json: pingSeries(machine, rangeStart, rangeEnd) });
    }
    const history = path.match(/^\/nodes\/([^/]+)\/public-ip-history$/);
    if (history)
      return route.fulfill({ json: { node_id: history[1], events: [], visible_days: 14, retention_days: 90 } });
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
  // Hide the content layer without changing layout. WebKit needs its paint boundary to settle
  // before a raw capture; the full page snapshots below retain the real content and shadows.
  const hidden = await page.addStyleTag({
    content: '.fg-view { opacity: 0 !important; transition: none !important; }',
  });
  try {
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const png = await page.screenshot({ clip: { x: 2, y: 160, width: 8, height: 96 } });
    // Scrolling can change WebKit's compositing by one color channel. Compare the exact lattice
    // positions here; loading/ready screenshots independently check its colors and appearance.
    const pattern = await page.evaluate(async base64 => {
      const image = new Image();
      image.src = `data:image/png;base64,${base64}`;
      await image.decode();
      const canvas = document.createElement('canvas');
      canvas.width = image.width;
      canvas.height = image.height;
      const context = canvas.getContext('2d')!;
      context.drawImage(image, 0, 0);
      const rgba = context.getImageData(0, 0, canvas.width, canvas.height).data;
      const colors: string[] = [];
      const counts = new Map<string, number>();
      for (let index = 0; index < rgba.length; index += 4) {
        const color = Array.from(rgba.slice(index, index + 4)).join(',');
        colors.push(color);
        counts.set(color, (counts.get(color) ?? 0) + 1);
      }
      const background = [...counts].sort((a, b) => b[1] - a[1])[0][0];
      return colors.map(color => (color === background ? '0' : '1')).join('');
    }, png.toString('base64'));
    return { png, pattern };
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
    { name: 'desktop', width: 1440, height: 900, touch: false, theme: 'light' },
    { name: 'tablet', width: 1024, height: 768, touch: true, theme: 'dark' },
    { name: 'phone', width: 390, height: 844, touch: true, theme: 'light' },
  ]) {
    // Grid geometry is theme-independent; keep both themes across the device profiles.
    const { theme } = device;
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
        await test.info().attach('startup-grid', { body: startup.png, contentType: 'image/png' });
        enter();
        await expect(page.locator('.fg-view')).toBeVisible();
        expect(await page.locator('.fg-top').evaluate(el => getComputedStyle(el).borderBottomWidth)).toBe('0px');
        if (!device.touch) {
          const header = page.locator('.fg-top');
          await expect(header).toHaveCSS('position', 'relative');
          await expect(header).toHaveCSS(
            'background-color',
            await page.locator('html').evaluate(el => getComputedStyle(el).backgroundColor),
          );
          expect(await header.evaluate(el => el.closest('.fg-desk') !== null)).toBe(true);
        }
        await expect(page.locator('.fg-view .loading-boundary-guard')).toBeAttached();
        await expectOrigin(page, origin);
        expect((await gridPixels(page)).pattern === startup.pattern, 'grid jumped when the shell mounted').toBe(true);
        await expectAligned(page);
        const name = `${engine}-${device.name}-${theme}`;
        // This gap is already aligned before either the pane module or its data has settled.
        await expect(page).toHaveScreenshot(`${name}-loading.png`, {
          clip: { x: 0, y: 0, width: Math.min(device.width, 720), height: 240 },
        });
        release();
        await expect(page.locator('.fg-view .loading-boundary-guard')).toHaveCount(0);
        await expect(page.locator('.node-card')).toHaveCount(machines.length);
        await expect(page.locator('.nd-throughput-panel')).toHaveCount(0);
        await expect(page.locator('.node-card-rate:not(.none)').first()).toBeVisible();
        await waitForVisualAssets(page);
        await expectAligned(page);
        expect(await page.evaluate(() => getComputedStyle(document.body, '::before').backgroundPosition)).toBe(origin);
        await expect(page).toHaveScreenshot(`${name}-ready.png`, {
          clip: { x: 0, y: 0, width: Math.min(device.width, 720), height: 320 },
        });

        const before = await gridPixels(page);
        await test.info().attach('loaded-grid', { body: before.png, contentType: 'image/png' });
        expect(before.pattern === startup.pattern, 'grid jumped when data finished loading').toBe(true);
        const headerClip = { x: 720, y: 8, width: 48, height: 24 };
        const headerPixels = device.touch ? null : await page.screenshot({ clip: headerClip });
        await page.locator('body').evaluate(el => el.style.setProperty('--grid-image', 'none'));
        if (headerPixels)
          expect(
            (await page.screenshot({ clip: headerClip })).equals(headerPixels),
            'grid never paints through the desktop navigation',
          ).toBe(true);
        const absent = await gridPixels(page);
        expect(absent.pattern === before.pattern, 'grid is visible in the gutter').toBe(false);
        await page.locator('body').evaluate(el => el.style.removeProperty('--grid-image'));
        const maxScroll = await page.evaluate(touch => {
          const scroller = touch ? document.scrollingElement! : document.querySelector('.fg-desk')!;
          return scroller.scrollHeight - scroller.clientHeight;
        }, device.touch);
        expect(maxScroll, 'the real list has enough content to exercise scrolling').toBeGreaterThan(200);
        for (const offset of [73, 173, maxScroll]) {
          await scroll(page, offset, device.touch);
          await expectAligned(page);
          const immediate = await gridPixels(page);
          if (immediate.pattern !== before.pattern) {
            await test.info().attach(`grid-${offset}-immediate`, { body: immediate.png, contentType: 'image/png' });
            await test.info().attach(`grid-${offset}-expected`, { body: before.png, contentType: 'image/png' });
            await test.info().attach(`grid-${offset}-state`, {
              body: JSON.stringify({
                wasBlank: immediate.pattern === absent.pattern,
                dom: await page.evaluate(() => ({
                  scrollY,
                  origin: getComputedStyle(document.body, '::before').backgroundPosition,
                })),
              }),
              contentType: 'application/json',
            });
          }
          expect(immediate.pattern === before.pattern, `grid moved at ${offset}px`).toBe(true);
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
