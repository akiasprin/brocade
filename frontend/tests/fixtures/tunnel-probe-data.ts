import type { ExternalOutbound, TunnelProbeHealthReason, TunnelProbeRun, TunnelProbeView } from '../../src/api';

export const probeTunnel: ExternalOutbound = {
  id: 'custom-5f2a-9c1e',
  tenant: 'platform',
  name: '新加坡 家宽出口',
  address: 'sg-home.example.net',
  port: 443,
  protocol: {
    t: 'vless',
    v: { credential: '<redacted>', encryption: 'none', flow: 'xtls-rprx-vision', transport: { t: 'raw' } },
  },
  security: {
    t: 'reality',
    v: {
      server_name: 'www.example.net',
      fingerprint: 'chrome',
      public_key: 'A'.repeat(43),
      short_id: '6ba85179e30d4fc2',
    },
  },
  bindings: [],
};

/** Synthetic API responses for production-component tests. Sampling mirrors the Store query:
 * epoch-aligned buckets, failure first, newest among equal outcomes. */
export function probeView(windowSecs = 86400, scene = 'healthy'): TunnelProbeView {
  const now = Math.floor(Date.now() / 1000);
  const all: TunnelProbeRun[] = [];
  for (let index = 0; index < 2016; index++) {
    const at = now - 604800 + 90 + index * 300;
    const ago = now - at;
    if (scene === 'empty' || (scene === 'stale' && ago < 3600) || (ago > 39600 && ago < 42600)) continue;
    const failed = scene === 'all-failed' || (scene === 'down' && ago < 1200) || (ago > 21600 && ago < 22500);
    const ttfb =
      index % 91 === 0
        ? 620
        : scene === 'slow' && ago < 3600
          ? 340 + (index % 180)
          : 155 + Math.round((65 * (1 + Math.sin(index / 38))) / 2) + (index % 17);
    all.push({
      id: index + 1,
      tenant_id: 'platform',
      outbound_id: probeTunnel.id,
      outbound_name: probeTunnel.name,
      protocol: 'vless',
      trigger: 'scheduled',
      source: 'serving',
      topology_revision: 41,
      serving_generation: 17,
      settings_revision: 3,
      draft_sha256: null,
      timeout_secs: 10,
      status: failed ? 'failed' : 'succeeded',
      phase: 'finished',
      result: failed ? 'connect-failed' : 'ok',
      ttfb_ms: failed ? null : ttfb,
      http_status: failed ? null : 200,
      exit_ip: failed ? null : '203.0.113.209',
      exit_loc: failed ? null : 'SG',
      attempt_count: 1,
      error_code: failed ? 'connect-failed' : null,
      error_detail: failed ? 'dial tcp 198.51.100.200:443: connection refused' : null,
      queued_at_unix_secs: at - 3,
      started_at_unix_secs: at - 2,
      finished_at_unix_secs: at,
      cancel_requested: false,
    });
  }
  const latest = all.at(-1) ?? null;
  if (latest && scene !== 'all-failed' && scene !== 'stale') {
    all.push({
      ...latest,
      id: 3000,
      trigger: 'manual',
      finished_at_unix_secs: now - 1500,
      queued_at_unix_secs: now - 1503,
    });
    all.push({
      ...latest,
      id: 3001,
      trigger: 'manual',
      source: 'draft',
      serving_generation: null,
      draft_sha256: 'a'.repeat(64),
      finished_at_unix_secs: now - 2700,
      queued_at_unix_secs: now - 2703,
    });
  }
  const runs = all.filter(run => run.source === 'serving' && run.finished_at_unix_secs! >= now - windowSecs);
  const successful = runs.filter(run => run.result === 'ok');
  const sorted = successful.map(run => run.ttfb_ms!).sort((a, b) => a - b);
  const percentile = (q: number) => sorted[Math.max(0, Math.ceil(sorted.length * q) - 1)] ?? null;
  const bucketSecs = Math.max(60, Math.floor(windowSecs / 240));
  const buckets = new Map<number, TunnelProbeRun>();
  for (const run of [...runs].sort((a, b) => a.finished_at_unix_secs! - b.finished_at_unix_secs!)) {
    const id = Math.floor(run.finished_at_unix_secs! / bucketSecs);
    const previous = buckets.get(id);
    if (!previous || run.result !== 'ok' || previous.result === 'ok') buckets.set(id, run);
  }
  const health =
    scene === 'empty' || scene === 'stale'
      ? 'unknown'
      : scene === 'paused'
        ? 'paused'
        : scene === 'down' || scene === 'all-failed'
          ? 'down'
          : scene === 'slow'
            ? 'degraded'
            : 'healthy';
  const reason: TunnelProbeHealthReason =
    scene === 'empty'
      ? { kind: 'no-samples' }
      : scene === 'stale'
        ? { kind: 'stale', latest_finished_at_unix_secs: latest?.finished_at_unix_secs ?? null }
        : scene === 'paused'
          ? { kind: 'not-scheduled' }
          : health === 'down'
            ? { kind: 'consecutive-failures', count: 4, latest: 'connect-failed' }
            : health === 'degraded'
              ? { kind: 'slow', max_ttfb_ms: 510, threshold_ms: 300, samples: 5 }
              : { kind: 'healthy', max_ttfb_ms: 240, samples: 5 };
  if (scene === 'running' && latest)
    all.push({
      ...latest,
      id: 4000,
      trigger: 'manual',
      status: 'running',
      phase: 'requesting',
      result: null,
      ttfb_ms: null,
      http_status: null,
      exit_ip: null,
      exit_loc: null,
      finished_at_unix_secs: null,
      queued_at_unix_secs: now - 5,
      started_at_unix_secs: now - 3,
    });
  const ordered = [...all].sort((a, b) => b.queued_at_unix_secs - a.queued_at_unix_secs);
  return {
    item: {
      tenant_id: 'platform',
      outbound_id: probeTunnel.id,
      name: probeTunnel.name,
      protocol: 'vless',
      supported: true,
      unsupported_reason: null,
      health,
      health_reason: reason,
      latest_run: ordered[0] ?? null,
      policy: {
        enabled: scene !== 'paused',
        interval_secs: 300,
        timeout_secs: 10,
        next_run_at_unix_secs: now + 90,
        updated_at_unix_secs: now - 604800,
      },
    },
    retention_days: 7,
    slow_ttfb_ms: 300,
    bucket_secs: bucketSecs,
    summary: {
      window_secs: windowSecs,
      total: runs.length,
      succeeded: successful.length,
      success_rate: runs.length ? successful.length / runs.length : null,
      p50_ms: percentile(0.5),
      p95_ms: percentile(0.95),
      failures: runs.length - successful.length,
      timeouts: 0,
      connect_failures: runs.length - successful.length,
      target_failures: 0,
      exit_ips: successful.length ? 1 : 0,
    },
    points: [...buckets.values()]
      .map(run => ({
        run_id: run.id,
        finished_at_unix_secs: run.finished_at_unix_secs!,
        result: run.result!,
        ttfb_ms: run.ttfb_ms,
      }))
      .sort((a, b) => a.finished_at_unix_secs - b.finished_at_unix_secs),
    recent_runs: ordered.slice(0, 20),
  };
}
