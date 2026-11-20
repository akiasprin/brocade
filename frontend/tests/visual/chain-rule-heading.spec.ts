import { waitForVisualAssets } from './helpers/visual-ready';
import { devices, expect, test, type Locator, type Page } from '@playwright/test';

for (const [width, theme] of [
  [390, 'light'],
  [1440, 'dark'],
] as const) {
  for (const role of ['system-admin', 'public']) {
    test(`chain rule headings ${role} ${width}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(
        `/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=${theme}&role=${role}#/chains/chain/apac/hk-direct`,
      );
      await page.getByRole('switch', { name: /：链路可视化$/, checked: true }).click();
      const tree = page.locator('.chain-rule-tree');
      await expect(tree.locator('.rule-editor')).toHaveCount(1);
      await expect(tree.locator('.chain-rule-node-head .who > b')).toHaveText('香港 HKIX');
      await expect(tree.locator('.rule-editor-title')).toHaveCount(0);
      await expect(tree).not.toContainText('hk-direct / hk-hkix');
      await waitForVisualAssets(page);
      await expect(tree).toHaveScreenshot(`chain-${role}-${width}.png`);
      expect(errors).toEqual([]);
    });
  }
}

async function openTwoHopRules(page: Page, role: 'public' | 'readonly' = 'public') {
  await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
  await page.goto(
    `/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=light&role=${role}#/chains/chain/apac/jp-iij`,
  );
  await page.getByRole('switch', { name: /：链路可视化$/, checked: true }).click();
  const tree = page.locator('.chain-rule-tree');
  await expect(tree.locator('.chain-rule-node-head[role="button"]')).toHaveCount(2);
  await tree.locator('.chain-rule-node-head[aria-expanded="false"]').click();
  await expect(tree.locator('.chain-rule-table:visible')).toHaveCount(2);
  await waitForVisualAssets(page);
  return tree;
}

async function expectReadonlyRuleLayout(tree: Locator) {
  const protocol = tree.locator('.hop-target-wire-value');
  await expect(protocol).toHaveText('VLESS-NONE（不加密）');
  await expect(tree.locator('.hop-target-wire select')).toHaveCount(0);
  expect(
    await protocol.evaluate(element => {
      const bounds = element.getBoundingClientRect();
      const text = document.createRange();
      text.selectNodeContents(element);
      return [...text.getClientRects()].every(
        line => line.right <= bounds.right + 1 && line.bottom <= bounds.bottom + 1,
      );
    }),
  ).toBe(true);
  const link = tree.getByRole('button', { name: '查看本机 DNS 解析策略' });
  await expect(link).toBeVisible();
  await expect
    .poll(() =>
      link.evaluate(element => {
        const bounds = element.getBoundingClientRect();
        const fields = element.closest('.rule-action-fields')!.getBoundingClientRect();
        const panel = element.closest('.node-chain-use')!.getBoundingClientRect();
        return bounds.right <= Math.min(fields.right, panel.right) + 1;
      }),
    )
    .toBe(true);
  const editorWidth = await tree
    .locator('.rule-editor')
    .first()
    .evaluate(element => parseFloat(getComputedStyle(element).width));
  if (editorWidth > 1120) {
    await expect
      .poll(() =>
        tree
          .locator('.chain-rule-table')
          .first()
          .evaluate(table => {
            const match = table.querySelector('.rule-match-kind')!.getBoundingClientRect();
            const action = table.querySelector('.rule-action-select')!.getBoundingClientRect();
            return Math.abs(match.top + match.height / 2 - (action.top + action.height / 2));
          }),
      )
      .toBeLessThanOrEqual(1);
  }
  const dialog = tree.page().getByRole('dialog', { name: /的本机 DNS 解析策略$/ });
  await link.click();
  await expect(dialog).toBeVisible();
  await tree.page().keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await link.evaluate(element => element.blur());
}

for (const width of [320, 390, 400, 430, 768, 900, 1024, 1180, 1440, 1920]) {
  test(`public rules keep inspection actions accessible at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    const tree = await openTwoHopRules(page);
    await expect(tree.getByRole('button', { name: /加一条|加 AI 组合|还原|保存到草稿/ })).toHaveCount(0);
    await expectReadonlyRuleLayout(tree);
    if ([320, 400, 1024, 1440].includes(width)) await expect(tree).toHaveScreenshot(`guest-rules-${width}.png`);
  });
}

const iphoneTest = test.extend({ browserName: 'webkit' });
const { viewport, screen, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices['iPhone 13'];
for (const orientation of ['portrait', 'landscape'] as const) {
  iphoneTest.describe(`touch WebKit visitor rules in ${orientation}`, () => {
    const landscape = orientation === 'landscape';
    const landscapeSize = { width: screen.height, height: screen.width };
    iphoneTest.use({
      viewport: landscape ? landscapeSize : viewport,
      screen: landscape ? landscapeSize : screen,
      userAgent,
      deviceScaleFactor,
      isMobile,
      hasTouch,
    });
    iphoneTest('keeps the DNS view link accessible', async ({ page }) => {
      const tree = await openTwoHopRules(page);
      await expect(tree.getByRole('button', { name: '＋ 加一条' })).toHaveCount(0);
      await expectReadonlyRuleLayout(tree);
      if (!landscape) await expect(tree).toHaveScreenshot('guest-rules-iphone.png');
    });
  });
}

test('signed-in read-only rule operators keep disabled edit affordances', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const tree = await openTwoHopRules(page, 'readonly');
  await expectReadonlyRuleLayout(tree);
  await expect(tree.getByRole('button', { name: '＋ 加一条' }).first()).toBeDisabled();
  await expect(tree.getByRole('button', { name: '保存到草稿' }).first()).toBeDisabled();
});

for (const width of [390, 1440]) {
  test(`chain rule folding ${width}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(
      '/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=light&role=public#/chains/chain/apac/jp-iij',
    );
    await page.getByRole('switch', { name: /：链路可视化$/, checked: true }).click();
    const tree = page.locator('.chain-rule-tree');
    await expect(tree.locator('.chain-rule-node-head[role="button"]')).toHaveCount(2);
    await expect(tree.locator('.chain-rule-body:not([hidden])')).toHaveCount(1);
    await expect(tree.locator('.chain-rule-node-head[role="button"]').nth(0)).toHaveAttribute('aria-expanded', 'true');
    await expect(tree.locator('.chain-rule-node-head[role="button"]').nth(1)).toHaveAttribute('aria-expanded', 'false');
    await waitForVisualAssets(page);
    await expect(tree).toHaveScreenshot(`chain-folding-${width}.png`);
  });
}
