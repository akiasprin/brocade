import { expect, test } from '@playwright/test';
import { HTTP_ORIGIN, serveHttpFixtures, expectInsecureHttp } from './helpers/http-origin';

test('maintenance commands work on an actual insecure HTTP origin', async ({ page }) => {
  const errors: string[] = [];
  const commands: { kind: string; body: { request_id: string } }[] = [];
  page.on('pageerror', error => errors.push(error.message));
  // Fetch fixture assets through loopback while the document keeps a non-localhost
  // HTTP origin. The browser itself withholds randomUUID; no Crypto API is mocked.
  await serveHttpFixtures(page);
  await page.goto(`${HTTP_ORIGIN}/tests/fixtures/ip-maintenance-preview.html?scene=waiting`);
  const card = page.locator('.care-config');
  await expect(card).toHaveAttribute('aria-busy', 'false');
  await expectInsecureHttp(page);
  expect(await page.evaluate(() => typeof crypto.getRandomValues)).toBe('function');
  await page.exposeFunction('recordMaintenanceCommand', (kind: string, body: { request_id: string }) => {
    commands.push({ kind, body });
  });
  await page.evaluate(() => {
    const fetch = window.fetch;
    window.fetch = async (input, init) => {
      const kind = String(input).match(/\/ip-maintenance\/(run|stop|session-reset)$/)?.[1];
      if (kind && init?.method === 'POST') {
        await (
          window as unknown as {
            recordMaintenanceCommand: (kind: string, body: unknown) => Promise<void>;
          }
        ).recordMaintenanceCommand(kind, JSON.parse(String(init.body)));
      }
      return fetch(input, init);
    };
  });
  await card.getByRole('button', { name: '立即执行', exact: true }).click();
  await expect.poll(() => commands.map(command => command.kind)).toEqual(['run']);
  await card.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(() => commands.map(command => command.kind)).toEqual(['run', 'stop']);
  await card.getByRole('button', { name: '策略', exact: true }).click();
  const policy = page.getByRole('dialog', { name: '养护策略', exact: true });
  const reset = policy.getByRole('button', { name: '重置养护会话', exact: true });
  await expect(reset).toBeEnabled();
  await reset.click();
  await page
    .getByRole('dialog', { name: '重置养护会话', exact: true })
    .getByRole('button', { name: '确认重置', exact: true })
    .click();
  await expect.poll(() => commands.map(command => command.kind)).toEqual(['run', 'stop', 'session-reset']);
  for (const { body } of commands)
    expect(body.request_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(new Set(commands.map(command => command.body.request_id)).size).toBe(3);
  await expect(page.locator('.care-config .callout.err')).toHaveCount(0);
  expect(errors).toEqual([]);
});
