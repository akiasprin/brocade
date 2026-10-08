import assert from 'node:assert/strict';
import test from 'node:test';
import {
  formatBytes,
  parseBaseUrl,
  parseBudget,
  parseRepeats,
  parseRoutes,
  percentile,
  summarizeSamples,
} from '../scripts/page-performance.mjs';

test('frontend baseline accepts only safe origins and known routes', () => {
  assert.equal(parseBaseUrl('https://console.example.com').origin, 'https://console.example.com');
  assert.equal(parseBaseUrl('http://127.0.0.1:4173').origin, 'http://127.0.0.1:4173');
  assert.throws(() => parseBaseUrl('http://example.net'), /HTTPS origin/);
  assert.throws(() => parseBaseUrl('https://user:secret@example.net'), /without credentials/);
  assert.throws(() => parseBaseUrl('https://example.net/admin'), /without credentials or path/);
  assert.deepEqual(
    parseRoutes('usage,nodes').map(route => route.name),
    ['usage', 'nodes'],
  );
  assert.throws(() => parseRoutes('nodes,unknown'), /unique route names/);
  assert.throws(() => parseRoutes('nodes,nodes'), /unique route names/);
});

test('repeat and budget validation bound production sampling', () => {
  assert.equal(parseRepeats(), 3);
  assert.equal(parseRepeats('10'), 10);
  assert.throws(() => parseRepeats('1'), /2 to 10/);
  assert.throws(() => parseRepeats('2.5'), /2 to 10/);
  assert.equal(parseBudget(), null);
  assert.equal(parseBudget('1500'), 1500);
  assert.throws(() => parseBudget('0'), /positive/);
});

test('route summaries calculate percentiles and rank slow API resources without query values', () => {
  const sample = (ready, apiDuration, errors = 0) => ({
    ready_ms: ready,
    ttfb_ms: 20,
    fcp_ms: 40,
    transfer_bytes: 1024,
    decoded_bytes: 2048,
    long_task_ms: 5,
    browser_errors: errors,
    http_errors: 0,
    resources: [
      {
        name: '/usage/monthly?offset',
        initiator_type: 'fetch',
        duration_ms: apiDuration,
        transfer_bytes: 400,
        decoded_bytes: 800,
      },
      {
        name: '/assets/index.js',
        initiator_type: 'script',
        duration_ms: 200,
        transfer_bytes: 200,
        decoded_bytes: 500,
      },
    ],
  });
  const result = summarizeSamples([sample(100, 30), sample(200, 90, 1), sample(150, 60)]);
  assert.equal(percentile([100, 200, 150], 0.5), 150);
  assert.equal(result.ready_p50_ms, 150);
  assert.equal(result.ready_p95_ms, 200);
  assert.equal(result.browser_errors, 1);
  assert.deepEqual(result.slowest_api, [
    { name: '/usage/monthly?offset', p50_ms: 60, p95_ms: 90, transfer_p50_bytes: 400, samples: 3 },
  ]);
});

test('human byte formatting stays compact', () => {
  assert.equal(formatBytes(800), '800 B');
  assert.equal(formatBytes(1536), '1.5 KiB');
  assert.equal(formatBytes(2 * 1024 * 1024), '2 MiB');
});
