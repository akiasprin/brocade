import { expect, test, type Locator } from '@playwright/test';

const cases = [
  { width: 1024, machines: 29, theme: 'dark' },
  { width: 1280, machines: 28, theme: 'light' },
  { width: 1280, machines: 29, theme: 'light' },
  { width: 2300, machines: 29, theme: 'dark' },
  { width: 1280, machines: 48, theme: 'dark' },
  { width: 900, machines: 48, theme: 'dark' },
  { width: 390, machines: 29, theme: 'light' },
] as const;

for (const { width, machines, theme } of cases) {
  test(`${machines} machines retain columns when the container fits at ${width}px`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-28T08:00:00Z'));
    await page.setViewportSize({ width, height: 900 });
    await page.goto(
      `/tests/fixtures/fleet-traffic-preview.html?scene=heavy&machines=${machines}&theme=${theme}&w=${width}&nav=0&run=0`,
    );
    const ledger = page.getByRole('region', { name: '机队当前速率' });
    const board = page.getByRole('region', { name: '用户与机器的当前速率', includeHidden: true });
    await expect(ledger).toBeVisible();
    await expect(board.locator('thead th[data-node]')).toHaveCount(machines);
    await expect(board).not.toHaveAttribute('aria-busy');
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`fleet-${machines}-${width}-${theme}.png`);

    expect(
      await page.evaluate(() => Math.max(document.documentElement.scrollWidth, document.body.scrollWidth) > innerWidth),
    ).toBe(false);
    if (width <= 680) {
      await expect(board).toBeHidden();
      await expect(page.getByRole('region', { name: '按用户的当前速率' })).toBeVisible();
      return;
    }

    await expect(board).toBeVisible();
    await expectLabelSpacing(board);
    const left = (await ledger.boundingBox())!;
    const right = (await board.boundingBox())!;
    if (width > 900) {
      expect(right.x).toBeGreaterThanOrEqual(left.x + left.width);
      expect(right.y).toBeCloseTo(left.y, 0);
      expect(left.width).toBeCloseTo(300, 0);
    } else {
      expect(right.y).toBeGreaterThanOrEqual(left.y + left.height);
    }
    const scroller = board.locator('.flt-scroll');
    const overflow = await scroller.evaluate(element => element.scrollWidth - element.clientWidth);
    if (width < 1600) {
      expect(overflow).toBeGreaterThan(0);
      await scroller.evaluate(element => (element.scrollLeft = element.scrollWidth));
      const last = await board.locator('tfoot .flt-sum').boundingBox();
      const bounds = (await scroller.boundingBox())!;
      expect(last!.x + last!.width).toBeLessThanOrEqual(bounds.x + bounds.width + 1);
    } else {
      expect(overflow).toBeLessThanOrEqual(1);
    }
  });
}

test('a narrow paper stacks inside a wide desktop viewport', async ({ page }) => {
  await page.setViewportSize({ width: 2300, height: 900 });
  await page.goto('/tests/fixtures/fleet-traffic-preview.html?scene=heavy&machines=29&w=900&nav=0&run=0');
  const ledger = page.getByRole('region', { name: '机队当前速率' });
  const board = page.getByRole('region', { name: '用户与机器的当前速率' });
  await expect(board).toBeVisible();
  const left = (await ledger.boundingBox())!;
  const right = (await board.boundingBox())!;
  expect(right.y).toBeGreaterThanOrEqual(left.y + left.height);
});

async function expectLabelSpacing(board: Locator) {
  const labels = await board.locator('.flt-mlabel').evaluateAll(elements =>
    elements.map(element => {
      const head = element.closest('th')!.getBoundingClientRect();
      const label = element.getBoundingClientRect();
      return {
        center: head.left + head.width / 2,
        height: parseFloat(getComputedStyle(element).lineHeight),
        top: label.top,
        tableTop: element.closest('table')!.getBoundingClientRect().top,
      };
    }),
  );
  expect(labels.length).toBeGreaterThan(6);
  for (const [index, label] of labels.entries()) {
    expect(label.top).toBeGreaterThanOrEqual(label.tableTop);
    if (index === 0) continue;
    // Compare the distance perpendicular to the 45° labels, including their line box.
    const distance = (label.center - labels[index - 1].center) / Math.SQRT2;
    expect(distance - label.height).toBeGreaterThanOrEqual(3);
  }
}

