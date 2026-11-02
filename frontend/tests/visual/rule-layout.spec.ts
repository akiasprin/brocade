import { expect, test, type Locator } from '@playwright/test';

async function expectRuleLayout(table: Locator) {
  const layout = await table.evaluate(table => {
    const bounds = table.getBoundingClientRect();
    return {
      overflow: table.scrollWidth - table.clientWidth,
      right: bounds.right,
      viewport: innerWidth,
      rows: [...table.querySelectorAll<HTMLTableRowElement>('tbody > tr')].map(row => {
        const cells = [...row.cells];
        const action = row.querySelector('.rule-action-cell')!;
        const operations = [...row.querySelectorAll('.rule-operations-cell button')];
        const matchFields = [...row.querySelectorAll('.rule-match-fields > .f')];
        const actionSelect = row.querySelector('select.rule-action-select');
        const dialSelect = row.querySelector('.rule-dial-fields > select');
        return {
          tableRow: getComputedStyle(row).display === 'table-row',
          actionDisplay: getComputedStyle(action).display,
          heights: cells.map(cell => cell.getBoundingClientRect().height),
          overflow: cells.map(cell => cell.scrollWidth - cell.clientWidth),
          buttonTops: operations.map(button => button.getBoundingClientRect().top),
          selectStyles:
            actionSelect && dialSelect
              ? [actionSelect, dialSelect].map(select => {
                  const style = getComputedStyle(select);
                  return {
                    height: select.getBoundingClientRect().height,
                    font: style.font,
                    border: style.border,
                    radius: style.borderRadius,
                    padding: style.padding,
                    background: style.background,
                    color: style.color,
                  };
                })
              : [],
          matchBounds: matchFields.map(field => {
            const rect = field.getBoundingClientRect();
            return { top: rect.top, bottom: rect.bottom, left: rect.left, width: rect.width };
          }),
        };
      }),
    };
  });
  expect(layout.overflow).toBeLessThanOrEqual(1);
  expect(layout.right).toBeLessThanOrEqual(layout.viewport);
  expect(layout.rows.length).toBeGreaterThan(0);
  for (const row of layout.rows) {
    for (const overflow of row.overflow) expect(overflow).toBeLessThanOrEqual(1);
    if (row.tableRow) {
      expect(row.actionDisplay).toBe('table-cell');
      expect(Math.max(...row.heights) - Math.min(...row.heights)).toBeLessThanOrEqual(1);
    }
    if (row.buttonTops.length) expect(Math.max(...row.buttonTops) - Math.min(...row.buttonTops)).toBeLessThan(1);
    if (row.selectStyles.length) expect(row.selectStyles[0]).toEqual(row.selectStyles[1]);
    if (row.matchBounds.length > 1) {
      const [kind, value] = row.matchBounds;
      expect(Math.abs(kind.top - value.top)).toBeLessThan(1);
    }
  }
}

const webkitTest = test.extend({ browserName: 'webkit' });
for (const width of [390, 820, 1280]) {
  webkitTest(`WebKit chain rules fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(table).toHaveScreenshot(`webkit-rules-${width}.png`);
  });
}

for (const width of [320, 390, 670, 820, 1050, 1280, 1440, 1920]) {
  for (const theme of ['dark', 'light']) {
    test(`chain rules fit ${width}px in ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 2000 });
      await page.goto(`/tests/fixtures/rule-coverage-preview.html?layout&theme=${theme}`);
      const table = page.locator('.rule-table');
      await expect(table.locator('tbody > tr')).toHaveCount(5);
      await expect(table.locator('.rule-dial-fields > .mono').nth(1)).toHaveText(
        '2001:db8:1234:5678:90ab:cdef:1234:5678',
      );
      await page.evaluate(() => document.fonts.ready);
      await expectRuleLayout(table);
      await expect(table).toHaveScreenshot(`rules-${width}-${theme}.png`);
    });
  }
}

test('chain rules adapt to a narrow panel inside a desktop viewport', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1000 });
  await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&container=860&longName');
  await expect(page.locator('.rule-table tbody > tr')).toHaveCount(5);
  await page.evaluate(() => document.fonts.ready);
  await expectRuleLayout(page.locator('.rule-table'));
  await expect(page.locator('.rule-table')).toHaveScreenshot('rules-desktop-narrow-panel.png');
});

for (const width of [390, 670, 820, 1050]) {
  test(`shared DNS rule controls fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/dns-groups-preview.html?theme=dark&scene=editing');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(3);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(table).toHaveScreenshot(`dns-rules-${width}.png`);
  });
}

for (const width of [640, 641, 1050, 1051, 1440, 1441]) {
  test(`chain rules at the ${width}px container boundary`, async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 2000 });
    await page.goto(`/tests/fixtures/rule-coverage-preview.html?layout&container=${width}`);
    const panel = page.locator('.node-chain-sheet');
    expect(await panel.evaluate(el => Number.parseFloat(getComputedStyle(el).width))).toBe(width);
    const table = panel.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(table.locator('.rule-action-cell').first()).toHaveCSS(
      'display',
      width <= 640 ? 'block' : 'table-cell',
    );
    await expect(table).toHaveScreenshot(`rules-container-${width}.png`);
  });
}

for (const width of [390, 820, 1440]) {
  test(`readonly chain rules fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&role=readonly&theme=light');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(table).toHaveScreenshot(`readonly-rules-${width}.png`);
  });
}

