import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test, type Page } from '@playwright/test';

const cases = [
  ['healthy', 1440, 'light', 86400],
  ['down', 1440, 'light', 86400],
  ['slow', 1440, 'dark', 86400],
  ['healthy', 390, 'light', 86400],
  ['down', 320, 'dark', 604800],
  ['empty', 390, 'light', 86400],
  ['all-failed', 1440, 'dark', 86400],
  ['stale', 1440, 'light', 86400],
  ['paused', 390, 'dark', 86400],
  ['healthy', 1440, 'dark', 604800],
  ['long-references', 390, 'light', 86400],
  ['no-references', 1440, 'light', 86400],
  ['draft-changed', 320, 'light', 86400],
] as const;

async function open(page: Page, scene: string, width: number, theme: string, windowSecs = 86400) {
  await page.setViewportSize({ width, height: 900 });
  await page.clock.setFixedTime(new Date('2026-11-17T02:00:00Z'));
  await page.goto(
    `/tests/fixtures/tunnel-probe-preview.html?scene=${scene}&theme=${theme}#/tunnels/custom/custom-5f2a-9c1e`,
  );
  await expect(page.locator('.tnd-head')).toBeVisible();
  expect((await page.locator('.tnd-head h1').boundingBox())!.width).toBeGreaterThanOrEqual(24);
  await expect(page.locator('.tnd-probe > header button, .tnd-subhead button')).toHaveCount(0);
  await expect(page.locator('.tnd-verdict button')).toHaveCount(0);
  await expect(page.locator('.tnd-page-actions')).toHaveCount(0);
  await expect(page.locator('.tnd-connection-actions')).toHaveCount(0);
  await expect(page.locator('.tnd-head .title-action-group button')).toHaveCount(scene === 'draft-changed' ? 3 : 2);
  await expect(page.locator('.tnd-head .title-action-primary .tnd-probe-actions')).toHaveCount(
    scene === 'error' ? 0 : 1,
  );
  if (windowSecs === 604800) await page.getByRole('button', { name: '7 天', exact: true }).click();
  if (scene !== 'unpublished' && scene !== 'error') {
    await page.locator('.tnd-chart').scrollIntoViewIfNeeded();
    await expect(page.locator('.tnd-chart canvas')).toBeVisible();
    await page.locator('.tnd-head').evaluate(el => {
      (document.activeElement as HTMLElement | null)?.blur();
      for (let parent = el.parentElement; parent; parent = parent.parentElement)
        parent.scrollTo({ top: 0, behavior: 'instant' });
      window.scrollTo({ top: 0, behavior: 'instant' });
    });
    await expect(page.locator('.tnd-head')).toBeInViewport({ ratio: 1 });
  }
  await waitForVisualAssets(page);
}

