import { waitForVisualAssets } from './helpers/visual-ready';
import { devices, expect, test, type Locator, type Page } from '@playwright/test';

async function expectRuleLayout(table: Locator) {
  // ResizeObserver updates the shared row flow on the next animation frame. Under a full
  // cross-browser run WebKit can expose the pre-update cell widths for one measurement.
  // Wait for that existing layout contract to converge without relaxing its 1px limit.
  await expect
    .poll(
      () =>
        table.evaluate(table =>
          Math.max(
            table.scrollWidth - table.clientWidth,
            ...[...table.querySelectorAll('td')].map(cell => cell.scrollWidth - cell.clientWidth),
          ),
        ),
      { message: 'rule table and cells fit after the responsive layout settles' },
    )
    .toBeLessThanOrEqual(1);
  const layout = await table.evaluate(table => {
    const bounds = table.getBoundingClientRect();
    const editor = table.closest('.rule-editor');
    return {
      // 断点量的是规则编辑器自己的内容宽度，而不是设备或视口身份。
      chain: table.classList.contains('chain-rule-table'),
      editorWidth: editor ? parseFloat(getComputedStyle(editor).width) : null,
      overflow: table.scrollWidth - table.clientWidth,
      right: bounds.right,
      viewport: innerWidth,
      rows: [...table.querySelectorAll<HTMLTableRowElement>('tbody > tr')].map(row => {
        const cells = [...row.cells];
        const action = row.querySelector('.rule-action-cell')!;
        const operations = [...row.querySelectorAll('.rule-ops-cell button')];
        const matchFields = [...row.querySelectorAll('.rule-match-cell > :is(.f, .geo-field, .rule-match-kind)')];
        const actionSelect = row.querySelector('select.rule-action-select');
        const actionControl = row.querySelector('.rule-action-select');
        const target = row.querySelector('.external-target-picker');
        const dialSelect = row.querySelector('.rule-dial-select');
        const address = row.querySelector<HTMLElement>('.rule-dial-address');
        const connection = row.querySelector('.rule-connection');
        let connectionLayout = null;
        if (dialSelect && address && connection) {
          const dial = dialSelect.getBoundingClientRect();
          const addressBounds = address.getBoundingClientRect();
          const connectionBounds = connection.getBoundingClientRect();
          const addressStyle = getComputedStyle(address);
          connectionLayout = {
            connectionLeft: connectionBounds.left,
            dialRight: dial.right,
            dialCenter: dial.top + dial.height / 2,
            addressLeft: addressBounds.left,
            addressRight: addressBounds.right,
            addressCenter: addressBounds.top + addressBounds.height / 2,
            connectionRight: connectionBounds.right,
            overflow: address.scrollWidth - address.clientWidth,
            ellipsis: addressStyle.textOverflow === 'ellipsis',
            nowrap: addressStyle.whiteSpace === 'nowrap',
            problem: address.classList.contains('is-problem'),
          };
        }
        const boundsOf = (element: Element | null) => {
          if (!element) return null;
          const rect = element.getBoundingClientRect();
          return { left: rect.left, right: rect.right, width: rect.width };
        };
        const iconFields = [
          [
            'match',
            row.querySelector('.rule-match-kind > select.f'),
            row.querySelector('.rule-match-kind-chevron svg'),
          ],
          ['geo', row.querySelector('.geo-select'), row.querySelector('.geo-chevron svg')],
          ['target', row.querySelector('.external-target-trigger'), row.querySelector('.external-target-chevron svg')],
        ] as const;
        const dropdownArrows = iconFields.flatMap(([kind, control, icon]) => {
          if (!control || !icon) return [];
          const controlBounds = control.getBoundingClientRect();
          const arrow = icon.getBoundingClientRect();
          return [
            {
              kind,
              width: arrow.width,
              height: arrow.height,
              rightInset: controlBounds.right - arrow.right,
              centerOffset: Math.abs(arrow.top + arrow.height / 2 - (controlBounds.top + controlBounds.height / 2)),
            },
          ];
        });
        const nativeCarets = [...row.querySelectorAll('.rule-action-cell select.f, .rule-dial-select')]
          .filter(control => control.getClientRects().length)
          .map(control => {
            const style = getComputedStyle(control);
            return {
              image: style.backgroundImage,
              size: style.backgroundSize,
              position: style.backgroundPosition,
              origin: style.backgroundOrigin,
            };
          })
          .filter(caret => caret.image !== 'none');
        return {
          tableRow: getComputedStyle(row).display === 'table-row',
          actionDisplay: getComputedStyle(action).display,
          heights: cells.map(cell => cell.getBoundingClientRect().height),
          overflow: cells.map(cell => cell.scrollWidth - cell.clientWidth),
          buttonTops: operations.map(button => button.getBoundingClientRect().top),
          actionControl: boundsOf(actionControl),
          target: boundsOf(target),
          dropdownArrows,
          nativeCarets,
          connection: connectionLayout,
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
            return { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right, width: rect.width };
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
    for (const arrow of row.dropdownArrows) {
      expect(arrow.width, `${arrow.kind} arrow width`).toBe(14);
      expect(arrow.height, `${arrow.kind} arrow height`).toBe(14);
      expect(arrow.rightInset, `${arrow.kind} arrow right inset`).toBeCloseTo(8, 1);
      expect(arrow.centerOffset, `${arrow.kind} arrow vertical center`).toBeLessThanOrEqual(1);
    }
    for (const caret of row.nativeCarets) {
      expect(caret.size).toBe('14px 14px');
      expect(caret.position).toBe('calc(100% - 8px) 50%');
      expect(caret.origin).toBe('border-box');
    }
    if (row.tableRow) {
      expect(row.actionDisplay).toBe('table-cell');
      expect(Math.max(...row.heights) - Math.min(...row.heights)).toBeLessThanOrEqual(1);
    }
    if (row.buttonTops.length) expect(Math.max(...row.buttonTops) - Math.min(...row.buttonTops)).toBeLessThan(1);
    if (row.selectStyles.length) expect(row.selectStyles[0]).toEqual(row.selectStyles[1]);
    if (row.connection) {
      // 拨号和地址在所有宽度下都共享一行；长地址只能在地址自己的轨道内省略。
      expect(row.connection.dialRight).toBeLessThanOrEqual(row.connection.addressLeft);
      expect(Math.abs(row.connection.dialCenter - row.connection.addressCenter)).toBeLessThanOrEqual(1);
      expect(row.connection.addressRight).toBeLessThanOrEqual(row.connection.connectionRight + 1);
      if (row.connection.overflow > 1 && !row.connection.problem) {
        expect(row.connection.ellipsis).toBe(true);
        expect(row.connection.nowrap).toBe(true);
      }
    }
    if (layout.chain && row.matchBounds.length > 1) {
      const [kind, value] = row.matchBounds;
      const editorWidth = layout.editorWidth ?? Number.POSITIVE_INFINITY;
      if (editorWidth <= 640) {
        // 窄屏字段独占一行，同组控件必须使用完全相同的列线。
        expect(value.top).toBeGreaterThanOrEqual(kind.bottom);
        expect(Math.abs(kind.left - value.left)).toBeLessThan(1);
        expect(Math.abs(kind.width - value.width)).toBeLessThan(1);
      } else {
        if (Math.abs(kind.top - value.top) >= 1) {
          expect(value.top).toBeGreaterThanOrEqual(kind.bottom);
          expect(Math.abs(kind.left - value.left)).toBeLessThan(1);
          expect(Math.abs(kind.width - value.width)).toBeLessThan(1);
        }
        if (editorWidth <= 1120) {
          // 中宽共享 132px 类型列与弹性内容列（与机器 DNS 面板相同），目标框和拨号行不能另猜宽度。
          expect(kind.width).toBe(132);
          expect(value.left).toBeGreaterThanOrEqual(kind.right);
          if (row.actionControl && row.target) {
            expect(Math.abs(row.actionControl.left - kind.left)).toBeLessThan(1);
            expect(Math.abs(row.actionControl.width - kind.width)).toBeLessThan(1);
            expect(Math.abs(row.target.left - value.left)).toBeLessThan(1);
            expect(Math.abs(row.target.right - value.right)).toBeLessThan(1);
          }
          if (row.connection) expect(Math.abs(row.connection.connectionLeft - value.left)).toBeLessThan(1);
        }
      }
    }
    if (layout.chain && (layout.editorWidth ?? Infinity) <= 640 && row.actionControl && row.target) {
      expect(Math.abs(row.actionControl.left - row.target.left)).toBeLessThan(1);
      expect(Math.abs(row.actionControl.width - row.target.width)).toBeLessThan(1);
    }
  }
}

// 机器 DNS 与链路规则共用骨架：中宽「类型列 + 弹性内容列」、窄屏卡片头放操作；控件一律 28px。
async function expectDnsUnifiedLayout(table: Locator) {
  const rows = await table.evaluate(table =>
    [...table.querySelectorAll('tbody > tr')].map(row => {
      const bounds = (selector: string) => {
        const rect = row.querySelector(selector)?.getBoundingClientRect();
        return rect
          ? { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, width: rect.width }
          : null;
      };
      const style = getComputedStyle(row);
      return {
        grid: style.display === 'grid',
        medium: style.gridTemplateAreas.includes('ops'),
        kind: bounds('.rule-match-cell > button.choice-select'),
        content: bounds('.rule-match-cell > .geo-field'),
        choice: bounds('.egress-dns-choice'),
        endpoint: bounds('.egress-dns-endpoint'),
        ops: [...row.querySelectorAll('.dns-row-actions button')].map(button => button.getBoundingClientRect().bottom),
        heights: [...row.querySelectorAll('button.choice-select, input.f')]
          .filter(element => element.getClientRects().length)
          .map(element => element.getBoundingClientRect().height),
      };
    }),
  );
  for (const row of rows) {
    for (const height of row.heights) expect(height).toBeCloseTo(28, 0);
    if (!row.grid || !row.kind || !row.content || !row.choice) continue;
    if (row.medium) {
      expect(row.kind.width).toBeCloseTo(132, 0);
      expect(Math.abs(row.choice.left - row.kind.left)).toBeLessThan(1);
      expect(Math.abs(row.choice.width - row.kind.width)).toBeLessThan(1);
      if (row.endpoint) expect(Math.abs(row.endpoint.left - row.content.left)).toBeLessThan(1);
    } else {
      expect(row.content.top).toBeGreaterThanOrEqual(row.kind.bottom);
      expect(Math.abs(row.kind.width - row.content.width)).toBeLessThan(1);
      expect(Math.abs(row.choice.width - row.kind.width)).toBeLessThan(1);
      for (const bottom of row.ops) expect(bottom).toBeLessThanOrEqual(row.kind.top);
    }
  }
}

async function expectDnsChoiceArrows(table: Locator) {
  const choices = table.locator('button.choice-select');
  expect(await choices.count()).toBeGreaterThan(0);
  for (const choice of await choices.all()) {
    const arrow = choice.locator('.choice-chevron.single svg');
    await expect(arrow).toBeVisible();
    await expect(choice.locator('.choice-chevron br')).toHaveCount(0);
    // Flex 子项会按 CSS 规则块化为 flex；两种值都保留横向 flex 对齐。
    await expect(choice).toHaveCSS('display', /^(?:inline-)?flex$/);
    await arrow.scrollIntoViewIfNeeded();
    const metrics = await arrow.evaluate(icon => {
      const button = icon.closest('button')!;
      const arrowBounds = icon.getBoundingClientRect();
      const buttonBounds = button.getBoundingClientRect();
      const x = arrowBounds.left + arrowBounds.width / 2;
      const y = arrowBounds.top + arrowBounds.height / 2;
      return {
        width: arrowBounds.width,
        height: arrowBounds.height,
        centerOffset: Math.abs(y - (buttonBounds.top + buttonBounds.height / 2)),
        rightInset: buttonBounds.right - arrowBounds.right,
        arrowHitsButton: document.elementFromPoint(x, y) === button,
      };
    });
    expect(metrics.width).toBe(14);
    expect(metrics.height).toBe(14);
    expect(metrics.centerOffset).toBeLessThanOrEqual(1);
    expect(metrics.rightInset).toBeCloseTo(8, 1);
    expect(metrics.arrowHitsButton).toBe(true);
  }
}

const webkitTest = test.extend({ browserName: 'webkit' });
webkitTest.describe('touch WebKit match-type select', () => {
  const { viewport, screen, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices['iPhone 13'];
  webkitTest.use({
    viewport,
    screen,
    userAgent,
    deviceScaleFactor,
    isMobile,
    hasTouch,
    reducedMotion: 'no-preference',
  });

  webkitTest('keeps the arrow anchored to the native select', async ({ page }) => {
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&theme=light');
    const kind = page.getByRole('combobox', { name: '匹配类型（规则 2）' });
    const cell = page.locator('.rule-match-cell').filter({ has: kind });
    const arrow = cell.locator('.rule-match-kind-chevron svg');
    await expect(arrow).toBeVisible();
    await expect(kind).toHaveCSS('background-image', 'none');
    await expect(kind).toHaveValue('geosite');
    await waitForVisualAssets(page);
    await expect(cell).toHaveScreenshot('iphone-match-type-and-geo-content.png');
    for (const value of ['geoip', 'port', 'geosite']) {
      await kind.selectOption(value);
      await expect(kind).toHaveValue(value);
      await expect(cell.locator(value === 'port' ? 'input.f' : '.geo-select')).toBeVisible();
      const { width, height, centerOffset, rightInset, arrowHitsSelect } = await arrow.evaluate(icon => {
        const select = icon.closest('.rule-match-kind')!.querySelector('select')!;
        const a = icon.getBoundingClientRect();
        const b = select.getBoundingClientRect();
        return {
          width: a.width,
          height: a.height,
          centerOffset: Math.abs(a.top + a.height / 2 - (b.top + b.height / 2)),
          rightInset: b.right - a.right,
          arrowHitsSelect: document.elementFromPoint(a.left + a.width / 2, a.top + a.height / 2) === select,
        };
      });
      expect(width).toBe(14);
      expect(height).toBe(14);
      expect(centerOffset).toBeLessThanOrEqual(0.5);
      expect(rightInset).toBeCloseTo(8);
      expect(arrowHitsSelect).toBe(true);
    }
  });
  webkitTest('keeps custom DNS choice arrows anchored in the machine rules', async ({ page }) => {
    await page.goto('/tests/fixtures/dns-groups-preview.html?theme=light&scene=editing');
    const table = page.locator('.node-egress-rules-table');
    await expect(table.locator('tbody > tr')).toHaveCount(3);
    await waitForVisualAssets(page);
    await expectDnsChoiceArrows(table);
    await expect(table.locator('tbody > tr').first()).toHaveScreenshot('iphone-dns-choice-arrows.png');
  });
});

for (const width of [390, 820, 1280]) {
  webkitTest(`WebKit chain rules fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/rule-coverage-preview.html?layout');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    await waitForVisualAssets(page);
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
      await waitForVisualAssets(page);
      await expectRuleLayout(table);
      await expect(table).toHaveScreenshot(`rules-${width}-${theme}.png`);
    });
  }
}

test('chain rules adapt to a narrow panel inside a desktop viewport', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1000 });
  await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&container=860&longName');
  await expect(page.locator('.rule-table tbody > tr')).toHaveCount(5);
  await waitForVisualAssets(page);
  await expectRuleLayout(page.locator('.rule-table'));
  await expect(page.locator('.rule-table')).toHaveScreenshot('rules-desktop-narrow-panel.png');
});

for (const width of [390, 670, 820, 1050]) {
  test(`shared DNS rule controls fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 2000 });
    await page.goto('/tests/fixtures/dns-groups-preview.html?theme=dark&scene=editing');
    const table = page.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(3);
    await waitForVisualAssets(page);
    await expectRuleLayout(table);
    await expectDnsUnifiedLayout(table);
    await expectDnsChoiceArrows(table);
    await expect(table).toHaveScreenshot(`dns-rules-${width}.png`);
  });
}

// 断点量的是每个规则编辑器的内容盒，而不是视口或设备类型。
for (const width of [340, 341, 640, 641, 700, 701, 1120, 1121]) {
  test(`chain rules at the ${width}px editor-content boundary`, async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 2000 });
    await page.goto(`/tests/fixtures/rule-coverage-preview.html?layout&container=${width}`);
    const tree = page.locator('.chain-rule-tree');
    const panel = page.locator('.rule-sheet-card');
    const editor = tree.locator('.rule-editor');
    const table = tree.locator('.rule-table');
    await expect(table.locator('tbody > tr')).toHaveCount(5);
    const adjustment = await editor.evaluate(
      (element, target) => target - parseFloat(getComputedStyle(element).width),
      width,
    );
    await panel.evaluate((element, difference) => {
      const current = parseFloat(getComputedStyle(element).maxWidth);
      element.style.maxWidth = `${current + difference}px`;
    }, adjustment);
    await expect
      .poll(() => editor.evaluate(element => Math.round(parseFloat(getComputedStyle(element).width))))
      .toBe(width);
    await waitForVisualAssets(page);
    await expect(editor).toHaveAttribute('data-rule-flow', /^(inline|pairs|stacked)$/);
    await expectRuleLayout(table);
    await expect(table.locator('tbody > tr').first()).toHaveCSS('display', width <= 1120 ? 'grid' : 'table-row');
    await expect(table.locator('.rule-action-cell').first()).toHaveCSS(
      'display',
      width <= 1120 ? 'block' : 'table-cell',
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
    await waitForVisualAssets(page);
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
    await waitForVisualAssets(page);
    await expectRuleLayout(table);
    await expect(page.locator('.rule-editor')).toHaveScreenshot(`chain-dns-rules-${width}.png`);
    await table.getByRole('button', { name: '查看本机 DNS 解析策略' }).first().click();
    const dns = page.getByRole('dialog', { name: /的本机 DNS 解析策略$/ });
    await expect(dns.locator('.rule-dns-nav-row')).toHaveCount(1);
    await expect(dns.getByRole('textbox', { name: /^DNS 地址/ })).toHaveValue('192.0.2.53, 2001:db8::53');
    await expect(dns.locator('.rule-dns-dialog')).toHaveScreenshot(`chain-dns-policies-${width}.png`);
  });
}

async function openDnsInspection(page: Page, readOnly: boolean) {
  await page.goto(
    `/tests/fixtures/rule-coverage-preview.html?layout&dns&dnsInspection&theme=light${readOnly ? '&role=readonly' : ''}`,
  );
  await page.getByRole('button', { name: '查看本机 DNS 解析策略' }).first().click();
  const dialog = page.getByRole('dialog', { name: /的本机 DNS 解析策略$/ });
  await expect(dialog.locator('.rule-dns-nav-row')).toHaveCount(1);
  await waitForVisualAssets(page);
  return dialog;
}

for (const width of [320, 390, 768, 980, 1440]) {
  test(`read-only DNS inspection reuses the disabled editing controls at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 760 });
    const dialog = await openDnsInspection(page, true);
    const address = dialog.getByRole('textbox', { name: /^DNS 地址/ });
    await expect(address).toHaveValue('192.0.2.53, 198.51.100.53, 2001:db8:1234:5678:90ab:cdef:1234:5678');
    await expect(address).toBeDisabled();
    await expect(dialog.getByRole('combobox', { name: /^失败处理/ })).toHaveValue('machine');
    await expect(dialog.getByRole('combobox', { name: /^失败处理/ })).toBeDisabled();
    if (width === 320 || width === 1440)
      await expect(dialog.locator('.rule-dns-dialog')).toHaveScreenshot(`dns-inspection-${width}.png`);
    await dialog.getByRole('button', { name: '关闭', exact: true }).last().click();
    await expect(dialog).toBeHidden();
  });
}

webkitTest('DNS options stay readable while editing at 320px', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 760 });
  const dialog = await openDnsInspection(page, false);
  const fallback = dialog.getByRole('combobox', { name: /^失败处理/ });
  await expect(fallback).toHaveValue('machine');
  const fits = await fallback.evaluate(select => {
    if (!(select instanceof HTMLSelectElement)) return false;
    const bounds = select.getBoundingClientRect();
    const style = getComputedStyle(select);
    const canvas = document.createElement('canvas').getContext('2d')!;
    canvas.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
    const available = select.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    const required = canvas.measureText(select.selectedOptions[0].textContent ?? '').width;
    return available + 1 >= required && bounds.right <= select.parentElement!.getBoundingClientRect().right + 1;
  });
  expect(fits).toBe(true);
  await fallback.selectOption('stop');
  await expect(fallback).toHaveValue('stop');
});

test('machine details display all stored addresses in read-only rows at 320px', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 760 });
  await page.goto('/tests/fixtures/dns-groups-preview.html?scene=editing&theme=light&role=readonly');
  const table = page.locator('.node-egress-rules-table');
  const address = table.getByRole('textbox', { name: /^DNS 地址/ }).first();
  await expect(address).toHaveValue('192.0.2.53, 198.51.100.53, 2001:db8::53');
  await expect(address).toBeDisabled();
  await waitForVisualAssets(page);
  await expect(table).toHaveScreenshot('machine-dns-readonly-320.png');
});

// 访客与登录看到同一套控件，只是禁用：列宽、控件底色和不透明度都与编辑态一致。
for (const width of [390, 820, 1280]) {
  test(`guest machine DNS matches the signed-in layout at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 760 });
    const measure = async (role: string) => {
      await page.goto(`/tests/fixtures/dns-groups-preview.html?scene=editing&theme=light${role}`);
      const table = page.locator('.node-egress-rules-table');
      await expect(table.locator('.geo-chip').first()).toBeVisible();
      await waitForVisualAssets(page);
      return table.evaluate(element => {
        const row = element.querySelector('tbody > tr')!;
        const choice = row.querySelector('button.choice-select')!;
        const style = getComputedStyle(choice);
        return {
          kind: Math.round(row.querySelector('.rule-match-cell > button.choice-select')!.getBoundingClientRect().width),
          content: Math.round(row.querySelector('.rule-match-cell > .geo-field')!.getBoundingClientRect().left),
          controls: row.querySelectorAll('.rule-action-cell :is(button.choice-select, input)').length,
          background: style.backgroundColor,
          opacity: style.opacity,
        };
      });
    };
    const signedIn = await measure('');
    const guest = await measure('&role=readonly');
    await expect(page.locator('.node-egress-rules-table button.choice-select').first()).toBeDisabled();
    expect(guest.controls).toBe(signedIn.controls);
    expect(guest.background).toBe(signedIn.background);
    expect(guest.opacity).toBe('1');
    expect(Math.abs(guest.kind - signedIn.kind)).toBeLessThanOrEqual(1);
    expect(Math.abs(guest.content - signedIn.content)).toBeLessThanOrEqual(1);
  });
}

