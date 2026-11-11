import { expect, test, type Locator } from '@playwright/test';

async function expectRuleLayout(table: Locator) {
  const layout = await table.evaluate(table => {
    const bounds = table.getBoundingClientRect();
    return {
      // 链路规则表的匹配类型与内容同一行顶端对齐，中等宽度上下排列且同宽；机器 DNS 卡的规则表另有排法。
      chain: table.classList.contains('chain-rule-table'),
      overflow: table.scrollWidth - table.clientWidth,
      right: bounds.right,
      viewport: innerWidth,
      rows: [...table.querySelectorAll<HTMLTableRowElement>('tbody > tr')].map(row => {
        const cells = [...row.cells];
        const action = row.querySelector('.rule-action-cell')!;
        const operations = [...row.querySelectorAll('.rule-ops-cell button')];
        const matchFields = [...row.querySelectorAll('.rule-match-cell > .f')];
        const actionSelect = row.querySelector('select.rule-action-select');
        const dialSelect = row.querySelector('.rule-dial-select');
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
                  // 处理方式按动作着色并加粗，拨号按钮与它只共享尺寸、字体和字号。
                  return {
                    height: select.getBoundingClientRect().height,
                    font: `${style.fontFamily} ${style.fontSize}`,
                    radius: style.borderRadius,
                    padding: style.padding,
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
    if (layout.chain && row.matchBounds.length > 1) {
      const [kind, value] = row.matchBounds;
      if (Math.abs(kind.top - value.top) >= 1) {
        expect(value.top).toBeGreaterThanOrEqual(kind.bottom);
        expect(Math.abs(kind.left - value.left)).toBeLessThan(1);
        expect(Math.abs(kind.width - value.width)).toBeLessThan(1);
      }
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

// 断点量的是机器树自己（`rules` 容器）：树向面板两侧各伸出 12px，面板内容宽度比树窄 24px。
for (const width of [640, 641, 1050, 1051, 1440, 1441]) {
  test(`chain rules at the ${width}px container boundary`, async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 2000 });
    await page.goto(`/tests/fixtures/rule-coverage-preview.html?layout&container=${width - 24}`);
    const tree = page.locator('.chain-rule-tree');
    expect(await tree.evaluate(el => el.clientWidth)).toBe(width);
    const table = tree.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(table.locator('tbody > tr').first()).toHaveCSS('display', width <= 640 ? 'block' : 'table-row');
    await expect(table.locator('.rule-action-cell').first()).toHaveCSS('display', width <= 640 ? 'flex' : 'table-cell');
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
  test(`chain rules open custom DNS policies from the egress rule at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&dns');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await page.evaluate(() => document.fonts.ready);
    await expectRuleLayout(table);
    await expect(page.locator('.rule-editor')).toHaveScreenshot(`chain-dns-rules-${width}.png`);
    await table.getByRole('button', { name: '查看本机 DNS 解析策略' }).first().click();
    const dns = page.getByRole('dialog', { name: /的本机 DNS 解析策略$/ });
    await expect(dns.locator('.rule-dns-row')).toHaveCount(1);
    await expect(dns.getByRole('textbox', { name: /^DNS 地址/ })).toHaveValue('192.0.2.53, 2001:db8::53');
    await expect(dns.locator('.rule-dns-dialog')).toHaveScreenshot(`chain-dns-policies-${width}.png`);
  });
}
