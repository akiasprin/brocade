import { expect, test } from '@playwright/test';
import { STACK_COLD_PAIR_CONTRACT, type StackRelease, type StackReleaseView } from '../../src/api/cvm-stack-releases';
import { waitForVisualAssets } from './helpers/visual-ready';

const time = new Date('2026-11-04T10:30:00Z');
const now = time.getTime() / 1_000;
const source = '9a3d1c7e'.repeat(8);
const target = '3fe2b61a'.repeat(8);
const pairId = '2f03bd11'.repeat(8);
const cubelet = 'af03d149'.repeat(8);
const shim = '61fc41ca'.repeat(8);

test.skip(process.env.BROCADE_PRODUCT_FLAVOR === 'base', 'Cube runtime releases require the CVM frontend');
const release: StackRelease = {
  id: 17,
  node_id: 'jp-iij',
  source_sha256: source,
  target_sha256: target,
  status: 'ready',
  expires_at: now + 7_200,
  roots_sha256: '18c24b5a'.repeat(8),
};
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
      expires_at: now + 43_200,
    },
  ],
  hosts: [
    {
      node_id: 'jp-iij',
      bundle_sha256: source,
      cubelet_sha256: cubelet,
      shim_sha256: shim,
      storage_backend: 'xfs',
      observed_at: now - 30,
    },
  ],
  releases: [release],
};

for (const profile of [
  { width: 1440, height: 1000, theme: 'light' },
  { width: 320, height: 760, theme: 'dark' },
]) {
  test(`Cube stack release ${profile.width} ${profile.theme}`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: profile.width, height: profile.height });
    await page.clock.setFixedTime(time);
    let approvals = 0;
    let reads = 0;
    await page.route('**/cvm/stack-releases', route => {
      expect(route.request().method()).toBe('GET');
      reads++;
      return route.fulfill({ json: approvals ? { ...data, releases: [{ ...release, status: 'applying' }] } : data });
    });
    await page.route('**/cvm/stack-releases/17/approve', route => {
      expect(route.request().method()).toBe('POST');
      approvals++;
      return route.fulfill({ json: { ...release, status: 'approved' } });
    });
    await page.goto(`/tests/fixtures/navigation-preview.html?stack-release&theme=${profile.theme}#/deploy`);
    const tab = page.getByRole('tab', { name: 'Cube 运行栈' });
    await expect(tab).toBeVisible();
    expect(reads).toBe(0);
    expect(await tab.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    await tab.click();
    await page.getByLabel('目标宿主').selectOption('jp-iij');
    await page.getByLabel('兼容运行包对').selectOption(pairId);
    await expect(page.getByRole('button', { name: '批准冷切换' })).toBeEnabled();
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await waitForVisualAssets(page);
    await testInfo.attach('Cube runtime release', { body: await page.screenshot(), contentType: 'image/png' });

    await page.getByRole('button', { name: '批准冷切换' }).click();
    const dialog = page.getByRole('dialog', { name: '批准冷切换 #17' });
    await expect(dialog.getByRole('button', { name: '确认批准' })).toBeDisabled();
    if (profile.width === 320)
      await testInfo.attach('Cube cold switch approval', { body: await dialog.screenshot(), contentType: 'image/png' });
    await dialog.getByRole('checkbox').check();
    await dialog.getByRole('button', { name: '确认批准' }).click();
    await expect(page.getByText('正在切换')).toBeVisible();
    await expect(page.getByRole('button', { name: '取消发布' })).toHaveCount(0);
    expect(approvals).toBe(1);
  });
}
