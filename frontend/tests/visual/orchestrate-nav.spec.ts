import { waitForVisualAssets } from './helpers/visual-ready';
import { test, expect, type Page } from '@playwright/test';

// 容器版把「线路」「容器」合成顶栏的「编排」组；基础版没有容器，线路仍是独立入口（见 product-variants）。
test.skip(process.env.BROCADE_PRODUCT_FLAVOR === 'base', 'base flavor keeps 线路 as a single entry');

const mainNav = (page: Page) => page.getByRole('navigation', { name: '主导航' });

async function open(page: Page, hash: string, theme = 'light') {
  await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
  await page.goto(`/tests/fixtures/navigation-preview.html?theme=${theme}#${hash}`);
  await expect(page.locator('.fg-top')).toBeVisible();
  await waitForVisualAssets(page);
}

async function fits(page: Page) {
  await expect
    .poll(() =>
      page.locator('.fg-top').evaluate(top => {
        const nav = top.querySelector('nav')!;
        return top.scrollWidth <= top.clientWidth + 1 && nav.scrollWidth <= nav.clientWidth + 1;
      }),
    )
    .toBe(true);
}

test('desktop entry opens 线路 first, switches in place and returns to the last sub-page', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/nodes');
  const main = mainNav(page);
  const group = main.getByRole('group', { name: '编排' });
  await expect(main.getByRole('button', { name: '线路', exact: true })).toHaveCount(0);
  await expect(main.getByRole('button', { name: '容器', exact: true })).toHaveCount(0);
  await expect(main).toHaveScreenshot('orchestrate-closed-1440-light.png');

  await main.getByRole('button', { name: '编排', exact: true }).click();
  await expect(page).toHaveURL(/#\/chains$/);
  await expect(group.getByRole('button', { name: '线路', exact: true })).toHaveAttribute('aria-current', 'true');
  await expect(group.getByRole('button', { name: '容器', exact: true })).toHaveAttribute('aria-current', 'false');
  // 基准记录静止态：点击后指针仍停在组按钮上，会把悬停底色带进截图。
  await page.mouse.move(720, 600);
  await expect(main).toHaveScreenshot('orchestrate-chains-1440-light.png');

  await group.getByRole('button', { name: '容器', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm$/);
  await expect(group.getByRole('button', { name: '容器', exact: true })).toHaveAttribute('aria-current', 'true');

  await main.getByRole('button', { name: '机器', exact: true }).click();
  await expect(page).toHaveURL(/#\/nodes$/);
  await expect(group.getByRole('button', { name: '容器', exact: true })).toHaveCount(0);
  await main.getByRole('button', { name: '编排', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm$/);
});

test('dark palette keeps the expanded group legible', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/cvm', 'dark');
  await expect(mainNav(page)).toHaveScreenshot('orchestrate-cvm-1440-dark.png');
});

for (const width of [1440, 390]) {
  test(`drilled pages turn the group into a way back ${width}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
    await page.goto('/tests/fixtures/chain-unlock-preview.html?scene=calm&theme=light#/chains/chain/apac/hk-direct');
    const main = mainNav(page);
    const back = main.getByRole('button', { name: '返回线路', exact: true });
    await expect(back).toBeVisible();
    await expect(main.getByRole('button', { name: '线路', exact: true })).toHaveAttribute('aria-current', 'true');
    await waitForVisualAssets(page);
    await expect(main).toHaveScreenshot(`orchestrate-drilled-${width}-light.png`);
    await back.click();
    await expect(page).toHaveURL(/#\/chains$/);
    await expect(main.getByRole('button', { name: '编排', exact: true })).toBeVisible();
  });
}

for (const width of [430, 390, 375, 360, 320]) {
  test(`phone row keeps four entries and the sub-page switch complete ${width}`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await open(page, '/chains');
    await fits(page);
    const main = mainNav(page);
    for (const name of ['机器', '线路', '容器', '用户', '用量'])
      await expect(main.getByRole('button', { name, exact: true })).toBeVisible();
    await expect(main.getByRole('button', { name: '线路', exact: true })).toHaveAttribute('aria-current', 'true');
    await expect(page.locator('.fg-top')).toHaveScreenshot(`orchestrate-phone-${width}-light.png`);
  });
}
