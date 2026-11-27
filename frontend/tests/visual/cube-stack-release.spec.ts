import { expect, test } from '@playwright/test';
import {
  STACK_COLD_PAIR_CONTRACT,
  STACK_FRESH_INSTALL_CONTRACT,
  type StackRelease,
  type StackReleaseView,
} from '../../src/api/cvm-stack-releases';
import { waitForVisualAssets } from './helpers/visual-ready';

const time = new Date('2026-11-04T10:30:00Z');
const now = time.getTime() / 1_000;
const source = '9a3d1c7e'.repeat(8);
const target = '3fe2b61a'.repeat(8);
const pairId = '2f03bd11'.repeat(8);
const installId = 'e4a90c13'.repeat(8);
const cubelet = 'af03d149'.repeat(8);
const shim = '61fc41ca'.repeat(8);
const installed = {
  bundle_sha256: source,
  cubelet_sha256: cubelet,
  shim_sha256: shim,
  storage_backend: 'xfs',
  observed_at: now - 30,
} as const;
const release: StackRelease = {
  id: 17,
  node_id: 'jp-iij',
  source_sha256: source,
  target_sha256: target,
  status: 'ready',
  start_deadline_at: now + 7_200,
  roots_sha256: '18c24b5a'.repeat(8),
};
const recovered: StackRelease = {
  ...release,
  id: 15,
  node_id: 'tw-hinet',
  status: 'recovered',
  error: '卷根摘要与预检记录不一致，已恢复来源包。',
};
const history: StackRelease[] = [recovered, { ...release, id: 12, node_id: 'jp-ntt', status: 'succeeded' }];
const data: StackReleaseView = {
  pairs: [
    {
      id: pairId,
      contract: STACK_COLD_PAIR_CONTRACT,
      source_sha256: source,
      source_cubelet_sha256: cubelet,
      source_shim_sha256: shim,
      target_sha256: target,
      storage_backend: 'xfs',
    },
    { id: installId, contract: STACK_FRESH_INSTALL_CONTRACT, target_sha256: target, storage_backend: 'xfs' },
  ],
  hosts: [
    { ...installed, node_id: 'jp-iij' },
    { ...installed, node_id: 'hk-hkix', observed_at: now - 12 },
    { ...installed, node_id: 'tw-hinet', observed_at: now - 42 },
    { ...installed, node_id: 'de-fra', observed_at: now - 360 },
    { ...installed, node_id: 'jp-ntt', bundle_sha256: target },
    { node_id: 'us-gia', installable: 'xfs', observed_at: now - 55 },
  ],
  releases: [release, recovered, history[1]],
};

test.skip(process.env.BROCADE_PRODUCT_FLAVOR === 'base', 'Cube runtime releases require the CVM frontend');

test('Cube stack release empty state on mobile', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 760 });
  await page.clock.setFixedTime(time);
  await page.route('**/cvm/stack-releases', route => route.fulfill({ json: { pairs: [], hosts: [], releases: [] } }));
  await page.goto('/tests/fixtures/navigation-preview.html?stack-release&theme=dark#/deploy');
  await page.getByRole('tab', { name: 'Cube' }).click();
  await expect(page.getByText(/暂无有效的宿主运行栈上报/)).toBeVisible();
  await expect(page.getByRole('button', { name: '发布历史' })).toHaveAttribute('aria-expanded', 'false');
  await waitForVisualAssets(page);
  await expect(page).toHaveScreenshot('empty-mobile-dark.png');
});

