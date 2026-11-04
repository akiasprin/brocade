import { expect, test, type Page } from '@playwright/test';

async function open(page: Page, width: number, theme = 'light', extra = '') {
  await page.setViewportSize({ width, height: 900 });
  await page.goto(`/tests/fixtures/rule-layout-preview.html?theme=${theme}${extra}`);
  await page.locator('.rule-machine-node').filter({ hasText: '香港 AKILE II' }).click();
  await page.evaluate(() => document.fonts.ready);
}

async function fits(page: Page) {
  const layout = await page.locator('.rule-machine-detail').evaluate(root => ({
    overflow: root.scrollWidth - root.clientWidth,
    bodyOverflow: document.documentElement.scrollWidth - innerWidth,
    controls: [...root.querySelectorAll<HTMLElement>('input,button')]
      .filter(el => el.getClientRects().length)
      .map(el => ({ right: el.getBoundingClientRect().right, left: el.getBoundingClientRect().left })),
  }));
  expect(layout.overflow).toBeLessThanOrEqual(1);
  expect(layout.bodyOverflow).toBeLessThanOrEqual(1);
  const width = page.viewportSize()!.width;
  for (const control of layout.controls) {
    expect(control.right).toBeLessThanOrEqual(width);
    expect(control.left).toBeGreaterThanOrEqual(0);
  }
}

for (const width of [390, 820, 1440])
  for (const theme of ['light', 'dark']) {
    test(`B machine workspace ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      for (const tab of ['规则', '连接', 'DNS 解析']) {
        await page.getByRole('button', { name: new RegExp(`^${tab}`) }).click();
        await fits(page);
        await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`b-${tab}-${width}-${theme}.png`);
      }
      if (width <= 820) {
        await page.getByRole('button', { name: '‹ 机器', exact: true }).click();
        await expect(page.locator('.rule-machine-tree')).toBeVisible();
        await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`b-machines-${width}-${theme}.png`);
      }
    });
  }

for (const width of [390, 1440])
  for (const theme of ['light', 'dark']) {
    test(`B selection and action menus ${width} ${theme}`, async ({ page }) => {
      await open(page, width, theme);
      const target = page.locator('.rule-editor:visible .external-target-trigger').first();
      await target.click();
      await expect(page.getByRole('textbox', { name: '搜索转发目标' })).toBeFocused();
      await expect(page.getByRole('dialog')).toHaveScreenshot(`b-target-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await expect(target).toBeFocused();
      await page.getByRole('button', { name: '香港 AKILE II 的更多操作' }).click();
      await expect(page.getByRole('menu')).toHaveScreenshot(`b-actions-${width}-${theme}.png`);
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: /^连接/ }).click();
      await page.getByRole('combobox', { name: '连接路径' }).click();
      await expect(page.getByRole('listbox')).toHaveScreenshot(`b-dial-${width}-${theme}.png`);
    });
  }

test('B keeps edits across machines and tabs and restores all editor state', async ({ page }) => {
  await open(page, 1440);
  const match = page.getByRole('textbox', { name: '匹配内容（规则 1）' });
  await match.fill('netflix');
  await page.getByRole('button', { name: /^连接/ }).click();
  await page.locator('.rule-editor:visible .hop-target-port input').fill('24000');
  await page.locator('.rule-machine-node').filter({ hasText: '台湾 AKILE HiNET' }).click();
  await page.locator('.rule-machine-node').filter({ hasText: '香港 AKILE II' }).click();
  await expect(page.locator('.rule-editor:visible .hop-target-port input')).toHaveValue('24000');
  await page.getByRole('button', { name: /^规则/ }).click();
  await expect(match).toHaveValue('netflix');
  await page.getByRole('button', { name: '还原', exact: true }).click();
  await expect(match).toHaveValue('category-ai-!cn');
  await page.getByRole('button', { name: /^连接/ }).click();
  await expect(page.locator('.rule-editor:visible .hop-target-port input')).toHaveValue('20002');
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeDisabled();
});

