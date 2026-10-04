import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { chromium, webkit } from '@playwright/test';
import ts from 'typescript';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const bootstrap = html.match(/<script id="appearance-bootstrap">([\s\S]*?)<\/script>/)[1];
const appearance = ts.transpileModule(
  readFileSync(new URL('../src/forge/browser-appearance.ts', import.meta.url), 'utf8'),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } },
).outputText;
const palettes = ['dailan', 'jinzi', 'songlv', 'oufen', 'xuanmo'];

for (const [engine, launcher] of Object.entries({ chromium, webkit })) {
  test(`${engine}: button, disclosure and link controls share one box and text center`, async () => {
    const browser = await launcher.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 960, height: 640 } });
      await page.setContent(`<!doctype html><html data-theme="light" data-palette="dailan"><head>
        <style>${styles}</style></head><body>
        <div class="cgc-tbar-row">
          <details><summary class="btn"><span>查看 1 个修订</span></summary></details>
          <button class="btn" disabled><span>批准</span></button>
          <a class="btn" href="#"><span>查看其余 44 条</span></a>
        </div>
        </body></html>`);

      const controls = await page.locator('.btn').evaluateAll(elements =>
        elements.map(element => {
          const box = element.getBoundingClientRect();
          const text = element.querySelector('span').getBoundingClientRect();
          const computed = getComputedStyle(element);
          return {
            height: box.height,
            offset: text.y + text.height / 2 - box.y - box.height / 2,
            display: computed.display,
            alignItems: computed.alignItems,
            justifyContent: computed.justifyContent,
          };
        }),
      );
      assert.deepEqual(
        controls.map(({ height }) => height),
        [28, 28, 28],
      );
      for (const control of controls) {
        assert.ok(['flex', 'inline-flex'].includes(control.display));
        assert.equal(control.alignItems, 'center');
        assert.equal(control.justifyContent, 'center');
        assert.ok(Math.abs(control.offset) <= 0.5, `text vertical offset was ${control.offset}px`);
      }
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: title icons and Chinese, Latin or mixed text share a vertical center`, async () => {
    const browser = await launcher.launch();
    try {
      for (const mobile of [false, true]) {
        const page = await browser.newPage({
          viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 },
          hasTouch: mobile,
          isMobile: mobile,
          reducedMotion: 'reduce',
        });
        const panels = ['机器', '运行状态', 'DNS', 'VPN Gate', '解析 DNS']
          .flatMap(text =>
            ['list', 'panel', 'summary'].map(kind => {
              const icon = `<span class="${kind === 'list' ? 'list-ico' : 'panel-title-icon'}"><svg width="14" height="14"></svg></span>`;
              const tag = kind === 'summary' ? 'summary' : 'header';
              const container = kind === 'summary' ? 'details' : 'section';
              const title = kind === 'list' ? `${icon}<h4>${text}</h4>` : `<h4 class="panel-title">${icon}${text}</h4>`;
              return `<${container} class="panel ${kind === 'list' ? 'titled node-list-panel' : 'config-panel'}"><${tag}>${title}</${tag}></${container}>`;
            }),
          )
          .join('');
        await page.setContent(`<!doctype html><html><head>
          ${html.match(/<meta name="viewport"[^>]+>/)[0]}<style>${styles}</style></head>
          <body><main class="cardpage">${panels}</main></body></html>`);
        const centers = await page.locator('h4').evaluateAll(headings =>
          headings.map(heading => {
            const text = [...heading.childNodes].find(node => node.nodeType === Node.TEXT_NODE);
            const range = document.createRange();
            range.selectNode(text);
            const textRect = range.getBoundingClientRect();
            const iconRect = heading.parentElement.querySelector('svg').getBoundingClientRect();
            return {
              text: text.textContent,
              lineHeight: getComputedStyle(heading).lineHeight,
              offset: textRect.y + textRect.height / 2 - iconRect.y - iconRect.height / 2,
            };
          }),
        );
        assert.equal(centers.length, 15);
        for (const result of centers) {
          assert.equal(result.lineHeight, '14px');
          assert.ok(Math.abs(result.offset) <= 0.5, `${result.text}: vertical offset ${result.offset}px`);
        }
        await page.close();
      }
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: list headings inherit the ordinary panel typography in every theme`, async () => {
    const browser = await launcher.launch();
    try {
      for (const mobile of [false, true]) {
        const page = await browser.newPage({
          viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 },
          hasTouch: mobile,
          isMobile: mobile,
          reducedMotion: 'reduce',
        });
        const lists = ['node', 'chain', 'tunnel', 'user']
          .map(name => `<section class="panel titled ${name}-list-panel"><header><h4>列表标题</h4></header></section>`)
          .join('');
        await page.setContent(`<!doctype html><html><head>
          ${html.match(/<meta name="viewport"[^>]+>/)[0]}<style>${styles}</style></head><body><main class="cardpage">
          <section class="panel config-panel"><header><h4>普通面板</h4></header></section>
          ${lists}<section class="panel titled" data-page-title><header><h4>公共列表</h4></header></section>
          </main></body></html>`);
        for (const theme of ['light', 'dark']) {
          for (const palette of palettes) {
            const headings = await page.evaluate(
              ({ theme, palette }) => {
                Object.assign(document.documentElement.dataset, { theme, palette });
                return [...document.querySelectorAll('.panel > header h4')].map(element => {
                  const style = getComputedStyle(element);
                  return Object.fromEntries(
                    [
                      'fontSize',
                      'fontFamily',
                      'fontWeight',
                      'letterSpacing',
                      'lineHeight',
                      'color',
                      'textTransform',
                    ].map(property => [property, style[property]]),
                  );
                });
              },
              { theme, palette },
            );
            assert.equal(headings.length, 6);
            assert.equal(headings[0].fontSize, '12px');
            for (const heading of headings.slice(1)) assert.deepEqual(heading, headings[0]);
          }
        }
        await page.close();
      }
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: every popup menu uses the shared translucent glass material`, async () => {
    const browser = await launcher.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 960, height: 640 } });
      await page.setContent(`<!doctype html><html data-theme="dark" data-palette="dailan"><head>
        <style>${styles}</style></head><body>
        <div class="fg-menu">账户菜单</div>
        <div class="observe-range-menu">时间范围菜单</div>
        <div class="external-target-menu">目标选择菜单</div>
        <div class="cg-menu">发布操作菜单</div>
        </body></html>`);

      const selectors = ['.fg-menu', '.observe-range-menu', '.external-target-menu', '.cg-menu'];
      for (const theme of ['light', 'dark']) {
        for (const palette of palettes) {
          await page.evaluate(
            ({ theme, palette }) => Object.assign(document.documentElement.dataset, { theme, palette }),
            { theme, palette },
          );
          for (const selector of selectors) {
            const material = await page.locator(selector).evaluate(element => {
              const computed = getComputedStyle(element);
              const canvas = document.createElement('canvas');
              canvas.width = 1;
              canvas.height = 1;
              const context = canvas.getContext('2d');
              context.clearRect(0, 0, 1, 1);
              context.fillStyle = computed.backgroundColor;
              context.fillRect(0, 0, 1, 1);
              return {
                alpha: context.getImageData(0, 0, 1, 1).data[3] / 255,
                filter: computed.backdropFilter || computed.webkitBackdropFilter,
              };
            });
            assert.ok(material.alpha > 0.55 && material.alpha < 0.7, `${selector} alpha was ${material.alpha}`);
            assert.match(material.filter, /blur\(20px\)/, `${selector} keeps backdrop blur`);
          }
        }
      }
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: touch content scrolls with the document and safe-area space scrolls away`, async () => {
    const browser = await launcher.launch();
    try {
      const page = await browser.newPage({
        viewport: { width: 390, height: 844 },
        hasTouch: true,
        isMobile: true,
        reducedMotion: 'reduce',
      });
      await page.setContent(`<!doctype html><html data-theme="light" data-palette="dailan"><head>
        ${html.match(/<meta name="viewport"[^>]+>/)[0]}<style>${styles}</style>
        </head><body><div id="root"><div class="forge narrow"><div class="fg-left"><div class="fg-desk">
          <div class="fg-top"><button>Menu</button></div>
          <main class="fg-view" style="min-height:1800px"><input aria-label="Search"></main>
        </div></div></div></div></body></html>`);
      assert.equal(await page.evaluate(() => matchMedia('(hover: none) and (pointer: coarse)').matches), true);
      for (const viewport of [
        { width: 390, height: 844 },
        { width: 844, height: 390 },
        { width: 390, height: 480 },
      ]) {
        await page.setViewportSize(viewport);
        // Desktop WebKit has no notch. Inject nonzero inset tokens to verify geometry, not UIKit.
        await page.evaluate(({ width }) => {
          window.scrollTo(0, 0);
          const root = document.documentElement;
          root.style.setProperty('--safe-top', '59px');
          root.style.setProperty('--safe-bottom', '34px');
          root.style.setProperty('--safe-left', width > 820 ? '59px' : '0px');
          root.style.setProperty('--safe-right', width > 820 ? '59px' : '0px');
          document.querySelector('.forge').classList.toggle('narrow', width <= 820);
        }, viewport);
        // Reduced-motion transitions still take 0.01ms. Wait for the injected inset
        // to be painted before asserting geometry, especially on fast Chromium runs.
        await page.waitForFunction(() => getComputedStyle(document.querySelector('.forge')).paddingTop === '59px');
        assert.equal(await page.locator('.fg-top').evaluate(el => el.getBoundingClientRect().top), 59);
        const desk = await page.locator('.fg-desk').evaluate(el => ({
          overflow: getComputedStyle(el).overflowY,
          background: getComputedStyle(el).backgroundColor,
          scroll: el.scrollTop,
        }));
        assert.deepEqual(desk, { overflow: 'visible', background: 'rgba(0, 0, 0, 0)', scroll: 0 });
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await page.evaluate(() => window.scrollTo(0, 300));
        assert.equal(await page.evaluate(() => window.scrollY), 300);
        assert.equal(await page.locator('.fg-desk').evaluate(el => el.scrollTop), 0);
        assert.equal(await page.locator('.fg-top').evaluate(el => el.getBoundingClientRect().top), -241);
        assert.deepEqual(
          await page.evaluate(() => {
            const edges = [];
            for (let el = document.elementFromPoint(innerWidth / 2, 4); el; el = el.parentElement) {
              if (['fixed', 'sticky'].includes(getComputedStyle(el).position)) edges.push(el.className);
            }
            return edges;
          }),
          [],
          'no page-painted fixed color strip covers the top edge',
        );
      }

      await page.evaluate(() => {
        window.scrollTo(0, 0);
        document.querySelector('.fg-view').style.minHeight = '0';
      });
      await page.waitForFunction(() => getComputedStyle(document.querySelector('.fg-view')).minHeight === '0px');
      assert.equal(
        await page.evaluate(() => document.scrollingElement.scrollHeight),
        480,
        'short pages still fill the screen without a phantom outer scrollbar',
      );
      await page.locator('.forge').evaluate(el => {
        el.insertAdjacentHTML('beforeend', '<aside class="fg-rail"><header class="fg-rsh">Artifacts</header></aside>');
      });
      assert.equal(await page.locator('.fg-rsh').evaluate(el => el.getBoundingClientRect().top), 59);
      assert.equal(await page.locator('html').evaluate(el => getComputedStyle(el).overflowY), 'hidden');
      await page.locator('.fg-rail').evaluate(el => el.remove());
      assert.equal(await page.locator('html').evaluate(el => getComputedStyle(el).overflowY), 'auto');
      await page.evaluate(() => {
        document.body.insertAdjacentHTML(
          'beforeend',
          `<div class="dialog-layer modal">
          <div class="dialog-surface confirm-card" style="height:1000px">Confirmation</div></div>`,
        );
      });
      const dialog = await page.locator('.dialog-surface').boundingBox();
      assert.ok(dialog.y >= 59 && dialog.y + dialog.height <= 480 - 34, 'dialog controls stay in the safe area');
      assert.equal(await page.locator('html').evaluate(el => getComputedStyle(el).overflowY), 'hidden');
      await page.locator('.dialog-layer').evaluate(el => el.remove());
      assert.equal(await page.locator('html').evaluate(el => getComputedStyle(el).overflowY), 'auto');

      await page.locator('.fg-desk').evaluate(el => {
        el.classList.add('is-topo');
        el.querySelector('.fg-view').outerHTML = '<main class="fg-topo">Topology</main>';
      });
      assert.equal(await page.locator('.forge').evaluate(el => getComputedStyle(el).position), 'absolute');
      assert.equal(await page.locator('.fg-desk').evaluate(el => getComputedStyle(el).overflowY), 'hidden');
      assert.equal(await page.evaluate(() => document.scrollingElement.scrollHeight), 480);
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: the full-screen shell resizes and scrolls without fixed top-edge containers`, async () => {
    const browser = await launcher.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 390, height: 844 }, reducedMotion: 'reduce' });
      await page.setContent(`<!doctype html><html data-theme="light" data-palette="dailan">
        <head><style>${styles}</style></head><body><div id="root"><div id="stage"></div></div></body></html>`);
      const edge = () =>
        page.evaluate(() => {
          const result = [];
          for (let element = document.elementFromPoint(innerWidth / 2, 4); element; element = element.parentElement) {
            if (['fixed', 'sticky'].includes(getComputedStyle(element).position))
              result.push(element.className || element.id);
          }
          return result;
        });
      assert.equal(await page.locator('#stage').evaluate(element => getComputedStyle(element).position), 'absolute');
      assert.deepEqual(await edge(), [], 'startup must not seed the fixed-container color cache');
      await page.locator('#root').evaluate(root => {
        root.innerHTML = `<div class="forge"><div class="fg-left"><div class="fg-desk">
          <div class="fg-top"><button>Menu</button></div>
          <main class="fg-view" style="min-height:1800px">Content</main>
        </div></div></div>`;
      });
      for (const viewport of [
        { width: 390, height: 844 },
        { width: 844, height: 390 },
        { width: 1280, height: 800 },
        { width: 390, height: 480 },
      ]) {
        await page.setViewportSize(viewport);
        const bounds = await page.locator('.forge').boundingBox();
        assert.deepEqual(bounds, { x: 0, y: 0, ...viewport });
        assert.equal(await page.locator('.forge').evaluate(element => getComputedStyle(element).position), 'absolute');
        assert.deepEqual(await edge(), []);
        await page.locator('.fg-desk').evaluate(element => {
          element.scrollTop = 300;
        });
        assert.equal(await page.locator('.fg-top').evaluate(element => element.getBoundingClientRect().top), -300);
        assert.equal(await page.evaluate(() => window.scrollY), 0, 'only the workspace scrolls');
        assert.deepEqual(await edge(), [], 'scrolling must not expose a full-screen fixed ancestor');
        await page.locator('.fg-desk').evaluate(element => {
          element.scrollTop = 0;
        });
      }
    } finally {
      await browser.close();
    }
  });

  test(`${engine}: browser appearance follows the opaque header surface in every theme and palette`, async () => {
    const browser = await launcher.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 390, height: 844 }, reducedMotion: 'reduce' });
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      // Use the real early preference restore and production CSS, without business API traffic.
      await page.route('http://appearance.test/', route =>
        route.fulfill({
          contentType: 'text/html',
          body: `<!doctype html><html><head><meta name="color-scheme" content="dark"><meta name="theme-color"><style>${styles}</style><script>${bootstrap}</script></head><body><header class="fg-top">Brocade</header><button id="appearance">Change appearance</button></body></html>`,
        }),
      );
      await page.addInitScript(() => {
        localStorage.setItem('brocade-console:theme', 'light');
        localStorage.setItem('brocade-console:palette', 'jinzi');
      });
      await page.goto('http://appearance.test/');
      // Even before the application JS, the page canvas has the saved header color.
      assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
      assert.equal(await page.locator('html').getAttribute('data-palette'), 'jinzi');
      assert.equal(
        await page.locator('html').evaluate(root => getComputedStyle(root).backgroundColor),
        await page.locator('.fg-top').evaluate(header => getComputedStyle(header).backgroundColor),
      );
      await page.addScriptTag({
        type: 'module',
        content: `${appearance}
          window.stopAppearance = observeBrowserAppearance();
          document.querySelector('#appearance').onclick = () => {
            Object.assign(document.documentElement.dataset, window.nextAppearance);
          };`,
      });
      await page.waitForFunction(() => typeof window.stopAppearance === 'function');
      const check = async theme => {
        await page.waitForFunction(
          () =>
            document.querySelector('meta[name="theme-color"]').content ===
              getComputedStyle(document.documentElement).getPropertyValue('--surface').trim() &&
            document.querySelector('meta[name="color-scheme"]').content === document.documentElement.dataset.theme,
        );
        const state = await page.evaluate(() => ({
          scheme: document.querySelector('meta[name="color-scheme"]').content,
          color: document.querySelector('meta[name="theme-color"]').content,
          root: getComputedStyle(document.documentElement).backgroundColor,
          body: getComputedStyle(document.body).backgroundColor,
          header: getComputedStyle(document.querySelector('.fg-top')).backgroundColor,
        }));
        assert.equal(state.scheme, theme);
        assert.match(state.color, /^#[\da-f]{6}$/i, 'browser chrome receives the opaque destination surface');
        const rgb = `rgb(${state.color
          .slice(1)
          .match(/../g)
          .map(value => parseInt(value, 16))
          .join(', ')})`;
        // The reduced-motion CSS still has a 0.01ms transition. The meta must select the
        // destination immediately; independently wait for the painted surfaces to arrive.
        await page.waitForFunction(
          expected =>
            [document.documentElement, document.body, document.querySelector('.fg-top')].every(
              element => getComputedStyle(element).backgroundColor === expected,
            ),
          rgb,
        );
        assert.notEqual(state.color, '#ffffff', 'light palettes are tinted, not pure white');
        return state.color;
      };
      await check('light');
      for (const theme of ['dark', 'light']) {
        const colors = [];
        for (const palette of palettes) {
          await page.evaluate(
            ({ theme, palette }) => {
              window.nextAppearance = { theme, palette };
            },
            { theme, palette },
          );
          await page.locator('#appearance').click();
          colors.push(await check(theme));
        }
        if (theme === 'light') assert.equal(new Set(colors).size, 5);
      }
      await page.evaluate(() => window.stopAppearance());
      const previous = await page.locator('meta[name="theme-color"]').getAttribute('content');
      await page.evaluate(() => {
        document.documentElement.dataset.theme = 'dark';
      });
      assert.equal(
        await page.locator('meta[name="theme-color"]').getAttribute('content'),
        previous,
        'HMR cleanup disconnects synchronization',
      );
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
    }
  });
}
