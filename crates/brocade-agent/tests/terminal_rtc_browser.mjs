import { chromium } from '../../../frontend/node_modules/@playwright/test/index.mjs';
import ts from '../../../frontend/node_modules/typescript/lib/typescript.js';
import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';

const origin = process.argv[2];
const source = await readFile(
  new URL('../../../frontend/src/panes/cvm/terminal-transport.ts', import.meta.url),
  'utf8',
);
const code = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
}).outputText;
const timer = setTimeout(() => {
  console.error('browser RTC proof timed out');
  process.exit(1);
}, 30000);
const browser = await chromium.launch({ headless: true, executablePath: process.env.BROCADE_TEST_CHROMIUM });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.goto(origin);
  const result = await page.evaluate(
    async ({ origin, code }) => {
      class Control extends EventTarget {
        static OPEN = 1;
        static CLOSED = 3;
        readyState = 0;
        bufferedAmount = 0;
        binaryType = 'arraybuffer';
        sent = [];
        constructor() {
          super();
          globalThis.testControl = this;
          queueMicrotask(() => {
            this.readyState = 1;
            this.onopen?.(new Event('open'));
          });
        }
        send(text) {
          this.sent.push(text);
          const value = JSON.parse(text);
          if (value.type === 'control_probe')
            queueMicrotask(() =>
              this.onmessage?.(
                new MessageEvent('message', { data: JSON.stringify({ type: 'control_grant', nonce: value.nonce }) }),
              ),
            );
          if (value.rtc_offer)
            void fetch(`${origin}/offer`, { method: 'POST', body: value.rtc_offer })
              .then(r => r.text())
              .then(sdp =>
                this.onmessage?.(new MessageEvent('message', { data: JSON.stringify({ type: 'rtc_answer', sdp }) })),
              );
        }
        close() {
          if (this.readyState === 3) return;
          this.readyState = 3;
          this.onclose?.(new CloseEvent('close'));
        }
      }
      globalThis.WebSocket = Control;
      const { terminalTransport } = await import(`data:text/javascript;base64,${btoa(code)}`);
      const routes = [];
      let ready = 0;
      const credentials = { ticket: 'synthetic-ticket', webrtc: { supported: true, stun_servers: [] } };
      const socket = terminalTransport(new URL(`${origin.replace('http', 'ws')}/control`), credentials, {
        direct: true,
        route: kind => routes.push(kind),
        directReady: () => ready++,
        directLost: () => {},
        permissionDenied: () => {
          throw new Error('permission denied');
        },
      });
      socket.onopen = () => socket.send(JSON.stringify({ ticket: credentials.ticket }));
      const expected = new Uint8Array(16384);
      for (let i = 0; i < expected.length; i++) expected[i] = i % 251;
      const received = [];
      await new Promise((resolve, reject) => {
        const deadline = setTimeout(() => reject(new Error('RTC round trip timeout')), 15000);
        const start = setInterval(() => {
          if (routes[0] !== 'webrtc') return;
          clearInterval(start);
          socket.send(expected);
          socket.send(JSON.stringify({ type: 'resize', cols: 100, rows: 40 }));
        }, 10);
        socket.onmessage = event => {
          received.push(typeof event.data === 'string' ? event.data : Array.from(new Uint8Array(event.data)));
          if (received.length === 2) {
            clearTimeout(deadline);
            resolve();
          }
        };
        socket.onclose = () => reject(new Error('unexpected close'));
      });
      const controls = globalThis.testControl.sent.map(value => JSON.parse(value));
      socket.onclose = null;
      socket.close();
      return { routes, ready, received, controls };
    },
    { origin, code },
  );
  assert.deepEqual(errors, []);
  assert.deepEqual(result.routes, ['webrtc']);
  assert.equal(result.ready, 1);
  assert.equal(result.controls.filter(value => value.ticket).length, 1);
  assert.ok(result.controls.every(value => value.ticket || value.type === 'control_probe'));
  assert.ok(result.controls[0].rtc_offer);
  assert.deepEqual(
    result.received[0],
    Array.from({ length: 16384 }, (_, i) => i % 251),
  );
  assert.equal(result.received[1], JSON.stringify({ type: 'resize', cols: 100, rows: 40 }));
  console.log(
    JSON.stringify({
      status: 'passed',
      browser: 'Chromium',
      adapter: 'production',
      peer: 'Rust str0m/RustCrypto',
      binary_bytes: 16384,
      ordered_control: true,
      console_frames: result.controls.length,
      terminal_frames_over_console: 0,
    }),
  );
} finally {
  await browser.close();
  clearTimeout(timer);
}
