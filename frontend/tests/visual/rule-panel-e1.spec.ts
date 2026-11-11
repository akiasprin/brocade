import { expect, test, type Page } from '@playwright/test';

async function open(page: Page, width: number, theme = 'light', extra = '') {
  await page.setViewportSize({ width, height: 900 });
  await page.goto(`/tests/fixtures/rule-layout-preview.html?theme=${theme}${extra}`);
  await expect(page.locator('.chain-rule-node-head')).not.toHaveCount(0);
  // 链路规则默认只展开当前机器；这组截图要看整棵树，逐台展开。
  await expect(page.locator('.chain-rule-node-head[aria-expanded="true"]')).toHaveCount(1);
  const collapsed = page.locator('.chain-rule-node-head[aria-expanded="false"]');
  while ((await collapsed.count()) > 0) await collapsed.first().click();
  await page.evaluate(() => document.fonts.ready);
}

function head(page: Page, name: string) {
  return page.locator('.chain-rule-node-head').filter({ has: page.getByText(name, { exact: true }) });
}

// 机器树嵌套：子机器在父机器的 .chain-rule-children 里，按名字过滤节点会连带祖先，所以从行头取紧随的正文。
function machine(page: Page, name: string) {
  return head(page, name).locator(
    'xpath=following-sibling::div[contains(concat(" ", @class, " "), " chain-rule-body ")]',
  );
}

async function fits(page: Page) {
  const layout = await page.locator('.chain-rule-tree').evaluate(root => ({
    overflow: root.scrollWidth - root.clientWidth,
    bodyOverflow: document.documentElement.scrollWidth - innerWidth,
    controls: [...root.querySelectorAll<HTMLElement>('input,button,select')]
      .filter(el => el.getClientRects().length)
      .map(el => ({ right: el.getBoundingClientRect().right, left: el.getBoundingClientRect().left })),
  }));
  expect(layout.overflow).toBeLessThanOrEqual(1);
  expect(layout.bodyOverflow).toBeLessThanOrEqual(1);
  expect(layout.controls.length).toBeGreaterThan(0);
  const width = page.viewportSize()!.width;
  for (const control of layout.controls) {
    expect(control.right).toBeLessThanOrEqual(width);
    expect(control.left).toBeGreaterThanOrEqual(0);
  }
}

for (const width of [390, 820, 1440])
  for (const theme of ['light', 'dark']) {
    test(`E1 machine tree ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      await expect(page.locator('.chain-rule-node-head[aria-expanded="true"]')).toHaveCount(3);
      await fits(page);
      await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`e1-tree-${width}-${theme}.png`);
    });
  }

for (const width of [390, 1440])
  for (const theme of ['light', 'dark']) {
    test(`E1 target, dial and DNS layers ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      const akile = machine(page, '香港 AKILE II');
      const target = akile.locator('.external-target-trigger');
      await target.click();
      await expect(page.getByRole('textbox', { name: '搜索转发目标' })).toBeFocused();
      await expect(page.getByRole('dialog')).toHaveScreenshot(`e1-target-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(target).toBeFocused();

      const dial = akile.getByRole('combobox', { name: '连接路径' });
      await dial.click();
      const menu = page.getByRole('listbox', { name: '连接路径' });
      await expect(menu.locator(':focus')).toHaveCount(1);
      await expect(menu.getByRole('option', { selected: true })).toHaveText(/走 XRAY 公网 IPv6/);
      await expect(menu.getByRole('textbox', { name: '自定义地址' })).toBeVisible();
      await expect(menu).toHaveScreenshot(`e1-dial-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(menu).toHaveCount(0);
      await expect(dial).toBeFocused();

      await akile.getByRole('button', { name: '查看本机 DNS 解析策略' }).click();
      const dns = page.getByRole('dialog', { name: '香港 AKILE II 的本机 DNS 解析策略' });
      await expect(dns.locator('.rule-dns-row')).toHaveCount(6);
      const box = await dns.locator('.rule-dns-dialog').boundingBox();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(width);
      await expect(dns.locator('.rule-dns-dialog')).toHaveScreenshot(`e1-dns-${width}-${theme}.png`);
      await dns.getByRole('button', { name: '完成' }).click();
      await expect(dns).toHaveCount(0);
    });
  }

test('E1 dial menu writes a custom address and shows it inline', async ({ page }) => {
  await open(page, 1440);
  const akile = machine(page, '香港 AKILE II');
  const dial = akile.getByRole('combobox', { name: '连接路径' });
  await expect(dial).toHaveText('走 XRAY 公网 IPv6');
  await dial.click();
  const address = page.getByRole('listbox', { name: '连接路径' }).getByRole('textbox', { name: '自定义地址' });
  await address.fill('198.51.100.7');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('listbox')).toHaveCount(0);
  await expect(dial).toBeFocused();
  await expect(dial).toHaveText('自定义');
  const cell = akile.locator('.rule-action-cell').first();
  // 端口跟随目标监听，地址栏只显示主机。
  await expect(cell.locator('.rule-dial-address')).toHaveText('198.51.100.7');
  const line = await cell.evaluate(element => {
    const tops = [...element.querySelectorAll<HTMLElement>(':scope > *')]
      .filter(child => child.getClientRects().length)
      .map(child => Math.round(child.getBoundingClientRect().top));
    return { rows: new Set(tops).size, overflow: element.scrollWidth - element.clientWidth };
  });
  expect(line).toEqual({ rows: 1, overflow: 0 });
});

