import { fitViewportTo, waitForVisualAssets } from '../helpers/visual-ready';
import { expect, test } from '@playwright/test';

for (const [width, theme] of [
  [1440, 'dark'],
  [390, 'light'],
] as const) {
  test(`ordinary user sees provider names without management inventory ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=list&role=user&theme=${theme}`);
    await expect(page.getByRole('region', { name: '容器清单', exact: true })).toBeVisible();
    const catalog = await page.evaluate(async () => fetch('/cvm/networks').then(response => response.json()));
    expect(catalog.providers).toEqual([]);
    expect(catalog.can_manage).toBe(false);
    expect(catalog.provider_names['remote-ingress']).toBe('GreenVPS · 香港网络');
    await expect(page.getByRole('tab', { name: '网络', exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: 'home-workspace', exact: true }).click();
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const entries = page.getByRole('list', { name: '访问入口列表' });
    const ssh = entries.getByRole('listitem').filter({ hasText: '203.0.113.20:30022' });
    await expect(ssh).toContainText('GreenVPS · 香港网络');
    await expect(entries).not.toContainText('remote-ingress');
    await waitForVisualAssets(page);
    await expect(entries).toHaveScreenshot(`ordinary-user-entry-names-${width}-${theme}.png`);
  });
}

for (const [width, theme, role] of [
  [1440, 'light', 'system-admin'],
  [1440, 'dark', 'user'],
  [900, 'dark', 'public'],
  [390, 'light', 'system-admin'],
  [390, 'dark', 'readonly'],
  [320, 'light', 'system-admin'],
  [320, 'dark', 'public'],
] as const) {
  test(`instance dashboard ${width} ${theme} ${role}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}&role=${role}`);
    const header = page.locator('.cvm-page-head');
    const redacted = role === 'public' || role === 'readonly';
    // 运行状态是标题图标角上的状态灯，与机器详情一致。
    await expect(header.getByRole('img', { name: '运行中', exact: true })).toBeVisible();
    await waitForVisualAssets(page);
    const title = (await header.getByRole('heading', { level: 1 }).boundingBox())!;
    expect(title.width).toBeGreaterThanOrEqual(40);
    const tabs = (await header.getByRole('tablist').boundingBox())!;
    const actions = header.getByRole('group', { name: '页面操作', exact: true });
    if (await actions.count()) {
      const tools = (await actions.boundingBox())!;
      const overlap = Math.min(tabs.x + tabs.width, tools.x + tools.width) - Math.max(tabs.x, tools.x);
      const vertical = Math.min(tabs.y + tabs.height, tools.y + tools.height) - Math.max(tabs.y, tools.y);
      expect(overlap <= 0 || vertical <= 0).toBe(true);
    }
    await expect(page.getByRole('button', { name: '返回列表', exact: true })).toHaveCount(0);
    // 指标带沿用机器详情的 KPI 砖：任何宽度都是六块，窄屏折成两列或三列。
    await expect(page.getByRole('group', { name: '当前容器读数' }).locator('.kpi')).toHaveCount(6);
    if (width <= 620 && !redacted) {
      await expect(header.getByRole('tab', { name: '网络', exact: true }).locator('.nd-tab-badge')).toBeVisible();
      await expect(
        header.getByRole('tab', { name: '监控与诊断', exact: true }).getByText('监控', { exact: true }),
      ).toBeVisible();
    }
    await expect(page.locator('.cvm-overview .cvm-runtime > header')).toContainText('持续采样');
    await expect(header.locator('.nd-ident-meta')).toContainText('已运行');
    await expect(header.locator('.nd-ident-meta')).not.toContainText(/Debian|alice/);
    await expect(header.getByRole('status')).toHaveCount(0);
    await expect(header.getByRole('tab')).toHaveCount(role === 'system-admin' ? 4 : 3);
    const overview = page.getByRole('region', { name: '接入与网络', exact: true });
    if (redacted) {
      await expect(overview.getByText('网络详情已脱敏。', { exact: true })).toBeVisible();
      await expect(overview.locator('.cvm-net-summary')).toHaveCount(0);
    } else {
      await expect(overview.locator('.cvm-net-summary dd').first().locator('.cvm-chip')).toHaveCount(3);
    }
    await expect(page.getByRole('region', { name: '流量与事件' }).locator('.history-chart-legend')).toBeVisible();
    await expect(header).toHaveScreenshot(`identity-${width}-${theme}-${role}.png`);
    const widgets = page.locator(
      '.cvm-overview > .cvm-kpis, .cvm-overview > .cvm-overview-pair, .cvm-overview > .cvm-runtime',
    );
    await expect(widgets).toHaveCount(3);
    const bodyWidth = (await page.locator('.cvm-overview').boundingBox())!.width;
    for (const [index, widget] of (await widgets.all()).entries()) {
      await widget.scrollIntoViewIfNeeded();
      if (width <= 820) expect((await widget.boundingBox())!.width).toBeGreaterThan(bodyWidth - 2);
      await expect(widget).toHaveScreenshot(`overview-${index}-${width}-${theme}-${role}.png`);
    }
    await header.scrollIntoViewIfNeeded();
    await expect(page).toHaveScreenshot(`dashboard-${width}-${theme}-${role}.png`);
    for (const box of await page
      .locator('.cvm-overview .chart-card, .cvm-overview .kpi, .cvm-overview .cvm-runtime')
      .all()) {
      const overflow = await box.evaluate(el => el.scrollWidth - el.clientWidth);
      expect(overflow, (await box.textContent()) ?? '').toBeLessThanOrEqual(1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await page.getByRole('tab', { name: '网络', exact: true }).click();
    const network = page.getByRole('tabpanel', { name: '网络', exact: true });
    const nics = network.getByRole('region', { name: '网卡', exact: true });
    const routes = network.getByRole('region', { name: '路由与命中测试', exact: true });
    await expect(nics.getByRole('heading', { name: '网卡', exact: true })).toBeVisible();
    // 网卡列表末尾的「还可挂载」提示行不算网卡。
    const cards = nics.locator('.cvm-nrows > .cvm-nrow:not(.cvm-nrow-hint)');
    await expect(cards).toHaveCount(2);
    for (const [index, card] of (await cards.all()).entries()) {
      await card.scrollIntoViewIfNeeded();
      await expect(card).toHaveScreenshot(`detail-nic-${index}-${width}-${theme}-${role}.png`);
    }
    if (redacted) {
      for (const name of ['终端', '开机', '关机', '重启', '实例更多操作'])
        await expect(header.getByRole('button', { name, exact: true })).toHaveCount(0);
      for (const name of ['挂载网卡', '添加入口', '添加路由'])
        await expect(network.getByRole('button', { name, exact: true })).toHaveCount(0);
      // 卸载、更换连接方式都在网卡的 ⋯ 菜单里。
      await expect(network.getByRole('button', { name: /网卡操作$/ })).toHaveCount(0);
      await expect(routes.getByRole('table', { name: '目标路由' })).toHaveCount(0);
    } else {
      await expect(routes.getByRole('table', { name: '目标路由' })).toBeVisible();
    }
    await fitViewportTo(page, network);
    await expect(network).toHaveScreenshot(`network-edit-${width}-${theme}-${role}.png`);
    await page.getByRole('tab', { name: '监控与诊断', exact: true }).click();
    await expect(network).toHaveCount(0);
    const artifacts = page.getByRole('tab', { name: '产物', exact: true });
    if (role === 'system-admin') {
      await artifacts.click();
      await expect(page.getByRole('tabpanel', { name: '产物', exact: true })).toBeVisible();
    } else {
      await expect(artifacts).toHaveCount(0);
    }
    await page.getByRole('tab', { name: '概览', exact: true }).click();
    await expect(page.locator('.cvm-overview')).toBeVisible();
  });
}

