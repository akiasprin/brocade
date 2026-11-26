import { waitForVisualAssets } from './helpers/visual-ready';
import { expect, test } from '@playwright/test';

for (const width of [390, 1440]) {
  test(`legacy Agent defaults to base at ${width}`, async ({ page }) => {
    await page.clock.install({ time: new Date('2026-10-25T12:00:00Z') });
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/tests/fixtures/ui-regressions.html?scene=agent-binary&theme=light');
    await waitForVisualAssets(page);
    for (const name of ['旧 Agent（未上报身份）', '旧 Agent（身份为空）', '基础版 Agent']) {
      await expect(page.getByRole('checkbox', { name: `升级 ${name}` })).toBeEnabled();
    }
    // 基础版 Console 只下发基础版；容器版 Agent 无法在线换回，原因写在机器名下。
    await expect(page.getByRole('checkbox', { name: '升级 容器版 Agent' })).toBeDisabled();
    await expect(page.getByText('容器版 Agent 换回基础版须停机重装')).toBeVisible();
    await expect(page.locator('.cgc-st').filter({ hasText: /^可升级$/ })).toHaveCount(3);
    await expect(page).toHaveScreenshot(`legacy-agent-base-${width}.png`);
  });
}