test('E1 keeps edits while a machine is collapsed and restores all editor state', async ({ page }) => {
  await open(page, 1440);
  const akileHead = head(page, '香港 AKILE II');
  const akile = machine(page, '香港 AKILE II');
  const match = akile.getByRole('textbox', { name: '匹配内容（规则 1）' });
  await match.fill('netflix');
  const port = akile.locator('.hop-target-port input');
  await port.fill('24000');
  await akileHead.click();
  await expect(akileHead).toHaveAttribute('aria-expanded', 'false');
  await expect(akile).toBeHidden();
  await expect(head(page, '台湾 AKILE HiNET')).toBeVisible();
  await akileHead.press('Enter');
  await expect(akileHead).toHaveAttribute('aria-expanded', 'true');
  await expect(match).toHaveValue('netflix');
  await expect(port).toHaveValue('24000');
  await page.getByRole('button', { name: '还原', exact: true }).click();
  await expect(match).toHaveValue('category-ai-!cn');
  await expect(port).toHaveValue('20002');
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeDisabled();
});

test('E1 keyboard searches a target and stages connection and downstream edits together', async ({ page }) => {
  await open(page, 1440);
  const akile = machine(page, '香港 AKILE II');
  await akile.locator('.external-target-trigger').click();
  await page.getByRole('textbox', { name: '搜索转发目标' }).fill('台湾');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await akile.getByRole('combobox', { name: '连接路径' }).click();
  await page.getByRole('option', { name: /公网 IPv4/ }).click();
  const taiwan = machine(page, '台湾 AKILE HiNET');
  await taiwan.getByRole('button', { name: '＋ 加一条' }).click();
  await taiwan.getByRole('textbox', { name: '匹配内容（规则 1）' }).fill('example.com');
  await page.getByRole('button', { name: '保存到草稿' }).click();
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { ruleLayoutDraftOps: () => unknown[] }).ruleLayoutDraftOps()),
    )
    .toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          op: 'put_step',
          node_id: 'hk-akile',
          step: expect.objectContaining({
            rules: expect.arrayContaining([
              expect.objectContaining({ a: expect.objectContaining({ dial: { t: 'addr', v: '192.0.2.88:20002' } }) }),
            ]),
          }),
        }),
        expect.objectContaining({
          op: 'put_step',
          node_id: 'tw-hinet',
          step: expect.objectContaining({
            rules: expect.arrayContaining([expect.objectContaining({ m: { t: 'domain_suffix', v: ['example.com'] } })]),
          }),
        }),
      ]),
    );
});

test('E1 readonly expands machines with disabled editors and no removal action', async ({ page }) => {
  await open(page, 390, 'light', '&role=readonly');
  const akile = machine(page, '香港 AKILE II');
  await expect(akile.getByRole('combobox', { name: '处理方式（规则 1）' })).toBeDisabled();
  await expect(akile.locator('.hop-target-port input')).toBeDisabled();
  await expect(page.getByRole('button', { name: /从这条链移除/ })).toHaveCount(0);
  await akile.getByRole('button', { name: '查看本机 DNS 解析策略' }).click();
  const dns = page.getByRole('dialog', { name: '香港 AKILE II 的本机 DNS 解析策略' });
  await expect(dns.getByRole('textbox', { name: /^DNS 地址/ }).first()).toBeDisabled();
  await expect(dns.getByRole('button', { name: '＋ 添加策略' })).toHaveCount(0);
  await dns.getByRole('button', { name: '关闭', exact: true }).last().click();
  await expect(dns).toHaveCount(0);
});

test('E1 fits a narrow panel in a desktop viewport', async ({ page }) => {
  await open(page, 1920, 'dark', '&container=860');
  await fits(page);
  await expect(page.locator('.rule-sheet-card')).toHaveScreenshot('e1-narrow.png');
});

for (const scene of ['shared', 'reverse', 'listener']) {
  test(`E1 ${scene} next hops retain their ownership`, async ({ page }) => {
    await open(page, 1440, 'light', `&${scene}`);
    const akile = machine(page, '香港 AKILE II');
    const hops = akile.locator('.rule-next-hops');
    await expect(hops.locator('.listener-reference-row')).toHaveCount(1);
    if (scene === 'shared') {
      await expect(akile.locator('.rule-table .rule-dial-select')).toHaveCount(2);
      await expect(hops).toContainText('台湾 AKILE HiNET · TCP 20002');
    } else if (scene === 'reverse') {
      await expect(hops.locator('.hop-target-port')).toContainText('本机端口');
      await expect(hops).toContainText('台湾 AKILE HiNET 从这里接入');
      await expect(akile.getByRole('combobox', { name: '连接路径' })).toContainText('反向 IPv6');
    } else {
      await expect(hops).toContainText('TCP 23000');
      await expect(hops.locator('.hop-target-port')).toHaveCount(0);
      await expect(hops).not.toContainText('不可发布');
      await expect(akile.getByRole('combobox', { name: '连接路径' })).toContainText('公网 IPv4');
    }
    await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`e1-${scene}-next-hops.png`);
  });
}

test('E1 keyboard adds a new machine, and restore removes its unsaved subtree', async ({ page }) => {
  await open(page, 1440);
  const akile = machine(page, '香港 AKILE II');
  await akile.locator('.external-target-trigger').click();
  await page.getByRole('textbox', { name: '搜索转发目标' }).fill('日本');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(akile.locator('.external-target-trigger')).toContainText('日本 IIJ');
  await expect(head(page, '日本 IIJ')).toHaveCount(1);
  // 台湾仍有本链的规则表，只是不再从入口走到：保存前留在「未从入口展开」里。
  await expect(page.locator('.chain-rule-orphans')).toContainText('台湾 AKILE HiNET');
  await page.getByRole('button', { name: '还原', exact: true }).click();
  await expect(akile.locator('.external-target-trigger')).toContainText('台湾 AKILE HiNET');
  await expect(head(page, '日本 IIJ')).toHaveCount(0);
  await expect(page.locator('.chain-rule-orphans')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeDisabled();
});