test('geo chips and empty input share the ordinary input text inset', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1200 });
  await page.goto('/tests/fixtures/rule-coverage-preview.html?layout&theme=light');
  const rows = page.locator('.chain-rule-table tbody > tr');
  await expect(rows).toHaveCount(5);
  await waitForVisualAssets(page);
  const ordinaryInput = rows.nth(0).locator('.rule-match-cell input.f');
  const textLeft = (element: HTMLElement) => {
    const style = getComputedStyle(element);
    return element.getBoundingClientRect().left + parseFloat(style.borderLeftWidth) + parseFloat(style.paddingLeft);
  };
  const ordinaryLeft = await ordinaryInput.evaluate(textLeft);
  const chip = rows.nth(1).locator('.geo-chip').first();
  await expect(chip).toBeVisible();
  await expect(chip.locator('..')).toHaveCSS('display', 'grid');
  await expect(chip).toHaveCSS('padding-left', '3px');
  await expect(chip).toHaveCSS('padding-right', '3px');
  // 徽章让位靠输入框左内距，不用负外边距：负外边距会让 max-content 列比徽章窄，宽输入框里的名字也被截断。
  await expect(chip).toHaveCSS('margin-left', '0px');
  await expect(chip.locator('..')).toHaveCSS('padding-left', '3px');
  expect(
    await chip
      .locator('.geo-chip-name > span:last-child')
      .evaluate(element => element.scrollWidth - element.clientWidth),
  ).toBeLessThanOrEqual(0);
  const expectChipCentered = async () => {
    const spacing = await chip.evaluate(element => {
      const chipBounds = element.getBoundingClientRect();
      const selectBounds = element.parentElement!.getBoundingClientRect();
      return {
        height: chipBounds.height,
        above: chipBounds.top - selectBounds.top,
        below: selectBounds.bottom - chipBounds.bottom,
      };
    });
    expect(spacing.height).toBe(20);
    expect(Math.abs(spacing.above - spacing.below)).toBeLessThanOrEqual(1);
  };
  await expectChipCentered();
  const glyph = chip.locator('.geo-chip-name > :first-child');
  const glyphLeft = await glyph.evaluate(element => element.getBoundingClientRect().left);
  expect(Math.abs(glyphLeft - ordinaryLeft)).toBeLessThanOrEqual(1);
  await expect(page.locator('.chain-rule-table')).toHaveScreenshot('geo-selector-grid-alignment.png');

  await rows.nth(0).getByRole('combobox', { name: '匹配类型（规则 1）' }).selectOption('geosite');
  const emptySelect = rows.nth(0).locator('.geo-select');
  const emptyInput = emptySelect.locator('input');
  await expect(emptyInput).toBeVisible();
  await expect(emptySelect).toHaveCSS('display', 'flex');
  const emptyLeft = await emptyInput.evaluate(textLeft);
  expect(Math.abs(emptyLeft - ordinaryLeft)).toBeLessThanOrEqual(1);

  await page.setViewportSize({ width: 390, height: 1200 });
  await waitForVisualAssets(page);
  await expectChipCentered();
  await expect(rows.nth(1).locator('.geo-select')).toHaveScreenshot('geo-chip-centering-390.png');
});

