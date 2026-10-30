import { expect, test, type Page } from '@playwright/test';

type Point = [number, number | null];
interface HistoryOptions {
  xAxis: { min: number; max: number }[];
  yAxis: { max: number; interval: number; axisLabel: { formatter: (value: number) => string } }[];
  series: { name: string; stack?: string; barWidth: number; data: Point[] }[];
}

async function inspect(page: Page) {
  return page.evaluate(async () => {
    const moduleUrl = '/tests/fixtures/traffic-bars-mockup-echarts.ts';
    const metadataUrl = '/tests/fixtures/traffic-bars-mockup-data.ts';
    const { getInstanceByDom } = await import(moduleUrl);
    const { historyBucketMillis } = await import(metadataUrl);
    const host = document.querySelector<HTMLElement>('.ndtp-bars .observe-plot-canvas')!;
    const chart = getInstanceByDom(host) as {
      id: string;
      getOption: () => HistoryOptions;
      convertToPixel: (finder: { xAxisIndex: number } | { yAxisIndex: number }, value: number) => number;
    };
    const value = chart.getOption();
    const xAxis = value.xAxis[0];
    const yAxis = value.yAxis[0];
    const totals = new Map<number, number>();
    for (const series of value.series) {
      for (const [time, amount] of series.data) {
        if (amount !== null) totals.set(time, (totals.get(time) ?? 0) + amount);
      }
    }
    const sample = [...totals.entries()][Math.floor(totals.size / 2)];
    const x = (time: number) => chart.convertToPixel({ xAxisIndex: 0 }, time);
    const step = historyBucketMillis(xAxis.min, xAxis.max);
    return {
      id: chart.id,
      max: yAxis.max,
      peak: Math.max(...totals.values()),
      tick: yAxis.axisLabel.formatter(yAxis.max),
      unit: host.closest('.rut-hour')!.querySelector('.chart-unit')!.textContent,
      stacks: value.series.map(series => series.stack ?? null),
      widths: value.series.map(series => series.barWidth),
      band: x(xAxis.min + step) - x(xAxis.min),
      plot: x(xAxis.max) - x(xAxis.min),
      samples: totals.size,
      nulls: value.series[0].data.filter(point => point[1] === null).length,
      hover: { x: x(sample[0]), y: chart.convertToPixel({ yAxisIndex: 0 }, sample[1] / 2) },
    };
  });
}

async function ready(page: Page) {
  await page.locator('.ndtp-bars').scrollIntoViewIfNeeded();
  await expect
    .poll(() =>
      page.locator('.ndtp-bars .observe-plot-canvas').evaluateAll(async hosts => {
        if (hosts.length !== 1) return 0;
        const moduleUrl = '/tests/fixtures/traffic-bars-mockup-echarts.ts';
        const { getInstanceByDom } = await import(moduleUrl);
        return getInstanceByDom(hosts[0])?.getOption()?.series?.[0]?.data?.length ?? 0;
      }),
    )
    .toBeGreaterThan(0);
}

async function open(page: Page, query: string, width: number) {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
  await page.setViewportSize({ width, height: 900 });
  const params = new URLSearchParams(query);
  if (!params.has('nav')) params.set('nav', '0');
  await page.goto(`/tests/fixtures/traffic-bars-mockup.html?${params}`);
  const history = page.locator('.rut-hour');
  await expect(history.locator('canvas')).toBeVisible();
  await expect(history.locator('.chart-unit')).toContainText('bit/s');
  await ready(page);
  await page.evaluate(() => document.fonts.ready);
  return { history, errors };
}

for (const option of ['a', 'b', 'c']) {
  test(`${option}: a duplex peak fits and all readings use the stacked axis unit`, async ({ page }) => {
    const { history, errors } = await open(page, `option=${option}&history=units&theme=light`, 1440);
    const value = await inspect(page);
    expect(value.peak).toBe(3_200_000_000);
    expect(value.max).toBeGreaterThan(value.peak);
    expect(value.stacks).toEqual(['traffic', 'traffic']);
    expect(value.unit).toBe('(Gbit/s)');
    expect(value.tick).toBe('4');
    await history.locator('canvas').hover({ position: value.hover });
    const tip = history.locator('.observe-chart-tip');
    await expect(tip).toBeVisible();
    await expect(tip).toContainText('1.60 Gbit/s');
    await expect(tip).not.toContainText('Mbit/s');
    if (option === 'a') await expect(history).toHaveScreenshot('units-tooltip-desktop-light.png');
    expect(errors).toEqual([]);
  });
}

test('one window keeps its cadence when resizing the same chart', async ({ page }) => {
  const { history, errors } = await open(page, 'option=a&history=single&theme=light', 1440);
  const initial = await inspect(page);
  expect(initial.samples).toBe(1);
  expect(initial.widths[0]).toBeLessThan(initial.plot / 100);
  expect(initial.widths[0]).toBeGreaterThanOrEqual(initial.band);
  expect(initial.widths[0]).toBeLessThanOrEqual(initial.band * 1.25);
  await page.setViewportSize({ width: 390, height: 900 });
  await expect.poll(async () => (await inspect(page)).widths[0]).toBeLessThan(initial.widths[0]);
  const narrow = await inspect(page);
  expect(narrow.id).toBe(initial.id);
  expect(narrow.samples).toBe(1);
  expect(narrow.widths[0]).toBeGreaterThanOrEqual(narrow.band);
  expect(narrow.widths[0]).toBeLessThanOrEqual(narrow.band * 1.25);
  await expect(history).toHaveScreenshot('single-resized-phone-light.png');
  expect(errors).toEqual([]);
});

test('gaps stay empty and a period revisited from cache keeps its window width', async ({ page }) => {
  const { history, errors } = await open(page, 'nav=1&option=a&scene=gaps&theme=light', 1440);
  const day = await inspect(page);
  expect(day.nulls).toBeGreaterThan(0);
  expect(day.widths[0]).toBeLessThanOrEqual(day.band * 1.25);
  for (const period of ['当周', '当月', '当天']) {
    await page
      .getByRole('group', { name: '统计范围', exact: true })
      .getByRole('button')
      .filter({ hasText: period })
      .click();
    await expect(history).toContainText(period);
    await ready(page);
    const next = await inspect(page);
    expect(next.max).toBeGreaterThan(next.peak);
    expect(next.widths[0]).toBeGreaterThanOrEqual(next.band);
    expect(next.widths[0]).toBeLessThanOrEqual(next.band * 1.25);
  }
  expect((await inspect(page)).widths[0]).toBeCloseTo(day.widths[0], 4);
  expect(errors).toEqual([]);
});

for (const [name, width, query] of [
  ['duplex-desktop-light', 1440, 'option=a&history=duplex&theme=light'],
  ['units-phone-dark', 390, 'option=a&history=units&width=phone&theme=dark'],
  ['single-card-light', 760, 'option=a&history=single&width=card&theme=light'],
  ['gaps-week-phone-dark', 390, 'option=a&scene=gaps&period=week&width=phone&theme=dark'],
  ['current-units-desktop-light', 1440, 'option=current&history=units&theme=light'],
] as const) {
  test(`${name}: history preview`, async ({ page }) => {
    const { history, errors } = await open(page, query, width);
    await expect(history).toHaveScreenshot(`${name}.png`);
    expect(errors).toEqual([]);
  });
}
