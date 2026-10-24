import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/visual',
  outputDir: '../.tools/frontend-visual/results',
  reporter: [['list'], ['html', { outputFolder: '../.tools/frontend-visual/report', open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:4318',
    browserName: 'chromium',
    reducedMotion: 'reduce',
    locale: 'zh-CN',
    timezoneId: 'Asia/Taipei',
  },
  webServer: {
    command: 'node scripts/visual-preview.mjs',
    url: 'http://127.0.0.1:4318/tests/fixtures/user-header-preview.html',
    reuseExistingServer: false,
  },
});
