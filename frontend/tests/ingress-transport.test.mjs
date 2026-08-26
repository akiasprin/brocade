import assert from 'node:assert/strict';
import test from 'node:test';
import { compatibleXhttpMode, projectionForTransport, transportKindFor } from '../src/ingress-transport.ts';

const split = {
  v4: {
    host: 'upload.example.net',
    port: 443,
    download: {
      host: 'cdn.example.net',
      port: 443,
      origin_port: 8443,
      http_host: 'download.route.example',
      mux: 24,
    },
  },
};

test('switching between XHTTP security shapes preserves the public download', () => {
  assert.deepEqual(projectionForTransport(split, 'vless-tls-xhttp'), {
    v4: {
      host: 'upload.example.net',
      port: 443,
      download: {
        host: 'cdn.example.net',
        port: 443,
        origin_port: null,
        http_host: 'download.route.example',
        mux: 24,
      },
    },
  });
  assert.deepEqual(projectionForTransport(split, 'vless-reality-xhttp'), split);
});

test('leaving XHTTP removes its unsupported independent download', () => {
  assert.deepEqual(projectionForTransport(split, 'vless-reality'), {
    v4: { host: 'upload.example.net', port: 443, download: null },
  });
});

test('an independent download cannot retain stream-one', () => {
  assert.equal(compatibleXhttpMode('stream-one', true), 'stream-up');
  assert.equal(compatibleXhttpMode('stream-one', false), 'stream-one');
  assert.equal(compatibleXhttpMode('auto', true), 'auto');
});

test('security and network selectors map only to supported transport shapes', () => {
  assert.equal(transportKindFor('reality', false), 'vless-reality');
  assert.equal(transportKindFor('reality', true), 'vless-reality-xhttp');
  assert.equal(transportKindFor('tls', false), 'vless-tls');
  assert.equal(transportKindFor('tls', true), 'vless-tls-xhttp');
});
