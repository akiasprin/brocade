import { test, expect } from '@playwright/test';
for (const [width, height] of [
  [320, 568],
  [390, 844],
  [844, 390],
  [1440, 1000],
]) {
  test(`long confirmation scrolls inside and keeps actions at ${width}x${height}`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await page.goto(`/tests/fixtures/layer-coverage-preview.html?theme=${width === 390 ? 'light' : 'dark'}`);
    const opener = page.getByRole('button', { name: '打开确认', exact: true });
    await opener.click();
    const dialog = page.getByRole('dialog'),
      body = dialog.locator('.confirm-body');
    const action = dialog.getByRole('button', { name: '确认合成动作', exact: true });
    await expect(action).toBeInViewport({ ratio: 1 });
    expect(await body.evaluate(e => e.scrollHeight > e.clientHeight + 300)).toBe(true);
    await body.evaluate(e => (e.scrollTop = e.scrollHeight));
    await expect(dialog.getByText(/^机器 80/)).toBeInViewport({ ratio: 1 });
    for (let i = 0; i < 8; i++) {
      await page.keyboard.press('Tab');
      expect(await dialog.evaluate(e => e.contains(document.activeElement))).toBe(true);
    }
    await expect(dialog.locator('.dialog-surface')).toHaveScreenshot(`confirm-${width}x${height}.png`);
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(opener).toBeFocused();
    await opener.click();
    await action.click();
    await expect(page.getByText('已执行合成动作')).toBeVisible();
  });
  test(`rollout 64 rows has bounded popover and viewport resize at ${width}x${height}`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await page.goto('/tests/fixtures/layer-coverage-preview.html?theme=light');
    const opener = page.locator('button.source-ban-rollout');
    await opener.click();
    const surface = page.locator('.source-ban-nodes');
    await expect(surface).toBeInViewport({ ratio: 1 });
    expect(await surface.evaluate(e => e.scrollWidth <= e.clientWidth + 1)).toBe(true);
    await surface.evaluate(e => (e.scrollTop = e.scrollHeight));
    await expect(surface.locator('li').last()).toBeInViewport({ ratio: 1 });
    await expect(surface).toHaveScreenshot(`rollout-${width}x${height}.png`);
    await page.setViewportSize({ width: Math.max(320, width - 80), height: Math.max(320, height - 80) });
    await expect(surface).toBeInViewport({ ratio: 1 });
    await page.keyboard.press('Escape');
    await expect(opener).toBeFocused();
  });
}
test('long error URL remains inside its callout', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/tests/fixtures/layer-coverage-preview.html?error&theme=light');
  const callout = page.locator('.callout.err');
  await expect(callout).toHaveScreenshot('error-url.png');
  test.fail(true, 'UI-014: ErrorBox 长 URL 没有换行约束，文字越过提示块');
  expect(await callout.evaluate(e => e.scrollWidth <= e.clientWidth + 1)).toBe(true);
});
