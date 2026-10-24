import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

const server = await createServer({
  root: fileURLToPath(new URL('..', import.meta.url)),
  configFile: false,
  plugins: [react()],
  optimizeDeps: { entries: ['tests/fixtures/user-header-preview.html'] },
  server: { host: '127.0.0.1', port: 4318, strictPort: true, watch: null },
});
await server.listen();
