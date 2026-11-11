import { expect, test, type Page } from '@playwright/test';

// 新建链的「路径」与链路规则渲染同一套部件：规则表、目标菜单、拨号菜单、下一跳与 DNS 弹层。
// 整棵树一屏放下：向导的页头和页脚吸附在视口上，截长元素时会盖住其中一段。
async function open(page: Page, width: number, theme: string, height = 900) {
  await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
  await page.setViewportSize({ width, height });
  await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#/nodes/chain/jp-iij`);
  await expect(page.getByRole('heading', { name: '新建链', exact: true })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

const entry = (page: Page) => page.locator('.wzr-tree .chain-rule-tree > .chain-rule-node > .chain-rule-body');

// 入口加一条转发（菜单自动打开后收起），再加一条域名出网规则，最后在主干末尾加一跳。
async function build(page: Page) {
  const rules = entry(page);
  await rules.getByRole('button', { name: '＋ 加一条' }).click();
  await rules.getByRole('combobox', { name: /第 1 条动作/ }).selectOption('forward');
  await expect(page.locator('.external-target-menu')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('.external-target-menu')).toHaveCount(0);
  await rules.getByRole('button', { name: '＋ 加一条' }).click();
  await rules.getByRole('textbox', { name: /第 2 条匹配取值/ }).fill('netflix');
  const hop = page.getByRole('combobox', { name: '在路径末尾添加机器' });
  const candidate = await hop.locator('option').nth(1).getAttribute('value');
  await hop.selectOption(candidate!);
}

async function fits(page: Page) {
  const layout = await page.locator('.wzr-tree').evaluate(root => ({
    overflow: root.scrollWidth - root.clientWidth,
    bodyOverflow: document.documentElement.scrollWidth - innerWidth,
    controls: [...root.querySelectorAll<HTMLElement>('input,button,select')]
      .filter(element => element.getClientRects().length)
      .map(element => element.getBoundingClientRect())
      .map(rect => ({ left: rect.left, right: rect.right })),
  }));
  expect(layout.overflow).toBeLessThanOrEqual(1);
  expect(layout.bodyOverflow).toBeLessThanOrEqual(1);
  expect(layout.controls.length).toBeGreaterThan(0);
  const width = page.viewportSize()!.width;
  for (const control of layout.controls) {
    expect(control.left).toBeGreaterThanOrEqual(0);
    expect(control.right).toBeLessThanOrEqual(width);
  }
}

for (const width of [390, 820, 1440])
  for (const theme of ['light', 'dark']) {
    test(`wizard path matches chain rules ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme, 2600);
      await build(page);
      const tree = page.locator('.wzr-tree .chain-rule-tree');
      // 入口、改成转发时带出的目标和末尾新加的一跳都是刚加入路径的机器，默认展开；结构与链路规则一致。
      await expect(tree.locator('.chain-rule-node-head[aria-expanded="true"]')).toHaveCount(3);
      await expect(tree.locator('table.chain-rule-table:visible')).toHaveCount(3);
      await expect(entry(page).locator('.rule-ops-cell')).toHaveCount(3);
      await expect(entry(page).locator('.rule-next-hops')).toContainText('下一跳');
      await expect(entry(page).getByRole('combobox', { name: /的连接方式$/ })).toHaveCount(2);
      await expect(entry(page).locator('select.egress-dns-choice')).toHaveCount(1);
      await fits(page);
      await expect(page.locator('.wzr-tree')).toHaveScreenshot(`wizard-path-${width}-${theme}.png`);
    });
  }

for (const width of [390, 1440])
  for (const theme of ['light', 'dark']) {
    test(`wizard path menus and DNS layer ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      await build(page);
      const rules = entry(page);

      const target = rules.getByRole('button', { name: /第 1 条转发目标/ });
      await target.click();
      await expect(page.getByRole('textbox', { name: '搜索转发目标' })).toBeFocused();
      const labels = await page.locator('.external-target-menu-label').allTextContents();
      expect(labels.slice(0, 2)).toEqual(['BROCADE 节点', '在机器上新建本链监听']);
      expect(labels).toContain('外部出站');
      await expect(page.locator('.dialog-surface.external-target-menu')).toHaveScreenshot(
        `wizard-target-${width}-${theme}.png`,
      );
      await page.keyboard.press('Escape');
      await expect(target).toBeFocused();

      const dial = rules.getByRole('combobox', { name: /的连接方式$/ }).first();
      await dial.click();
      const menu = page.getByRole('listbox');
      await expect(menu.getByRole('textbox', { name: '自定义地址' })).toBeVisible();
      await expect(menu).toHaveScreenshot(`wizard-dial-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(menu).toHaveCount(0);

      await rules.getByRole('combobox', { name: /DNS 解析方式（新建链：.* 第 2 条）/ }).selectOption('custom');
      const dns = page.getByRole('dialog', { name: /的本机 DNS 解析策略$/ });
      await expect(dns.locator('.rule-dns-row')).toHaveCount(1);
      await dns.getByRole('textbox', { name: /^DNS 地址/ }).fill('1.1.1.1');
      await expect(dns.locator('.rule-dns-dialog')).toHaveScreenshot(`wizard-dns-${width}-${theme}.png`);
      await dns.getByRole('button', { name: '完成' }).click();
      await expect(dns).toHaveCount(0);
      await expect(rules.getByRole('button', { name: '查看本机 DNS 解析策略' }).first()).toBeVisible();
    });
  }