test('B keyboard searches a target and stages connection and downstream edits together', async ({ page }) => {
  await open(page, 1440);
  await page.locator('.rule-editor:visible .external-target-trigger').click();
  await page.getByRole('textbox', { name: '搜索转发目标' }).fill('台湾');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('button', { name: /^连接/ }).click();
  await page.getByRole('combobox', { name: '连接路径' }).click();
  await page.getByRole('option', { name: /公网 IPv4/ }).click();
  await page.locator('.rule-machine-node').filter({ hasText: '台湾 AKILE HiNET' }).click();
  await page.getByRole('button', { name: /^规则/ }).click();
  await page.getByRole('button', { name: '＋ 添加规则' }).click();
  await page.getByRole('textbox', { name: '匹配内容（规则 1）' }).fill('example.com');
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

test('B readonly can navigate with disabled editors and no removal action', async ({ page }) => {
  await open(page, 390, 'light', '&role=readonly');
  await expect(page.locator('.rule-editor:visible').getByRole('combobox').first()).toBeDisabled();
  await page.getByRole('button', { name: /^连接/ }).click();
  await expect(page.locator('.rule-editor:visible .hop-target-port input')).toBeDisabled();
  await page.getByRole('button', { name: '香港 AKILE II 的更多操作' }).click();
  await expect(page.getByRole('menuitem', { name: /从这条链移除/ })).toHaveCount(0);
});

test('B fits a narrow panel in a desktop viewport', async ({ page }) => {
  await open(page, 1920, 'dark', '&container=860');
  for (const tab of ['规则', '连接', 'DNS 解析']) {
    await page.getByRole('button', { name: new RegExp(`^${tab}`) }).click();
    await fits(page);
    await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`b-narrow-${tab}.png`);
  }
});

for (const scene of ['shared', 'reverse', 'listener']) {
  test(`B ${scene} connections retain their ownership`, async ({ page }) => {
    await open(page, 1440, 'light', `&${scene}`);
    await page.getByRole('button', { name: /^连接/ }).click();
    await expect(page.locator('.rule-editor:visible .rule-connection-path')).toHaveCount(1);
    if (scene === 'reverse') {
      await expect(page.locator('.rule-editor:visible .hop-target-port')).toContainText('本机端口');
      await expect(page.getByRole('combobox', { name: '连接路径' })).toContainText('反向 IPv6');
    } else if (scene === 'listener') {
      await expect(page.locator('.rule-editor:visible .listener-reference-panel')).toContainText('TCP 23000');
      await expect(page.locator('.rule-editor:visible .hop-target-port')).toHaveCount(0);
      await expect(page.locator('.rule-editor:visible .listener-reference-panel')).not.toContainText('不可发布');
      await expect(page.getByRole('combobox', { name: '连接路径' })).toContainText('公网 IPv4');
    }
    await expect(page.locator('.rule-sheet-card')).toHaveScreenshot(`b-${scene}-connection.png`);
  });
}

test('B keyboard adds a new machine, and restore removes its unsaved subtree', async ({ page }) => {
  await open(page, 1440);
  await page.locator('.rule-editor:visible .external-target-trigger').click();
  await page.getByRole('textbox', { name: '搜索转发目标' }).fill('日本');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(page.locator('.rule-editor:visible .external-target-trigger')).toContainText('日本 IIJ');
  await expect(page.locator('.rule-machine-tree')).toContainText('日本 IIJ');
  await page.getByRole('button', { name: '还原', exact: true }).click();
  await expect(page.locator('.rule-editor:visible .external-target-trigger')).toContainText('台湾 AKILE HiNET');
  await expect(page.locator('.rule-machine-tree')).not.toContainText('日本 IIJ');
  await expect(page.getByRole('button', { name: '保存到草稿' })).toBeDisabled();
});
