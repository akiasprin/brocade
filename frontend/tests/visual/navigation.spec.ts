import { test, expect, type Page } from '@playwright/test';

async function open(page: Page, path = '/nodes/node/jp-iij', query = '') {
  await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
  await page.goto(`/tests/fixtures/navigation-preview.html?${query}#${path}`);
  await expect(page.locator('.fg-top')).toBeVisible();
  await expect(page.locator('.fg-top .ft-now')).toHaveCount((await page.viewportSize())!.width > 820 ? 1 : 0);
  await expect(page.locator('.fg-top .fg-blast')).toHaveCount(
    ['pending', 'deploying', 'grant'].includes(new URLSearchParams(query).get('state') ?? '') &&
      !query.includes('role=public')
      ? 1
      : 0,
  );
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

test('ordinary pages avoid full deployment checks; the publish page still verifies', async ({ page }) => {
  const reads: string[] = [];
  await page.clock.install({ time: new Date('2026-11-02T10:00:00Z') });
  page.on('console', message => {
    if (message.text() === '[fixture] deployment-verification-read') reads.push(message.text());
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/nodes/node/jp-iij', 'state=pending');
  await expect(page.locator('.fg-blast')).toHaveText('配置待发布');
  await page.clock.runFor(11_000);
  expect(reads).toHaveLength(0);
  await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: /发布/ }).click();
  await expect.poll(() => reads.length).toBeGreaterThan(0);
});

test('published grants and new users do not advertise a configuration release', async ({ page }) => {
  const reads: string[] = [];
  const indexes: string[] = [];
  page.on('console', message => {
    if (message.text() === '[fixture] deployment-verification-read') reads.push(message.text());
    if (message.text().startsWith('[fixture] artifact-index-read ')) indexes.push(message.text());
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/nodes/node/jp-iij', 'state=grants-published&theme=light');
  await expect(page.locator('.nd-page-head')).toBeVisible();
  await expect.poll(() => indexes.length).toBe(2);
  await expect(page.locator('.fg-blast')).toHaveCount(0);
  await expect(page.locator('.fg-badge.chg')).toHaveCount(0);
  expect(reads).toHaveLength(0);
  const flavor = process.env.BROCADE_PRODUCT_FLAVOR === 'base' ? 'base' : 'cvm';
  await expect(page).toHaveScreenshot(`${flavor}-published-grants-navigation.png`, {
    clip: { x: 0, y: 0, width: 1440, height: 190 },
  });
});

for (const theme of ['light', 'dark']) {
  for (const width of [1440, 1180, 840, 390, 320]) {
    test(`D navigation ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await open(page, '/nodes/node/jp-iij', `theme=${theme}`);
      await expect(
        page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '返回机器', exact: true }),
      ).toBeVisible();
      await expect(page.locator('.fg-crumb')).toHaveCount(0);
      await expect(page.locator('.fg-top')).toHaveCSS('border-bottom-width', '0px');
      await expect(page.locator('.nd-page-head')).toBeVisible();
      await expect(page.locator('.fg-top')).not.toContainText('30 秒均值');
      await fits(page);
      await expect(page).toHaveScreenshot(`navigation-${width}-${theme}.png`, {
        clip: { x: 0, y: 0, width, height: 190 },
      });
      const detailWidth = await page.locator('nav .is-back').evaluate(button => button.getBoundingClientRect().width);
      await page.getByRole('button', { name: '返回机器', exact: true }).click();
      await expect(page).toHaveURL(/#\/nodes$/);
      await expect(page.locator('nav .is-back')).toHaveCount(0);
      expect(
        await page.locator('nav [aria-current="true"]').evaluate(button => button.getBoundingClientRect().width),
      ).toBe(detailWidth);
    });
  }
}

for (const state of ['pending', 'deploying', 'grant']) {
  test(`top bar ${state} adapts to window and artifact rail`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await open(page, '/nodes/node/jp-iij', `state=${state}&theme=light&account=administrator-with-a-long-account-name`);
    await expect(page.locator('.fg-blast')).toHaveText(
      state === 'pending' ? '配置待发布' : state === 'deploying' ? '发布 #42 · 进行中' : '权限失败 2',
    );
    await fits(page);
    await expect(page).toHaveScreenshot(`runtime-${state}.png`, { clip: { x: 0, y: 0, width: 1440, height: 80 } });
    for (const width of [1180, 1024, 840]) {
      await page.setViewportSize({ width, height: 900 });
      await fits(page);
      await expect(page.locator('.fg-blast')).toBeVisible();
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByRole('button', { name: /账户与更多功能/ }).click();
    await page.getByRole('menuitem', { name: /产物/ }).click();
    await expect(page.locator('.fg-rail')).toBeVisible();
    await fits(page);
    await expect(page.locator('.fg-blast')).toBeVisible();
  });
}

test('menu pages add one temporary desktop entry and return to their root', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/tunnels');
  await expect(page.locator('nav .fg-nv-context')).toHaveText('隧道');
  await page.locator('.tunnel-row.manual-row').filter({ hasText: '新加坡家宽 Socks' }).click();
  await expect(page.getByRole('button', { name: '返回隧道', exact: true })).toBeVisible();
  await expect(page.locator('.tnd-head')).toBeVisible();
  await expect(page.locator('.module-load-error')).toHaveCount(0);
  await fits(page);
  await expect(page).toHaveScreenshot('tunnel-detail-navigation.png', {
    clip: { x: 0, y: 0, width: 1440, height: 150 },
  });
  await page.getByRole('button', { name: '返回隧道', exact: true }).click();
  await expect(page).toHaveURL(/#\/tunnels$/);
  await page.getByRole('button', { name: /账户与更多功能/ }).click();
  await page.getByRole('menuitem', { name: '拓扑', exact: true }).click();
  await expect(page.locator('nav .fg-nv-context')).toHaveText('拓扑');
  await expect(page.locator('nav .fg-nv-context')).toHaveCount(1);
});

test('third-level wizard returns to module root', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/nodes/chain/jp-iij');
  await expect(page.getByRole('button', { name: '返回机器', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: '新建链', exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot('wizard-navigation.png', { clip: { x: 0, y: 0, width: 1440, height: 190 } });
  await page.getByRole('button', { name: '返回机器', exact: true }).click();
  await expect(page).toHaveURL(/#\/nodes$/);
});

test('desktop roster keeps its icon; mobile user details expose return', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await open(page, '/users/user/alice');
  await expect(page.locator('nav .is-back')).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('button', { name: '返回用户', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '返回用户', exact: true }).click();
  await expect(page).toHaveURL(/#\/users$/);
});

for (const role of ['readonly', 'public']) {
  test(`${role} navigation keeps viewer access and suppresses internal runtime for visitors`, async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 900 });
    await open(page, '/nodes/node/jp-iij', `role=${role}&state=grant`);
    await expect(
      page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '发布', exact: true }),
    ).toHaveCount(0);
    await expect(page.locator('.fg-top .fg-blast')).toHaveCount(role === 'public' ? 0 : 1);
    await fits(page);
  });
}

test('phone keeps containers visible and usage reachable from More for signed-in viewers and guests', async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 900 });
  await open(page, '/nodes', 'theme=light');
  await fits(page);
  const main = page.getByRole('navigation', { name: '主导航' });
  await expect(main.getByRole('button', { name: '容器', exact: true })).toBeVisible();
  await expect(main.getByRole('button', { name: '用量', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: /账户与更多功能/ }).click();
  await expect(page.getByRole('menuitem', { name: '用量', exact: true })).toBeVisible();
  await open(page, '/nodes', 'role=public');
  await fits(page);
  await expect(main.getByRole('button', { name: '容器', exact: true })).toBeVisible();
  await expect(main.getByRole('button', { name: '用量', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: '登录与更多功能', exact: true }).click();
  await expect(page.getByRole('menuitem', { name: '用量', exact: true })).toBeVisible();
});
