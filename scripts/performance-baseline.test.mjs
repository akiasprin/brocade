import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { main, percentile, sample, serverTimings } from './performance-baseline.mjs';

test('percentiles use observed values and timings only accept numeric allowlisted fields', () => {
  assert.equal(percentile([4, 1, 3, 2], .5), 2);
  assert.equal(percentile([], .95), null);
  assert.deepEqual(serverTimings('handler;dur=1.25, db;dur=0.5, secret;dur=42, pool;dur=NaN'),
    { handler_ms: 1.25, db_ms: .5 });
});

test('credentials require HTTPS except for a forwarded loopback origin', async () => {
  await assert.rejects(main({ BROCADE_BASE_URL: 'http://example.net', BROCADE_ADMIN_TOKEN: 'secret' }), /HTTPS/);
  await assert.rejects(main({ BROCADE_BASE_URL: 'https://user:secret@example.net' }), /without credentials/);
  await assert.rejects(main({ BROCADE_ADMIN_TOKEN: 'secret', BROCADE_BASELINE_NODE: '../private' }), /invalid baseline/);
});

test('sample uses GET, counts the body without retaining it, and refuses redirects', async () => {
  const server = createServer((request, response) => {
    assert.equal(request.method, 'GET');
    assert.equal(request.headers.authorization, 'Bearer test-secret');
    if (request.url === '/redirect') {
      response.writeHead(302, { Location: 'http://example.invalid/secret' }).end();
    } else {
      response.writeHead(200, { 'Server-Timing': 'handler;dur=12, db;dur=5, pool;dur=1' });
      response.end('private-payload');
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    const result = await sample(base, 'test-secret');
    assert.equal(result.status, 200);
    assert.equal(result.decoded_body_bytes, 15);
    assert.deepEqual(result.server, { handler_ms: 12, db_ms: 5, pool_ms: 1 });
    assert.ok(!JSON.stringify(result).includes('private'));
    await assert.rejects(sample(`${base}/redirect`, 'test-secret'));
  } finally {
    await new Promise(resolve => server.close(resolve));
  }
});