async function expandChainRules(page: Page) {
  await page.getByRole('switch', { checked: true }).click();
  const collapsed = page.locator('.chain-rule-node-head[aria-expanded="false"]');
  while (await collapsed.count()) await collapsed.first().click();
}

for (const width of [390, 1440]) {
  test(`compact listener and plain local route fit ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1600 });
    await page.goto('/tests/fixtures/rule-layout-preview.html?theme=light&local');
    await expandChainRules(page);
    const editor = page.locator('[data-rule-location$="/hk-akile"]').first();
    const target = editor.locator('.listener-target');
    const compact = target.locator('.listener-compact-label');
    const port = target.locator('.listener-compact-port');
    const route = editor.locator('.listener-local-route');
    await expect(compact).toBeVisible();
    const chevron = target.locator('.external-target-chevron');
    await expect(chevron.locator('svg')).toBeVisible();
    const arrowCenterDifference = await chevron.evaluate(element => {
      const arrow = element.querySelector('svg')!.getBoundingClientRect();
      const target = element.closest('.external-target-trigger')!.getBoundingClientRect();
      return Math.abs(arrow.top + arrow.height / 2 - (target.top + target.height / 2));
    });
    expect(arrowCenterDifference).toBeLessThanOrEqual(1);
    await expect(port).toBeVisible();
    await expect(port).toHaveText(':20001');
    await expect(target.locator('.external-target-copy > b:not(.listener-compact-label)')).toBeHidden();
    await expect(route).toHaveText('本机回环:20001');
    await waitForVisualAssets(page);
    const singleLine = await target.evaluate(element => {
      const name = element.querySelector('.listener-compact-name')!.getBoundingClientRect();
      const port = element.querySelector('.listener-compact-port')!.getBoundingClientRect();
      return {
        topDifference: Math.abs(name.top - port.top),
        portRight: port.right,
        targetRight: element.getBoundingClientRect().right,
      };
    });
    expect(singleLine.topDifference).toBeLessThanOrEqual(1);
    expect(singleLine.portRight).toBeLessThanOrEqual(singleLine.targetRight + 1);
    const routeStyle = await route.evaluate(element => {
      const style = getComputedStyle(element);
      return {
        background: style.backgroundColor,
        border: style.borderTopWidth,
        radius: style.borderRadius,
        padding: style.padding,
        minHeight: style.minHeight,
        color: style.color,
        font: `${style.fontFamily} ${style.fontSize}`,
      };
    });
    expect(routeStyle.background).toMatch(/^(transparent|rgba\(0, 0, 0, 0\))$/);
    expect(routeStyle.border).toBe('0px');
    expect(routeStyle.radius).toBe('0px');
    expect(routeStyle.padding).toBe('0px');
    expect(routeStyle.minHeight).toBe('0px');
    const ordinaryAddress = page.locator('.rule-dial-address').first();
    await expect(ordinaryAddress).toBeVisible();
    const ordinaryFont = await ordinaryAddress.evaluate(
      element => `${getComputedStyle(element).fontFamily} ${getComputedStyle(element).fontSize}`,
    );
    expect(routeStyle.font).toBe(ordinaryFont);
    const ordinaryColor = await editor.evaluate(element => {
      const probe = document.createElement('span');
      probe.style.color = 'var(--ink-2)';
      element.append(probe);
      const color = getComputedStyle(probe).color;
      probe.remove();
      return color;
    });
    expect(routeStyle.color).toBe(ordinaryColor);
    await expectRuleLayout(editor.locator('.chain-rule-table'));
    await expect(editor).toHaveScreenshot(`local-loopback-${width}.png`);
  });
}
