import { expect, type Page } from '@playwright/test';

export const HTTP_ORIGIN = 'http://brocade-http.test';

/** Keep the browser on an untrusted HTTP origin while serving local production fixtures. */
export async function serveHttpFixtures(page: Page) {
  await page.route(`${HTTP_ORIGIN}/**`, async route => {
    const url = new URL(route.request().url());
    url.host = '127.0.0.1:4318';
    await route.fulfill({ response: await route.fetch({ url: url.toString() }) });
  });
}

export async function expectInsecureHttp(page: Page) {
  expect(await page.evaluate(() => isSecureContext)).toBe(false);
  expect(await page.evaluate(() => typeof crypto.randomUUID)).toBe('undefined');
  expect(await page.evaluate(() => typeof navigator.clipboard)).toBe('undefined');
}
