import { expect, test, type Locator } from '@playwright/test';

async function chartState(plot: Locator) {
  return plot.locator('.observe-plot-canvas').evaluate(async host => {
    const moduleUrl = '/tests/fixtures/observe-focus-preview.tsx';
    const { getInstanceByDom } = await import(moduleUrl);
    const option = getInstanceByDom(host)?.getOption();
    if (!option?.series?.length) return null;
    return {
      losses: option.series
        .filter((series: { type: string }) => series.type === 'custom')
        .flatMap((series: { data: unknown[] }) => series.data),
      curves: option.series
        .filter((series: { type: string }) => series.type === 'line')
        .map(
          (series: {
            name: string;
            lineStyle: { width: number; opacity: number };
            stack?: string;
            data: unknown[];
          }) => ({
            name: series.name,
            width: series.lineStyle.width,
            opacity: series.lineStyle.opacity,
            stack: series.stack,
            data: series.data,
          }),
        ),
    };
  });
}

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`machine chart focus at ${width}px in ${theme}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.setFixedTime(new Date('2026-10-28T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/observe-focus-preview.html?theme=${theme}`);
    await page.evaluate(() => document.fonts.ready);
    const throughput = page.getByRole('region', { name: '吞吐', exact: true });
    await throughput.scrollIntoViewIfNeeded();
    await expect.poll(() => chartState(throughput.locator('.ndtp-ec').first())).not.toBeNull();
    await expect(throughput).toHaveScreenshot(`throughput-default-${width}-${theme}.png`);
    for (const [index, names] of [
      [0, ['接收', '发送']],
      [1, ['接入', '中继', '转发']],
    ] as const) {
      const block = throughput.locator('.nd-throughput-block').nth(index);
      for (const name of names) {
        const button = block.getByRole('button', { name: `${name}曲线` });
        await button.focus();
        await page.keyboard.press('Enter');
        await expect(button).toHaveAttribute('aria-pressed', 'true');
        const state = await chartState(block.locator('.ndtp-ec'));
        expect(state?.curves.map((curve: { opacity: number }) => curve.opacity)).toEqual(
          names.map(value => (value === name ? 1 : 0.14)),
        );
      }
    }
    await page.mouse.move(0, 0);
    await expect(throughput).toHaveScreenshot(`throughput-focused-${width}-${theme}.png`);
    // Each plot retains its selection when another plot changes. Space also toggles it off.
    await expect(throughput.getByRole('button', { name: '发送曲线' })).toHaveAttribute('aria-pressed', 'true');
    for (const name of ['发送', '转发']) {
      await throughput.getByRole('button', { name: `${name}曲线` }).focus();
      await page.keyboard.press('Space');
    }
    await page.evaluate(() => (document.activeElement as HTMLElement)?.blur());
    await expect(throughput).toHaveScreenshot(`throughput-default-${width}-${theme}.png`);

    const ping = page.getByRole('region', { name: 'Ping', exact: true });
    await ping.scrollIntoViewIfNeeded();
    const icmp = ping.locator('.ping-probe-block').first();
    const tcp = ping.locator('.ping-probe-block').last();
    await expect
      .poll(async () => (await chartState(icmp.locator('.ping-probe-chart')))?.losses.length)
      .toBeGreaterThan(0);
    const allLoss = (await chartState(icmp.locator('.ping-probe-chart')))!.losses;
    await expect(ping).toHaveScreenshot(`ping-default-${width}-${theme}.png`);
    await icmp.getByRole('button', { name: '广东移动曲线，丢包率 0%' }).click();
    await expect.poll(async () => (await chartState(icmp.locator('.ping-probe-chart')))?.losses).toEqual([]);
    await tcp.getByRole('button', { name: /广东电信曲线/ }).click();
    expect((await chartState(tcp.locator('.ping-probe-chart')))!.losses.length).toBeGreaterThan(0);
    await page.mouse.move(0, 0);
    await expect(ping).toHaveScreenshot(`ping-focused-${width}-${theme}.png`);
    await ping.getByRole('radio', { name: 'IPv6', exact: true }).click();
    await expect.poll(async () => (await chartState(icmp.locator('.ping-probe-chart')))?.losses).toEqual([]);
    await icmp.getByRole('button', { name: '广东移动曲线，丢包率 0%' }).click();
    await expect.poll(async () => (await chartState(icmp.locator('.ping-probe-chart')))?.losses).toEqual(allLoss);

    for (const [category, title, name] of [
      ['CPU', 'CPU 时间占比', '内核态'],
      ['内存', '容量构成', '匿名页'],
      ['磁盘', '块设备吞吐', '写入'],
      ['连接表', '连接与套接字', 'TCP 已建立'],
    ]) {
      await page.getByRole('button', { name: new RegExp(`展开.*${category}`) }).click();
      const card = page
        .locator('.history-chart-card')
        .filter({ has: page.getByLabel(`${title} 图例`, { exact: true }) });
      await card.scrollIntoViewIfNeeded();
      await expect.poll(() => chartState(card.locator('.history-chart'))).not.toBeNull();
      const original = (await chartState(card.locator('.history-chart')))!.curves;
      const button = card.getByRole('button', { name: `${name}曲线` });
      await button.focus();
      await page.keyboard.press('Enter');
      await expect(button).toHaveAttribute('aria-pressed', 'true');
      const focused = (await chartState(card.locator('.history-chart')))!.curves;
      expect(focused.map((curve: { opacity: number; name: string }) => curve.opacity)).toEqual(
        original.map((curve: { name: string }) => (curve.name === name ? 1 : 0.14)),
      );
      expect(focused.map((curve: { stack?: string; data: unknown[] }) => [curve.stack, curve.data])).toEqual(
        original.map((curve: { stack?: string; data: unknown[] }) => [curve.stack, curve.data]),
      );
      await page.mouse.move(0, 0);
      await expect(card).toHaveScreenshot(`history-${category}-${width}-${theme}.png`);
      await page.keyboard.press('Space');
      await expect(button).toHaveAttribute('aria-pressed', 'false');
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}
