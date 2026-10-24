import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './tests/coverage',
  outputDir: '../.tools/playwright-coverage/test-results',
  reporter: [['list'], ['json', { outputFile: '../.tools/playwright-coverage/results.json' }]],
  expect: { timeout: 5000 },
  use: {
    baseURL: 'http://127.0.0.1:4326',
    actionTimeout: 7000,
    reducedMotion: 'reduce',
    locale: 'zh-CN',
    timezoneId: 'Asia/Taipei',
  },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'webkit', use: { browserName: 'webkit' } },
  ],
  snapshotPathTemplate: '{testDir}/{testFilePath}-snapshots/{arg}-{projectName}-{platform}{ext}',
  webServer: {
    command: 'node scripts/coverage-preview.mjs',
    url: 'http://127.0.0.1:4326/tests/fixtures/rule-coverage-preview.html',
    reuseExistingServer: false,
  },
});
