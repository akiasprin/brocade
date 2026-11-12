import { expect, test } from '@playwright/test';

for (const width of [1440, 390]) {
  for (const preference of ['default', 'saved light']) {
    test(`login ${width}: ${preference} uses light mode even with a dark system theme`, async ({ page }) => {
      await page.setViewportSize({ width, height: 844 });
      await page.emulateMedia({ colorScheme: 'dark' });
      if (preference === 'saved light')
        await page.addInitScript(() => localStorage.setItem('brocade-console:theme', 'light'));
      await page.route('**/branding', route => route.fulfill({ json: { site_name: '控制台', icon_data_url: null } }));
      await page.route('**/bootstrap', route => route.fulfill({ status: 401, json: { error: 'Unauthorized' } }));
      await page.route('**/auth/state', route => route.fulfill({ json: { initialized: true, public_open: false } }));
      await page.goto('/');
      await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible();
      await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
      await expect(page.locator('html')).toHaveAttribute('data-palette', 'dailan');
      await expect(page.locator('meta[name="color-scheme"]')).toHaveAttribute('content', 'light');
      await page.evaluate(() => document.fonts.ready);
      // The new default must render exactly like an explicit light preference.
      await expect(page).toHaveScreenshot(`login-${width}-light.png`);
    });
  }
}
