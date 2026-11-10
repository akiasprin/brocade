import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
const server = await createServer({
  root: fileURLToPath(new URL('..', import.meta.url)),
  configFile: false,
  define: { __BROCADE_CVM__: process.env.BROCADE_PRODUCT_FLAVOR !== 'base' },
  plugins: [react()],
  optimizeDeps: { entries: ['tests/fixtures/rule-coverage-preview.html'] },
  server: { host: '127.0.0.1', port: 4326, strictPort: true, watch: null },
});
await server.listen();
