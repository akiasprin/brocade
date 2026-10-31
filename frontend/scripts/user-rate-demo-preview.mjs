import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { mkdtemp } from 'node:fs/promises';
import { rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const cacheDir = await mkdtemp(join(tmpdir(), 'brocade-user-rate-demo-'));
process.once('exit', () => rmSync(cacheDir, { recursive: true, force: true }));
const server = await createServer({
  root: fileURLToPath(new URL('..', import.meta.url)),
  configFile: false,
  cacheDir,
  plugins: [react()],
  optimizeDeps: { entries: ['tests/fixtures/user-app-rate-demo.html'] },
  server: { host: '127.0.0.1', port: 5176, strictPort: true, watch: null },
});
for (const signal of ['SIGINT', 'SIGTERM'])
  process.once(signal, async () => {
    await server.close();
    process.exit(0);
  });
await server.listen();
console.log('User group rate demo: http://127.0.0.1:5176/tests/fixtures/user-app-rate-demo.html');
