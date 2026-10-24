import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, before, test } from 'node:test';
import { chromium } from '@playwright/test';
import react from '@vitejs/plugin-react';
import { createServer } from 'vite';

// 用量页在大读数、长名称和大量机器同时出现时不得撑宽页面。heavy 场景：Gbit/s 速率、每月数百 TiB、
// 运营商全称的机器名、邮箱长度的最高用户，以及 48 台机器（矩阵必须横向滚动）。

let server;
let browser;
let base;
let cacheDirectory;

before(async () => {
  cacheDirectory = await mkdtemp(join(tmpdir(), 'brocade-usage-layout-'));
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL('..', import.meta.url)),
    plugins: [react()],
    cacheDir: join(cacheDirectory, 'node_modules/.vite'),
    server: { host: '127.0.0.1', port: 0 },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}/tests/fixtures/fleet-traffic-preview.html`;
  browser = await chromium.launch();
});

after(async () => {
  await browser?.close();
  await server?.close();
  if (cacheDirectory) await rm(cacheDirectory, { recursive: true, force: true });
});

async function usagePage(
  t,
  width,
  { reducedMotion = 'reduce', delay = 0, run = 0, scene = 'heavy', theme = 'dark' } = {},
) {
  const page = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('requestfailed', request => {
    if (['document', 'script', 'stylesheet'].includes(request.resourceType())) {
      errors.push(`${request.failure()?.errorText}: ${request.url()}`);
    }
  });
  t.after(async () => {
    const failures = [...errors];
    await page.close();
    assert.deepEqual(failures, [], 'no browser or React exceptions');
  });
  await page.addInitScript(() => {
    window.usageEntrances = [];
    document.addEventListener('animationstart', event => {
      if (event.animationName !== 'motion-surface-mount') return;
      const element = event.target;
      if (element.matches('.usage-tab-body > .usage-cockpit, .usage-tab-body > .flt-panel > .flt-cockpit')) {
        window.usageEntrances.push(element.matches('.flt-cockpit') ? 'realtime' : 'monthly');
      }
    });
  });
  await page.goto(`${base}?scene=${scene}&theme=${theme}&run=${run}&nav=0&delay=${delay}`);
  return page;
}

/** The base styles give html and body `overflow: hidden` (the shell scrolls inside), so content that
 * escapes sideways widens the body's scroll area even when the document itself stays put. */
const pageScrollsSideways = page =>
  page.evaluate(() => Math.max(document.documentElement.scrollWidth, document.body.scrollWidth) > innerWidth);

/** Every key–value pair of a ledger: whether the value stays in its own column, is cut off, sits
 * right under its label, and which unbreakable segments were split across lines anyway. */
function readFacts(ledger) {
  return ledger.evaluate(section =>
    [...section.querySelectorAll('.usage-facts > div')].map(item => {
      const box = item.getBoundingClientRect();
      const label = item.querySelector('dt');
      const value = item.querySelector('dd');
      const valueBox = value.getBoundingClientRect();
      const segments = [...value.querySelectorAll('.usage-nobreak')];
      return {
        label: label.textContent,
        inside: valueBox.left >= box.left - 0.5 && valueBox.right <= box.right + 0.5,
        clipped: value.scrollWidth > value.clientWidth + 1,
        gap: Math.round(valueBox.top - label.getBoundingClientRect().bottom),
        split: segments.filter(segment => segment.getClientRects().length > 1).map(segment => segment.textContent),
        segmentTops: segments.map(segment => Math.round(segment.getBoundingClientRect().top - valueBox.top)),
      };
    }),
  );
}

function assertFactsFit(facts, where) {
  assert.ok(facts.length >= 4, `${where}: ledger facts are rendered`);
  for (const fact of facts) {
    assert.ok(fact.inside, `${where}: ${fact.label} stays inside its column`);
    assert.equal(fact.clipped, false, `${where}: ${fact.label} is not cut off`);
    assert.equal(fact.gap, 4, `${where}: ${fact.label} sits right under its label`);
    assert.deepEqual(fact.split, [], `${where}: ${fact.label} keeps each reading on one line`);
  }
}

async function assertCompositionOnLeft(ledger) {
  const layout = await ledger.evaluate(element => {
    const hero = element.querySelector('.user-usage-hero, .usage-hero').getBoundingClientRect();
    const composition = element.querySelector('.usage-compose').getBoundingClientRect();
    const facts = element.querySelector('.usage-facts').getBoundingClientRect();
    const columns = getComputedStyle(element).display === 'grid';
    return {
      sameLeft: Math.abs(hero.left - composition.left) < 1,
      below: composition.top >= hero.bottom,
      factsAside: facts.left >= composition.right,
      factsBelow: facts.top >= composition.bottom,
      columns,
    };
  });
  assert.equal(layout.sameLeft, true, 'total, direction readings and split bar share the left edge');
  assert.equal(layout.below, true, 'composition follows the total');
  assert.equal(layout.columns ? layout.factsAside : layout.factsBelow, true);
}

for (const width of [320, 390]) {
  test(`realtime ledger wraps long names and paired readings at ${width}px`, async t => {
    const page = await usagePage(t, width);
    const ledger = page.getByRole('region', { name: '机队当前速率' });
    await ledger.getByText('最忙机器', { exact: true }).waitFor();
    const facts = await readFacts(ledger);
    assertFactsFit(facts, `${width}px realtime`);
    const busiest = facts.find(fact => fact.label === '最忙机器');
    assert.ok(busiest.segmentTops[0] > 4, 'the busiest machine reading moves below its long name');
    assert.equal(await pageScrollsSideways(page), false);
  });

  test(`monthly ledger keeps TiB readings inside the page at ${width}px`, async t => {
    const page = await usagePage(t, width);
    await page.getByRole('tab', { name: '汇总' }).click();
    const ledger = page.getByRole('region', { name: /用量概览$/ });
    await ledger.getByRole('button', { name: /^在每日流量中查看/ }).waitFor();
    const facts = await readFacts(ledger);
    assertFactsFit(facts, `${width}px monthly`);
    const peak = facts.find(fact => fact.label === '峰值日');
    assert.equal(peak.segmentTops.length, 2, 'peak date and reading are separate segments');
    // At the narrowest width the line breaks after the separator, not inside the date or reading.
    if (width === 320) assert.ok(peak.segmentTops[1] > peak.segmentTops[0], 'peak reading wraps as a whole');
    assert.equal(await pageScrollsSideways(page), false);
  });
}

for (const width of [760, 1024]) {
  test(`a matrix wider than the board scrolls inside it at ${width}px`, async t => {
    const page = await usagePage(t, width);
    const scroller = page.locator('.flt-scroll');
    await scroller.locator('tbody tr').first().waitFor();
    const overflow = await scroller.evaluate(element => element.scrollWidth - element.clientWidth);
    assert.ok(overflow > 200, 'the 48-machine matrix needs horizontal scrolling');
    // Visually hidden cell text is absolutely positioned; it must be clipped by the scroller too.
    assert.equal(await pageScrollsSideways(page), false);
  });
}

for (const theme of ['light', 'dark']) {
  for (const width of [320, 390, 640, 680]) {
    for (const scene of ['public', 'heavy']) {
      test(`${scene} flow rows stay inside the paper at ${width}px in ${theme} mode`, async t => {
        const page = await usagePage(t, width, { scene, theme });
        const list = page.getByRole('region', { name: '按用户的当前速率' });
        await list.waitFor();
        const rows = await list.locator('.flt-flow').evaluateAll(elements =>
          elements.map(element => {
            const box = element.getBoundingClientRect();
            const listBox = element.closest('.flt-list').getBoundingClientRect();
            const reading = element.querySelector('.flt-flow-total').getBoundingClientRect();
            return {
              tag: element.tagName,
              inside: box.left >= listBox.left - 0.5 && box.right <= listBox.right + 0.5,
              readingInside: reading.left >= box.left && reading.right <= box.right - 15,
            };
          }),
        );
        assert.ok(rows.length > 0, 'the flow list contains measured users');
        for (const row of rows) {
          assert.equal(row.tag, scene === 'public' ? 'DIV' : 'BUTTON');
          assert.equal(row.inside, true, 'row width includes its padding in both access modes');
          assert.equal(row.readingInside, true, 'the rate retains its right inset');
        }
        assert.equal(await pageScrollsSideways(page), false);
      });
    }
  }
}

for (const scene of ['prod', 'heavy']) {
  for (const width of [640, 760, 1024, 1280]) {
    test(`${scene} realtime summary keeps the composition under the total at ${width}px`, async t => {
      const page = await usagePage(t, width, { scene });
      const ledger = page.getByRole('region', { name: '机队当前速率' });
      await ledger.waitFor();
      await assertCompositionOnLeft(ledger);
      assert.equal(await pageScrollsSideways(page), false);
    });
  }
}

for (const theme of ['light', 'dark']) {
  for (const width of [320, 390, 640, 760, 900, 1024]) {
    test(`monthly summary keeps composition under the total at ${width}px in ${theme} mode`, async t => {
      const page = await usagePage(t, width, { theme });
      await page.getByRole('tab', { name: '汇总' }).click();
      const ledger = page.getByRole('region', { name: /用量概览$/ });
      await ledger.waitFor();
      await assertCompositionOnLeft(ledger);
      assert.equal(await pageScrollsSideways(page), false);
    });
  }
}

test('delayed realtime content and both tab directions use the shared surface entrance', async t => {
  const page = await usagePage(t, 1280, { reducedMotion: 'no-preference', delay: 400 });
  const paper = await page.locator('.nd-paper').elementHandle();
  assert.ok(paper, 'the paper is mounted before data arrives');
  await page.waitForFunction(() => window.usageEntrances.includes('realtime'));
  assert.equal(
    await page.locator('.nd-paper').evaluate(element => getComputedStyle(element).animationName),
    'motion-surface-mount',
  );
  const realtime = page.locator('.usage-tab-body > .flt-panel > .flt-cockpit');
  const entrance = await realtime.evaluate(element => {
    const style = getComputedStyle(element);
    return { name: style.animationName, duration: style.animationDuration, easing: style.animationTimingFunction };
  });
  assert.deepEqual(entrance, {
    name: 'motion-surface-mount',
    duration: '0.24s',
    easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
  });

  await page.getByRole('tab', { name: '汇总' }).click();
  await page.waitForFunction(() => window.usageEntrances.includes('monthly'));
  assert.equal(await page.getByRole('tabpanel', { name: '汇总' }).count(), 1);

  await page.getByRole('tab', { name: '汇总' }).press('ArrowLeft');
  await page.waitForFunction(() => window.usageEntrances.filter(kind => kind === 'realtime').length === 2);
  assert.equal(await page.getByRole('tabpanel', { name: '实时' }).count(), 1);
  assert.equal(
    await paper.evaluate(element => element === document.querySelector('.nd-paper')),
    true,
    'tab changes retain the outer paper',
  );
  assert.deepEqual(await page.evaluate(() => window.usageEntrances), ['realtime', 'monthly', 'realtime']);
});

test('monthly refreshes and day selection do not replay the content entrance', async t => {
  const page = await usagePage(t, 1280, { reducedMotion: 'no-preference' });
  await page.waitForFunction(() => window.usageEntrances.includes('realtime'));
  await page.getByRole('tab', { name: '汇总' }).click();
  await page.waitForFunction(() => window.usageEntrances.includes('monthly'));
  const cockpit = await page.locator('.usage-cockpit').elementHandle();
  await page.getByRole('button', { name: '上月', exact: true }).click();
  await page.getByRole('region', { name: '2026 年 9 月用量概览' }).waitFor();
  await page.getByRole('button', { name: /^在每日流量中查看/ }).click();
  await page.locator('.usage-day-detail').waitFor();
  assert.equal(await cockpit.evaluate(element => element === document.querySelector('.usage-cockpit')), true);
  assert.deepEqual(await page.evaluate(() => window.usageEntrances), ['realtime', 'monthly']);
});

test('realtime readings update without replaying the content entrance', async t => {
  const page = await usagePage(t, 1280, { reducedMotion: 'no-preference', run: 1 });
  await page.waitForFunction(() => window.usageEntrances.includes('realtime'));
  const surface = await page.locator('.flt-panel').elementHandle();
  const before = await page.locator('.flt-ledger .user-usage-hero-value').textContent();
  await page.waitForFunction(
    previous => document.querySelector('.flt-ledger .user-usage-hero-value')?.textContent !== previous,
    before,
  );
  assert.equal(await surface.evaluate(element => element === document.querySelector('.flt-panel')), true);
  assert.deepEqual(await page.evaluate(() => window.usageEntrances), ['realtime']);
});

test('usage entrances honor reduced motion on a narrow screen', async t => {
  const page = await usagePage(t, 390);
  await page.locator('.usage-tab-body > .flt-panel').waitFor();
  await page.getByRole('tab', { name: '汇总' }).click();
  const cockpit = page.locator('.usage-tab-body > .usage-cockpit');
  await cockpit.waitFor();
  const duration = await cockpit.evaluate(element => parseFloat(getComputedStyle(element).animationDuration));
  assert.ok(duration <= 0.00001, 'reduced-motion content has no perceptible entrance duration');
  assert.equal(await pageScrollsSideways(page), false);
});
