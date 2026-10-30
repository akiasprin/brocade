import { test, expect } from '@playwright/test';
import { build } from 'vite';
import react from '@vitejs/plugin-react';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, basename } from 'node:path';
let output: string;
test.beforeAll(async () => {
  output = await mkdtemp(join(tmpdir(), 'brocade-module-recovery-'));
  await build({
    configFile: false,
    root: resolve('.'),
    plugins: [react()],
    logLevel: 'silent',
    build: { outDir: output, emptyOutDir: true },
  });
});
test.afterAll(async () => {
  if (output) await rm(output, { recursive: true, force: true });
});
for (const target of ['shell', 'nodes'] as const)
  for (const persistent of [false, true])
    test(`production ${target}: ${persistent ? 'persistent failure remains readable' : 'one failed request recovers'}`, async ({
      page,
    }) => {
      const who = {
        operator_id: 'recovery-test',
        role: 'system-admin',
        tenant_scope: null,
        token_prefix: null,
        masked_assets: false,
      };
      let attempts = 0,
        navigations = 0;
      await page.setViewportSize({ width: 390, height: 844 });
      await page.route('**/*', async route => {
        const request = route.request(),
          path = new URL(request.url()).pathname;
        if (request.isNavigationRequest()) {
          navigations++;
          await route.fulfill({ body: await readFile(join(output, 'index.html')), contentType: 'text/html' });
          return;
        }
        if (path.startsWith('/assets/')) {
          if (basename(path).startsWith(`${target}-`) && path.endsWith('.js')) {
            attempts++;
            if (persistent || attempts === 1) {
              await route.abort('failed');
              return;
            }
          }
          try {
            await route.fulfill({
              body: await readFile(join(output, 'assets', basename(path))),
              contentType: path.endsWith('.css') ? 'text/css' : 'text/javascript',
            });
          } catch {
            await route.fulfill({ status: 404, body: '' });
          }
          return;
        }
        const body: Record<string, unknown> = {
          '/bootstrap': { who, initial: { node_count: 1, chain_group_count: [] } },
          '/auth/state': { initialized: true, public_open: false },
          '/auth/whoami': who,
          '/notifications': {
            events: [],
            active: [],
            retention_days: 7,
            cleared_through_event_id: 0,
            latest_event_id: 0,
            last_seen_event_id: 0,
            unread_count: 0,
          },
          '/branding': { site_name: '回归控制台', icon_data_url: null },
          '/nodes/agent-state': { nodes: [] },
          '/model/snapshot': {
            snapshot: { revision: 1, nodes: [], apps: [], external_outbounds: [] },
            node_egress_dns: [],
            redacted: false,
          },
          '/revisions': { current_revision: null, revisions: [] },
          '/deployments': { deployments: [] },
          '/deployments/active': { deployments: [] },
          '/settings': {},
          '/tenants': { tenants: [] },
        };
        await route.fulfill(
          path in body ? { json: body[path] } : { status: 404, json: { error: `Unexpected fixture request ${path}` } },
        );
      });
      await page.goto('/module-recovery#/nodes');
      await expect.poll(() => attempts).toBeGreaterThanOrEqual(2);
      if (persistent) {
        await expect(page.getByRole('alert').filter({ hasText: '这部分界面未能加载' })).toBeVisible();
        await expect(page.getByRole('button', { name: '重新加载界面' })).toBeVisible();
        await expect(page.getByRole('status')).toContainText('界面仍无法加载');
        await expect(page).toHaveScreenshot(`production-${target}-failure.png`);
      } else {
        await expect(page.locator('.forge')).toBeVisible();
        await expect(page.locator('.module-load-error')).toHaveCount(0);
      }
      expect(navigations).toBe(2);
    });