for (const width of [390, 820, 1440]) {
  test(`chain rules share space with custom DNS policies at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&dns');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    const dns = page.locator('.machine-dns-policy-table');
    await expect(dns.locator('.machine-dns-shared-row')).toHaveCount(1);
    await expect(dns.getByRole('textbox', { name: /DNS 地址/ })).toHaveValue('192.0.2.53, 2001:db8::53');
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(page.locator('.rule-editor')).toHaveScreenshot(`chain-dns-rules-${width}.png`);
  });
}

async function openReportedScene(page: import('@playwright/test').Page, theme = 'light', readonly = false) {
  await page.goto(`/tests/fixtures/rule-layout-preview.html?theme=${theme}${readonly ? '&role=readonly' : ''}`);
  await page.locator('.chain-rule-node-head').filter({ hasText: '香港 AKILE II' }).click();
  await expect(page.locator('.machine-dns-shared-row')).toHaveCount(6);
  await page.evaluate(() => document.fonts.ready);
}

async function expectAdoptedLayout(page: import('@playwright/test').Page, width: number) {
  const editor = page.locator('.rule-editor:visible');
  await expect(editor.locator('.rule-editor-table tbody > tr')).toHaveCount(2);
  await expect(editor.locator('.rule-editor-table .machine-dns-shared-row')).toHaveCount(0);
  await expect(editor.locator('.node-egress-dns-limit')).toHaveCount(0);
  await expect(editor.locator('.rule-dial-fields wbr')).toHaveCount(0);
  const geometry = await editor.evaluate(root => {
    const bounds = (el: Element) => {
      const r = el.getBoundingClientRect();
      return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: r.width, height: r.height };
    };
    const dns = root.querySelector('.machine-dns-policy-table')!;
    const rows = [...dns.querySelectorAll('tbody tr')];
    return {
      overflow: root.scrollWidth - root.clientWidth,
      match: [...root.querySelectorAll('.rule-match-fields > select')].map(bounds),
      dns: rows.map(row => ({
        height: row.getBoundingClientRect().height,
        controls: [...row.querySelectorAll('input, select')].map(bounds),
        header: [...dns.querySelectorAll('th')].slice(2, 7).map(bounds),
        operations: row.querySelector('.rule-operations-cell')
          ? bounds(row.querySelector('.rule-operations-cell')!)
          : null,
        match: bounds(row.querySelector('.machine-dns-match')!),
        colon: row.querySelector('.egress-dns-endpoint i') !== null,
      })),
      rule: [...root.querySelectorAll('.rule-editor-table tbody > tr')].map(row => ({
        match: [...row.querySelectorAll('.rule-match-fields > .f')].map(el => ({
          ...bounds(el),
          border: getComputedStyle(el).borderWidth,
        })),
        action: bounds(row.querySelector('.rule-action-fields')!),
        actionFields: [
          ...row.querySelectorAll(
            '.rule-action-fields > :is(.rule-action-select, .external-target-picker, .rule-dial-fields)',
          ),
        ].map(bounds),
        dialAddress: [...row.querySelectorAll('.rule-dial-fields > .mono')].map(el => ({
          ...bounds(el),
          whiteSpace: getComputedStyle(el).whiteSpace,
        })),
        buttons: [...row.querySelectorAll('.rule-row-actions button')].map(bounds),
        operations: row.querySelector('.rule-operations-cell')
          ? bounds(row.querySelector('.rule-operations-cell')!)
          : null,
      })),
      fields: [...root.querySelectorAll('input.f, select.f')].map(el => ({
        ...bounds(el),
        font: getComputedStyle(el).fontFamily,
        select: el.tagName === 'SELECT',
      })),
      listener: [...root.querySelectorAll('.hop-target-facts .hopfld > .f')].map(el => ({
        ...bounds(el),
        parent: bounds(el.parentElement!),
      })),
    };
  });
  expect(geometry.overflow).toBeLessThanOrEqual(1);
  expect(geometry.dns).toHaveLength(6);
  expect(geometry.match[0].width).toBeCloseTo(geometry.match[1].width, 0);
  expect(new Set(geometry.fields.filter(field => field.select).map(field => field.font)).size).toBe(1);
  for (const row of geometry.rule) {
    if (width >= 1280) for (const field of row.actionFields) expect(field.top).toBeCloseTo(row.actionFields[0].top, 0);
    for (const address of row.dialAddress) expect(address.whiteSpace).toBe('nowrap');
    if (width <= 640)
      for (const button of row.buttons) {
        expect(button.width).toBe(44);
        expect(button.height).toBe(44);
      }
    for (const field of row.match) expect(parseFloat(field.border)).toBeGreaterThan(0);
    if (row.match.length > 1) expect(row.match[0].top).toBeCloseTo(row.match[1].top, 0);
    if (width <= 640 && row.operations) expect(row.operations.top).toBeGreaterThanOrEqual(row.action.bottom);
  }
  for (const row of geometry.dns) {
    expect(row.colon).toBe(false);
    expect(row.controls).toHaveLength(5);
    const [address, port, transport, strategy, fallback] = row.controls;
    expect(address.top).toBeCloseTo(port.top, 0);
    expect(transport.top).toBeCloseTo(strategy.top, 0);
    expect(transport.top).toBeCloseTo(fallback.top, 0);
    if (width >= 1280) {
      for (let i = 0; i < 5; i++) {
        expect(row.controls[i].top).toBeCloseTo(address.top, 0);
        expect(row.controls[i].left).toBeCloseTo(row.header[i].left, 0);
      }
      expect(address.width).toBeGreaterThanOrEqual(200);
    }
    if (width <= 640) {
      expect(row.height).toBeLessThanOrEqual(220);
      expect(address.top).toBeGreaterThanOrEqual(row.match.bottom);
      expect(transport.top).toBeGreaterThanOrEqual(address.bottom);
      if (row.operations) expect(row.operations.top).toBeGreaterThanOrEqual(transport.bottom);
    }
  }
  if (width <= 820) {
    for (const field of geometry.fields) expect(field.height).toBe(44);
    for (const field of geometry.listener) expect(field.right).toBeGreaterThanOrEqual(field.parent.right - 8);
  }
}

for (const width of [320, 390, 820, 1050, 1280, 1600]) {
  for (const theme of ['light', 'dark']) {
    test(`adopted rule layout ${width}px ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 844 });
      await openReportedScene(page, theme);
      await expectAdoptedLayout(page, width);
      await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`adopted-rules-${width}-${theme}.png`);
    });
  }
}
for (const width of [390, 1280]) {
  webkitTest(`WebKit adopted rule layout ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await openReportedScene(page);
    await expectAdoptedLayout(page, width);
    await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`webkit-adopted-rules-${width}.png`);
  });
}
test('phone icons edit, reorder and delete DNS policies and save to the draft', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openReportedScene(page);
  const policies = page.locator('.machine-dns-policy-table');
  await policies.getByLabel('DNS 地址（机器策略：geosite dazn）').fill('192.0.2.53, 2001:db8::53');
  await policies.getByRole('button', { name: 'DNS 优先级下移（geosite dazn）', exact: true }).click();
  await expect(policies.locator('tbody tr').first()).toContainText('dmm');
  await policies.getByRole('button', { name: '删除策略（geosite abema）', exact: true }).click();
  await expect(policies.locator('tbody tr')).toHaveCount(5);
  await page.getByRole('button', { name: '保存到草稿', exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { ruleLayoutDraftOps: () => unknown[] }).ruleLayoutDraftOps()),
    )
    .toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          op: 'set_node_egress_dns',
          node_id: 'hk-akile',
          selector: { t: 'geosite', v: ['dazn'] },
          resolution: expect.objectContaining({ address: ['192.0.2.53', '2001:db8::53'] }),
        }),
        expect.objectContaining({
          op: 'set_node_egress_dns',
          selector: { t: 'geosite', v: ['abema'] },
          resolution: null,
        }),
        expect.objectContaining({ op: 'reorder_node_egress_dns', node_id: 'hk-akile' }),
      ]),
    );
  await expect(policies.locator('tbody tr')).toHaveCount(5);
  await expect(policies.locator('tbody tr').first()).toContainText('dmm');
  await expect(policies.getByLabel('DNS 地址（机器策略：geosite dazn）')).toHaveValue('192.0.2.53, 2001:db8::53');
  await expect(page.getByRole('alert')).toHaveCount(0);
});
for (const width of [390, 1280]) {
  test(`adopted readonly rule layout ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await openReportedScene(page, 'light', true);
    await expectAdoptedLayout(page, width);
    await expect(page.locator('.rule-row-actions')).toHaveCount(0);
    await page.getByLabel('DNS 解析策略说明').click();
    await expect(page.locator('.machine-dns-help')).toHaveAttribute('open', '');
    const tip = await page.locator('.machine-dns-help-text').boundingBox();
    expect(tip).not.toBeNull();
    expect(tip!.x).toBeGreaterThanOrEqual(0);
    expect(tip!.x + tip!.width).toBeLessThanOrEqual(width);
    await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`adopted-readonly-${width}.png`);
  });
}
