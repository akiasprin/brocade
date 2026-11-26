import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/visual',
  outputDir: '../.tools/frontend-visual/results',
  reporter: [['list'], ['html', { outputFolder: '../.tools/frontend-visual/report', open: 'never' }]],
  // A full 12-worker WebKit/Chromium run can keep a fixture's loading boundary active past the
  // default five seconds while Vite transforms a large module. Assertions remain strict; only
  // the time allowed for the production surface to become ready is increased.
  expect: { timeout: 10_000 },
  use: {
    baseURL: 'http://127.0.0.1:4318',
    browserName: 'chromium',
    reducedMotion: 'reduce',
    locale: 'zh-CN',
    timezoneId: 'Asia/Taipei',
  },
  webServer: {
    command: 'node scripts/visual-preview.mjs',
    gracefulShutdown: { signal: 'SIGTERM', timeout: 5000 },
    url: 'http://127.0.0.1:4318/tests/fixtures/user-header-preview.html',
    reuseExistingServer: false,
  },
});