for (const engine of ['chromium', 'webkit'] as const) {
  const engineTest = test.extend({
    page: async ({ playwright, baseURL }, use) => {
      const browser = await playwright[engine].launch();
      try {
        const page = await browser.newPage({
          baseURL,
          reducedMotion: 'reduce',
          locale: 'zh-CN',
          timezoneId: 'Asia/Taipei',
        });
        page.on('pageerror', error => console.error('preview error:', error.message));
        await use(page);
      } finally {
        await browser.close();
      }
    },
  });
  engineTest.describe(engine, () => {
    for (const [scene, width, theme, windowSecs] of cases)
      engineTest(`comparison ${scene} ${width} ${theme} ${windowSecs}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await open(page, scene, width, theme, windowSecs);
        expect(errors).toEqual([]);
        expect(await page.locator('.fg-view').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        await expect(page).toHaveScreenshot(`${engine}-${scene}-${width}-${theme}-${windowSecs}.png`);
        if (scene === 'long-references' || scene === 'no-references') {
          await page.locator('.tnd-refs').scrollIntoViewIfNeeded();
          if (scene === 'no-references') {
            await expect(page.getByText('当前配置中尚无规则引用。')).toBeVisible();
            await expect(page.locator('.tnd-ref-source')).toHaveCount(0);
          } else {
            await expect(page.locator('.tnd-ref')).toHaveCount(2);
            await expect(page.locator('.tnd-refnode').first()).toContainText('长名称检查');
            expect(await page.locator('.tnd-refs').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
          }
          await expect(page.locator('.tnd-refs')).toHaveScreenshot(`${engine}-${scene}-references.png`);
        }
      });

    engineTest('page actions stay at the upper right and the statistics range aligns right', async ({ page }) => {
      for (const width of [1440, 390, 320]) {
        await open(page, 'healthy', width, width === 320 ? 'dark' : 'light');
        const head = (await page.locator('.tnd-head').boundingBox())!;
        const probe = (await page.locator('.tnd-probe').boundingBox())!;
        expect(probe.y - head.y - head.height).toBeLessThanOrEqual(24);
        const probeHeader = page.locator('.tnd-head');
        const run = probeHeader.getByRole('button', { name: '立即拨测', exact: true });
        await expect(run).toBeVisible();
        const headerBounds = (await probeHeader.boundingBox())!;
        const runBounds = (await run.boundingBox())!;
        expect(headerBounds.x + headerBounds.width - runBounds.x - runBounds.width).toBeGreaterThanOrEqual(0);
        expect(headerBounds.x + headerBounds.width - runBounds.x - runBounds.width).toBeLessThanOrEqual(20);
        expect(
          Math.abs(runBounds.y + runBounds.height / 2 - headerBounds.y - headerBounds.height / 2),
        ).toBeLessThanOrEqual(2);
        await expect(page.locator('.tnd-verdict > b, .tnd-verdict > .tnd-why')).toHaveCount(0);
        await expect(page.locator('.tnd-verdict').getByRole('img', { name: '正常', exact: true })).toBeVisible();
        await expect(page.locator('.tnd-when')).toBeVisible();
        const actions = page.locator('.tnd-head .title-actions');
        const identity = (await page.locator('.tnd-identity').boundingBox())!;
        const edit = (await actions.getByRole('button', { name: '编辑', exact: true }).boundingBox())!;
        const remove = (await actions.getByRole('button', { name: '删除', exact: true }).boundingBox())!;
        expect(edit.x).toBeGreaterThanOrEqual(identity.x + identity.width);
        expect(remove.x).toBeGreaterThanOrEqual(edit.x + edit.width - 1);
        expect(Math.abs(edit.y + edit.height / 2 - head.y - head.height / 2)).toBeLessThanOrEqual(2);
        expect(remove.x + remove.width).toBeLessThanOrEqual(head.x + head.width);
        expect(runBounds.x).toBeGreaterThanOrEqual(remove.x + remove.width + 6);
        const controls = (await page.locator('.tnd-window-controls').boundingBox())!;
        const range = (await page.locator('.tnd-window-controls .segsw').boundingBox())!;
        expect(controls.x + controls.width - range.x - range.width).toBeCloseTo(16, 0);
        expect(await page.locator('.tnd-head').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        await expect(page.locator('.tnd-head')).toHaveScreenshot(`${engine}-header-actions-${width}.png`);
        await actions.getByRole('button', { name: '编辑', exact: true }).click();
        await expect(page.getByRole('dialog', { name: '编辑隧道' })).toBeVisible();
        await page.getByRole('dialog', { name: '编辑隧道' }).getByRole('button', { name: '取消', exact: true }).click();
        await expect(page.getByRole('dialog', { name: '编辑隧道' })).toHaveCount(0);
        await actions.getByRole('button', { name: '删除', exact: true }).click();
        await expect(page.getByRole('dialog', { name: '删除隧道' })).toBeVisible();
        await page
          .getByRole('dialog', { name: '删除隧道' })
          .locator('header')
          .getByRole('button', { name: '关闭', exact: true })
          .click();
        await expect(page.getByRole('dialog', { name: '删除隧道' })).toHaveCount(0);
      }
    });

    engineTest(
      'references follow connection details without oversized rows, and history remains keyboard accessible',
      async ({ page }) => {
        await open(page, 'down', 320, 'dark', 604800);
        await expect(page.locator('.tnd-refrows > .tnd-ref')).toHaveCount(2);
        await page.locator('.tnd-aside').scrollIntoViewIfNeeded();
        const refs = (await page.locator('.tnd-refs').boundingBox())!;
        const connection = (await page.locator('.tnd-conn').boundingBox())!;
        expect(refs.y - connection.y - connection.height).toBeGreaterThanOrEqual(10);
        expect(refs.y - connection.y - connection.height).toBeLessThanOrEqual(14);
        const rows = await page.locator('.tnd-ref').evaluateAll(elements =>
          elements.map(el => {
            const heading = el.querySelector('.tnd-ref-heading')!.getBoundingClientRect();
            const nodes = el.querySelector('.tnd-refnodes')!.getBoundingClientRect();
            return {
              height: el.getBoundingClientRect().height,
              gap: nodes.y - heading.bottom,
              overflow: el.scrollWidth > el.clientWidth,
            };
          }),
        );
        for (const row of rows) {
          expect(row.height).toBeLessThanOrEqual(60);
          expect(row.gap).toBeGreaterThanOrEqual(2);
          expect(row.gap).toBeLessThanOrEqual(8);
          expect(row.overflow).toBe(false);
        }
        await expect(page.locator('.tnd-impact')).toContainText('可能受影响');
        await expect(page.locator('.tnd-impact')).not.toContainText('流量会失败');
        await expect(page.locator('.tnd-aside')).toHaveScreenshot(`${engine}-phone-reference-stack.png`);
        const summary = page.locator('.tnd-run-group > summary').first();
        await summary.focus();
        await page.keyboard.press('Enter');
        const detail = summary.locator('..');
        await expect(detail).toHaveAttribute('open', '');
        await expect(detail.locator('article').first()).toBeVisible();
        expect(await summary.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
        await expect(summary).toHaveScreenshot(`${engine}-phone-expanded-summary.png`);
      },
    );

    engineTest(
      'spikes can be inspected without hover and the window/filter controls operate on production components',
      async ({ page }) => {
        await open(page, 'healthy', 1440, 'light');
        const chart = page.locator('.tnd-chart');
        const bounds = (await chart.boundingBox())!;
        await page.mouse.move(bounds.x + 42 + (bounds.width - 56) * (1 - 41000 / 86400), bounds.y + 80);
        await expect(page.locator('.observe-chart-tip')).toContainText('无已完成');
        await page.mouse.move(1, 1);
        const scale = page.getByRole('button', { name: /个尖峰 · 完整纵轴/ });
        await scale.click();
        await expect(page.getByRole('button', { name: '收起纵轴' })).toHaveAttribute('aria-pressed', 'true');
        await page.getByRole('button', { name: '7 天', exact: true }).click();
        await expect(page.locator('.tnd-metrics').getByText(/7 天/)).toBeVisible();
        await page.getByRole('button', { name: '只看失败', exact: true }).click();
        await expect(page.locator('.tnd-runs')).toHaveCount(0);
        await expect(page.getByText('最近 20 次记录中没有失败。')).toBeVisible();
      },
    );

    engineTest('unpublished, running and read-error states retain their original controls', async ({ page }) => {
      await open(page, 'unpublished', 390, 'light');
      await expect(page.getByRole('button', { name: '拨测草稿', exact: true })).toBeVisible();
      await expect(page.getByText('发布后开始定时拨测')).toBeVisible();
      await open(page, 'running', 390, 'dark');
      await expect(page.getByRole('button', { name: '取消', exact: true })).toBeVisible();
      await expect(page.locator('.tnd-probe [role="status"]')).toContainText('请求落点');
      await open(page, 'error', 390, 'light');
      await expect(page.getByText(/拨测记录读取失败/)).toBeVisible();
      await expect(page.locator('.tnd-impact')).toHaveCount(0);
    });
  });
}
