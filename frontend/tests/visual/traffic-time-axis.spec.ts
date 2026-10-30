import { expect, test, type Page } from '@playwright/test';

async function inspect(page: Page) {
  return page.locator('.ndtp-bars .observe-plot-canvas').evaluate(async host => {
    const moduleUrl = '/tests/fixtures/node-traffic-acceptance.tsx';
    const { getInstanceByDom } = await import(moduleUrl);
    const chart = getInstanceByDom(host);
    if (!chart) return null;
    const option = chart.getOption();
    const axis = option?.xAxis?.[0];
    if (!axis?.axisLabel?.customValues?.length) return null;
    const series = option.series;
    const sample = series[0].data.find((point: [number, number | null]) => point[1] !== null);
    return {
      labels: axis.axisLabel.customValues.map(axis.axisLabel.formatter),
      visibleLabels: chart
        .getModel()
        .getComponent('xAxis', 0)
        .axis.getViewLabels()
        .map((label: { formattedLabel: string }) => label.formattedLabel),
      tooltip: option.tooltip[0].formatter(
        series.map((line: { name: string; data: [number, number | null][] }, index: number) => ({
          seriesName: line.name,
          color: index === 0 ? '#68a5e9' : '#e2a0c8',
          value: line.data.find(point => point[0] === sample[0]),
        })),
      ),
    };
  });
}

async function inspectSites(page: Page) {
  return page.locator('.site-usage-chart').evaluate(async host => {
    const moduleUrl = '/tests/fixtures/node-traffic-acceptance.tsx';
    const { getInstanceByDom } = await import(moduleUrl);
    const chart = getInstanceByDom(host);
    const option = chart?.getOption();
    const axis = option?.xAxis?.[0];
    if (!axis?.axisLabel?.customValues?.length) return null;
    return {
      labels: axis.axisLabel.customValues.map(axis.axisLabel.formatter),
      end: axis.max,
      tooltip: option.tooltip[0].formatter(
        option.series.map((line: { data: [number, number | null][] }, seriesIndex: number) => ({
          seriesIndex,
          value: line.data.at(-1),
        })),
      ),
    };
  });
}

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  for (const period of ['当天', '当周', '当月']) {
    test(`${period} dates in the production traffic dialog at ${width}px`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
      await page.setViewportSize({ width, height: 950 });
      await page.goto(`/tests/fixtures/node-traffic-acceptance.html?scene=solo&run=0&theme=${theme}`);
      await page.getByRole('button', { name: '香港测试机器', exact: true }).click();
      await page
        .getByRole('group', { name: '流量统计范围', exact: true })
        .getByRole('button', { name: period, exact: true })
        .click();
      const history = page.getByRole('region', { name: '所选区间历史流量' });
      await expect(history).toContainText(`${period} ·`);
      await expect.poll(() => inspect(page)).not.toBeNull();
      await page.evaluate(() => document.fonts.ready);
      await expect(history).toHaveScreenshot(`traffic-${period}-${width}-${theme}.png`);
      const value = (await inspect(page))!;
      expect(value.labels.length).toBeGreaterThan(2);
      expect(value.labels.length).toBeLessThanOrEqual(8);
      expect(value.visibleLabels).toEqual(value.labels);
      if (period === '当天') {
        expect(value.labels.every((label: string) => /^\d{2}:\d{2}$/.test(label))).toBe(true);
      } else {
        expect(new Set(value.labels).size).toBe(value.labels.length);
        expect(value.labels.every((label: string) => /^10-\d{2}$/.test(label))).toBe(true);
        expect(value.tooltip).toMatch(/10-\d{2} \d{2}:\d{2}:\d{2}/);
      }
      expect(errors).toEqual([]);
      const sites = page.getByRole('img', { name: '站点流量随时间分布' });
      await sites.scrollIntoViewIfNeeded();
      await expect.poll(() => inspectSites(page)).not.toBeNull();
      const siteValue = (await inspectSites(page))!;
      expect(siteValue.labels.length).toBeGreaterThan(2);
      expect(new Set(siteValue.labels).size).toBe(siteValue.labels.length);
      if (period === '当天') expect(siteValue.labels.every((label: string) => /^\d{2}:\d{2}$/.test(label))).toBe(true);
      else expect(siteValue.labels.every((label: string) => /^10-\d{2}/.test(label))).toBe(true);
      await expect(sites).toHaveScreenshot(`sites-${period}-${width}-${theme}.png`);
    });
  }

  test(`a custom range keeps its date and time at ${width}px`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
    await page.setViewportSize({ width, height: 950 });
    await page.goto(`/tests/fixtures/node-traffic-acceptance.html?scene=solo&run=0&theme=${theme}`);
    await page.getByRole('button', { name: '香港测试机器', exact: true }).click();
    await page.getByRole('button', { name: '自定义', exact: true }).click();
    await page.getByLabel('开始时间（UTC+8）').fill('2026-10-17T06:00');
    await page.getByLabel('结束时间（UTC+8）').fill('2026-10-18T18:00');
    await page.getByRole('button', { name: '应用', exact: true }).click();
    const history = page.getByRole('region', { name: '所选区间历史流量' });
    await expect(history).toContainText('自选区间 ·');
    await expect.poll(() => inspect(page)).not.toBeNull();
    const value = (await inspect(page))!;
    expect(value.labels.every((label: string) => /^10-\d{2}\n\d{2}:\d{2}$/.test(label))).toBe(true);
    expect(new Set(value.labels).size).toBe(value.labels.length);
    expect(value.visibleLabels).toEqual(value.labels);
    await page.evaluate(() => document.fonts.ready);
    // The custom-range banner can place HTML title glyphs on a fractional pixel. Metadata is
    // asserted above; compare the plotted dates directly, independently of that title placement.
    await expect(history.locator('.ndtp-bars')).toHaveScreenshot(`traffic-custom-${width}-${theme}.png`);
  });
}

test.describe('fixed calendar ranges in a UTC browser', () => {
  test.use({ timezoneId: 'UTC' });
  for (const [width, theme] of [
    [1440, 'dark'],
    [390, 'light'],
  ] as const) {
    test(`past site traffic keeps UTC+8 dates and actual endpoints at ${width}px`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
      await page.setViewportSize({ width, height: 950 });
      await page.goto(`/tests/fixtures/node-traffic-acceptance.html?scene=solo&run=0&theme=${theme}`);
      await page.getByRole('button', { name: '香港测试机器', exact: true }).click();
      await page.getByRole('button', { name: '自定义', exact: true }).click();
      await page.getByLabel('开始时间（UTC+8）').fill('2026-10-17T06:00');
      await page.getByLabel('结束时间（UTC+8）').fill('2026-10-18T18:00');
      await page.getByRole('button', { name: '应用', exact: true }).click();
      const sites = page.getByRole('img', { name: '站点流量随时间分布' });
      await sites.scrollIntoViewIfNeeded();
      await expect.poll(() => inspectSites(page)).not.toBeNull();
      const value = (await inspectSites(page))!;
      expect(value.end).toBe(36);
      expect(value.labels[0]).toBe('10-17\n06:00');
      expect(value.labels.at(-1)).toBe('10-18\n18:00');
      expect(new Set(value.labels).size).toBe(value.labels.length);
      expect(value.tooltip).toContain('10-18 17:00 – 10-18 18:00');
      expect(value.tooltip).not.toContain('现在');
      await page.evaluate(() => document.fonts.ready);
      await expect(sites).toHaveScreenshot(`sites-custom-${width}-${theme}.png`);
      expect(errors).toEqual([]);
    });
  }
});
