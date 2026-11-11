import { defineConfig } from 'vitest/config';

export default defineConfig({
  define: { __BROCADE_CVM__: process.env.BROCADE_PRODUCT_FLAVOR !== 'base' },
  test: {
    environment: 'jsdom',
    include: ['tests/**/*.test.tsx'],
    restoreMocks: true,
    // Each worker loads the component tree and jsdom; leave room for release builds.
    maxWorkers: 4,
  },
});