test('workbench tabs keep the same baseline and bottom rule on phones', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=fleet&theme=light');
  const header = page.locator('.cvm-page-head');
  const tabs = header.getByRole('tablist');
  const body = page.locator('.cvm-page > .nd-paper > .nd-paper-body');
  await expect(tabs.getByRole('tab')).toHaveCount(3);
  const top = (await header.boundingBox())!.y;
  for (const name of ['容器', '宿主', '网络']) {
    await tabs.getByRole('tab', { name, exact: true }).click();
    await expect(tabs.getByRole('tab', { name, exact: true })).toHaveAttribute('aria-selected', 'true');
    await expect(header).toHaveCSS('height', '90px');
    await expect(header).toHaveCSS('border-bottom-width', '0px');
    await expect(tabs).toHaveCSS('border-bottom-width', '1px');
    expect((await tabs.boundingBox())!.y - top).toBe(50);
    expect((await body.boundingBox())!.y - top).toBe(90);
  }
});

test('instance identity, tabs, and actions fit around the workspace breakpoint', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&theme=light');
  const workspace = page.locator('.cvm-page');
  const header = workspace.locator('.cvm-instance-head');
  const tabs = header.getByRole('tablist');
  const actions = header.getByRole('group', { name: '页面操作', exact: true });
  await expect(actions).toBeVisible();
  for (const width of [520, 700, 701, 820, 821, 1440]) {
    await workspace.evaluate((el, value) => {
      el.style.width = `${value}px`;
    }, width);
    await expect(tabs, `${width}px workspace tabs`).toHaveCSS('border-bottom-width', width <= 820 ? '1px' : '0px');
    const fit = async () => {
      const bounds = (await tabs.boundingBox())!;
      const tools = (await actions.boundingBox())!;
      const last = (await tabs.getByRole('tab').last().boundingBox())!;
      return {
        lastTabInside: last.x + last.width <= bounds.x + bounds.width + 1,
        clearOfActions: width <= 820 ? bounds.y >= tools.y + tools.height : last.x + last.width <= tools.x,
      };
    };
    // 页签按可用宽度收起文字由 ResizeObserver 在下一帧完成；等布局稳定后再判定，条件本身不放宽。
    await expect
      .poll(fit, { message: `${width}px workspace tabs and actions` })
      .toEqual({ lastTabInside: true, clearOfActions: true });
    if (width > 820) {
      await expect(header.locator('.nd-page-identity')).toHaveCSS('padding-right', '21px');
      await expect(header).toHaveCSS('border-bottom-width', '1px');
    }
  }
});

