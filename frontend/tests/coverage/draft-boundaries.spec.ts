import { test, expect } from '@playwright/test';

async function openBranches(page: import('@playwright/test').Page, width: number, height: number) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(() => {
    let sequence = 0;
    // Stable branch ordering keeps screenshots independent of random UUID sorting.
    Object.defineProperty(crypto, 'randomUUID', {
      value: () => `00000000-0000-4000-8000-${String(++sequence).padStart(12, '0')}`,
    });
  });
  await page.goto('/tests/fixtures/draft-coverage-preview.html?theme=light');
  const summaries = page.locator('.fg-draft .callout details > summary');
  await expect(summaries).toHaveCount(3);
  await expect(page.getByRole('button', { name: '提交', exact: true })).toBeDisabled();
  for (const summary of await summaries.all()) await summary.click();
  return page.getByRole('button', { name: '采用这份草稿', exact: true });
}

test('three expanded branches keep every selection reachable on a short phone', async ({ page }) => {
  const choices = await openBranches(page, 390, 568);
  await expect(page).toHaveScreenshot('draft-three-short-phone.png');
  test.fail(true, 'UI-019: 固定底部冲突容器没有整体高度/滚动约束，第一份采用按钮移出视口');
  await expect(choices.first()).toBeInViewport({ ratio: 1 });
});

test('conflict notice counts all three retained versions', async ({ page }) => {
  await openBranches(page, 1440, 1000);
  const notice = page.locator('.fg-draft .callout > p');
  await expect(notice).toHaveScreenshot('draft-three-notice.png');
  test.fail(true, 'UI-020: 三份冲突仍显示“两份修改都已保留”');
  await expect(notice).not.toContainText('两份修改');
});

test('choosing a branch resolves the conflict and restores submission', async ({ page }) => {
  const choices = await openBranches(page, 1440, 1000);
  await choices.first().click();
  await expect(page.locator('.fg-draft .callout details')).toHaveCount(0);
  await expect(page.locator('.fg-draft-count')).toContainText('24 处未提交');
  await expect(page.getByRole('button', { name: '提交', exact: true })).toBeEnabled();
});