for (const profile of [
  { width: 1440, height: 1000, theme: 'light', mode: '升级', host: '香港 HKIX', nodeId: 'hk-hkix', pair: pairId },
  { width: 390, height: 844, theme: 'dark', mode: '安装', host: '洛杉矶 CN2 GIA', nodeId: 'us-gia', pair: installId },
]) {
  test(`Cube stack release ${profile.width} ${profile.theme}`, async ({ page }) => {
    await page.setViewportSize({ width: profile.width, height: profile.height });
    await page.clock.setFixedTime(time);
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    let releases = data.releases;
    let reads = 0;
    let historyReads = 0;
    let creates = 0;
    let cancels = 0;
    const actualDeadline = now + 18_180;
    await page.route('**/cvm/stack-releases', route => {
      const request = route.request();
      if (request.method() === 'GET') {
        reads++;
        return route.fulfill({ json: { ...data, releases } });
      }
      expect(request.method()).toBe('POST');
      expect(request.postDataJSON()).toEqual({ node_id: profile.nodeId, pair_id: profile.pair });
      const created: StackRelease = {
        ...release,
        id: ++creates + release.id,
        node_id: profile.nodeId,
        source_sha256: profile.mode === '安装' ? null : source,
        status: 'preparing',
        start_deadline_at: actualDeadline,
        roots_sha256: null,
      };
      releases = [created, ...releases];
      return route.fulfill({ json: created });
    });
    await page.route('**/cvm/stack-releases/history', route => {
      expect(route.request().method()).toBe('GET');
      historyReads++;
      return route.fulfill({ json: history });
    });
    await page.route('**/cvm/stack-releases/17/cancel', route => {
      expect(route.request().method()).toBe('POST');
      cancels++;
      const cancelled: StackRelease = { ...release, status: 'cancelled' };
      releases = releases.map(item => (item.id === release.id ? cancelled : item));
      return route.fulfill({ json: cancelled });
    });
    await page.goto(`/tests/fixtures/navigation-preview.html?stack-release&theme=${profile.theme}#/deploy`);
    const tab = page.getByRole('tab', { name: 'Cube' });
    await expect(tab).toBeVisible();
    expect(reads).toBe(0);
    expect(await tab.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    await tab.click();
    await expect(page.getByRole('button', { name: '取消发布 #17' })).toBeVisible();
    await expect(page.getByRole('button', { name: `${profile.mode} Cube` })).toBeEnabled();
    expect(historyReads).toBe(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await waitForVisualAssets(page);
    await expect(page).toHaveScreenshot(`overview-${profile.width}-${profile.theme}.png`, { fullPage: true });

    await page.getByRole('button', { name: `${profile.mode} Cube` }).click();
    const toolbar = page.getByRole('group', { name: `${profile.mode}范围` });
    await page.getByRole('radio', { name: `${profile.mode} ${profile.host}` }).check();
    await expect(toolbar).toContainText('维护窗口 5 小时，预计');
    if (profile.mode === '安装') await expect(toolbar).not.toContainText('停止');
    else await expect(toolbar).toContainText('等待回执');
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await expect(page.getByRole('region', { name: 'Cube 宿主' })).toHaveScreenshot(
      `scope-${profile.width}-${profile.theme}.png`,
    );
    await toolbar.getByRole('button', { name: profile.mode, exact: true }).click();
    const submitted = page.getByRole('status').filter({ hasText: '维护窗口 5 小时' });
    await expect(submitted.locator('time')).toHaveAttribute('datetime', new Date(actualDeadline * 1_000).toISOString());
    await expect(submitted).not.toContainText('预计');
    expect(creates).toBe(1);
    await page.getByRole('button', { name: '取消发布 #17' }).click();
    await expect(page.getByText('#17 已取消')).toBeVisible();
    expect(cancels).toBe(1);
    expect(historyReads).toBe(0);

    await page.getByRole('button', { name: '发布历史' }).click();
    const records = page.getByRole('region', { name: '运行栈发布记录' });
    await records.getByRole('button', { name: /#15/ }).click();
    await expect(records.getByText('卷根 SHA-256')).toBeVisible();
    expect(historyReads).toBe(1);
    await page.mouse.move(0, 0);
    await expect(records).toHaveScreenshot(`history-${profile.width}-${profile.theme}.png`);
    await page.getByRole('button', { name: '收起发布历史' }).click();
    await expect(records.getByRole('button', { name: /#15/ })).toHaveCount(0);
    expect(errors).toEqual([]);
  });
}
