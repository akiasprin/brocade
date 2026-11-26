import { expect, test } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { HTTP_ORIGIN, serveHttpFixtures, expectInsecureHttp } from './helpers/http-origin';

const fixture = '/tests/fixtures/user-header-preview.html?theme=light';

test.beforeEach(async ({ page }) => {
  await serveHttpFixtures(page);
  await page.goto(HTTP_ORIGIN + fixture);
  await expect(page.locator('.user-account-state')).toBeVisible();
  await expectInsecureHttp(page);
});

test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: 'wait' });
});

test('HTTP copy writes the full UUID and preserves keyboard focus', async ({ page, context }) => {
  const button = page.locator('.user-dhead .user-uuid-copy');
  await button.click();
  await expect(button).toHaveAttribute('data-copy-state', 'done');
  await expect(button).toBeFocused();
  // Read the resulting system clipboard from a trusted loopback page. The page
  // under test has no clipboard API and must use its real legacy copy path.
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: 'http://127.0.0.1:4318' });
  const reader = await context.newPage();
  await reader.goto('http://127.0.0.1:4318' + fixture);
  expect(await reader.evaluate(() => navigator.clipboard.readText())).toBe('00000000-0000-4000-8000-000000000000');
  await reader.close();
});

test('HTTP custom rules can be added and saved with a generated identity', async ({ page }) => {
  await page.getByRole('button', { name: '订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
  await dialog.getByRole('button', { name: '规则', exact: true }).click();
  const input = dialog.getByRole('textbox', { name: '添加域名、IP 段或进程' });
  await input.fill('http-check.example.invalid');
  await input.press('Enter');
  await dialog.getByRole('button', { name: '保存规则', exact: true }).click();
  await expect(dialog.getByText('规则已保存，更新订阅后生效。', { exact: true })).toBeVisible();
  const custom = await page.evaluate(async () => {
    const response = await fetch('/users/preview/alice/clash-rules');
    return (await response.json()).rules.custom as { id: string; value: string }[];
  });
  const saved = custom.find(rule => rule.value === 'http-check.example.invalid');
  expect(saved?.id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
});

test('HTTP subscription QR export downloads a readable SVG', async ({ page }) => {
  await page.getByRole('button', { name: '订阅', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'alice 的订阅与节点' });
  await expect(dialog.getByRole('img', { name: 'Clash 订阅地址二维码' })).toBeVisible();
  const downloaded = page.waitForEvent('download');
  await dialog.getByRole('button', { name: '下载二维码', exact: true }).click();
  const download = await downloaded;
  expect(download.suggestedFilename()).toBe('clash-subscription.svg');
  expect(await download.failure()).toBeNull();
  const svg = await readFile((await download.path())!, 'utf8');
  expect(svg).toContain('<svg');
  expect(svg).toContain('<path');
  expect(svg).not.toContain('var(--');
});

test('HTTP model identities retain secure randomness and collision checks', async ({ page }) => {
  const ids = await page.evaluate(async () => {
    const model = await import(new URL('/src/model-id.ts', location.origin).href);
    const first = model.modelIdPair();
    const second = model.modelIdPair(new Set([first.ingressId, first.chainId]));
    return {
      first,
      second,
      app: model.appId(),
      custom: model.tunnelId(),
      warp: model.warpTunnelId(),
      vpngate: model.vpngateTunnelId(),
    };
  });
  expect(ids.first.ingressId).toMatch(/^ing-[0-9a-f]{4}$/);
  expect(ids.first.chainId).toMatch(new RegExp(`^chn-${ids.first.ingressId.slice(4)}-[0-9a-f]{4}$`));
  expect(ids.second.ingressId).not.toBe(ids.first.ingressId);
  expect(ids.second.chainId).not.toBe(ids.first.chainId);
  expect(ids.app).toMatch(/^app-[0-9a-f]{4}$/);
  expect(ids.custom).toMatch(/^custom-[0-9a-f]{4}-[0-9a-f]{4}$/);
  expect(ids.warp).toMatch(/^warp-[0-9a-f]{4}-[0-9a-f]{4}$/);
  expect(ids.vpngate).toMatch(/^vpngate-[0-9a-f]{4}-[0-9a-f]{4}$/);
});
