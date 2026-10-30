import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { mkdtemp } from 'node:fs/promises';
import { rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const cacheDir = await mkdtemp(join(tmpdir(), 'brocade-visual-vite-'));
process.once('exit', () => rmSync(cacheDir, { recursive: true, force: true }));
const server = await createServer({
  root: fileURLToPath(new URL('..', import.meta.url)),
  configFile: false,
  cacheDir,
  plugins: [react()],
  optimizeDeps: {
    entries: [
      'tests/fixtures/user-header-preview.html',
      'tests/fixtures/dns-groups-preview.html',
      'tests/fixtures/observe-focus-preview.html',
    ],
  },
  server: { host: '127.0.0.1', port: 4318, strictPort: true, watch: null },
});
for (const signal of ['SIGINT', 'SIGTERM'])
  process.once(signal, async () => {
    await server.close();
    process.exit(0);
  });
await server.listen();
