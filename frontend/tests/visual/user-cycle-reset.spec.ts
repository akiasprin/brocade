import { test, expect, type Locator } from '@playwright/test';

/**
 * 「重置」在「本期接入 / 本期转发」那一行：与标签文字行居中，文字右缘（扣掉悬停面内距）对齐读数栏内容右缘。
 * 两列读数栏里合计列会被拉高，标签盒随之变高，所以量的是标签首行而不是整个盒。
 */
async function expectResetInLabelRow(ledger: Locator) {
  const action = ledger.getByRole('button', { name: '重置', exact: true });
  await expect(action).toBeVisible();
  const offsets = await action.evaluate(button => {
    const ledger = button.closest('.user-usage-ledger')!;
    const style = getComputedStyle(ledger);
    const contentRight =
      ledger.getBoundingClientRect().right - parseFloat(style.borderRightWidth) - parseFloat(style.paddingRight);
    const box = button.getBoundingClientRect();
    const label = ledger.querySelector('.user-usage-hero-label')!;
    const labelLine = label.getBoundingClientRect().top + parseFloat(getComputedStyle(label).lineHeight) / 2;
    return {
      right: contentRight - (box.right - parseFloat(getComputedStyle(button).paddingRight)),
      middle: box.top + box.height / 2 - labelLine,
    };
  });
  expect(Math.abs(offsets.right)).toBeLessThanOrEqual(1);
  expect(Math.abs(offsets.middle)).toBeLessThanOrEqual(1);
}

for (const width of [390, 1440])
  for (const theme of ['dark', 'light'])
    test(`user cycle reset ${width} ${theme}`, async ({ page }) => {
      await page.clock.setFixedTime(new Date('2026-10-29T07:00:00Z'));
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`/tests/fixtures/user-header-preview.html?user=alice&theme=${theme}`);
      const card = page.locator('.user-usage-card:not(.user-forwarding-usage)');
      await expect(card.getByRole('button', { name: '重置', exact: true })).toBeEnabled();
      await expectResetInLabelRow(card.locator('.user-usage-ledger'));
      await expect(card).toHaveScreenshot(`user-cycle-reset-action-${width}-${theme}.png`);
      await card.getByRole('button', { name: '重置', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'alice 的本期用量归零' });
      await expect(dialog).toBeVisible();
      await expect(dialog).toHaveScreenshot(`user-cycle-reset-confirm-${width}-${theme}.png`);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      await dialog.getByRole('button', { name: '取消', exact: true }).click();
      await expect(dialog).toBeHidden();
      await page.getByRole('tab', { name: '转发', exact: true }).click();
      await expectResetInLabelRow(page.locator('.user-forwarding-usage .user-usage-ledger'));
    });

// 卡宽 401–620px 时读数栏分两列：「重置」落在右列组成条上方，不能压住组成条。
test('user cycle reset clears the composition column in a two-column ledger', async ({ page }) => {
  await page.setViewportSize({ width: 560, height: 1100 });
  await page.goto('/tests/fixtures/user-header-preview.html?user=alice');
  const ledger = page.locator('.user-usage-card:not(.user-forwarding-usage) .user-usage-ledger');
  await expectResetInLabelRow(ledger);
  const layout = await ledger.evaluate(element => ({
    columns: getComputedStyle(element).gridTemplateColumns.split(' ').length,
    actionBottom: element.querySelector('.user-cycle-reset-action')!.getBoundingClientRect().bottom,
    composeTop: element.querySelector('.user-usage-compose')!.getBoundingClientRect().top,
  }));
  expect(layout.columns).toBe(2);
  expect(layout.actionBottom).toBeLessThanOrEqual(layout.composeTop);
});

test('readonly user detail has no reset action', async ({ page }) => {
  await page.goto('/tests/fixtures/user-header-preview.html?user=alice&role=readonly');
  await expect(page.locator('.user-usage-card:not(.user-forwarding-usage)')).toBeVisible();
  await expect(page.getByRole('button', { name: '重置', exact: true })).toHaveCount(0);
});