test('monthly ledger sizing and matrix spacing stay fixed when the paper resizes', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-28T08:00:00Z'));
  await page.setViewportSize({ width: 2300, height: 1100 });
  await page.goto('/tests/fixtures/fleet-traffic-preview.html?scene=prod&w=2300&nav=0&run=0');
  const ledger = page.getByRole('region', { name: '机队当前速率' });
  const board = page.getByRole('region', { name: '用户与机器的当前速率' });
  await expect(ledger).toBeVisible();
  await expect(board.locator('thead th[data-node]')).toHaveCount(24);
  await page.evaluate(() => document.fonts.ready);

  const readings = [];
  const matrices = [];
  for (const width of [2300, 1280, 1024, 2300]) {
    // Keep the desktop viewport fixed to also exercise a narrower panel inside it.
    await page.locator('.fg-view').evaluate((element, value) => {
      (element as HTMLElement).style.width = `${value}px`;
    }, width);
    // Screenshot comparison waits for the resized layout to settle before measuring it.
    await expect(page.locator('.fg-view')).toHaveScreenshot(`fleet-resize-${width}.png`);
    await expectLabelSpacing(board);
    matrices.push(await matrixDimensions(board));
    readings.push(
      await ledger.evaluate(element => ({
        width: element.getBoundingClientRect().width,
        padding: getComputedStyle(element).padding,
        fonts: [
          '.usage-hero-value',
          '.usage-hero-label',
          '.rut-hero-sub',
          '.rut-status',
          '.usage-io dd',
          '.usage-facts dt',
          '.usage-facts dd',
        ].map(selector => parseFloat(getComputedStyle(element.querySelector(selector)!).fontSize)),
      })),
    );
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  }
  await page.getByRole('tab', { name: '汇总' }).click();
  const monthly = page.getByRole('region', { name: /用量概览$/ });
  await expect(monthly).toBeVisible();
  const expected = await monthly.evaluate(element => ({
    width: element.getBoundingClientRect().width,
    padding: getComputedStyle(element).padding,
    fonts: ['.usage-hero-value', '.usage-hero-label', '.usage-io dd', '.usage-facts dt', '.usage-facts dd'].map(
      selector => getComputedStyle(element.querySelector(selector)!).fontSize,
    ),
  }));
  for (const reading of readings) {
    expect(reading.width).toBeCloseTo(expected.width, 1);
    expect(reading.padding).toBe(expected.padding);
    expect(reading.fonts[0]).toBe(parseFloat(expected.fonts[0]));
    expect(reading.fonts[1]).toBe(parseFloat(expected.fonts[1]));
    expect(reading.fonts.slice(4)).toEqual(expected.fonts.slice(2).map(parseFloat));
  }
  expect(readings[3]).toEqual(readings[0]);
  expect(matrices).toHaveLength(4);
  for (const matrix of matrices) {
    expect(matrix).toEqual(matrices[0]);
    expect(matrix.cell).toEqual({ width: 24, height: 24 });
    expect(matrix.rowHeight).toBe(30);
    expect(matrix.columnWidths.every(width => width === 30 || width === 38)).toBe(true);
  }
});

async function matrixDimensions(board: Locator) {
  return board.locator('.flt-grid').evaluate(element => {
    const cell = element.querySelector('.flt-cell')!.getBoundingClientRect();
    return {
      width: element.getBoundingClientRect().width,
      cell: { width: cell.width, height: cell.height },
      rowHeight: element.querySelector('tbody tr')!.getBoundingClientRect().height,
      columnWidths: [...element.querySelectorAll('thead th[data-node]')].map(
        head => head.getBoundingClientRect().width,
      ),
      fonts: [getComputedStyle(element).fontSize, getComputedStyle(element.querySelector('.flt-mlabel')!).fontSize],
    };
  });
}

test('few machine columns keep horizontal labels on a large paper', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-10-28T08:00:00Z'));
  await page.setViewportSize({ width: 2300, height: 900 });
  await page.goto('/tests/fixtures/fleet-traffic-preview.html?scene=few&w=2300&nav=0&run=0');
  const board = page.getByRole('region', { name: '用户与机器的当前速率' });
  await expect(board.locator('thead th[data-node]')).toHaveCount(3);
  for (const label of await board.locator('.flt-mlabel').all()) {
    expect(await label.evaluate(element => getComputedStyle(element).transform)).toBe('none');
  }
  await expect(page).toHaveScreenshot('fleet-few-2300.png');
  const wide = await matrixDimensions(board);
  await page.setViewportSize({ width: 1024, height: 900 });
  await expect(page).toHaveScreenshot('fleet-few-1024.png');
  expect(await matrixDimensions(board)).toEqual(wide);
});