test('dashboard fits a narrow desktop workspace', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-11-04T10:30:00Z') });
  await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&theme=light');
  const workspace = page.locator('.cvm-page');
  await workspace.evaluate(el => {
    (el as HTMLElement).style.width = '520px';
  });
  await expect(workspace.locator('.kpi-band .kpi')).toHaveCount(6);
  // The entries and readings settle independently of the workspace query.
  const summary = workspace.getByRole('region', { name: '接入与网络' }).locator('.cvm-net-summary');
  await expect(summary.locator('dd').first().locator('.cvm-chip')).toHaveCount(3);
  await expect(workspace.locator('.cvm-overview .cvm-runtime > header')).toContainText('持续采样');
  await expect(workspace.getByRole('region', { name: '流量与事件' }).locator('.history-chart-legend')).toBeVisible();
  await waitForVisualAssets(page);
  expect(await workspace.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  for (const text of await workspace.locator('.kpi, .cvm-net-summary, .cvm-overview-card').all())
    expect(await text.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  await fitViewportTo(page, workspace);
  await expect(workspace).toHaveScreenshot('narrow-desktop-dashboard.png');
});

for (const [width, theme] of [
  [1440, 'light'],
  [390, 'dark'],
] as const) {
  test(`overview expands a real metric without leaving the page ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-11-04T10:30:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}`);
    const cpu = page.getByRole('button', { name: /^CPU/ });
    await expect(cpu).toContainText('%');
    await cpu.click();
    await expect(cpu).toHaveAttribute('aria-expanded', 'true');
    const chart = page.locator('.cvm-kpis .kpi-detail-motion-content');
    await chart.scrollIntoViewIfNeeded();
    await expect(chart.locator('canvas')).toBeVisible();
    await expect(chart).toContainText('采样峰值');
    await waitForVisualAssets(page);
    await expect(chart).toHaveScreenshot(`overview-expand-${width}-${theme}.png`);
    await cpu.click();
    await expect(chart).toHaveCount(0);
    await expect(page.getByRole('tab', { name: '概览' })).toHaveAttribute('aria-selected', 'true');
  });
}
