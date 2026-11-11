// Production-component browser acceptance. Screenshots and report stay outside the repository.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from '@playwright/test';

const root = fileURLToPath(new URL('..', import.meta.url));
const output = process.env.BROCADE_TRAFFIC_SCREENSHOTS ?? '/tmp/brocade-node-traffic-acceptance';
await mkdir(output, { recursive: true });
const server = await createServer({
  configFile: false,
  define: { __BROCADE_CVM__: process.env.BROCADE_PRODUCT_FLAVOR !== 'base' },
  root,
  plugins: [react()],
  server: { host: '127.0.0.1', port: 0 },
});
await server.listen();
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
const browser = await chromium.launch();
const results = [];
try {
  for (const width of [1280, 1024, 760, 390, 320]) {
    for (const theme of ['dark', 'light']) {
      const page = await browser.newPage({
        viewport: { width, height: 1000 },
        reducedMotion: 'reduce',
        hasTouch: width <= 390,
      });
      await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      for (const scene of ['typical', 'many', 'solo', 'gaps', 'history', 'quiet', 'sites-off']) {
        errors.length = 0;
        await page.goto(`${base}/tests/fixtures/node-traffic-acceptance.html?scene=${scene}&theme=${theme}&run=0`);
        const opener = page.getByRole('button', { name: '香港测试机器', exact: true });
        await opener.click();
        await page.getByRole('dialog', { name: '香港测试机器 · 用量', exact: true }).waitFor();
        const header = page.locator('.node-usage-header');
        const close = header.getByRole('button', { name: '关闭机器用量' });
        // Read the whole row together: incoming data can resize the centered modal between calls.
        const { rangeBox, closeBox, flagBox } = await header.evaluate(el => {
          const box = selector => {
            const { x, y, width, height } = el.querySelector(selector).getBoundingClientRect();
            return { x, y, width, height };
          };
          return {
            rangeBox: box('[aria-label="流量统计范围"]'),
            closeBox: box('[aria-label="关闭机器用量"]'),
            flagBox: box('.geo-flag.square'),
          };
        });
        assert.ok(rangeBox.x + rangeBox.width <= closeBox.x, 'statistics range is left of close');
        assert.equal(rangeBox.y + rangeBox.height / 2, closeBox.y + closeBox.height / 2, 'header actions share a row');
        assert.equal(flagBox.width, flagBox.height, 'machine flag is square');
        if (width <= 820)
          assert.ok(closeBox.width >= 44 && closeBox.height >= 44, 'touch close target is at least 44px');
        assert.equal(await close.innerText(), '', 'close is icon-only');
        assert.equal(await header.locator('p').count(), 0, 'no machine subtitle');
        assert.equal(await page.locator('.node-usage-body [aria-label="流量统计范围"]').count(), 0);
        await page.locator('.nti-body[aria-busy="false"]').waitFor();
        await page.locator('.site-usage-body[aria-busy="false"]').waitFor();
        await page.getByText('实时 · 每秒更新', { exact: true }).waitFor();
        const live = await page.locator('.rut-node').elementHandle();
        await page.locator('.rut-hour .ndtp-bars canvas').waitFor();
        await page.getByText('当天 · 每 5 分钟 · 已观测窗口均值', { exact: true }).waitFor();
        assert.equal(
          await page.locator('.rut-access-grid, .rut-node .rut-main').count(),
          0,
          'machine matrix and its controls are removed',
        );

        if (scene === 'history') {
          await page.getByRole('button', { name: '当月', exact: true }).click();
          await page.locator('.ntf-label.right, .nti-key').filter({ hasText: '未按接入链记录' }).waitFor();
        }
        assert.deepEqual(
          await page.locator('.nt-details > section').evaluateAll(els => els.map(el => el.getAttribute('aria-label'))),
          ['实时流量', '流入统计', '流出统计'],
        );
        assert.equal(
          await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth),
          false,
          `${scene}/${width}/${theme}: horizontal overflow`,
        );
        const flowWidth = await page.locator('.nti-card').evaluate(el => el.getBoundingClientRect().width);
        const flow = page.locator('.ntf-svg');
        const bars = page.locator('.nti-bars');
        if (scene !== 'quiet') {
          await (flowWidth >= 720 ? flow : bars).waitFor();
          assert.equal(await flow.count(), flowWidth >= 720 ? 1 : 0);
          if (flowWidth >= 720) {
            assert.equal(
              await page.locator('.ntf-label.left .user-generated-avatar').count(),
              0,
              'flow users have no avatars',
            );
          }
        }
        // Exercise the same production interactions at both desktop and phone widths.
        if (scene === 'typical') {
          if (flowWidth >= 720) {
            const user = page.locator('.ntf-label.left').first();
            await user.focus();
            assert.ok((await page.locator('.ntf-link.dim').count()) > 0);
            await user.click();
            await page.locator('.nid-table').waitFor();
            await page.keyboard.press('Escape');
            await page.locator('.node-inflow-dialog').waitFor({ state: 'detached' });
            assert.equal(
              await user.evaluate(el => document.activeElement === el),
              true,
              'detail focus returns to the user',
            );
          } else {
            const legend = page.locator('.nti-key').first();
            await legend.click();
            assert.equal(await legend.getAttribute('aria-pressed'), 'true');
            const user = page.locator('.nti-bar-btn').first();
            await user.click();
            await page.locator('.nid-table').waitFor();
            await page.keyboard.press('Escape');
            await page.locator('.node-inflow-dialog').waitFor({ state: 'detached' });
          }

          const site = page.locator('.site-usage-key button.site-usage-row').first();
          await site.click();
          await page.locator('.node-site-users').waitFor();
          assert.ok((await page.locator('.node-site-users').innerText()).includes('本机主要用户'));
          await site.click();
          await page.getByRole('button', { name: '当周', exact: true }).click();
          await page.getByText('当周 · 每 15 分钟 · 已观测窗口均值', { exact: true }).waitFor();
          await page.locator('.site-usage-body[aria-busy="false"]').waitFor();
          assert.equal(await live.evaluate(el => el === document.querySelector('.rut-node')), true);
          await page.getByRole('button', { name: '当月', exact: true }).click();
          await page.getByText('当月 · 每 1 小时 · 已观测窗口均值', { exact: true }).waitFor();
          await page.getByRole('button', { name: '当天', exact: true }).click();
        }
        // Offscreen ECharts surfaces draw on entry. Visit each before the full-page screenshot.
        await page.locator('.site-usage').scrollIntoViewIfNeeded();
        if (scene === 'typical' || scene === 'sites-off') {
          const forwarding = page.locator('.site-usage-key button.site-usage-row').filter({ hasText: '端口转发' });
          await forwarding.waitFor();
          assert.equal(await forwarding.locator('.site-brand-icon svg').count(), 1, 'forwarding has its own icon');
          await forwarding.click();
          await page.getByLabel('端口转发的主要用户', { exact: true }).waitFor();
          await forwarding.click();
        }
        assert.ok(
          await page.locator('.node-usage-body').evaluate(el => el.scrollTop > 0 || el.scrollHeight <= el.clientHeight),
          `${scene}: dialog body scrolls`,
        );
        const surface = await page.locator('.node-usage-dialog').boundingBox();
        assert.ok(surface.y >= 0 && surface.y + surface.height <= 1000, 'dialog fits the viewport');
        assert.equal(await page.getByRole('button', { name: '关闭机器用量' }).isVisible(), true);
        await page.waitForTimeout(180);
        await page.locator('.rut-node').scrollIntoViewIfNeeded();
        await page.waitForTimeout(180);
        await page.locator('.node-usage-body').evaluate(el => (el.scrollTop = 0));
        assert.deepEqual(errors, [], `${scene}/${width}/${theme}: browser errors`);
        const file = `${scene}-${width}-${theme}.png`;
        await page.screenshot({ path: `${output}/${file}`, fullPage: true });
        if (scene === 'typical' && width === 1280 && theme === 'light')
          await page.locator('.nti-card').screenshot({ path: `${output}/inflow-restored.png` });
        await page.keyboard.press('Escape');
        await page.getByRole('dialog').waitFor({ state: 'detached' });
        assert.equal(await opener.evaluate(el => document.activeElement === el), true, 'focus returns to the machine');
        results.push({
          scene,
          width,
          theme,
          inflow: scene === 'quiet' ? 'empty' : flowWidth >= 720 ? 'flow' : 'bars',
          file,
        });
      }
      await page.close();
      console.log(`Passed seven scenes: ${width}px ${theme}`);
    }
  }
  await writeFile(`${output}/report.json`, JSON.stringify(results, null, 2));
  console.log(`Passed ${results.length} browser cases; screenshots: ${output}`);
} finally {
  await browser.close();
  await server.close();
}
