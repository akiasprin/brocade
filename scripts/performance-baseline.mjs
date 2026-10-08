#!/usr/bin/env node
// Read-only, sequential endpoint sampling. Tokens stay in the environment, paths are never output.
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

export function percentile(values, fraction) {
  if (!values.length) return null;
  const ordered = [...values].sort((a, b) => a - b);
  return ordered[Math.max(0, Math.ceil(ordered.length * fraction) - 1)];
}

export function serverTimings(header) {
  const result = {};
  for (const part of (header ?? '').split(',')) {
    const match = /^\s*(handler|db|pool);dur=(\d+(?:\.\d+)?)\s*$/.exec(part);
    if (match) result[`${match[1]}_ms`] = Number(match[2]);
  }
  return result;
}

export async function sample(url, token) {
  const started = performance.now();
  // Refuse redirects: never forward an operator credential to another origin.
  const response = await fetch(url, {
    method: 'GET', redirect: 'error', signal: AbortSignal.timeout(30_000),
    headers: { Authorization: `Bearer ${token}`, 'Cache-Control': 'no-cache' },
  });
  const headersMs = performance.now() - started;
  const reader = response.body?.getReader();
  let bytes = 0;
  try {
    if (reader) for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > 16 * 1024 * 1024) throw new Error('response exceeds 16 MiB baseline cap');
    }
  } finally {
    if (reader) await reader.cancel();
  }
  return {
    status: response.status, headers_ms: headersMs, total_ms: performance.now() - started,
    decoded_body_bytes: bytes, server: serverTimings(response.headers.get('server-timing')),
  };
}

export async function main(env = process.env) {
  const base = new URL(env.BROCADE_BASE_URL ?? 'http://127.0.0.1:8080');
  if (!['http:', 'https:'].includes(base.protocol) || base.username || base.password || base.search || base.hash || base.pathname !== '/') {
    throw new Error('BROCADE_BASE_URL must be an HTTP(S) origin without credentials or path');
  }
  if (base.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(base.hostname)) {
    throw new Error('use HTTPS or an SSH-forwarded loopback origin');
  }
  const token = env.BROCADE_ADMIN_TOKEN;
  if (!token) throw new Error('BROCADE_ADMIN_TOKEN is required');
  const repeats = Number(env.BROCADE_BASELINE_REPEATS ?? '10');
  if (!Number.isInteger(repeats) || repeats < 2 || repeats > 30) throw new Error('repeats must be 2..30');
  const cases = [
    ['bootstrap', '/bootstrap'], ['nodes', '/nodes/agent-state'], ['model', '/model/snapshot'],
    ['ping_latest', '/ping-probe/nodes/latest'],
  ];
  // Fixed route shapes, validated IDs; no arbitrary URLs, subscriptions, streams or mutation APIs.
  if (env.BROCADE_BASELINE_NODE) {
    if (!/^[a-zA-Z0-9_-]{1,128}$/.test(env.BROCADE_BASELINE_NODE)) throw new Error('invalid baseline node ID');
    const node = encodeURIComponent(env.BROCADE_BASELINE_NODE);
    cases.push(['node_load', `/load/nodes/${node}/overview?windows=2880`],
      ['ping_24h', `/ping-probe/nodes/${node}/series?window_secs=86400`]);
  }
  const output = { schema: 1, captured_at: new Date().toISOString(),
    note: 'First request is not proof of a cold process/cache. Server timings exclude network, body streaming and browser rendering.', cases: [] };
  for (const [name, path] of cases) {
    const samples = [];
    for (let i = 0; i < repeats; i++) {
      const result = await sample(new URL(path, base), token);
      samples.push(result);
      if (result.status !== 200) throw new Error(`${name}: expected 200, received ${result.status}`);
      await new Promise(resolve => setTimeout(resolve, 200));
    }
    const warm = samples.slice(1);
    output.cases.push({ name, first: samples[0], warm_p50_ms: percentile(warm.map(s => s.total_ms), .5),
      warm_p95_ms: percentile(warm.map(s => s.total_ms), .95), samples });
  }
  process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => {
    // Network errors may contain URLs; only controlled validation/status messages are printable.
    const safe = /^(BROCADE_|use HTTPS|repeats |invalid baseline|response exceeds|\w+: expected)/.test(error.message);
    process.stderr.write(`${safe ? error.message : 'baseline failed; inspect connectivity without logging credentials'}\n`);
    process.exitCode = 1;
  });
}
